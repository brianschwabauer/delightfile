//! The Places card's worker on Linux: udisks2 over the system bus for the
//! disks, gvfs through `gio` for the shares and the phones, and one
//! `gio mount --monitor` to hear a phone arrive. Why each of them is asked the
//! way it is, and what the card makes of the answers, is `crate::mounts`'s
//! header; this is the half of it that talks to this machine, moved here
//! whole.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Receiver, Sender};

use crate::mounts::{
    gvfs_root, Address, Answer, Connected, Device, Event, Gio, Phone, Reply, Request, Share,
};
use crate::platform::linux::dbus::{Bus, Interfaces, Value};
use crate::platform::linux::gio::{
    attempt, first_line, landing, listen, phones_from, shares_from, Attempt,
};

/// The udisks2 names, in one place.
const SERVICE: &str = "org.freedesktop.UDisks2";
const MANAGER_PATH: &str = "/org/freedesktop/UDisks2";
const OBJECT_MANAGER: &str = "org.freedesktop.DBus.ObjectManager";
const BLOCK: &str = "org.freedesktop.UDisks2.Block";
const FILESYSTEM: &str = "org.freedesktop.UDisks2.Filesystem";
const DRIVE: &str = "org.freedesktop.UDisks2.Drive";

/// Turn a `GetManagedObjects` reply into the rows the card shows.
///
/// The filter, and why each half of it is there:
///
/// - **A `Filesystem` interface**, because a row that cannot be mounted is a row
///   whose `Enter` does nothing. That drops the whole-disk objects (`/dev/sda`
///   as opposed to `/dev/sda1`), swap partitions, and unformatted space.
/// - **A `Drive`**, because everything else is a loop device, a ramdisk or a
///   device-mapper node — real to the kernel and not what anybody means by
///   "the disks".
/// - **`HintIgnore` false**, which is udisks2's own "do not show this to a
///   person" flag, set for things like the EFI system partition on some setups.
///
/// Sorted removable-first and then by label, because the answer to `M` is
/// almost always the USB stick that was just plugged in.
pub fn devices_from(objects: &[(String, Interfaces)]) -> Vec<Device> {
    let drives: std::collections::HashMap<&str, &Interfaces> = objects
        .iter()
        .filter(|(_, interfaces)| interfaces.contains_key(DRIVE))
        .map(|(path, interfaces)| (path.as_str(), interfaces))
        .collect();

    let mut out: Vec<Device> = Vec::new();
    for (path, interfaces) in objects {
        let (Some(block), Some(filesystem)) = (interfaces.get(BLOCK), interfaces.get(FILESYSTEM))
        else {
            continue;
        };
        let get =
            |props: &std::collections::HashMap<String, Value>, key: &str| props.get(key).cloned();
        if get(block, "HintIgnore")
            .and_then(|v| v.as_bool())
            .unwrap_or(false)
        {
            continue;
        }
        let drive_path = get(block, "Drive")
            .and_then(|v| v.as_str().map(str::to_string))
            .filter(|path| path != "/");
        let Some(drive_path) = drive_path else {
            continue;
        };
        let drive = drives.get(drive_path.as_str()).and_then(|i| i.get(DRIVE));

        let node = get(block, "Device")
            .and_then(|v| v.as_bytestring())
            .unwrap_or_default();
        let label = get(block, "IdLabel")
            .and_then(|v| v.as_str().map(str::to_string))
            .filter(|label| !label.is_empty())
            .unwrap_or_else(|| {
                // No label: the device node's last component, which is what
                // every other tool falls back to and what the user will
                // recognise from `lsblk`.
                node.rsplit('/').next().unwrap_or("disk").to_string()
            });
        let mount = get(filesystem, "MountPoints")
            .and_then(|v| v.as_bytestrings())
            .unwrap_or_default()
            .into_iter()
            .find(|m| !m.is_empty())
            .map(PathBuf::from);

        let drive_str = |key: &str| {
            drive
                .and_then(|props| props.get(key))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string()
        };
        let hardware = [drive_str("Vendor"), drive_str("Model")]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" ");

        out.push(Device {
            object: path.clone(),
            drive: Some(drive_path),
            node,
            label,
            fs: get(block, "IdType")
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default(),
            size: get(block, "Size").and_then(|v| v.as_u64()).unwrap_or(0),
            mount,
            removable: drive
                .and_then(|props| props.get("Removable"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            ejectable: drive
                .and_then(|props| props.get("Ejectable"))
                .and_then(|v| v.as_bool())
                .unwrap_or(false),
            hardware,
        });
    }
    out.sort_by(|a, b| {
        b.removable
            .cmp(&a.removable)
            .then_with(|| a.label.to_lowercase().cmp(&b.label.to_lowercase()))
            .then_with(|| a.node.cmp(&b.node))
    });
    out
}

/// The command a mount with questions to ask is re-run under, in a terminal
/// where it can ask them. `$1` is the address, handed over as an *argument* by
/// [`crate::open::spawn_detached`] and never spliced into this line: a URL is
/// text somebody typed.
pub const TERMINAL_MOUNT: Option<&str> =
    Some(r#"setsid uwsm-app -- "${TERMINAL:-ghostty}" -e gio mount "$1""#);

/// What a connect that came back with no share to go to says in place of
/// "Connected to": nothing, since `gio mount` has said it is done by then and
/// only the share's folder was not found.
pub const CONNECT_UNSEEN: Option<&str> = None;

/// How long a new mount is given to appear under gvfs-fuse's directory after
/// `gio mount` has said it is done. gvfs-fuse hears about mounts over D-Bus, a
/// moment after the mount itself; two seconds is far more than that moment and
/// far less than a user waiting on a window that is not going to change.
const ARRIVAL: Duration = Duration::from_secs(2);

/// The names in gvfs-fuse's directory; none when it is not there.
fn gvfs_entries(root: &Path) -> Vec<String> {
    std::fs::read_dir(root)
        .map(|dir| {
            dir.flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

/// `gio mount -li`, started and not yet waited for.
///
/// In two halves because gio spends half a second of every listing waiting for
/// gvfs's volume monitors to report in, whatever there is to report. So the
/// worker starts it, asks udisks2 its question while gio waits, and collects
/// both: one round trip for the card, with the quicker half hidden inside the
/// slower one.
///
/// `-i` for the activation root a phone's volume is mounted by, which the
/// plain listing leaves out; the rest of what it adds is indented under the
/// line it is about, where the share parser does not look.
struct GioListing(Option<std::process::Child>);

impl GioListing {
    fn start() -> GioListing {
        let child = Command::new("gio")
            .args(["mount", "-li"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        GioListing(match child {
            Ok(child) => Some(child),
            Err(e) => {
                log::debug!("gio mount -li did not start: {e}");
                None
            }
        })
    }

    /// The phones and the shares. No gio, or a gio that failed, is neither: a
    /// machine without gvfs has nothing mounted through it.
    fn finish(self) -> (Vec<Phone>, Vec<Share>) {
        let Some(child) = self.0 else {
            return (Vec::new(), Vec::new());
        };
        let Ok(output) = child.wait_with_output() else {
            return (Vec::new(), Vec::new());
        };
        let listing = String::from_utf8_lossy(&output.stdout);
        let root = gvfs_root();
        let entries = gvfs_entries(&root);
        (
            phones_from(&listing, &root, &entries),
            shares_from(&listing, &root, &entries),
        )
    }
}

/// Every share gvfs has mounted, now. Blocks for as long as gio does.
pub fn list_shares() -> Vec<Share> {
    GioListing::start().finish().1
}

/// The real `gio`, its stdin closed, so a question it asks is answered by
/// end-of-file ([`attempt`]).
pub fn system_gio() -> Gio {
    std::sync::Arc::new(|args: &[&str]| {
        Command::new("gio").args(args).stdin(Stdio::null()).output()
    })
}

/// A `gio` that could not be started, as a sentence.
fn gio_error(e: &std::io::Error) -> String {
    if e.kind() == std::io::ErrorKind::NotFound {
        "gio is not installed: network shares need gvfs".to_string()
    } else {
        format!("gio: {e}")
    }
}

/// `u` on a share, or on a phone: `gio mount -u <url>`, which is what a
/// desktop's own eject button beside either does.
fn unmount_gio(url: &str, gio: &Gio) -> Reply {
    match gio(&["mount", "-u", url]) {
        Ok(output) if output.status.success() => Reply::Unmounted,
        Ok(output) => Reply::Failed(
            first_line(&String::from_utf8_lossy(&output.stderr))
                .unwrap_or("gio could not unmount it")
                .to_string(),
        ),
        Err(e) => Reply::Failed(gio_error(&e)),
    }
}

/// `Enter` or `m` on a phone or a camera: `gio mount <root>`, nobody there to
/// answer anything. Called on a task-engine worker, since a phone takes a
/// moment to open and may be asking its owner whether to allow it.
///
/// Mounted, or gio's own first words about why not. A device asks no
/// questions — MTP has no password — so where a share would fall back to a
/// terminal ([`Connected::NeedsTerminal`]) this has nothing to fall back to,
/// and "already mounted" is mounted, as it is for a share.
pub fn mount_gio(root: &str, gio: &Gio) -> Connected {
    let output = match gio(&["mount", root]) {
        Ok(output) => output,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Connected::Failed("gio is not installed: phones need gvfs".to_string())
        }
        Err(e) => return Connected::Failed(gio_error(&e)),
    };
    let stderr = String::from_utf8_lossy(&output.stderr);
    if output.status.success() || stderr.to_lowercase().contains("already mounted") {
        return Connected::Mounted(None);
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Connected::Failed(
        first_line(&stderr)
            .or_else(|| first_line(&stdout))
            .unwrap_or("gio mount failed without saying why")
            .to_string(),
    )
}

/// `gio mount <url>` with stdin closed, and then where the mount is.
///
/// Called on a task-engine worker: it blocks for as long as the server takes
/// to answer, and then for as long as gvfs-fuse takes to show the mount.
pub fn connect(url: &str, gio: &Gio) -> Connected {
    let output = match gio(&["mount", url]) {
        Ok(output) => output,
        Err(e) => return Connected::Failed(gio_error(&e)),
    };
    match attempt(
        output.status.success(),
        &String::from_utf8_lossy(&output.stdout),
        &String::from_utf8_lossy(&output.stderr),
    ) {
        Attempt::Mounted => {}
        Attempt::NeedsTerminal => return Connected::NeedsTerminal,
        Attempt::Failed(message) => return Connected::Failed(message),
    }
    let Some(address) = Address::parse(url) else {
        return Connected::Mounted(None);
    };
    let Some((share, within)) = landing(&list_shares(), &address) else {
        return Connected::Mounted(None);
    };
    let deadline = Instant::now() + ARRIVAL;
    while !share.is_dir() {
        if Instant::now() >= deadline {
            return Connected::Mounted(None);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // A path inside the share that is not there — a typo after the share's
    // name — still lands in the share, which is the nearest true answer.
    let inside = share.join(&within);
    if within.as_os_str().is_empty() || !inside.is_dir() {
        return Connected::Mounted(Some(share));
    }
    Connected::Mounted(Some(inside))
}

/// The worker loop.
///
/// The connection is opened lazily and kept: a card that is opened and closed
/// four times should not authenticate four times, and a connection that has
/// gone away is reopened on the next request rather than being an error the
/// user has to do something about.
pub fn run(
    requests: Receiver<Request>,
    replies: Sender<Answer>,
    notify: df_core::fs::Notifier,
    gio: Gio,
) {
    let mut bus: Option<Bus> = None;
    for request in requests {
        let reply = match &request {
            // gio, not the system bus: a share or a phone is put away whether
            // or not udisks2 is there to be asked.
            Request::GioUnmount(url) => unmount_gio(url, &gio),
            _ => udisks(&mut bus, &request),
        };
        let _ = replies.send(Answer { to: request, reply });
        notify();
    }
}

/// One request that needs udisks2, over the kept connection.
fn udisks(bus: &mut Option<Bus>, request: &Request) -> Reply {
    if bus.is_none() {
        match Bus::system() {
            Ok(connected) => *bus = Some(connected),
            Err(e) => return Reply::Failed(e),
        }
    }
    let Some(connection) = bus.as_mut() else {
        return Reply::Failed("the system bus is not connected".to_string());
    };
    match handle(connection, request) {
        Ok(reply) => reply,
        Err(e) => {
            // A broken connection is dropped so the next request reopens it; a
            // refusal is not, because the connection is fine.
            if e.contains("closed the connection") || e.contains("reading from") {
                *bus = None;
            }
            Reply::Failed(e)
        }
    }
}

fn handle(bus: &mut Bus, request: &Request) -> Result<Reply, String> {
    match request {
        Request::List => {
            // gio first, and collected last: see [`GioListing`]. Collected
            // whatever udisks2 said, so a failed call does not leave the child
            // behind unreaped.
            let gio = GioListing::start();
            let devices = list_devices(bus);
            let (phones, shares) = gio.finish();
            Ok(Reply::Listing {
                devices: devices?,
                phones,
                shares,
            })
        }
        Request::Mount(object) => {
            let mut args = Vec::new();
            crate::platform::linux::dbus::marshal_no_options(&mut args);
            let body = bus.call(SERVICE, object, FILESYSTEM, "Mount", Some("a{sv}"), &args)?;
            let path = crate::platform::linux::dbus::Reader::new(&body).string()?;
            Ok(Reply::Mounted(PathBuf::from(path)))
        }
        Request::Unmount(object) => {
            let mut args = Vec::new();
            crate::platform::linux::dbus::marshal_no_options(&mut args);
            bus.call(SERVICE, object, FILESYSTEM, "Unmount", Some("a{sv}"), &args)?;
            Ok(Reply::Unmounted)
        }
        Request::Eject { drive, .. } => {
            let mut args = Vec::new();
            crate::platform::linux::dbus::marshal_no_options(&mut args);
            bus.call(SERVICE, drive, DRIVE, "Eject", Some("a{sv}"), &args)?;
            Ok(Reply::Ejected)
        }
        // Answered in `run`, never through the bus.
        Request::GioUnmount(_) => Err("gio unmounts, not udisks2".to_string()),
    }
}

/// udisks2's half of a listing.
fn list_devices(bus: &mut Bus) -> Result<Vec<Device>, String> {
    let body = bus.call(
        SERVICE,
        MANAGER_PATH,
        OBJECT_MANAGER,
        "GetManagedObjects",
        None,
        &[],
    )?;
    let objects = crate::platform::linux::dbus::parse_managed_objects(&body)?;
    Ok(devices_from(&objects))
}

/// `gio mount --monitor --detail` for the life of the window, and the thread
/// that reads it.
///
/// The thread blocks on gio's stdout and does nothing else: it wakes when gio
/// prints, hands over each event whole, and rings the event loop once per
/// line that completed one — so a window with nothing being plugged in is a
/// window at zero frames. It ends when the pipe does, which is gio dying: it
/// says so ([`Monitor::gone`]) and rings once more, so the app can start
/// another ([`restart_due`]).
///
/// Dropping this kills gio — it holds nothing that needs a gentler end — and
/// reaps it, which closes the pipe, and then joins the thread. gio is tied to
/// the thread that started it ([`df_core::vfs::child::tie_to_this_thread`]),
/// the event loop's, so a window that dies without running its destructors
/// does not leave a gio behind, listening on the session bus for nobody.
pub struct Monitor {
    child: std::process::Child,
    events: Receiver<Event>,
    reader: Option<std::thread::JoinHandle<()>>,
    /// The pipe has ended: gio is dead, and nothing more will be heard.
    gone: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// When it was started, for [`restart_due`].
    started: Instant,
}

impl Monitor {
    /// Start listening, or `None` when there is no `gio` to listen with —
    /// which is nothing to tell anyone: a machine without gvfs has no phones
    /// for it to hear about. Call it on the event loop's thread; see above.
    pub fn start(notify: df_core::fs::Notifier) -> Option<Monitor> {
        let mut command = Command::new("gio");
        command.args(["mount", "--monitor", "--detail"]);
        Monitor::spawn(command, notify)
    }

    /// `command`'s stdout, heard as gio's is: [`Monitor::start`]'s gio, or a
    /// test's stand-in that prints gio's words and exits.
    fn spawn(mut command: Command, notify: df_core::fs::Notifier) -> Option<Monitor> {
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        df_core::vfs::child::tie_to_this_thread(&mut command);
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(e) => {
                log::debug!("gio mount --monitor did not start: {e}");
                return None;
            }
        };
        let (tx, events) = unbounded::<Event>();
        let gone = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let ended = std::sync::Arc::clone(&gone);
        let reader = child.stdout.take().and_then(|stdout| {
            std::thread::Builder::new()
                .name("df-gio-monitor".to_string())
                .spawn(move || {
                    listen(stdout, tx, std::sync::Arc::clone(&notify));
                    ended.store(true, std::sync::atomic::Ordering::SeqCst);
                    notify();
                })
                .map_err(|e| log::warn!("the gio monitor's reader did not start: {e}"))
                .ok()
        });
        if reader.is_none() {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        Some(Monitor {
            child,
            events,
            reader,
            gone,
            started: Instant::now(),
        })
    }

    /// Whether gio has died: its pipe ended, and the thread with it. A dead
    /// gio is reaped here and now, rather than left a zombie until the
    /// restart or the window's end: the frame the reader's last bell brings
    /// asks this.
    pub fn gone(&mut self) -> bool {
        if !self.gone.load(std::sync::atomic::Ordering::SeqCst) {
            return false;
        }
        if let Err(e) = self.child.try_wait() {
            log::debug!("the gio monitor could not be reaped: {e}");
        }
        true
    }

    /// When this watcher was started.
    pub fn started(&self) -> Instant {
        self.started
    }

    /// Whatever gio has said since the last call.
    pub fn drain(&self) -> Vec<Event> {
        self.events.try_iter().collect()
    }
}

impl Drop for Monitor {
    fn drop(&mut self) {
        if let Err(e) = self.child.kill() {
            log::debug!("the gio monitor would not stop: {e}");
        }
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mounts::tests::{disks, PIXEL};
    use crate::mounts::{restart_due, unlock_hint, Change, Protocol, UNLOCK};

    use std::collections::HashMap;

    fn props(pairs: &[(&str, Value)]) -> HashMap<String, Value> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.clone()))
            .collect()
    }

    fn bytes(text: &str) -> Value {
        Value::Array(text.bytes().map(Value::U8).chain([Value::U8(0)]).collect())
    }

    fn objects() -> Vec<(String, Interfaces)> {
        let drive: Interfaces = [(
            DRIVE.to_string(),
            props(&[
                ("Removable", Value::Bool(true)),
                ("Ejectable", Value::Bool(true)),
                ("Vendor", Value::Str("SanDisk".into())),
                ("Model", Value::Str("Cruzer".into())),
            ]),
        )]
        .into_iter()
        .collect();

        let usb: Interfaces = [
            (
                BLOCK.to_string(),
                props(&[
                    ("Device", bytes("/dev/sdb1")),
                    ("IdLabel", Value::Str("PHOTOS".into())),
                    ("IdType", Value::Str("vfat".into())),
                    ("Size", Value::U64(16_000_000_000)),
                    ("Drive", Value::Path("/drives/usb".into())),
                ]),
            ),
            (
                FILESYSTEM.to_string(),
                props(&[("MountPoints", Value::Array(vec![]))]),
            ),
        ]
        .into_iter()
        .collect();

        let internal_drive: Interfaces = [(
            DRIVE.to_string(),
            props(&[("Removable", Value::Bool(false))]),
        )]
        .into_iter()
        .collect();

        let root: Interfaces = [
            (
                BLOCK.to_string(),
                props(&[
                    ("Device", bytes("/dev/nvme0n1p2")),
                    ("IdLabel", Value::Str("".into())),
                    ("IdType", Value::Str("ext4".into())),
                    ("Size", Value::U64(500_000_000_000)),
                    ("Drive", Value::Path("/drives/nvme".into())),
                ]),
            ),
            (
                FILESYSTEM.to_string(),
                props(&[("MountPoints", Value::Array(vec![bytes("/")]))]),
            ),
        ]
        .into_iter()
        .collect();

        // A loop device: a filesystem with no drive behind it.
        let loop_dev: Interfaces = [
            (
                BLOCK.to_string(),
                props(&[
                    ("Device", bytes("/dev/loop0")),
                    ("Drive", Value::Path("/".into())),
                ]),
            ),
            (
                FILESYSTEM.to_string(),
                props(&[("MountPoints", Value::Array(vec![]))]),
            ),
        ]
        .into_iter()
        .collect();

        // The whole disk, which has no filesystem of its own.
        let whole: Interfaces = [(
            BLOCK.to_string(),
            props(&[
                ("Device", bytes("/dev/sdb")),
                ("Drive", Value::Path("/drives/usb".into())),
            ]),
        )]
        .into_iter()
        .collect();

        // Something udisks2 itself says not to show.
        let hidden: Interfaces = [
            (
                BLOCK.to_string(),
                props(&[
                    ("Device", bytes("/dev/nvme0n1p1")),
                    ("Drive", Value::Path("/drives/nvme".into())),
                    ("HintIgnore", Value::Bool(true)),
                ]),
            ),
            (
                FILESYSTEM.to_string(),
                props(&[("MountPoints", Value::Array(vec![]))]),
            ),
        ]
        .into_iter()
        .collect();

        vec![
            ("/drives/usb".to_string(), drive),
            ("/drives/nvme".to_string(), internal_drive),
            ("/block/nvme0n1p2".to_string(), root),
            ("/block/sdb1".to_string(), usb),
            ("/block/loop0".to_string(), loop_dev),
            ("/block/sdb".to_string(), whole),
            ("/block/nvme0n1p1".to_string(), hidden),
        ]
    }

    /// The filter, in one test: what is a row, what is not, and why.
    #[test]
    fn only_mountable_filesystems_on_real_drives_become_rows() {
        let devices = devices_from(&objects());
        let nodes: Vec<&str> = devices.iter().map(|d| d.node.as_str()).collect();
        assert_eq!(
            nodes,
            vec!["/dev/sdb1", "/dev/nvme0n1p2"],
            "removable first, and nothing else got in"
        );
        // The loop device has no drive, the whole disk has no filesystem, and
        // the EFI partition asked not to be shown.
        assert!(!nodes.contains(&"/dev/loop0"));
        assert!(!nodes.contains(&"/dev/sdb"));
        assert!(!nodes.contains(&"/dev/nvme0n1p1"));
    }

    /// Every fact a row shows, taken off the right interface.
    #[test]
    fn a_device_carries_what_the_row_needs() {
        let devices = devices_from(&objects());
        let usb = &devices[0];
        assert_eq!(usb.label, "PHOTOS");
        assert_eq!(usb.fs, "vfat");
        assert_eq!(usb.size, 16_000_000_000);
        assert!(usb.removable && usb.ejectable);
        assert_eq!(usb.hardware, "SanDisk Cruzer");
        assert!(!usb.is_mounted());
        assert_eq!(
            usb.detail(),
            "14.9 GB · vfat · not mounted",
            "size, filesystem, and where — here, nowhere"
        );
        assert_eq!(usb.drive.as_deref(), Some("/drives/usb"));

        // No label: the device node's last component, which is what `lsblk`
        // would have shown.
        let root = &devices[1];
        assert_eq!(root.label, "nvme0n1p2");
        assert!(root.is_mounted());
        assert_eq!(root.detail(), "465.7 GB · ext4 · /");
        assert!(!root.removable);
        assert!(!root.ejectable, "an unstated flag is false, not true");
        assert_eq!(root.hardware, "", "a drive with no vendor says nothing");
    }

    /// The two disks the card's own tests use (`crate::mounts::tests::disks`)
    /// are what this reply makes, so the card is tested on rows udisks2 could
    /// really send.
    #[test]
    fn the_card_tests_disks_are_what_the_reply_makes() {
        assert_eq!(devices_from(&objects()), disks());
    }

    /// An empty reply is a normal machine, not a failure.
    #[test]
    fn a_machine_with_no_disks_produces_no_rows() {
        assert!(devices_from(&[]).is_empty());
    }

    /// A watcher whose gio exits is gone once its last words are heard, and
    /// is started again once — ten seconds after it started, never before,
    /// and never a second time.
    #[test]
    fn a_dead_watcher_is_started_again_once_after_ten_seconds() {
        let rung = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let bell = std::sync::Arc::clone(&rung);
        let mut command = Command::new("sh");
        command.args([
            "-c",
            "printf '%s\\n' \"Volume added:       'Pixel 10a'\" '  Volume(0): Pixel 10a' \
             '    Type: GProxyVolume (GProxyVolumeMonitorMTP)' ''",
        ]);
        let mut monitor = Monitor::spawn(
            command,
            std::sync::Arc::new(move || {
                bell.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }),
        )
        .expect("sh starts");
        let proc = PathBuf::from(format!("/proc/{}", monitor.child.id()));
        let deadline = Instant::now() + Duration::from_secs(10);
        while !monitor.gone() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(monitor.gone(), "the pipe ended and nobody noticed");
        // Asked while it is gone, it is reaped: no zombie is left in the
        // process table (the exit can trail the pipe's end by a moment, so
        // it is asked until then).
        while proc.exists() && Instant::now() < deadline {
            assert!(monitor.gone());
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(!proc.exists(), "a dead gio was left a zombie");
        let heard = monitor.drain();
        assert_eq!(heard.len(), 1);
        assert_eq!(heard[0].change, Change::VolumeAdded);
        assert_eq!(heard[0].protocol, Some(Protocol::Mtp));
        assert_eq!(
            rung.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "once for the event, once for the end"
        );

        let started = monitor.started();
        let retry = crate::appearance::RETRY;
        assert!(
            !restart_due(false, 0, started, started + retry * 2),
            "alive"
        );
        assert!(
            !restart_due(true, 0, started, started + retry / 2),
            "too soon"
        );
        assert!(restart_due(true, 0, started, started + retry));
        assert!(!restart_due(true, 1, started, started + retry * 2), "twice");
    }

    /// A stand-in for gio that notes what it was asked and answers with
    /// `code`, `stdout` and `stderr`.
    fn fake_gio(
        code: i32,
        stdout: &'static str,
        stderr: &'static str,
    ) -> (Gio, std::sync::Arc<std::sync::Mutex<Vec<Vec<String>>>>) {
        use std::os::unix::process::ExitStatusExt;
        let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = std::sync::Arc::clone(&asked);
        let gio: Gio = std::sync::Arc::new(move |args: &[&str]| {
            log.lock()
                .expect("the log")
                .push(args.iter().map(|arg| arg.to_string()).collect());
            Ok(std::process::Output {
                status: std::process::ExitStatus::from_raw(code << 8),
                stdout: stdout.as_bytes().to_vec(),
                stderr: stderr.as_bytes().to_vec(),
            })
        });
        (gio, asked)
    }

    /// `gio mount <root>`: mounted, already mounted, or gio's first words —
    /// and for a locked phone, what to do about it instead.
    #[test]
    fn a_phone_is_mounted_by_its_root_and_a_locked_one_says_so() {
        let (gio, asked) = fake_gio(0, "", "");
        assert_eq!(mount_gio(PIXEL, &gio), Connected::Mounted(None));
        assert_eq!(
            *asked.lock().expect("the log"),
            vec![vec!["mount".to_string(), PIXEL.to_string()]]
        );
        let (gio, _) = fake_gio(2, "", "gio: mtp://x/: Location is already mounted\n");
        assert_eq!(mount_gio(PIXEL, &gio), Connected::Mounted(None));

        let locked =
            "gio: mtp://Google_Pixel_10a_4B021FDAQ00123/: Unable to open MTP device “003,012”\n";
        let (gio, _) = fake_gio(2, "", locked);
        let Connected::Failed(message) = mount_gio(PIXEL, &gio) else {
            panic!("a locked phone mounted");
        };
        assert_eq!(message, locked.trim());
        assert_eq!(unlock_hint(Protocol::Mtp, &message), Some(UNLOCK));
        assert_eq!(
            UNLOCK,
            "Unlock the phone and choose File transfer, then try again"
        );
        for said in [
            "gio: mtp://x/: Device is busy",
            "gio: mtp://x/: LIBMTP_ERROR_GENERAL",
        ] {
            assert_eq!(unlock_hint(Protocol::Mtp, said), Some(UNLOCK), "{said}");
        }
        // Anything else is in gio's words, and a camera has no lock screen.
        assert_eq!(
            unlock_hint(Protocol::Mtp, "gio: mtp://x/: No such device"),
            None
        );
        assert_eq!(unlock_hint(Protocol::Gphoto2, locked), None);

        let (gio, _) = fake_gio(1, "", "");
        assert!(matches!(mount_gio(PIXEL, &gio), Connected::Failed(_)));
    }

    /// `u` on a share or a phone is `gio mount -u <url>`, and its failure is
    /// gio's first line.
    #[test]
    fn a_gio_mount_is_put_away_by_its_url() {
        let (gio, asked) = fake_gio(0, "", "");
        assert!(matches!(unmount_gio(PIXEL, &gio), Reply::Unmounted));
        assert_eq!(
            *asked.lock().expect("the log"),
            vec![vec![
                "mount".to_string(),
                "-u".to_string(),
                PIXEL.to_string()
            ]]
        );
        let (gio, _) = fake_gio(2, "", "gio: mtp://x/: Device busy\nmore\n");
        assert!(matches!(
            unmount_gio(PIXEL, &gio),
            Reply::Failed(message) if message == "gio: mtp://x/: Device busy"
        ));
    }
}
