//! The Places card's worker on Windows: the lettered drives, an eject for
//! the removable ones, and Explorer for a server
//! (`plans/other-platforms/04-windows.md` W4.18, W4.19).
//!
//! Windows letters a disk as it is plugged in, so the card lists the letters
//! and nothing waits to be mounted: `Mount` is answered that Windows does it,
//! and `Unmount` that there is no such thing, only an eject. A listing asks
//! `GetLogicalDrives` for the letters and each root for its type, volume
//! label, file system and size; a mapped network letter is a share, with
//! the server it is mapped to (`WNetGetConnectionW`). A drive with no medium
//! in it — an empty card reader, a CD drive with no disc — is not listed:
//! there is nothing on it to go to. The rows themselves are
//! [`crate::platform::drives::rows`], where the mapping is tested.
//!
//! Ejecting is what Explorer's Eject does, one `DeviceIoControl` at a time
//! on the volume: lock it (nothing else has a file open on it), dismount it
//! (the file system lets go), allow removal, eject. Each step's refusal is
//! the answer, in the system's words. A mapped share is put away with
//! `WNetCancelConnection2W`.
//!
//! Connecting hands `\\server\share` to Explorer (`ShellExecuteW` "open"),
//! which asks for a password in its own dialog when the server wants one,
//! and the connect then waits up to [`CONNECT_WAIT`] for the share to answer,
//! so the window goes into it without a network timeout on its own thread.
//! No phone is listed and no `gio` is ever run.
#![allow(unsafe_code)] // the drive and volume queries, WNet and the eject's DeviceIoControl; each call says why it holds

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use df_core::fs::Notifier;
use windows_sys::Win32::NetworkManagement::WNet::{WNetCancelConnection2W, WNetGetConnectionW};
use windows_sys::Win32::Storage::FileSystem::{
    GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW, FILE_SHARE_READ,
    FILE_SHARE_WRITE,
};
use windows_sys::Win32::System::Ioctl::{
    FSCTL_DISMOUNT_VOLUME, FSCTL_LOCK_VOLUME, IOCTL_STORAGE_EJECT_MEDIA,
    IOCTL_STORAGE_MEDIA_REMOVAL,
};
use windows_sys::Win32::System::IO::DeviceIoControl;

use crate::mounts::{Answer, Connected, Event, Gio, Reply, Request};
use crate::platform::drives::{self, Drive, Kind};

/// What a phone's mount is answered: there is no such thing here.
const NOT_HERE: &str = "Not available on this platform";

/// What `Mount` is answered: a drive is lettered as it is plugged in.
const MOUNTS_ITSELF: &str = "Windows mounts drives itself";

/// What `Unmount` is answered.
const NO_UNMOUNT: &str = "Windows has no unmount; use Eject for removable drives";

/// How long a connect waits for its share to answer: time for Explorer to
/// reach the server and for a person to type a password into its dialog.
const CONNECT_WAIT: Duration = Duration::from_secs(10);

/// No mount asks questions in a terminal here: Explorer asks in its own
/// dialog.
pub const TERMINAL_MOUNT: Option<&str> = None;

/// Nothing connects here, so no connect comes back unseen.
pub const CONNECT_UNSEEN: Option<&str> = None;

/// A `gio` that is never there: it answers every run with "unsupported".
/// Nothing on this platform runs it.
pub fn system_gio() -> Gio {
    Arc::new(|_args: &[&str]| Err(std::io::Error::from(std::io::ErrorKind::Unsupported)))
}

/// The worker: a listing, an eject or a share put away for each request,
/// each answer rung once on `notify`.
pub fn run(requests: Receiver<Request>, replies: Sender<Answer>, notify: Notifier, _gio: Gio) {
    for request in requests {
        let reply = match &request {
            Request::List => {
                let (devices, shares) = drives::rows(list());
                Reply::Listing {
                    devices,
                    phones: Vec::new(),
                    shares,
                }
            }
            Request::Mount(_) => Reply::Failed(MOUNTS_ITSELF.to_string()),
            Request::Unmount(_) => Reply::Failed(NO_UNMOUNT.to_string()),
            Request::Eject { drive, .. } => match eject(drive) {
                Ok(()) => Reply::Ejected,
                Err(message) => Reply::Failed(message),
            },
            Request::GioUnmount(url) => {
                let (_, shares) = drives::rows(list());
                match shares.into_iter().find(|share| share.url == *url) {
                    Some(share) => match disconnect(&share.path) {
                        Ok(()) => Reply::Unmounted,
                        Err(message) => Reply::Failed(message),
                    },
                    None => Reply::Failed(format!("{url} is not mapped to a drive")),
                }
            }
        };
        let _ = replies.send(Answer { to: request, reply });
        notify();
    }
}

/// Connect to a server's share through Explorer, and go there once it
/// answers. `Mounted(None)` when it has not answered in [`CONNECT_WAIT`]:
/// Explorer may still be asking for a password.
pub fn connect(url: &str, _gio: &Gio) -> Connected {
    let unc = match drives::unc_of(url) {
        Ok(unc) => unc,
        Err(why) => return Connected::Failed(why.to_string()),
    };
    if let Err(error) = super::open::shell_open(OsStr::new(&unc)) {
        return Connected::Failed(format!("Explorer could not open {unc}: {error}"));
    }
    let path = PathBuf::from(&unc);
    let started = Instant::now();
    while started.elapsed() < CONNECT_WAIT {
        if path.is_dir() {
            return Connected::Mounted(Some(path));
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Connected::Mounted(None)
}

/// Mounting a phone or a camera: not here.
pub fn mount_gio(_root: &str, _gio: &Gio) -> Connected {
    Connected::Failed(NOT_HERE.to_string())
}

/// The watcher that hears a phone arrive. There is none here: no value of
/// this type exists, because [`Monitor::start`] never makes one.
pub enum Monitor {}

impl Monitor {
    pub fn start(_notify: Notifier) -> Option<Monitor> {
        None
    }

    pub fn gone(&mut self) -> bool {
        match *self {}
    }

    pub fn started(&self) -> Instant {
        match *self {}
    }

    pub fn drain(&self) -> Vec<Event> {
        match *self {}
    }
}

/// `text` as a NUL-terminated wide string.
fn wide(text: &str) -> Vec<u16> {
    OsStr::new(text).encode_wide().chain(Some(0)).collect()
}

/// Every lettered drive with something on it, in letter order.
fn list() -> Vec<Drive> {
    // SAFETY: no arguments; a bit mask of the letters in use.
    let letters = unsafe { GetLogicalDrives() };
    (0..26u8)
        .filter(|bit| letters & (1 << bit) != 0)
        .filter_map(|bit| describe(char::from(b'A' + bit)))
        .collect()
}

/// One letter, if it is a kind the card lists and has a medium in it.
fn describe(letter: char) -> Option<Drive> {
    let root = wide(&format!("{letter}:\\"));
    // SAFETY: a NUL-terminated root that outlives the call.
    let kind = Kind::of(unsafe { GetDriveTypeW(root.as_ptr()) })?;
    if kind == Kind::Remote {
        return Some(Drive {
            letter,
            kind,
            label: String::new(),
            fs: String::new(),
            size: 0,
            unc: mapped_to(letter),
        });
    }
    let mut name = [0u16; 261];
    let mut fs = [0u16; 261];
    // SAFETY: both buffers are live and as long as the sizes passed; the
    // outputs this does not want are null, which the call allows.
    let read = unsafe {
        GetVolumeInformationW(
            root.as_ptr(),
            name.as_mut_ptr(),
            name.len() as u32,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            fs.as_mut_ptr(),
            fs.len() as u32,
        )
    };
    // No volume to read: an empty reader or drive, which is not listed.
    if read == 0 {
        return None;
    }
    let mut total = 0u64;
    // SAFETY: the root outlives the call and the one output wanted is a
    // live u64; the others are null, which the call allows.
    unsafe {
        GetDiskFreeSpaceExW(
            root.as_ptr(),
            std::ptr::null_mut(),
            &mut total,
            std::ptr::null_mut(),
        )
    };
    Some(Drive {
        letter,
        kind,
        label: text_of(&name),
        fs: text_of(&fs),
        size: total,
        unc: None,
    })
}

/// A wide buffer's text, up to its NUL.
fn text_of(buffer: &[u16]) -> String {
    let end = buffer.iter().position(|&u| u == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}

/// The server a mapped letter points at, `\\server\share`.
fn mapped_to(letter: char) -> Option<String> {
    let local = wide(&format!("{letter}:"));
    let mut remote = [0u16; 1024];
    let mut len = remote.len() as u32;
    // SAFETY: the name outlives the call, the buffer is live and `len` says
    // how long it is.
    let error = unsafe { WNetGetConnectionW(local.as_ptr(), remote.as_mut_ptr(), &mut len) };
    (error == 0).then(|| text_of(&remote))
}

/// Eject the removable drive lettered `drive` (`E:`): lock, dismount, allow
/// removal, eject, as Explorer does.
fn eject(drive: &str) -> Result<(), String> {
    let volume = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(format!(r"\\.\{drive}"))
        .map_err(|error| format!("{drive} could not be opened to eject: {error}"))?;
    let handle = volume.as_raw_handle() as isize;
    let control = |code: u32, input: &[u8], what: &str| -> Result<(), String> {
        let mut returned = 0u32;
        // SAFETY: the handle is the open volume's, alive for `volume`'s
        // scope; the input is a live buffer of the length passed, and no
        // output is asked for.
        let done = unsafe {
            DeviceIoControl(
                handle,
                code,
                if input.is_empty() {
                    std::ptr::null()
                } else {
                    input.as_ptr().cast()
                },
                input.len() as u32,
                std::ptr::null_mut(),
                0,
                &mut returned,
                std::ptr::null_mut(),
            )
        };
        if done == 0 {
            return Err(format!(
                "{drive} could not be {what}: {}",
                std::io::Error::last_os_error()
            ));
        }
        Ok(())
    };
    control(
        FSCTL_LOCK_VOLUME,
        &[],
        "locked — a program has a file open on it",
    )?;
    control(FSCTL_DISMOUNT_VOLUME, &[], "dismounted")?;
    // PREVENT_MEDIA_REMOVAL { PreventMediaRemoval: FALSE }: one byte.
    control(IOCTL_STORAGE_MEDIA_REMOVAL, &[0], "released")?;
    control(IOCTL_STORAGE_EJECT_MEDIA, &[], "ejected")?;
    Ok(())
}

/// Put away the share mapped at `root` (`Z:\`).
fn disconnect(root: &Path) -> Result<(), String> {
    let letter = root.to_string_lossy().chars().next().unwrap_or('?');
    let name = wide(&format!("{letter}:"));
    // SAFETY: the name outlives the call; no flags, not forced, so a share
    // with a file open on it is refused rather than cut off.
    let error = unsafe { WNetCancelConnection2W(name.as_ptr(), 0, 0) };
    if error == 0 {
        Ok(())
    } else {
        Err(format!(
            "{letter}: could not be disconnected: {}",
            std::io::Error::from_raw_os_error(error as i32)
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The runner's system drive is lettered, fixed, NTFS, and has a size;
    /// the listing is in letter order.
    #[test]
    fn the_system_drive_is_listed() {
        let drives = list();
        let system = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".to_string());
        let letter = system.chars().next().expect("a letter");
        let found = drives
            .iter()
            .find(|drive| drive.letter.eq_ignore_ascii_case(&letter))
            .unwrap_or_else(|| panic!("{system} is not in {drives:?}"));
        assert_eq!(found.kind, Kind::Fixed);
        assert!(found.size > 0, "{found:?}");
        assert!(!found.fs.is_empty(), "{found:?}");
        let letters: Vec<char> = drives.iter().map(|d| d.letter).collect();
        let mut sorted = letters.clone();
        sorted.sort_unstable();
        assert_eq!(letters, sorted);
        let (devices, _) = drives::rows(drives);
        assert!(devices
            .iter()
            .any(|d| d.mount.as_deref() == Some(Path::new(&format!("{letter}:\\")))));
    }

    #[test]
    fn mount_and_unmount_say_what_windows_does_instead() {
        let (ask, requests) = crossbeam_channel::unbounded();
        let (replies, answers) = crossbeam_channel::unbounded();
        ask.send(Request::Mount("C:".to_string())).expect("sent");
        ask.send(Request::Unmount("C:".to_string())).expect("sent");
        drop(ask);
        run(requests, replies, Arc::new(|| {}), system_gio());
        let said: Vec<String> = answers
            .iter()
            .map(|answer| match answer.reply {
                Reply::Failed(message) => message,
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(said, [MOUNTS_ITSELF, NO_UNMOUNT]);
        assert!(matches!(
            connect("sftp://host/srv", &system_gio()),
            Connected::Failed(message) if message == "Windows opens smb:// shares only"
        ));
    }
}
