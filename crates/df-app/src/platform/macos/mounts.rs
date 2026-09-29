//! The Places card's worker on macOS: the volumes the system has mounted,
//! and `NSWorkspace` to put one away or to connect to a server.
//!
//! macOS mounts a disk itself as it is plugged in, so the card lists what is
//! mounted and nothing else — there is no unmounted stick to offer, and a
//! `Mount` request is answered that the system does it. A listing is
//! `NSFileManager`'s mounted volumes, hidden ones skipped (the system's own
//! `Data`, `Preboot` and the rest), each with the resource values the row
//! needs and `statfs` for where it was mounted from; the local ones are
//! disks and the rest shares ([`crate::platform::volumes::rows`], where the
//! mapping is and is tested). Unmounting and ejecting are one call on macOS,
//! `unmountAndEjectDeviceAtURL:error:`, and a share is put away the same
//! way, found by its address in a fresh listing.
//!
//! Connecting hands the address to Finder (`NSWorkspace.openURL`), which is
//! macOS's own connect flow: it asks for a password in its own dialog when
//! the server wants one, and mounts the share under `/Volumes`. What comes
//! back is where the share appeared, when a new entry turns up there within
//! [`CONNECT_WAIT`]. No phone or camera is listed — nothing mounts one as a
//! directory here — and no `gio` is ever run.
//!
//! **Unsafe.** The Foundation and AppKit calls are `unsafe` in objc2 0.2's
//! generated bindings, and `statfs` is a C call into a buffer this file owns.
//! Each is sound for the reason written at it.

#![allow(unsafe_code)] // NSFileManager, NSWorkspace and statfs; see the essay.

use std::ffi::{CStr, CString, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use df_core::fs::Notifier;
use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::{AnyClass, AnyObject};
use objc2::ClassType;
use objc2_app_kit::NSWorkspace;
use objc2_foundation::{
    NSArray, NSCopying, NSFileManager, NSNumber, NSString, NSURLResourceKey,
    NSURLVolumeIsEjectableKey, NSURLVolumeIsLocalKey, NSURLVolumeIsRemovableKey,
    NSURLVolumeLocalizedFormatDescriptionKey, NSURLVolumeNameKey, NSURLVolumeTotalCapacityKey,
    NSURLVolumeURLForRemountingKey, NSVolumeEnumerationOptions, NSURL,
};

use crate::mounts::{Answer, Connected, Event, Gio, Reply, Request};
use crate::platform::volumes::{self, Route, Volume};

/// What a phone's mount is answered: there is no such thing here.
const NOT_HERE: &str = "Not available on this platform";

/// What `Mount` is answered: a disk is mounted as it is plugged in.
const MOUNTS_ITSELF: &str = "macOS mounts disks itself";

/// How long a connect waits for its share to turn up under `/Volumes`: time
/// for Finder to reach the server and for a person to type a password into
/// its dialog, and no longer than a task should sit on the pool.
const CONNECT_WAIT: Duration = Duration::from_secs(10);

/// Where Finder mounts a server's share.
const VOLUMES: &str = "/Volumes";

/// No mount asks questions in a terminal here: Finder asks in its own dialog.
pub const TERMINAL_MOUNT: Option<&str> = None;

/// What a connect says when nothing new appeared under `/Volumes` in
/// [`CONNECT_WAIT`]: Finder was handed the address, and whether it connected
/// is behind a dialog this program cannot see, so it does not say it did
/// (02-macos.md M2.15). A share that does appear is "Connected to" as
/// anywhere.
pub const CONNECT_UNSEEN: Option<&str> = Some("Finder was asked to connect to");

/// A `gio` that is never there: it answers every run with "unsupported".
/// Nothing on this platform runs it.
pub fn system_gio() -> Gio {
    Arc::new(|_args: &[&str]| Err(std::io::Error::from(std::io::ErrorKind::Unsupported)))
}

/// The worker: a listing, or an unmount, for each request, each answer rung
/// once on `notify`.
pub fn run(requests: Receiver<Request>, replies: Sender<Answer>, notify: Notifier, _gio: Gio) {
    for request in requests {
        let reply = match &request {
            Request::List => {
                let (devices, shares) = volumes::rows(list());
                Reply::Listing {
                    devices,
                    phones: Vec::new(),
                    shares,
                }
            }
            Request::Mount(_) => Reply::Failed(MOUNTS_ITSELF.to_string()),
            Request::Unmount(object) => match eject(Path::new(object)) {
                Ok(()) => Reply::Unmounted,
                Err(message) => Reply::Failed(message),
            },
            Request::Eject { object, .. } => match eject(Path::new(object)) {
                Ok(()) => Reply::Ejected,
                Err(message) => Reply::Failed(message),
            },
            Request::GioUnmount(url) => {
                let (_, shares) = volumes::rows(list());
                match shares.into_iter().find(|share| share.url == *url) {
                    Some(share) => match eject(&share.path) {
                        Ok(()) => Reply::Unmounted,
                        Err(message) => Reply::Failed(message),
                    },
                    None => Reply::Failed(format!("{url} is not mounted")),
                }
            }
        };
        let _ = replies.send(Answer { to: request, reply });
        notify();
    }
}

/// Connect to a server through Finder, and say where its share appeared.
///
/// `Mounted(None)` when nothing new turned up under `/Volumes` in
/// [`CONNECT_WAIT`]: Finder may still be asking for a password, or the share
/// was mounted already; either way the card lists it once it is there, and
/// the window says Finder was asked rather than that it connected
/// ([`CONNECT_UNSEEN`]).
pub fn connect(url: &str, _gio: &Gio) -> Connected {
    if let Route::Refused(why) = volumes::route(url) {
        return Connected::Failed(why.to_string());
    }
    let before = entries(Path::new(VOLUMES));
    let opened = autoreleasepool(|_| {
        // SAFETY: a class method taking a live string; `None` for a string
        // that is not a URL.
        let Some(address) = (unsafe { NSURL::URLWithString(&NSString::from_str(url)) }) else {
            return false;
        };
        // SAFETY: the shared workspace, asked to open a live URL. It may be
        // asked from any thread.
        unsafe { NSWorkspace::sharedWorkspace().openURL(&address) }
    });
    if !opened {
        return Connected::Failed(format!("Finder could not open {url}"));
    }
    let started = Instant::now();
    while started.elapsed() < CONNECT_WAIT {
        if let Some(name) = volumes::appeared(&before, &entries(Path::new(VOLUMES))) {
            return Connected::Mounted(Some(Path::new(VOLUMES).join(name)));
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

/// The names in a directory, or none when it cannot be read.
fn entries(dir: &Path) -> Vec<OsString> {
    std::fs::read_dir(dir)
        .map(|entries| entries.flatten().map(|entry| entry.file_name()).collect())
        .unwrap_or_default()
}

/// Unmount the volume mounted at `path`, and eject what it is on when that
/// can be ejected.
fn eject(path: &Path) -> Result<(), String> {
    autoreleasepool(|_| {
        let url = file_url(path)?;
        // SAFETY: the shared workspace, asked about a live URL. It may be
        // asked from any thread.
        unsafe { NSWorkspace::sharedWorkspace().unmountAndEjectDeviceAtURL_error(&url) }
            .map_err(|error| error.localizedDescription().to_string())
    })
}

fn file_url(path: &Path) -> Result<Retained<NSURL>, String> {
    let text = path
        .to_str()
        .ok_or_else(|| format!("{}: not a name macOS can be asked about", path.display()))?;
    // SAFETY: a class method taking a live string.
    Ok(unsafe { NSURL::fileURLWithPath(&NSString::from_str(text)) })
}

/// Every mounted volume the system does not hide, in its order.
fn list() -> Vec<Volume> {
    autoreleasepool(|_| {
        // SAFETY: the resource keys are constant strings Foundation exports.
        let keys: Retained<NSArray<NSURLResourceKey>> = unsafe {
            NSArray::from_vec(vec![
                NSURLVolumeNameKey.copy(),
                NSURLVolumeTotalCapacityKey.copy(),
                NSURLVolumeIsRemovableKey.copy(),
                NSURLVolumeIsEjectableKey.copy(),
                NSURLVolumeIsLocalKey.copy(),
                NSURLVolumeLocalizedFormatDescriptionKey.copy(),
                NSURLVolumeURLForRemountingKey.copy(),
            ])
        };
        // SAFETY: the shared file manager, which may be asked from any
        // thread, with live keys and a constant option.
        let urls = unsafe {
            NSFileManager::defaultManager().mountedVolumeURLsIncludingResourceValuesForKeys_options(
                Some(&keys),
                NSVolumeEnumerationOptions::NSVolumeEnumerationSkipHiddenVolumes,
            )
        };
        let Some(urls) = urls else {
            return Vec::new();
        };
        urls.iter().filter_map(|url| volume(url, &keys)).collect()
    })
}

/// One volume's row values, or `None` for a URL that is not a path.
fn volume(url: &NSURL, keys: &NSArray<NSURLResourceKey>) -> Option<Volume> {
    // SAFETY: reads of a live URL's path and resource values; each answers
    // `None` or an error rather than failing.
    let path = PathBuf::from(unsafe { url.path() }?.to_string());
    let values = unsafe { url.resourceValuesForKeys_error(keys) }.ok()?;
    let (from, fs_type) = mounted_from(&path).unwrap_or_default();
    // SAFETY: the keys are constant strings Foundation exports.
    let key = |key: &'static NSURLResourceKey| values.get(key);
    let (name, format, size, removable, ejectable, local, remount) = unsafe {
        (
            key(NSURLVolumeNameKey).and_then(string),
            key(NSURLVolumeLocalizedFormatDescriptionKey).and_then(string),
            key(NSURLVolumeTotalCapacityKey).and_then(number),
            key(NSURLVolumeIsRemovableKey).and_then(number),
            key(NSURLVolumeIsEjectableKey).and_then(number),
            key(NSURLVolumeIsLocalKey).and_then(number),
            key(NSURLVolumeURLForRemountingKey).and_then(url_string),
        )
    };
    Some(Volume {
        path,
        name: name.unwrap_or_default(),
        format: format.unwrap_or_default(),
        size: size.map_or(0, |n| n.as_u64()),
        removable: removable.is_some_and(|n| n.as_bool()),
        ejectable: ejectable.is_some_and(|n| n.as_bool()),
        // A volume that does not say is taken as this machine's own: a share
        // always says it is not.
        local: local.is_none_or(|n| n.as_bool()),
        from,
        remount,
        fs_type,
    })
}

/// Whether `object` is an instance of `class` or of a subclass of it.
fn is_a(object: &AnyObject, class: &AnyClass) -> bool {
    let mut at = Some(object.class());
    while let Some(current) = at {
        if std::ptr::eq(current, class) {
            return true;
        }
        at = current.superclass();
    }
    false
}

/// A resource value that is a string, as one.
fn string(value: &AnyObject) -> Option<String> {
    is_a(value, NSString::class()).then(|| {
        // SAFETY: just checked to be an `NSString`.
        let text: &NSString = unsafe { &*(value as *const AnyObject).cast::<NSString>() };
        text.to_string()
    })
}

/// A resource value that is a number, as one.
fn number(value: &AnyObject) -> Option<&NSNumber> {
    is_a(value, NSNumber::class()).then(|| {
        // SAFETY: just checked to be an `NSNumber`, and borrowed from the
        // dictionary that holds it.
        unsafe { &*(value as *const AnyObject).cast::<NSNumber>() }
    })
}

/// A resource value that is a URL, as its address.
fn url_string(value: &AnyObject) -> Option<String> {
    if !is_a(value, NSURL::class()) {
        return None;
    }
    // SAFETY: just checked to be an `NSURL`.
    let url: &NSURL = unsafe { &*(value as *const AnyObject).cast::<NSURL>() };
    // SAFETY: a read of a live URL's address.
    unsafe { url.absoluteString() }.map(|text| text.to_string())
}

/// Where the filesystem at `path` was mounted from, and its type, from
/// `statfs`.
fn mounted_from(path: &Path) -> Option<(String, String)> {
    let c_path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut info = std::mem::MaybeUninit::<libc::statfs>::zeroed();
    // SAFETY: a NUL-terminated path and a buffer of the right type, which
    // `statfs` fills when it returns 0.
    if unsafe { libc::statfs(c_path.as_ptr(), info.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: filled by the call above.
    let info = unsafe { info.assume_init() };
    let text = |field: &[libc::c_char]| {
        // SAFETY: `statfs` NUL-terminates both fields within their length.
        unsafe { CStr::from_ptr(field.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    };
    Some((text(&info.f_mntfromname), text(&info.f_fstypename)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The runner has at least its boot volume, and it is a disk at `/`
    /// with a size and a name.
    #[test]
    fn the_boot_volume_is_listed_as_a_disk() {
        let (devices, _) = volumes::rows(list());
        let root = devices
            .iter()
            .find(|device| device.mount.as_deref() == Some(Path::new("/")))
            .unwrap_or_else(|| panic!("no disk at /: {devices:?}"));
        assert!(root.size > 0, "{root:?}");
        assert!(!root.label.is_empty(), "{root:?}");
        assert!(root.node.starts_with("/dev/"), "{root:?}");
    }

    #[test]
    fn statfs_says_where_the_root_came_from() {
        let (from, fs_type) = mounted_from(Path::new("/")).expect("statfs /");
        assert!(from.starts_with("/dev/disk"), "{from}");
        assert_eq!(fs_type, "apfs");
    }

    #[test]
    fn a_mount_is_answered_that_macos_does_it() {
        let (requests, asked) = crossbeam_channel::unbounded();
        let (replies, answers) = crossbeam_channel::unbounded();
        requests
            .send(Request::Mount("/Volumes/STICK".to_string()))
            .expect("queued");
        drop(requests);
        run(asked, replies, Arc::new(|| {}), system_gio());
        let answer = answers.recv().expect("answered");
        assert!(matches!(answer.reply, Reply::Failed(ref why) if why == MOUNTS_ITSELF));
    }

    #[test]
    fn sftp_is_refused_before_finder_is_asked() {
        assert_eq!(
            connect("sftp://me@host/srv", &system_gio()),
            Connected::Failed("use the sftp: bookmark instead".to_string())
        );
    }
}
