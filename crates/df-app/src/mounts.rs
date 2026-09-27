//! The Places card, `M` — PLAN §7.4's "`M`: udisks2 over hand-rolled D-Bus —
//! list/mount/unmount/eject", grown into every answer to "where can I go that
//! is not in this folder".
//!
//! Three sections, top to bottom. **Devices**: the disks udisks2 knows and,
//! under them, the phones and cameras gvfs's MTP and gphoto2 volume monitors
//! have seen ([`Phone`]). **Network**: the shares gvfs has mounted, the rclone
//! remotes, and the way to connect to another server. **Places**: every
//! pinned folder and every `[goto]` row, as the app's Places list gives them
//! ([`Place`]). Devices first because `M` `Enter` mounting the stick — or the
//! phone — that was just plugged in is the card's main job, so the cursor
//! starts on the first device ([`Card::with_places`]). The card only draws
//! the places and says which one is under the cursor; going there, and `d`
//! unpinning a pin, are the app's (`crate::app::places`).
//!
//! Every row is one line, the height of a row in the list panes: a glyph, the
//! name, and after it in quieter ink what the row is and where — a disk's
//! size, filesystem and mount point, a share's URL, a place's path — elided in
//! its middle to fit ([`row_layout`]). A place row ends in its `g` key, drawn
//! as the which-key card draws a key. While a call is out on a row, what it is
//! doing takes the detail's place, and when one fails, the failure does.
//!
//! ## Why udisks2 and not `/proc/mounts`
//!
//! Reading `/proc/mounts` would list what is mounted; it would not list the USB
//! stick that is plugged in and *not* mounted, which is the whole reason to
//! press `M`. udisks2 knows about block devices before anything has been done
//! with them, and it is the service that can mount one without root — polkit
//! grants a logged-in user permission for their own removable media, which is
//! why this works at all and why `mount(8)` would not.
//!
//! ## The shape of the conversation
//!
//! One call gets everything: `ObjectManager.GetManagedObjects` on
//! `/org/freedesktop/UDisks2` returns every object udisks2 knows with every
//! interface and every property. A *device* in this file is a block object that
//! has both a `Filesystem` interface (so it can be mounted) and a `Drive` (so
//! there is real hardware behind it, which filters out loop devices and
//! ramdisks). [`devices_from`] is that filter and it is a pure function, so the
//! rules are a test over a hand-built reply rather than something that needs a
//! machine with a USB stick in it.
//!
//! Acting is three more calls, each taking an empty options dictionary:
//! `Filesystem.Mount`, `Filesystem.Unmount`, `Drive.Eject`.
//!
//! ## Network shares belong to gvfs, and are reached through `gio`
//!
//! A share on a server is not a block device, and udisks2 has never heard of
//! one. What mounts `smb://` and `sftp://` on a desktop without root is gvfs:
//! its daemons hold the connection, and `gvfsd-fuse` shows each mount as a
//! directory under `/run/user/<uid>/gvfs`. That directory is what makes a share
//! somewhere this program can go — it is a path, and every listing, preview
//! and copy in the program already works on paths.
//!
//! gvfs is asked through `gio`, its command-line front end, and not over
//! D-Bus. Its mount tracker lives on the *session* bus behind an interface
//! gvfs keeps private, where [`crate::dbus`] speaks only to the system bus;
//! `gio mount` is the surface gvfs promises, and it is installed wherever gvfs
//! is. The listing is two lines per mount and [`shares_from`] reads it as a
//! pure function, so the parsing is a test over a captured listing rather than
//! something that needs a server.
//!
//! The listing names a mount by its URL (`smb://host/share/`); gvfs-fuse names
//! its directory by the gvfs *mount spec* (`smb-share:server=host,share=share`).
//! [`Spec`] is the bridge between the two: it builds the spec from the URL the
//! way gvfs does, and finds it among the directories that are really there.
//!
//! ## Phones and cameras are gvfs's too
//!
//! A phone plugged in over USB is not a block device either: it speaks MTP,
//! and a camera speaks PTP, and what turns either into a directory is gvfs —
//! `gvfs-mtp` and `gvfs-gphoto2`, whose volume monitors report the device
//! the moment it is plugged in, before anything is mounted. `gio mount -li`
//! lists those volumes beside the shares, each with the *activation root*
//! mounting it takes (`mtp://Google_Pixel_10a_…/`), and nests the mount
//! under it once there is one. [`phones_from`] reads both from the same
//! listing [`shares_from`] does, keyed by that root; `gio mount <root>` mounts
//! one ([`mount_gio`]) and `gio mount -u <root>` puts it away, and the
//! directory it appears as under gvfs-fuse is found through the same
//! [`Spec`] bridge (`mtp:host=…`).
//!
//! A phone that is locked, or not set to *File transfer*, cannot be opened:
//! gvfs says so in libmtp's words, and the card says what to do about it
//! instead ([`unlock_hint`]).
//!
//! ## Hearing a phone arrive
//!
//! udisks2 is asked when the card opens; a phone plugged in while the window
//! is up is heard ([`Monitor`]): one `gio mount --monitor` for the life of the
//! window, its stdout read by a thread that blocks on the pipe and rings the
//! event loop only when gio prints an event — nothing polls, and a window with
//! nothing plugged in stays at zero frames. The events are read by
//! [`Blocks`] and [`event_from`], pure functions over gio's own format.
//!
//! ## Cloud remotes are rows, not mounts
//!
//! Under the shares, the Network section lists the vfs's rclone services —
//! every remote in the user's `rclone.conf` ([`Cloud`]). They are not mounted
//! and are not gvfs's: `Enter` goes to `rclone://<name>` the way a `g` key
//! goes to `sftp://…`, and the mount verbs have nothing to act on, so the hint
//! strip does not offer them there ([`Geometry::cloud`]).
//!
//! ## Every one of them blocks
//!
//! Mounting waits for the filesystem; unmounting waits for the writeback; and
//! any of them may sit in a polkit prompt for as long as the user takes to type
//! a password. `gio mount -l` itself spends half a second waiting for gvfs's
//! volume monitors. So all of it lives on a worker thread ([`Mounts`]), the
//! event loop only ever sends it a request and reads its answers, and a row
//! that is being acted on is drawn as busy rather than the window being
//! unresponsive. Connecting to a server can take as long as a network timeout,
//! so that is a job on the task engine instead ([`connect`]), where it shows in
//! the task panel like any other long thing — and so is mounting a phone
//! ([`mount_gio`]), which waits on a USB device that may be asking its owner
//! whether to allow it.
//!
//! ## When udisks2 is not there
//!
//! A notice, once, and no card. A machine without udisks2 is a machine without
//! removable disks to manage, not a machine with an error in it — the same rule
//! [`df_core::git`] follows for a machine without git.
//!
//! ## When gvfs is not there
//!
//! No `gio`, or a `gio` with no gvfs behind it, is a Network section that says
//! nothing is mounted — which is true. The connect row stays, because it is
//! where the user finds out that connecting needs gvfs: from `gio`'s own error,
//! or from [`connect`] saying `gio` is missing. No phone is listed without
//! `gvfs-mtp` (or a camera without `gvfs-gphoto2`), and with no `gio` there is
//! no watcher either: nothing is started and nothing is said.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Receiver, Sender};

use crate::dbus::{Bus, Interfaces, Value};
use crate::viewport::Jump;

/// The udisks2 names, in one place.
const SERVICE: &str = "org.freedesktop.UDisks2";
const MANAGER_PATH: &str = "/org/freedesktop/UDisks2";
const OBJECT_MANAGER: &str = "org.freedesktop.DBus.ObjectManager";
const BLOCK: &str = "org.freedesktop.UDisks2.Block";
const FILESYSTEM: &str = "org.freedesktop.UDisks2.Filesystem";
const DRIVE: &str = "org.freedesktop.UDisks2.Drive";

/// How many rows the card shows before it scrolls, at most: a window too short
/// for them gets fewer ([`window`]).
///
/// Twenty one-line rows are about as tall as the ten two-line rows the card
/// had before its rows were one line — a disk or two, a phone, a share, the
/// connect row and a dozen places, all in view on an ordinary screen.
pub const ROWS: usize = 20;

/// One mountable filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    /// The block object's path — what `Mount` and `Unmount` are called on.
    pub object: String,
    /// The drive object's path, for `Eject`. `"/"` means "no drive", which is
    /// how udisks2 spells it and which this type stores as `None`.
    pub drive: Option<String>,
    /// `/dev/sda1`.
    pub node: String,
    /// The filesystem label, or the device node's last component when it has
    /// none — a row has to be *called* something.
    pub label: String,
    /// `ext4`, `vfat`, `ntfs`… Empty when udisks2 could not tell.
    pub fs: String,
    pub size: u64,
    /// Where it is mounted, or `None`. The first mount point only: a filesystem
    /// bind-mounted in four places is still one row, and the first is the one
    /// udisks2 made.
    pub mount: Option<PathBuf>,
    pub removable: bool,
    pub ejectable: bool,
    /// "SanDisk Cruzer" — the hardware behind it.
    pub hardware: String,
}

impl Device {
    pub fn is_mounted(&self) -> bool {
        self.mount.is_some()
    }

    /// What the row says after the name: the size, the filesystem, and where
    /// it is mounted or that it is not — `931 GB · ext4 · /run/media/me/x`.
    pub fn detail(&self) -> String {
        let mut parts = vec![crate::format::human_size(self.size)];
        if !self.fs.is_empty() {
            parts.push(self.fs.clone());
        }
        parts.push(match &self.mount {
            Some(path) => path.to_string_lossy().into_owned(),
            None => NOT_MOUNTED.to_string(),
        });
        parts.join(" · ")
    }
}

/// What a disk's or a phone's row says where the mount point would be.
const NOT_MOUNTED: &str = "not mounted";

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

// ── Network shares (gvfs) ───────────────────────────────────────────────────

/// What the connect prompt takes: gvfs's network-share backends, and not
/// every scheme gvfs can mount. `http://` is a download and `mtp://` is a
/// phone; neither is what "connect to server" means, and a prompt that took
/// them would be promising a share the Network section could never show.
pub const SCHEMES: &[&str] = &["smb", "sftp", "ftp", "dav", "davs", "nfs"];

/// [`SCHEMES`] as the prompt's error spells them.
const SCHEME_LIST: &str = "smb://, sftp://, ftp://, dav://, davs:// or nfs://";

/// The command a mount with questions to ask is re-run under, in a terminal
/// where it can ask them. `$1` is the address, handed over as an *argument* by
/// [`crate::open::spawn_detached`] and never spliced into this line: a URL is
/// text somebody typed.
pub const TERMINAL_MOUNT: &str = r#"setsid uwsm-app -- "${TERMINAL:-ghostty}" -e gio mount "$1""#;

/// How long a new mount is given to appear under gvfs-fuse's directory after
/// `gio mount` has said it is done. gvfs-fuse hears about mounts over D-Bus, a
/// moment after the mount itself; two seconds is far more than that moment and
/// far less than a user waiting on a window that is not going to change.
const ARRIVAL: Duration = Duration::from_secs(2);

/// One mounted network share: a row in the Network section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Share {
    /// The mount's root as gvfs spells it, `smb://host/share/`. What
    /// `gio mount -u` is given to put it away, and the row's identity across a
    /// refresh.
    pub url: String,
    /// `host/share`, `user@host` — see [`Address::label`].
    pub label: String,
    /// `smb`, `sftp`, `ftp`…
    pub scheme: String,
    /// The directory gvfs-fuse shows it as, which is where `Enter` goes.
    pub path: PathBuf,
}

/// A server address taken apart: `sftp://me@host:2222/srv` is scheme `sftp`,
/// user `me`, host `host`, port 2222 and path `/srv`.
///
/// Hand-rolled, like the D-Bus client beside it, because the grammar needed is
/// a dozen lines: gvfs's addresses are always `scheme://[user@]host[:port]/path`.
/// The user, host and path are percent-decoded, which is how gvfs keeps them
/// in a mount spec; the scheme is lower-cased, which is how gio compares it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Address {
    pub scheme: String,
    pub user: Option<String>,
    pub host: String,
    pub port: Option<u16>,
    /// Always starts with `/`; `/` when the address named no path.
    pub path: String,
}

impl Address {
    /// `None` for anything that is not `scheme://…`.
    pub fn parse(url: &str) -> Option<Address> {
        let (scheme, rest) = url.trim().split_once("://")?;
        let well_formed = scheme.starts_with(|c: char| c.is_ascii_alphabetic())
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'));
        if !well_formed {
            return None;
        }
        // A query or a fragment is not part of where a share is.
        let rest = rest.split(['?', '#']).next().unwrap_or_default();
        let (authority, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
        // From the right: a user name may itself contain an `@`, and the host
        // never does.
        let (user, host_port) = match authority.rsplit_once('@') {
            Some((user, host_port)) => (Some(decode(user)), host_port),
            None => (None, authority),
        };
        let (host, port) = split_port(host_port)?;
        Some(Address {
            scheme: scheme.to_ascii_lowercase(),
            user: user.filter(|user| !user.is_empty()),
            host: decode(host),
            port,
            path: if path.is_empty() {
                "/".to_string()
            } else {
                decode(path)
            },
        })
    }

    /// The path's non-empty segments: `/share/sub/` is `share`, `sub`.
    fn segments(&self) -> impl Iterator<Item = &str> {
        self.path.split('/').filter(|segment| !segment.is_empty())
    }

    /// What a row, a toast and a job call the share.
    ///
    /// `host/share` for SMB, because on a Windows or Samba server the share is
    /// the thing that was connected to — one server has a dozen of them.
    /// `user@host` for everything else, because there the account is what
    /// decides what you see, and one host as two users is two rows.
    pub fn label(&self) -> String {
        if self.scheme == "smb" {
            return match self.segments().next() {
                Some(share) => format!("{}/{share}", self.host),
                None => self.host.clone(),
            };
        }
        match &self.user {
            Some(user) => format!("{user}@{}", self.host),
            None => self.host.clone(),
        }
    }
}

/// `host:2121` into its halves, and `[::1]:2121`, whose host has colons of its
/// own and is bracketed for exactly that reason.
fn split_port(host_port: &str) -> Option<(&str, Option<u16>)> {
    let (host, rest) = match host_port.strip_prefix('[') {
        Some(bracketed) => bracketed.split_once(']')?,
        None => match host_port.rsplit_once(':') {
            Some((host, _)) => (host, &host_port[host.len()..]),
            None => (host_port, ""),
        },
    };
    let port = match rest.strip_prefix(':') {
        None if rest.is_empty() => None,
        // Something after the `]` that is not a port.
        None => return None,
        Some("") => None,
        Some(digits) => Some(digits.parse().ok()?),
    };
    Some((host, port))
}

/// Undo `%XX` escapes. A `%` that is not followed by two hex digits is kept as
/// it is, which is what every lenient URL reader does with one.
fn decode(text: &str) -> String {
    let hex = |byte: Option<&u8>| {
        byte.and_then(|b| (*b as char).to_digit(16))
            .map(|digit| digit as u8)
    };
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if let (Some(high), Some(low)) = (hex(bytes.get(i + 1)), hex(bytes.get(i + 2))) {
                out.push(high << 4 | low);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Escape a mount-spec value the way gvfs does when it names a directory:
/// `g_uri_escape_string` with `$&'()*+` let through and UTF-8 left alone.
/// That is why a user called `me@example.com` is `user=me%40example.com` under
/// `/run/user/<uid>/gvfs` while the `:` and `=` around it are literal.
fn escape(text: &str) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_ascii_alphanumeric() || !c.is_ascii() || "-._~$&'()*+".contains(c) {
            out.push(c);
        } else {
            // ASCII, so one byte.
            let _ = write!(out, "%{:02X}", c as u32);
        }
    }
    out
}

/// A gvfs mount spec: the `type:key=value,…` gvfs-fuse names a mount's
/// directory after — `smb-share:server=nas,share=media`,
/// `sftp:host=example.org,user=me`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Spec {
    kind: String,
    /// Sorted by key, which is the order gvfs writes them in.
    pairs: Vec<(String, String)>,
    /// The server-side path the mount is rooted at, when it is not `/` — the
    /// WebDAV share that lives under `/remote.php/webdav`. Written last, after
    /// the sorted keys, which is where gvfs puts it.
    prefix: Option<String>,
}

impl Spec {
    /// The spec gvfs would build for `address`, backend by backend.
    ///
    /// SMB's is the odd one: its type says whether a share was named, its host
    /// is called `server`, the share is the path's first segment, and both are
    /// case-folded, because neither DNS nor SMB tells `NAS/Media` from
    /// `nas/media`. `davs` is `dav` with `ssl=true`. Every other backend takes
    /// the scheme as its type and the host, user and port as they came.
    ///
    /// A phone or a camera ([`Protocol`]) is its host and nothing else — the
    /// device is the whole mount, and there is no account or port to it. Its
    /// host is whatever gvfs put in the activation root: a name
    /// (`Google_Pixel_10a_…`), or on an older gvfs the USB address in
    /// brackets (`[usb:001,005]`), which gvfs keeps in the spec and which the
    /// address grammar takes off as it would an IPv6 literal's, so they go
    /// back on.
    pub fn of(address: &Address) -> Spec {
        if Protocol::of_scheme(&address.scheme).is_some() {
            let host = if address.host.contains(':') {
                format!("[{}]", address.host)
            } else {
                address.host.clone()
            };
            return Spec {
                kind: address.scheme.clone(),
                pairs: vec![("host".to_string(), host)],
                prefix: None,
            };
        }
        let mut pairs: Vec<(String, String)> = Vec::new();
        let mut rest: Vec<&str> = address.segments().collect();
        let kind = match address.scheme.as_str() {
            "smb" => {
                pairs.push(("server".into(), address.host.to_lowercase()));
                if rest.is_empty() {
                    "smb-server".to_string()
                } else {
                    pairs.push(("share".into(), rest.remove(0).to_lowercase()));
                    "smb-share".to_string()
                }
            }
            "dav" | "davs" => {
                pairs.push(("host".into(), address.host.clone()));
                if address.scheme == "davs" {
                    pairs.push(("ssl".into(), "true".into()));
                }
                "dav".to_string()
            }
            other => {
                pairs.push(("host".into(), address.host.clone()));
                other.to_string()
            }
        };
        if let Some(user) = &address.user {
            // SMB's `DOMAIN;user`, which gvfs keeps as two keys.
            match user.split_once(';').filter(|_| address.scheme == "smb") {
                Some((domain, user)) => {
                    if !domain.is_empty() {
                        pairs.push(("domain".into(), domain.to_string()));
                    }
                    if !user.is_empty() {
                        pairs.push(("user".into(), user.to_string()));
                    }
                }
                None => pairs.push(("user".into(), user.clone())),
            }
        }
        if let Some(port) = address.port {
            pairs.push(("port".into(), port.to_string()));
        }
        pairs.sort();
        let prefix = (!rest.is_empty()).then(|| format!("/{}", rest.join("/")));
        Spec {
            kind,
            pairs,
            prefix,
        }
    }

    /// The directory name gvfs-fuse gives a mount with this spec.
    pub fn dir_name(&self) -> String {
        let mut fields: Vec<String> = self
            .pairs
            .iter()
            .map(|(key, value)| format!("{key}={}", escape(value)))
            .collect();
        if let Some(prefix) = &self.prefix {
            fields.push(format!("prefix={}", escape(prefix)));
        }
        format!("{}:{}", self.kind, fields.join(","))
    }

    /// Read a gvfs-fuse directory name back into a spec. `None` for a name
    /// that is not one — gvfs-fuse's directory holds nothing else, but a
    /// directory is not a promise.
    pub fn from_dir_name(name: &str) -> Option<Spec> {
        let (kind, fields) = name.split_once(':')?;
        let mut pairs = Vec::new();
        let mut prefix = None;
        for field in fields.split(',').filter(|field| !field.is_empty()) {
            let (key, value) = field.split_once('=')?;
            let value = decode(value);
            if key == "prefix" {
                let trimmed = value.trim_matches('/');
                prefix = (!trimmed.is_empty()).then(|| format!("/{trimmed}"));
            } else {
                pairs.push((key.to_string(), value));
            }
        }
        pairs.sort();
        Some(Spec {
            kind: kind.to_string(),
            pairs,
            prefix,
        })
    }

    /// Whether two specs are one server connection, prefix aside: the same
    /// backend and the same keys, with the values agreeing — ignoring case in
    /// the host names and the share, which gvfs folds on some paths through it
    /// and not on others.
    fn same_server(&self, other: &Spec) -> bool {
        let folded = |key: &str| matches!(key, "host" | "server" | "share");
        self.kind == other.kind
            && self.pairs.len() == other.pairs.len()
            && self
                .pairs
                .iter()
                .zip(&other.pairs)
                .all(|((key, a), (other_key, b))| {
                    key == other_key
                        && if folded(key) {
                            a.to_lowercase() == b.to_lowercase()
                        } else {
                            a == b
                        }
                })
    }
}

/// The mounts `gio mount -l` lists at the left margin, as `(name, url)`.
///
/// gio nests a mount under the volume and the drive it belongs to, and
/// everything nested is a disk that udisks2 has already told the other half of
/// the card about. A network share has no volume, so it is listed on its own:
///
/// ```text
/// Mount(0): share on nas -> smb://nas/share/
///   Type: GDaemonMount
/// ```
///
/// A top-level `file://` mount is dropped as well: that is an fstab entry, a
/// directory that is already on this machine rather than a share gvfs holds.
pub fn gio_mounts(listing: &str) -> Vec<(String, String)> {
    listing
        .lines()
        // At the margin only: an indented mount belongs to a volume.
        .filter(|line| line.starts_with("Mount("))
        .filter_map(mount_line)
        .filter(|(_, url)| !url.starts_with("file://"))
        .collect()
}

/// A `Mount(N): name -> url` line of gio's, indentation already taken off,
/// as `(name, url)`.
fn mount_line(line: &str) -> Option<(String, String)> {
    let (_, rest) = line.strip_prefix("Mount(")?.split_once("): ")?;
    // From the right: the URL is escaped and has no spaces in it; the name is
    // whatever the backend called the mount.
    let (name, url) = rest.rsplit_once(" -> ")?;
    Some((name.to_string(), url.trim().to_string()))
}

/// The directory among `present` gvfs-fuse made for a mount with this spec,
/// or the name gvfs would give it when none is there (see [`shares_from`]).
fn fuse_dir(spec: &Spec, present: &[(&String, Spec)]) -> String {
    present
        .iter()
        .find(|(_, there)| spec.same_server(there) && spec.prefix == there.prefix)
        .map(|(dir, _)| (*dir).clone())
        .unwrap_or_else(|| spec.dir_name())
}

/// What `root` holds that reads as a gvfs-fuse directory name.
fn specs_in(entries: &[String]) -> Vec<(&String, Spec)> {
    entries
        .iter()
        .filter_map(|name| Some((name, Spec::from_dir_name(name)?)))
        .collect()
}

/// The Network section's rows: [`gio_mounts`] out of `listing`, each with the
/// directory under `root` that gvfs-fuse shows it as.
///
/// `entries` is what `root` actually holds. The spec is rebuilt from each URL
/// and looked for among them field by field rather than as text, because the
/// text does not always agree — an SMB server name is case-folded on one side
/// and not the other, and a value can be escaped more than one way. When
/// nothing there matches (gvfs-fuse not running, or a backend whose spec this
/// file does not know) the path is the rebuilt name: right for every backend
/// in [`SCHEMES`], and an honest "is not there" for the rest.
///
/// A URL that does not parse is not a row, because there is nowhere to go.
/// A phone's or a camera's mount is not a share: it is a row of the Devices
/// section ([`phones_from`]), and listed here too it would be there twice.
pub fn shares_from(listing: &str, root: &Path, entries: &[String]) -> Vec<Share> {
    let present = specs_in(entries);
    let mut shares: Vec<Share> = gio_mounts(listing)
        .into_iter()
        .filter(|(_, url)| device_scheme(url).is_none())
        .filter_map(|(name, url)| {
            let address = Address::parse(&url)?;
            let dir = fuse_dir(&Spec::of(&address), &present);
            // A backend that is not a server (an archive) is called what gio
            // calls it; a URL-shaped label for it would be noise.
            let label = if SCHEMES.contains(&address.scheme.as_str()) && !address.host.is_empty() {
                address.label()
            } else {
                name
            };
            Some(Share {
                label,
                scheme: address.scheme,
                path: root.join(dir),
                url,
            })
        })
        .collect();
    shares.sort_by(|a, b| {
        a.label
            .to_lowercase()
            .cmp(&b.label.to_lowercase())
            .then_with(|| a.url.cmp(&b.url))
    });
    shares
}

// ── Phones and cameras (gvfs) ───────────────────────────────────────────────

/// Which of gvfs's volume monitors saw a phone or a camera: what kind of
/// thing it is, and so the glyph its row wears.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// `gvfs-mtp`: a phone, or anything else that speaks MTP.
    Mtp,
    /// `gvfs-gphoto2`: a camera, over PTP.
    Gphoto2,
}

impl Protocol {
    /// The protocol a URL scheme is, when it is one of these two.
    pub fn of_scheme(scheme: &str) -> Option<Protocol> {
        match scheme.to_ascii_lowercase().as_str() {
            "mtp" => Some(Protocol::Mtp),
            "gphoto2" => Some(Protocol::Gphoto2),
            _ => None,
        }
    }

    /// The protocol whose volume monitor a gio `Type:` line names —
    /// `GProxyVolume (GProxyVolumeMonitorMTP)`, `… (GProxyVolumeMonitorGPhoto2)`
    /// — and `None` for udisks2's, and for everything else.
    fn of_monitor(kind: &str) -> Option<Protocol> {
        let kind = kind.to_ascii_lowercase();
        if kind.contains("volumemonitormtp") {
            Some(Protocol::Mtp)
        } else if kind.contains("volumemonitorgphoto2") {
            Some(Protocol::Gphoto2)
        } else {
            None
        }
    }
}

/// The protocol of a URL whose scheme is a phone's or a camera's.
fn device_scheme(url: &str) -> Option<Protocol> {
    Protocol::of_scheme(url.split_once("://")?.0)
}

/// One phone or camera gvfs has seen: a row in the Devices section, under the
/// disks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Phone {
    /// The volume's activation root, `mtp://Google_Pixel_10a_…/`: what
    /// `gio mount` mounts and `gio mount -u` puts away, and the row's identity
    /// across a refresh.
    pub root: String,
    /// What gio calls it: `Pixel 10a`.
    pub name: String,
    pub protocol: Protocol,
    /// The directory gvfs-fuse shows it as, while it is mounted: where `Enter`
    /// goes.
    pub mount: Option<PathBuf>,
}

impl Phone {
    pub fn is_mounted(&self) -> bool {
        self.mount.is_some()
    }

    /// What the row says after the name: where it is, or that it is not
    /// mounted. The name is the model already (`Pixel 10a`), and the glyph
    /// says phone or camera, so neither is said twice.
    pub fn detail(&self) -> String {
        match &self.mount {
            Some(path) => path.to_string_lossy().into_owned(),
            None => NOT_MOUNTED.to_string(),
        }
    }
}

/// Whether two gio URLs are one root, trailing `/` or not.
fn same_root(a: &str, b: &str) -> bool {
    a.trim_end_matches('/') == b.trim_end_matches('/')
}

/// The Devices section's phones and cameras out of a `gio mount -li` listing,
/// each mounted one with the directory under `root` that gvfs-fuse shows it
/// as — `entries` being what `root` holds, matched as [`shares_from`]
/// matches a share's.
///
/// A phone is a **volume** that gvfs's MTP or gphoto2 monitor reported,
/// whatever it is nested under (neither monitor makes drives, so it is at the
/// margin), with the `activation_root=` that `-i` prints in its block. It is
/// **mounted** when a mount of that root is listed anywhere: nested under the
/// volume (gvfs's shadow of the mount) or at the margin (the daemon's own).
/// A margin mount of an `mtp://` or `gphoto2://` root that no volume claims —
/// mounted by address on a machine whose volume monitor is not running — is a
/// row as well, called what gio calls the mount. A volume with no root is not
/// a row, because there is nothing to hand `gio mount`.
///
/// Sorted by name, then by root, so two of one model keep their order.
pub fn phones_from(listing: &str, root: &Path, entries: &[String]) -> Vec<Phone> {
    let lines: Vec<(usize, &str)> = listing
        .lines()
        .map(|line| {
            let text = line.trim_start();
            (line.len() - text.len(), text.trim_end())
        })
        .collect();
    // Every mount of a device's root, wherever gio put it.
    let mounts: Vec<(String, String)> = lines
        .iter()
        .filter_map(|(_, text)| mount_line(text))
        .filter(|(_, url)| device_scheme(url).is_some())
        .collect();
    let mut phones: Vec<(String, String, Protocol)> = Vec::new();
    for (at, (indent, text)) in lines.iter().enumerate() {
        let Some((_, name)) = text
            .strip_prefix("Volume(")
            .and_then(|rest| rest.split_once("): "))
        else {
            continue;
        };
        let mut protocol = None;
        let mut activation = None;
        let mut nested = None;
        for (_, line) in lines[at + 1..]
            .iter()
            .take_while(|(inner, _)| inner > indent)
        {
            if let Some(kind) = line.strip_prefix("Type: ") {
                protocol = protocol.or(Protocol::of_monitor(kind));
            } else if let Some(url) = line.strip_prefix("activation_root=") {
                activation = activation.or(Some(url.to_string()));
            } else if let Some((_, url)) = mount_line(line) {
                nested = nested.or(Some(url));
            }
        }
        let (Some(protocol), Some(root)) = (protocol, activation.or(nested)) else {
            continue;
        };
        if !phones.iter().any(|(known, _, _)| same_root(known, &root)) {
            phones.push((root, name.to_string(), protocol));
        }
    }
    for (name, url) in &mounts {
        if !phones.iter().any(|(known, _, _)| same_root(known, url)) {
            if let Some(protocol) = device_scheme(url) {
                phones.push((url.clone(), name.clone(), protocol));
            }
        }
    }
    let present = specs_in(entries);
    let mut phones: Vec<Phone> = phones
        .into_iter()
        .map(|(url, name, protocol)| {
            let mounted = mounts.iter().any(|(_, mount)| same_root(mount, &url));
            let mount = Address::parse(&url)
                .filter(|_| mounted)
                .map(|address| root.join(fuse_dir(&Spec::of(&address), &present)));
            Phone {
                root: url,
                name,
                protocol,
                mount,
            }
        })
        .collect();
    phones.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.root.cmp(&b.root))
    });
    phones
}

/// Where gvfs-fuse shows its mounts: `$XDG_RUNTIME_DIR/gvfs`
/// ([`df_core::du::gvfs_root`]).
pub fn gvfs_root() -> PathBuf {
    df_core::du::gvfs_root()
}

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

/// How this program runs one `gio` command to its end: `gio` itself
/// ([`system_gio`]), or a test's stand-in, which notes what it was asked and
/// answers the way gio would have. The mounts and unmounts go through it; the
/// listing, which is started and collected in two halves, does not.
pub type Gio =
    std::sync::Arc<dyn Fn(&[&str]) -> std::io::Result<std::process::Output> + Send + Sync>;

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

/// The first line of `text` with anything on it.
fn first_line(text: &str) -> Option<&str> {
    text.lines().map(str::trim).find(|line| !line.is_empty())
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

/// What the card says to somebody whose phone would not open because of the
/// phone rather than the program.
pub const UNLOCK: &str = "Unlock the phone and choose File transfer, then try again";

/// [`UNLOCK`], when a phone's mount failed in the words libmtp and gvfs use
/// for a device they could not open or found busy — which is what a locked
/// phone, or one plugged in only to charge, looks like from here. `None` for
/// any other failure, whose own words say more, and for a camera, which has
/// no screen to unlock.
pub fn unlock_hint(protocol: Protocol, message: &str) -> Option<&'static str> {
    if protocol != Protocol::Mtp {
        return None;
    }
    let said = message.to_lowercase();
    [
        "unable to open mtp device",
        "device is busy",
        "libmtp_error",
    ]
    .iter()
    .any(|words| said.contains(words))
    .then_some(UNLOCK)
}

/// Check what was typed at the connect prompt, and return the address to hand
/// to `gio mount`. The error is the prompt's inline message.
///
/// The scheme is the check that matters: it decides whether gvfs has a backend
/// for the address at all, and it is the part a person types from memory.
/// Beyond that the address only has to name a server; whether the share exists
/// is the server's question, and gio's error answers it.
pub fn connect_url(text: &str) -> Result<String, String> {
    let text = text.trim();
    if text.is_empty() {
        return Err("type an address, like smb://host/share".to_string());
    }
    let Some(address) = Address::parse(text) else {
        return Err(format!("not an address: start with {SCHEME_LIST}"));
    };
    if !SCHEMES.contains(&address.scheme.as_str()) {
        return Err(format!(
            "{}:// is not a network share: use {SCHEME_LIST}",
            address.scheme
        ));
    }
    if address.host.is_empty() {
        return Err("no server in that address".to_string());
    }
    // The scheme lower-cased, which is how gio looks it up; the rest exactly
    // as typed, escapes and all.
    let (_, rest) = text.split_once("://").unwrap_or_default();
    Ok(format!("{}://{rest}", address.scheme))
}

/// What one `gio mount` with nobody to answer its questions came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attempt {
    Mounted,
    /// It wanted a password, a user name, or a yes to a host key, and with
    /// stdin closed there was nobody to give one.
    NeedsTerminal,
    /// It failed for a reason a terminal would not fix: the reason.
    Failed(String),
}

/// Read a finished `gio mount`.
///
/// **gio asks its questions on stdout, not stderr.** With stdin closed it
/// prints the prompt — `Authentication Required`, `Enter user and password
/// for…`, `User:` — reads end-of-file, gives up, and exits 2 with *nothing* on
/// stderr, because a mount the user cancelled is not an error it reports. So
/// both streams are searched for the words, and a failure that was silent on
/// stderr but said something on stdout is a question nobody could answer:
/// that is also how an unknown SSH host key looks, and it wants the terminal
/// just as much as a password does.
///
/// **"Already mounted" is not a failure.** gio refuses an address whose share
/// is up already — `Location is already mounted`, exit 2 — and what the person
/// who typed it wanted was to be there, which the share being up already makes
/// possible. So it reads as mounted, and the connect goes where it would have.
pub fn attempt(success: bool, stdout: &str, stderr: &str) -> Attempt {
    if success || stderr.to_lowercase().contains("already mounted") {
        return Attempt::Mounted;
    }
    let said = format!("{stdout}\n{stderr}").to_lowercase();
    let asked = ["password", "authenticat", "no mount operation"]
        .iter()
        .any(|word| said.contains(word));
    let unanswered = stderr.trim().is_empty() && !stdout.trim().is_empty();
    if asked || unanswered {
        return Attempt::NeedsTerminal;
    }
    Attempt::Failed(
        first_line(stderr)
            .unwrap_or("gio mount failed without saying why")
            .to_string(),
    )
}

/// What [`connect`] came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Connected {
    /// Mounted, and where to go: inside the share, at the path the address
    /// went on to name, when there is one; the share itself when not. `None`
    /// when the mount never appeared under gvfs-fuse's directory, so there is
    /// nowhere to go.
    Mounted(Option<PathBuf>),
    NeedsTerminal,
    Failed(String),
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

/// Where `address` lands among the mounted shares: the share it is on, and
/// the path inside that share the address went on to name.
///
/// `sftp://me@host/srv/www` is on the share `sftp://me@host/` and names
/// `srv/www` in it; `smb://nas/media/films` is on `smb://nas/media/` and names
/// `films`. When more than one share is under the address the deepest wins —
/// a WebDAV server can have a mount at its root and another under
/// `/remote.php/webdav`, and the address meant the one it is inside.
///
/// `.` and `..` are dropped from the rest of the path: they are the server's to
/// resolve, and joined onto a local path they would walk out of the share.
pub fn landing(shares: &[Share], address: &Address) -> Option<(PathBuf, PathBuf)> {
    let wanted = Spec::of(address);
    let asked: Vec<&str> = address.segments().collect();
    let folded = address.scheme == "smb";
    shares
        .iter()
        .filter_map(|share| {
            let root = Address::parse(&share.url)?;
            if !Spec::of(&root).same_server(&wanted) {
                return None;
            }
            let base: Vec<&str> = root.segments().collect();
            let under = base.len() <= asked.len()
                && base.iter().zip(&asked).all(|(a, b)| {
                    if folded {
                        a.to_lowercase() == b.to_lowercase()
                    } else {
                        a == b
                    }
                });
            if !under {
                return None;
            }
            let within: PathBuf = asked[base.len()..]
                .iter()
                .filter(|segment| !matches!(**segment, "." | ".."))
                .collect();
            Some((base.len(), share.path.clone(), within))
        })
        .max_by_key(|(depth, _, _)| *depth)
        .map(|(_, path, within)| (path, within))
}

// ── The worker ──────────────────────────────────────────────────────────────

/// What the event loop asks the worker to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// Everything the card lists that it does not know already: udisks2's
    /// disks, and gvfs's phones and shares.
    List,
    /// A block object's path.
    Mount(String),
    Unmount(String),
    /// The drive to eject, and the block object whose row asked.
    Eject {
        object: String,
        drive: String,
    },
    /// A share's URL or a phone's root, for `gio mount -u`.
    GioUnmount(String),
}

impl Request {
    /// The row the request is about — a block object's path, a share's URL,
    /// a phone's root, the identities [`Card::start`] keeps a busy row by —
    /// or `None` for a listing, which is about no row.
    pub fn row(&self) -> Option<&str> {
        match self {
            Request::List => None,
            Request::Mount(object) | Request::Unmount(object) => Some(object),
            Request::Eject { object, .. } => Some(object),
            Request::GioUnmount(url) => Some(url),
        }
    }

    /// What the row says while the request is out.
    pub fn doing(&self) -> &'static str {
        match self {
            Request::List => "listing…",
            Request::Mount(_) => "mounting…",
            Request::Unmount(_) | Request::GioUnmount(_) => "unmounting…",
            Request::Eject { .. } => "ejecting…",
        }
    }
}

/// One reply, with the request it answers, so a failed listing is never
/// taken for the failure of a mount that is out at the same moment.
#[derive(Debug, Clone)]
pub struct Answer {
    pub to: Request,
    pub reply: Reply,
}

/// What comes back.
#[derive(Debug, Clone)]
pub enum Reply {
    /// The card's contents from one request — so the card never shows the
    /// disks of one moment beside the phones or the shares of another.
    Listing {
        devices: Vec<Device>,
        phones: Vec<Phone>,
        shares: Vec<Share>,
    },
    /// A mount landed, and this is where. The card cds there on the next
    /// `Enter`.
    Mounted(PathBuf),
    Unmounted,
    Ejected,
    /// Something failed, already turned into a sentence by
    /// [`crate::dbus::readable_error`] or taken from gio's own stderr.
    Failed(String),
}

/// The worker, and the two channels either side of it.
///
/// Dropping it closes the request queue and joins the thread, so a test — and a
/// closed card — cannot outlive its connection.
pub struct Mounts {
    requests: Option<Sender<Request>>,
    replies: Receiver<Answer>,
    worker: Option<std::thread::JoinHandle<()>>,
    /// What has been asked and not yet answered, oldest first: the worker
    /// answers each request once, in order. Kept here rather than on the
    /// card, which is built anew each time `M` opens it — a mount asked for
    /// by a card that has since closed is still out ([`Mounts::in_flight`]).
    outstanding: Vec<Request>,
}

impl Mounts {
    /// Start the worker. `notify` is rung once per reply; `gio` is how it
    /// puts a share or a phone away.
    pub fn start(notify: df_core::fs::Notifier, gio: Gio) -> Mounts {
        let (tx, rx) = unbounded::<Request>();
        let (reply_tx, reply_rx) = unbounded::<Answer>();
        let handle = std::thread::Builder::new()
            .name("df-mounts".to_string())
            .spawn(move || run(rx, reply_tx, notify, gio));
        let worker = match handle {
            Ok(h) => Some(h),
            Err(e) => {
                log::warn!("the mounts worker did not start: {e}");
                None
            }
        };
        Mounts {
            requests: Some(tx),
            replies: reply_rx,
            worker,
            outstanding: Vec::new(),
        }
    }

    /// A worker with no thread behind it, for a test: what it is asked goes
    /// to the first end returned, for the test to read, and what the test
    /// sends down the second comes back as the worker's answers — so no test
    /// talks to this machine's udisks2 or runs its gio.
    #[cfg(test)]
    pub fn detached() -> (Mounts, Receiver<Request>, Sender<Answer>) {
        let (tx, rx) = unbounded::<Request>();
        let (answer, replies) = unbounded::<Answer>();
        (
            Mounts {
                requests: Some(tx),
                replies,
                worker: None,
                outstanding: Vec::new(),
            },
            rx,
            answer,
        )
    }

    pub fn ask(&mut self, request: Request) {
        if let Some(requests) = &self.requests {
            if requests.send(request.clone()).is_ok() {
                self.outstanding.push(request);
            }
        }
    }

    /// Whatever the worker has answered, each with the request it answers.
    pub fn drain(&mut self) -> Vec<Answer> {
        let answers: Vec<Answer> = self.replies.try_iter().collect();
        for answer in &answers {
            if let Some(at) = self
                .outstanding
                .iter()
                .position(|asked| *asked == answer.to)
            {
                self.outstanding.remove(at);
            }
        }
        answers
    }

    /// The oldest request about a row that is still out, as the row it is
    /// about and what the row says meanwhile: what a card opened while it is
    /// out shows on that row ([`Card::start`]).
    pub fn in_flight(&self) -> Option<(&str, &'static str)> {
        self.outstanding
            .iter()
            .find_map(|asked| Some((asked.row()?, asked.doing())))
    }
}

impl Drop for Mounts {
    fn drop(&mut self) {
        self.requests = None;
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// The worker loop.
///
/// The connection is opened lazily and kept: a card that is opened and closed
/// four times should not authenticate four times, and a connection that has
/// gone away is reopened on the next request rather than being an error the
/// user has to do something about.
fn run(
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
            crate::dbus::marshal_no_options(&mut args);
            let body = bus.call(SERVICE, object, FILESYSTEM, "Mount", Some("a{sv}"), &args)?;
            let path = crate::dbus::Reader::new(&body).string()?;
            Ok(Reply::Mounted(PathBuf::from(path)))
        }
        Request::Unmount(object) => {
            let mut args = Vec::new();
            crate::dbus::marshal_no_options(&mut args);
            bus.call(SERVICE, object, FILESYSTEM, "Unmount", Some("a{sv}"), &args)?;
            Ok(Reply::Unmounted)
        }
        Request::Eject { drive, .. } => {
            let mut args = Vec::new();
            crate::dbus::marshal_no_options(&mut args);
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
    let objects = crate::dbus::parse_managed_objects(&body)?;
    Ok(devices_from(&objects))
}

// ── Hearing a phone arrive: `gio mount --monitor` ───────────────────────────

/// What happened, in the words of `gio mount --monitor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    VolumeAdded,
    VolumeRemoved,
    MountAdded,
    MountRemoved,
    /// Anything else gio reports — a volume or a mount changed, a drive came
    /// or went — which says only that the card, if it is up, should look
    /// again.
    Other,
}

/// One event gio printed, read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub change: Change,
    /// The name in quotes on the event's line: `Pixel 10a`.
    pub name: String,
    /// Whether the volume or the mount is a phone's or a camera's, and which:
    /// from the `Type:` line `--detail` prints under the event, or from the
    /// scheme of its root.
    pub protocol: Option<Protocol>,
    /// Its root: a volume's `activation_root=`, a mount's URL.
    pub root: Option<String>,
}

/// gio's event lines, each `Head: 'name'` with the head padded so the names
/// line up: the head, what it is, and whether `--detail` prints the volume,
/// mount or drive under it — every one does, closing with a blank line,
/// except the eject button's.
const EVENTS: &[(&str, Change, bool)] = &[
    ("Volume added", Change::VolumeAdded, true),
    ("Volume removed", Change::VolumeRemoved, true),
    ("Mount added", Change::MountAdded, true),
    ("Mount removed", Change::MountRemoved, true),
    ("Volume changed", Change::Other, true),
    ("Mount changed", Change::Other, true),
    ("Mount pre-unmount", Change::Other, true),
    ("Drive connected", Change::Other, true),
    ("Drive disconnected", Change::Other, true),
    ("Drive changed", Change::Other, true),
    ("Drive eject button", Change::Other, false),
];

/// The event a line of gio's opens, if it opens one: what changed, the name,
/// and whether detail follows. Only a line at the margin: everything under
/// an event is indented.
fn event_line(line: &str) -> Option<(Change, String, bool)> {
    let (head, rest) = line.split_once(':')?;
    let (_, change, detail) = EVENTS.iter().find(|(known, _, _)| *known == head)?;
    let name = rest.trim();
    let name = name
        .strip_prefix('\'')
        .and_then(|name| name.strip_suffix('\''))
        .unwrap_or(name);
    Some((*change, name.to_string(), *detail))
}

/// The lines of gio's monitor, gathered into one block per event: the event's
/// line and the detail printed under it, up to the blank line that ends it.
///
/// Nothing is held back for a line that is not coming: an event with no
/// detail is a block the moment its line arrives, and an event line that
/// arrives while a block is open closes that block first. A line that belongs
/// to no event — gio saying something of its own — is dropped.
#[derive(Debug, Default)]
pub struct Blocks {
    open: Vec<String>,
}

impl Blocks {
    /// Take one line (its newline or not); returns the blocks it completed,
    /// oldest first.
    pub fn feed(&mut self, line: &str) -> Vec<Vec<String>> {
        let line = line.trim_end_matches(['\n', '\r']);
        let mut done = Vec::new();
        if let Some((_, _, detail)) = event_line(line) {
            if !self.open.is_empty() {
                done.push(std::mem::take(&mut self.open));
            }
            if detail {
                self.open.push(line.to_string());
            } else {
                done.push(vec![line.to_string()]);
            }
        } else if line.trim().is_empty() {
            if !self.open.is_empty() {
                done.push(std::mem::take(&mut self.open));
            }
        } else if !self.open.is_empty() {
            self.open.push(line.to_string());
        }
        done
    }

    /// The stream ended: the block that was open, if one was.
    pub fn finish(&mut self) -> Option<Vec<String>> {
        (!self.open.is_empty()).then(|| std::mem::take(&mut self.open))
    }
}

/// One block of [`Blocks`], read: `None` for a block that does not start
/// with an event's line.
///
/// The root is the first one the detail names — a volume's activation root,
/// which comes before any mount nested under it, or a mount's own URL.
pub fn event_from(block: &[String]) -> Option<Event> {
    let (first, rest) = block.split_first()?;
    let (change, name, _) = event_line(first)?;
    let mut protocol = None;
    let mut root: Option<String> = None;
    for line in rest {
        let line = line.trim();
        if let Some(kind) = line.strip_prefix("Type: ") {
            protocol = protocol.or(Protocol::of_monitor(kind));
        } else if let Some(url) = line.strip_prefix("activation_root=") {
            root = root.or(Some(url.to_string()));
        } else if let Some((_, url)) = mount_line(line) {
            root = root.or(Some(url));
        }
    }
    let protocol = protocol.or_else(|| root.as_deref().and_then(device_scheme));
    Some(Event {
        change,
        name,
        protocol,
        root,
    })
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

    /// Whether gio has died: its pipe ended, and the thread with it.
    pub fn gone(&self) -> bool {
        self.gone.load(std::sync::atomic::Ordering::SeqCst)
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

/// Whether a watcher whose gio has died is started again now.
///
/// **Once** in a window's life, and not before
/// [`crate::appearance::RETRY`] has passed since it was started — the
/// desktop-theme watcher's ten seconds, for its reason: something that died
/// at once dies again at once, and a restart a frame would be a process a
/// frame. And no frame is asked for it: the first frame something else
/// brings after it is due starts it, as the theme watcher's does. Once only,
/// because a gio that has died twice has something wrong with it that a
/// third start will not fix; the card still lists on `M` and `r`, so what is
/// lost is being told about a phone plugged in with the card shut.
pub fn restart_due(gone: bool, restarts: u32, started: Instant, now: Instant) -> bool {
    gone && restarts == 0 && now.saturating_duration_since(started) >= crate::appearance::RETRY
}

/// The reader thread: gio's stdout, line by line, until it ends. Any reader,
/// so a test can hand it gio's words without a gio.
fn listen(stdout: impl std::io::Read, events: Sender<Event>, notify: df_core::fs::Notifier) {
    use std::io::BufRead;
    let mut reader = std::io::BufReader::new(stdout);
    let mut blocks = Blocks::default();
    let mut bytes = Vec::new();
    let hand_over = |done: Vec<Vec<String>>| {
        let mut sent = false;
        for event in done.iter().filter_map(|block| event_from(block)) {
            sent |= events.send(event).is_ok();
        }
        if sent {
            notify();
        }
    };
    loop {
        bytes.clear();
        match reader.read_until(b'\n', &mut bytes) {
            Ok(0) | Err(_) => break,
            Ok(_) => hand_over(blocks.feed(&String::from_utf8_lossy(&bytes))),
        }
    }
    hand_over(blocks.finish().into_iter().collect());
}

// ── The card's state ────────────────────────────────────────────────────────

/// One row of the Places section: a pinned folder or a `[goto]` row, as the
/// app's Places list describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    /// What the folder is called: its last name, `Work`, `srv`.
    pub name: String,
    /// Where it is, as the lists show it: `~/Work`, `sftp://host/srv`.
    pub detail: String,
    /// The key that goes there, `g w`, drawn at the row's end; `None` for a
    /// pin with no key of its own.
    pub key: Option<String>,
    /// Where `Enter` goes.
    pub target: PathBuf,
    /// A place on another machine, which wears the network glyph rather than
    /// the folder.
    pub remote: bool,
    /// Pinned by hand, so `d` can take it off. A `[goto]` row is the config's,
    /// and the card offers no key for it.
    pub pinned: bool,
}

/// What the Places section says when there is nothing in it: what it is for,
/// and the key that fills it.
pub const PLACES_EMPTY: &str = "Nothing pinned · g b pins this folder";

/// One cloud remote in the Network section: an rclone service — a remote in
/// the user's `rclone.conf`, or a `type = "rclone"` service in `vfs.toml`.
///
/// Not a share: nothing is mounted, gvfs has never heard of it, and there is
/// nothing to unmount or eject. It is on this card because the card is where a
/// person looks for "where can I go that is not on this disk", and a Google
/// Drive is an answer to that. `Enter` goes there, as `rclone://<name>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cloud {
    /// The service's name, which is what the URL addresses.
    pub name: String,
    /// rclone's type for it — `s3`, `drive`, `dropbox` — or `rclone` when the
    /// config did not say.
    pub provider: String,
}

impl Cloud {
    /// Where `Enter` goes: the top of the remote.
    pub fn target(&self) -> PathBuf {
        PathBuf::from(df_core::vfs::VfsPath::rclone(&self.name, "").to_url())
    }

    /// What the row says the remote is: the service by the name people know
    /// it by — `Google Drive`, `S3` — where rclone's type for it is one of
    /// those, and rclone's own word for it otherwise.
    pub fn service(&self) -> String {
        let known = match self.provider.to_ascii_lowercase().as_str() {
            "drive" => "Google Drive",
            "s3" => "S3",
            "dropbox" => "Dropbox",
            "onedrive" => "OneDrive",
            "b2" => "Backblaze B2",
            "box" => "Box",
            "pcloud" => "pCloud",
            "mega" => "MEGA",
            "protondrive" => "Proton Drive",
            "googlephotos" => "Google Photos",
            "google cloud storage" | "gcs" => "Google Cloud Storage",
            "azureblob" => "Azure Blob Storage",
            "azurefiles" => "Azure Files",
            "swift" => "Swift",
            "webdav" => "WebDAV",
            "sftp" => "SFTP",
            "ftp" => "FTP",
            "smb" => "SMB",
            "jottacloud" => "Jottacloud",
            "koofr" => "Koofr",
            "yandex" => "Yandex Disk",
            _ => return self.provider.clone(),
        };
        known.to_string()
    }
}

/// Something the cursor can be on, in the card's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    /// An index into [`Card::devices`].
    Disk(usize),
    /// An index into [`Card::phones`].
    Phone(usize),
    /// An index into [`Card::places`].
    Place(usize),
    /// An index into [`Card::shares`].
    Share(usize),
    /// An index into [`Card::clouds`].
    Cloud(usize),
    /// The Network section's last row, always there.
    Connect,
}

/// One line of the card, from top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Line {
    /// A section's name: `Devices`, `Network`, `Places`.
    Section(&'static str),
    /// A section with nothing in it, and why: one line, a row's height.
    Empty(&'static str),
    Item(Item),
}

impl Line {
    fn height(self) -> f32 {
        match self {
            Line::Section(_) => SECTION_ROW,
            Line::Empty(_) | Line::Item(_) => ROW,
        }
    }
}

/// The sections, top to bottom. Always all three, each with its heading, so
/// the headings are a fixed part of the card's height ([`window`]).
const SECTIONS: [&str; 3] = ["Devices", "Network", "Places"];

/// The card's own state, while it is open.
pub struct Card {
    /// The Places section, handed in by the app when the card opens and again
    /// when a pin comes off it.
    pub places: Vec<Place>,
    pub devices: Vec<Device>,
    /// The phones and cameras, under the disks in the Devices section.
    pub phones: Vec<Phone>,
    pub shares: Vec<Share>,
    /// The rclone services, after the shares in the Network section. Handed
    /// in by the app when the card opens ([`Card::set_clouds`]): they are the
    /// vfs config's, known at once, and never part of a udisks2 or gvfs
    /// listing.
    pub clouds: Vec<Cloud>,
    /// Which of [`Card::items`] the cursor is on.
    pub cursor: usize,
    /// The first of [`Card::lines`] drawn.
    pub first: usize,
    /// A call is in flight, so the card is drawn busy and takes no new one.
    /// One at a time: two mounts of the same device is one of them failing with
    /// `AlreadyMounted`, and a card that let you start it is a card that
    /// produced an error you caused by being allowed to. Holds the busy row's
    /// identity — a block object's path, a share's URL, a phone's root — and
    /// what is being done to it (`mounting…`), which the row says in place of
    /// its detail ([`Card::start`]).
    pub busy: Option<(String, &'static str)>,
    /// The last call's failure, on the row it failed on, in place of the
    /// row's detail until the next call starts ([`Card::fail`]).
    pub failed: Option<(String, String)>,
    /// Nothing has come back yet.
    pub loading: bool,
    /// How tall the body may be before it scrolls: [`WINDOW`], or less in a
    /// window too short for it ([`Card::fit`], [`window`]).
    window: f32,
    /// When the lines last scrolled, for their bar.
    bar: crate::scrollbar::Linger,
    /// The wheel's roll that has not come to a whole line yet
    /// ([`crate::mouse::roll`]).
    carry: f32,
    /// The wheel has scrolled the lines off the cursor, and they stay where
    /// it left them until a key or a click moves the cursor: the panes' rule
    /// ([`crate::tab::Listing::attach`]). Without it the card would follow
    /// the cursor back on the next frame ([`Card::fit`]).
    detached: bool,
}

impl Card {
    pub fn new() -> Card {
        Card {
            places: Vec::new(),
            devices: Vec::new(),
            phones: Vec::new(),
            shares: Vec::new(),
            clouds: Vec::new(),
            cursor: 0,
            first: 0,
            busy: None,
            failed: None,
            loading: true,
            window: WINDOW,
            bar: crate::scrollbar::Linger::default(),
            carry: 0.0,
            detached: false,
        }
    }

    /// A card with its Places section filled, the cursor on its first row.
    ///
    /// `M` `Enter` mounting the stick — or the phone — just plugged in is the
    /// card's main job, so the Devices section is on top and the cursor starts
    /// on its first row, before udisks2 has named it: the places are this
    /// machine's own data and known the instant the card opens, the devices
    /// are not. Until the listing lands that row is the first of the Network
    /// section's; [`Card::update`] keeps the index, and the first device
    /// fills it. The places are at the bottom, one `↑` away round the top.
    pub fn with_places(places: Vec<Place>) -> Card {
        let mut card = Card::new();
        card.places = places;
        card.follow();
        card
    }

    /// The rclone services, put under the shares. The cursor keeps its index,
    /// which on a card still waiting for udisks2 is the first row — so it
    /// rests on the first cloud row until the devices arrive above it,
    /// exactly as it rests on the connect row when there are none (see
    /// [`Card::with_places`]).
    pub fn set_clouds(&mut self, clouds: Vec<Cloud>) {
        self.clouds = clouds;
        self.cursor = self.cursor.min(self.items().len().saturating_sub(1));
        self.rebuilt();
    }

    /// Everything the cursor can land on, in order: the disks, the phones,
    /// the shares, the cloud remotes, the connect row, and the places. Never
    /// empty — the connect row is always there.
    pub fn items(&self) -> Vec<Item> {
        (0..self.devices.len())
            .map(Item::Disk)
            .chain((0..self.phones.len()).map(Item::Phone))
            .chain((0..self.shares.len()).map(Item::Share))
            .chain((0..self.clouds.len()).map(Item::Cloud))
            .chain(std::iter::once(Item::Connect))
            .chain((0..self.places.len()).map(Item::Place))
            .collect()
    }

    /// Every line, headings and empty states included.
    pub fn lines(&self) -> Vec<Line> {
        let [devices, network, places] = SECTIONS;
        let mut lines = vec![Line::Section(devices)];
        match self.devices_empty() {
            Some(message) => lines.push(Line::Empty(message)),
            None => lines.extend(
                (0..self.devices.len())
                    .map(Item::Disk)
                    .chain((0..self.phones.len()).map(Item::Phone))
                    .map(Line::Item),
            ),
        }
        lines.push(Line::Section(network));
        match self.shares_empty() {
            Some(message) => lines.push(Line::Empty(message)),
            None => lines.extend((0..self.shares.len()).map(|i| Line::Item(Item::Share(i)))),
        }
        // After gvfs's shares, whatever gvfs said about them: "nothing
        // mounted" stays true with a Google Drive under it, because nothing
        // was mounted to reach one.
        lines.extend((0..self.clouds.len()).map(|i| Line::Item(Item::Cloud(i))));
        lines.push(Line::Item(Item::Connect));
        lines.push(Line::Section(places));
        if self.places.is_empty() {
            lines.push(Line::Empty(PLACES_EMPTY));
        } else {
            lines.extend((0..self.places.len()).map(|i| Line::Item(Item::Place(i))));
        }
        lines
    }

    pub fn selected(&self) -> Option<Item> {
        self.items().get(self.cursor).copied()
    }

    pub fn selected_device(&self) -> Option<&Device> {
        match self.selected()? {
            Item::Disk(i) => self.devices.get(i),
            _ => None,
        }
    }

    pub fn selected_phone(&self) -> Option<&Phone> {
        match self.selected()? {
            Item::Phone(i) => self.phones.get(i),
            _ => None,
        }
    }

    pub fn selected_share(&self) -> Option<&Share> {
        match self.selected()? {
            Item::Share(i) => self.shares.get(i),
            _ => None,
        }
    }

    pub fn selected_place(&self) -> Option<&Place> {
        match self.selected()? {
            Item::Place(i) => self.places.get(i),
            _ => None,
        }
    }

    pub fn selected_cloud(&self) -> Option<&Cloud> {
        match self.selected()? {
            Item::Cloud(i) => self.clouds.get(i),
            _ => None,
        }
    }

    /// A call about the row whose identity is `identity` has gone out: the
    /// row says `doing` (`mounting…`) until it comes back, and whatever the
    /// last call's failure said is over.
    pub fn start(&mut self, identity: impl Into<String>, doing: &'static str) {
        self.busy = Some((identity.into(), doing));
        self.failed = None;
    }

    /// The call came back and did what it was asked.
    pub fn finish(&mut self) {
        self.busy = None;
    }

    /// The call came back with `message`, which the row it was about says in
    /// place of its detail until the next call starts.
    pub fn fail(&mut self, message: impl Into<String>) {
        if let Some((identity, _)) = self.busy.take() {
            self.failed = Some((identity, message.into()));
        }
    }

    /// Whether a call is out about the row whose identity is `identity`.
    pub fn is_busy(&self, identity: &str) -> bool {
        self.busy.as_ref().is_some_and(|(busy, _)| busy == identity)
    }

    /// Take a new Places list — a pin came off it — keeping the cursor at the
    /// same height, so it lands on the row that moved up into the gap rather
    /// than jumping back to the top.
    pub fn set_places(&mut self, places: Vec<Place>) {
        self.places = places;
        self.cursor = self.cursor.min(self.items().len().saturating_sub(1));
        self.rebuilt();
    }

    /// `↑`/`↓`, **wrapping**: `↑` on the first row is the last row, and `↓`
    /// on the last row is the first.
    ///
    /// The palette's rule ([`crate::finder`]), not the file panes'. A pane
    /// clamps because a directory is a place with a top and a bottom, and
    /// running off the end of it would lose your place. `M` is a short menu
    /// you step around — a few places, a disk or two, a share, the connect
    /// row — and `↑` from the Places to reach the connect row is the move a
    /// hand expects of a menu. The page keys still stop at the ends
    /// ([`Card::jump`]).
    pub fn move_cursor(&mut self, delta: isize) {
        // Never empty (the connect row is always there); a zero would be a
        // division by it.
        let len = self.items().len().max(1) as isize;
        self.cursor = (self.cursor as isize + delta).rem_euclid(len) as usize;
        self.detached = false;
        self.follow();
    }

    /// `PageUp`/`PageDown`, `Ctrl+u`/`Ctrl+d`, `Home`/`End`: clamped at the
    /// ends, where the arrows wrap. A page is the rows on screen now
    /// ([`Card::visible_items`]), which is fewer while a section's heading or
    /// empty state is in view.
    pub fn jump(&mut self, jump: Jump) {
        let page = self.visible_items().len();
        self.cursor = jump.target(self.cursor, self.items().len(), page);
        self.detached = false;
        self.follow();
    }

    /// Put the cursor on `item`: a click.
    pub fn select(&mut self, item: Item) {
        if let Some(index) = self.items().iter().position(|i| *i == item) {
            self.cursor = index;
            self.detached = false;
            self.follow();
        }
    }

    /// Scroll so the cursor's row is in view.
    fn follow(&mut self) {
        let lines = self.lines();
        let Some(item) = self.selected() else { return };
        if let Some(at) = lines.iter().position(|line| *line == Line::Item(item)) {
            self.first = scroll(self.first, at, &lines, self.window);
        }
    }

    /// The view where it belongs after the lines or the window changed: on
    /// the cursor, or — while the wheel has taken it off the cursor — where
    /// the wheel left it, kept inside the lines, which may have got fewer.
    fn settle(&mut self) {
        if self.detached {
            let heights: Vec<f32> = self.lines().iter().map(|line| line.height()).collect();
            self.first = self.first.min(deepest(&heights, self.window));
        } else {
            self.follow();
        }
    }

    /// The lines were rebuilt under the card — a listing landed, a pin came
    /// off, the remotes arrived — so the view follows the cursor to wherever
    /// its row went, and that is not a scroll ([`crate::scrollbar::Linger`]).
    fn rebuilt(&mut self) {
        self.settle();
        self.bar = crate::scrollbar::Linger::default();
    }

    /// The window leaves the body `window` points ([`window`]): the lines
    /// scroll within that from now on, and the cursor's row is brought back
    /// into it now, since a window made shorter can have left it below the
    /// last line drawn.
    pub fn fit(&mut self, window: f32, now: std::time::Instant) {
        self.window = window;
        self.settle();
        self.bar.saw(self.first as f32, now);
    }

    /// The wheel over the card, in points: whole lines at a time, a notch
    /// counted in rows ([`crate::mouse::roll`]), the view leaving the cursor
    /// where it was. Returns whether the lines moved.
    pub fn wheel(&mut self, points: f32, now: std::time::Instant) -> bool {
        let heights: Vec<f32> = self.lines().iter().map(|line| line.height()).collect();
        let rows = crate::mouse::wheel_rows(points, ROW);
        let last = deepest(&heights, self.window);
        let first = crate::mouse::roll(self.first, last, &mut self.carry, rows);
        self.scroll_to_line(first, now)
    }

    /// Start the view at line `first`, kept inside the lines, off the cursor
    /// until a key or a click moves it. Returns whether the lines moved.
    fn scroll_to_line(&mut self, first: usize, now: std::time::Instant) -> bool {
        let heights: Vec<f32> = self.lines().iter().map(|line| line.height()).collect();
        let first = first.min(deepest(&heights, self.window));
        if first == self.first {
            return false;
        }
        self.first = first;
        self.detached = true;
        self.bar.saw(first as f32, now);
        true
    }

    /// When the lines last scrolled, for their bar's linger.
    pub fn scrolled_at(&self) -> Option<std::time::Instant> {
        self.bar.scrolled_at()
    }

    /// A hand let go of the bar: it lingers from now
    /// ([`crate::scrollbar::Linger::let_go`]).
    pub fn let_go(&mut self, now: std::time::Instant) {
        self.bar.let_go(now);
    }

    /// Start the view `points` down the lines, for a hand on the bar, which
    /// is measured in points since the lines are not one height ([`bar`]):
    /// at the line whose top is nearest, off the cursor until a key or a
    /// click moves it. Returns whether the lines moved.
    pub fn scroll_to(&mut self, points: f32, now: std::time::Instant) -> bool {
        let heights: Vec<f32> = self.lines().iter().map(|line| line.height()).collect();
        self.scroll_to_line(nearest(&heights, points), now)
    }

    /// The lines drawn from [`Card::first`], each with its top measured from
    /// the top of the body, stopping at the bottom of the window.
    ///
    /// Whole lines only. A row cut in half by the window's edge was drawn
    /// with its status under the `+N more` that says the list goes on — the
    /// ordinary look of the card once the Places section is over the disks —
    /// and half a row is not something a person can read or aim at anyway.
    pub fn visible(&self) -> Vec<(Line, f32)> {
        let mut top = 0.0;
        let mut out = Vec::new();
        for line in self.lines().into_iter().skip(self.first) {
            if top + line.height() > self.window + 0.01 {
                break;
            }
            out.push((line, top));
            top += line.height();
        }
        out
    }

    /// The rows among [`Card::visible`]: what `Control::PanelRow(i)` indexes,
    /// for the pointer.
    pub fn visible_items(&self) -> Vec<Item> {
        self.visible()
            .into_iter()
            .filter_map(|(line, _)| match line {
                Line::Item(item) => Some(item),
                _ => None,
            })
            .collect()
    }

    /// How tall the body is: all of it, or the window when all of it is more.
    /// A constant while scrolling, so the card does not change size under the
    /// cursor.
    fn body_height(&self) -> f32 {
        self.lines()
            .iter()
            .map(|line| line.height())
            .sum::<f32>()
            .min(self.window)
    }

    /// Whether the lines are more than the window shows, so the card scrolls
    /// and has its `+N more` line ([`MORE_LINE`]) — however far it is
    /// scrolled, so the card does not change size as the view reaches the
    /// end and the count runs out.
    pub fn overflows(&self) -> bool {
        self.lines().iter().map(|line| line.height()).sum::<f32>() > self.window + 0.01
    }

    /// How many rows are below the last one drawn: what the `+N more` line
    /// says, and nothing once the view is at the end.
    pub fn more(&self) -> usize {
        let items = self.items();
        let shown = self
            .visible_items()
            .last()
            .and_then(|last| items.iter().position(|item| item == last))
            .map_or(0, |at| at + 1);
        items.len().saturating_sub(shown)
    }

    /// What a row is across a refresh: the block object, the phone's root,
    /// the share's URL, the remote's name, the place's target, or the connect
    /// row.
    fn identity(&self, item: Item) -> Option<String> {
        match item {
            Item::Disk(i) => self.devices.get(i).map(|d| format!("disk {}", d.object)),
            Item::Phone(i) => self.phones.get(i).map(|p| format!("phone {}", p.root)),
            Item::Share(i) => self.shares.get(i).map(|s| format!("share {}", s.url)),
            Item::Cloud(i) => self.clouds.get(i).map(|c| format!("cloud {}", c.name)),
            Item::Connect => Some("connect".to_string()),
            Item::Place(i) => self
                .places
                .get(i)
                .map(|p| format!("place {}", p.target.display())),
        }
    }

    /// Take a new listing without losing the user's place.
    ///
    /// Keyed on each row's identity, so a refresh that arrives after a mount
    /// leaves the cursor on the disk that was just mounted rather than on
    /// whatever now sorts into that row (`delightful-ui` §8). The *first*
    /// listing has one exception: a cursor still where the card opened it —
    /// on the first row ([`Card::with_places`]), which is the Network
    /// section's first until the listing lands — stays at that index, so the
    /// first device arrives *under* it. The index does not move, so nothing
    /// jumps; it only gains the row it was waiting for. A cursor somebody has
    /// moved, round into the places or anywhere else, is kept by identity
    /// like any other.
    pub fn update(&mut self, devices: Vec<Device>, phones: Vec<Phone>, shares: Vec<Share>) {
        let on = self.selected().and_then(|item| self.identity(item));
        let waiting = self.loading && self.cursor == 0;
        self.devices = devices;
        self.phones = phones;
        self.shares = shares;
        self.loading = false;
        let items = self.items();
        let kept = on.filter(|_| !waiting).and_then(|identity| {
            items
                .iter()
                .position(|item| self.identity(*item).as_deref() == Some(identity.as_str()))
        });
        self.cursor = match kept {
            Some(at) => at,
            None if waiting => 0,
            None => self.cursor,
        }
        .min(items.len().saturating_sub(1));
        self.rebuilt();
    }

    /// What the Devices section says when it has no rows — no disk and no
    /// phone — or `None` when it has some.
    ///
    /// Two different nothings, and they must not look alike
    /// (`delightful-ui` §11): still asking, and nothing to manage.
    pub fn devices_empty(&self) -> Option<&'static str> {
        if !self.devices.is_empty() || !self.phones.is_empty() {
            return None;
        }
        Some(if self.loading {
            "asking udisks2…"
        } else {
            "no removable filesystems"
        })
    }

    /// The same for the Network section. A machine without gvfs lands here
    /// too: nothing is mounted through it, which is the truth.
    pub fn shares_empty(&self) -> Option<&'static str> {
        if !self.shares.is_empty() {
            return None;
        }
        Some(if self.loading {
            "asking gvfs…"
        } else {
            "nothing mounted"
        })
    }
}

/// The first line to draw so that line `at` is wholly inside a body `window`
/// points tall.
///
/// [`crate::viewport::first_visible`]'s job for a list whose lines are not all
/// one height: the view moves only when the cursor would leave it, a section's
/// heading comes back into view with the first row under it — a row at the top
/// of the card with no name over it is a row whose section you have to
/// remember — and the list never scrolls past its own end.
///
/// Everything between the row and the row before it comes back with it, not
/// only the heading: over the first share of a machine with nothing plugged
/// in that is the Devices section's heading and its empty state as well, and
/// the card scrolled back to its first row that still hid its first section
/// would be a card that looked as though it had no top.
fn scroll(first: usize, at: usize, lines: &[Line], window: f32) -> usize {
    let heights: Vec<f32> = lines.iter().map(|line| line.height()).collect();
    if heights.iter().sum::<f32>() <= window {
        return 0;
    }
    let mut first = first.min(at);
    if first == at {
        while first > 0 && !matches!(lines[first - 1], Line::Item(_)) {
            first -= 1;
        }
    }
    while first < at && heights[first..=at].iter().sum::<f32>() > window {
        first += 1;
    }
    first.min(deepest(&heights, window))
}

/// The line of these `heights` whose top is nearest `points` down them.
fn nearest(heights: &[f32], points: f32) -> usize {
    let mut top = 0.0;
    for (index, height) in heights.iter().enumerate() {
        if points < top + height / 2.0 {
            return index;
        }
        top += height;
    }
    heights.len()
}

/// The furthest the view goes down lines of these `heights` in a body
/// `window` points tall: the first of the last lines exactly filling it, or
/// the top when they all fit.
fn deepest(heights: &[f32], window: f32) -> usize {
    let mut deepest = heights.len();
    let mut tail = 0.0;
    while deepest > 0 && tail + heights[deepest - 1] <= window {
        deepest -= 1;
        tail += heights[deepest];
    }
    deepest
}

// ── The card ────────────────────────────────────────────────────────────────

/// One row: one line, the height of a row in the list panes
/// ([`crate::ui::ROW_HEIGHT`]) — the glyph, the name, and the detail after it.
///
/// It was two lines, the detail under the name, which made ten rows of the
/// card as tall as twenty of a pane and put what a row *is* a line away from
/// what it is called.
const ROW: f32 = crate::ui::ROW_HEIGHT;
/// A section's heading line: the help sheet's group-title line
/// ([`crate::chrome::HELP_ROW`]) with a few points more above it, so the
/// second section reads as starting rather than as continuing the first.
const SECTION_ROW: f32 = 26.0;
/// The most the lines may take before the card scrolls: [`ROWS`] rows and the
/// three headings. At most: a window too short for it gets less ([`window`]).
const WINDOW: f32 = ROWS as f32 * ROW + SECTIONS.len() as f32 * SECTION_ROW;
/// The row's own left/right inset, inside the card's [`PAD`].
const ROW_PAD: f32 = 10.0;
/// The width a row's glyph is centred in: a little over the glyph itself, so
/// glyphs that are not one width start their names in one column.
const ICON_COLUMN: f32 = 18.0;
/// Between the name and the detail after it, and between the detail and the
/// key chip: the gap between the panes, since it separates two columns of
/// facts rather than two words of one.
const DETAIL_GAP: f32 = crate::ui::GAP;
/// The most of a row's text the name may take, leaving the rest to the
/// detail. A name is short — a label, a model, a folder — and the detail is a
/// path that can be any length, so the name gets what it needs up to this and
/// the detail everything else.
const NAME_SHARE: f32 = 0.6;
/// A detail narrower than this is left out rather than squeezed to an `…`
/// that says nothing.
const MIN_DETAIL: f32 = 24.0;
/// The key chip's padding either side of its key.
const CHIP_PAD: f32 = 5.0;
/// The `+N more` line under the rows of a card that scrolls: a caption's
/// line, the hint strip's height. Its own line, and not the last row's
/// corner, which it used to share — with a place's key chip there now, the
/// count was drawn over the key.
const MORE_LINE: f32 = crate::chrome::HINT_ROW;
/// The card's inner padding — `chrome::CARD_PAD`, not a number of its own.
///
/// The plate is [`crate::chrome::card`], whose radius is
/// `CARD_ROW_RADIUS + CARD_PAD`; a row inset by anything else stops being
/// concentric with it (`delightful-ui` §15). This was 14 against a 10-derived
/// radius, so the gap *widened* by 4 px as it turned each corner — the same
/// mistake the yank tray made in the other direction.
const PAD: f32 = crate::chrome::CARD_PAD;
const TITLE: f32 = 20.0;
/// The title row and the air under it. It used to be two title rows' worth,
/// which was the space the keys were repeated in; with them gone to the hint
/// strip the heading is one line, and the rows start under it rather than
/// under a band of nothing.
const HEADING: f32 = TITLE + 8.0;
const MAX_WIDTH: f32 = 560.0;
const FONT: f32 = 13.0;
/// The detail's face: a step under the name's, as well as quieter, so it
/// reads as what the name is rather than as a second name.
const DETAIL_FONT: f32 = FONT - 1.0;

/// Where the card's pieces are.
#[derive(Debug, Clone)]
pub struct Geometry {
    pub card: egui::Rect,
    pub body: egui::Rect,
    /// Every line drawn, headings and empty states included, in order.
    pub lines: Vec<(Line, egui::Rect)>,
    /// The rows among them — the lines the pointer can press — in order. What
    /// `Control::PanelRow(i)` indexes, and what [`Card::visible_items`] names.
    pub rows: Vec<egui::Rect>,
    /// The `×` at the title row's far end.
    pub close: Option<egui::Rect>,
    /// The cursor is on a pinned place, so the hint strip offers `d` to unpin
    /// it. Only then: a `[goto]` row is the config's, and a key on the strip
    /// that could only ever say "not this one" is a key that lied.
    pub unpin: bool,
    /// The cursor is on a cloud remote, so the hint strip leaves out `m`, `u`
    /// and `e`: a remote is gone to, never mounted, and those three keys do
    /// nothing on its row. The same rule as [`Geometry::unpin`], from the
    /// other side — a key the row cannot use is not offered on it.
    pub cloud: bool,
    /// The band the body's bar is pointed at by, while there are lines the
    /// body does not show ([`crate::scrollbar::band`]).
    pub band: Option<egui::Rect>,
    /// The `+N more` line, under the body and over the hint strip, while the
    /// card scrolls ([`Card::overflows`]); `None` when every line is shown.
    pub more: Option<egui::Rect>,
}

impl Geometry {
    /// The row under `pos`. Only the part of a row inside the body counts: the
    /// last row can be cut by the window's edge, and the cut-off part is not
    /// on screen to be pressed.
    pub fn row_at(&self, pos: egui::Pos2) -> Option<usize> {
        if !self.body.contains(pos) {
            return None;
        }
        self.rows.iter().position(|r| r.contains(pos))
    }
}

/// Everything on the card that is not a line of the body: the padding, the
/// heading and the hint strip.
const FIXED: f32 = PAD * 2.0 + HEADING + crate::chrome::HINT_ROW;

/// How tall the body may be in `area`: [`WINDOW`], or less in a window too
/// short for it.
///
/// Every card with a list fits it by one rule ([`crate::dialog::fit_rows`]),
/// and now that a row is one height this card's is that rule too: the three
/// headings are a fixed part of the card, like its title and its hint strip,
/// and the rows are as many as the rest of the window has room for, up to
/// [`ROWS`] — and never none, so the cursor always has a whole line to be on.
///
/// The `+N more` line is a fixed part as well: room is kept for it whether or
/// not it turns out to be needed, so a card that does scroll has it inside
/// the height the window allowed rather than past its foot.
pub fn window(area: egui::Rect) -> f32 {
    let headings = SECTIONS.len() as f32 * SECTION_ROW;
    let fixed = FIXED + headings + MORE_LINE;
    let (rows, _) = crate::dialog::fit_rows(area, MAX_WIDTH, fixed, ROW, ROWS);
    rows as f32 * ROW + headings
}

/// Lay the card out, centred and biased above true centre
/// (`delightful-ui` §16), for the body [`Card::fit`] was last given.
pub fn geometry(area: egui::Rect, card: &Card) -> Geometry {
    let more_line = if card.overflows() { MORE_LINE } else { 0.0 };
    let rect = crate::dialog::place_card(area, MAX_WIDTH, FIXED + card.body_height() + more_line);
    let body_top = rect.top() + PAD + HEADING;
    let strip_top = rect.bottom() - PAD - crate::chrome::HINT_ROW;
    // Never upside down: a window shorter than the heading and the hint
    // strip leaves the body no height at all, and nothing is drawn or
    // pressed in it, rather than a body whose bottom is above its top.
    let body = egui::Rect::from_min_max(
        egui::pos2(rect.left() + PAD, body_top),
        egui::pos2(rect.right() - PAD, (strip_top - more_line).max(body_top)),
    );
    let more = card.overflows().then(|| {
        egui::Rect::from_min_max(
            egui::pos2(body.left(), body.bottom()),
            egui::pos2(
                body.right(),
                (body.bottom() + MORE_LINE)
                    .min(strip_top)
                    .max(body.bottom()),
            ),
        )
    });
    let mut lines = Vec::new();
    let mut rows = Vec::new();
    for (line, offset) in card.visible() {
        let rect = egui::Rect::from_min_size(
            egui::pos2(body.left(), body_top + offset),
            egui::vec2(body.width(), line.height()),
        );
        if matches!(line, Line::Item(_)) {
            rows.push(rect);
        }
        lines.push((line, rect));
    }
    let (window, total) = extent(card);
    Geometry {
        card: rect,
        body,
        lines,
        rows,
        close: Some(crate::chrome::close_button_rect(rect)),
        unpin: card.selected_place().is_some_and(|place| place.pinned),
        cloud: card.selected_cloud().is_some(),
        band: crate::scrollbar::band(rect, body, window, total),
        more,
    }
}

/// The card's bar, beside its body, while the lines are more than it shows.
/// Counted in points, since the headings are not a row's height ([`extent`]).
pub fn bar(geometry: &Geometry, card: &Card) -> Option<crate::scrollbar::Geometry> {
    let heights: Vec<f32> = card.lines().iter().map(|line| line.height()).collect();
    let above: f32 = heights[..card.first.min(heights.len())].iter().sum();
    let (window, total) = extent(card);
    crate::scrollbar::card(geometry.card, geometry.body, above, window, total)
}

/// The bar's view and its whole, in points: the window the lines scroll in,
/// and the lines as far down as a view can start plus that window.
///
/// The window rather than the lines laid out in it, which are fewer while a
/// heading is in view and more when rows are: a thumb measured from them
/// would lengthen and shorten as a drag went down the card, and near
/// [`crate::scrollbar::MIN_THUMB`] flip a line back and forth from one frame
/// to the next. And the reach rather than every line, because the view stops
/// at the last lines that fill the window ([`deepest`]), which can start above
/// the window's height from the end: the bottom of the thumb's travel is that
/// line, so a thumb pulled all the way down lands on it rather than on the
/// line before.
fn extent(card: &Card) -> (f32, f32) {
    let heights: Vec<f32> = card.lines().iter().map(|line| line.height()).collect();
    let reach: f32 = heights[..deepest(&heights, card.window)].iter().sum();
    (card.window, reach + card.window)
}

/// What one row says.
#[derive(Debug, Default)]
struct Face {
    /// The glyph in front of the name: what kind of thing the row is, the way
    /// a file row's icon says what kind of file.
    icon: Option<crate::icons::Icon>,
    name: String,
    /// What follows the name: the row's detail, or while a call is out on the
    /// row what the call is doing, and after one failed, the failure.
    detail: String,
    /// The detail's ink: `quiet` for the detail, the palette's amber while
    /// busy — so a polkit prompt behind the window is not read as the card
    /// having hung — and its red for a failure.
    tone: egui::Color32,
    /// A place's `g` key, drawn as a chip at the row's end.
    key: Option<String>,
}

fn face(card: &Card, item: Item, palette: &crate::theme::Palette, nerd: bool) -> Face {
    use crate::icons;
    // The icon, the name, the detail, the key, and the identity a call about
    // the row is kept under — none for a row nothing is ever done to.
    let row = match item {
        Item::Disk(i) => card.devices.get(i).map(|device| {
            (
                icons::drive(palette, nerd, device.removable),
                device.label.clone(),
                device.detail(),
                None,
                Some(device.object.clone()),
            )
        }),
        Item::Phone(i) => card.phones.get(i).map(|phone| {
            (
                match phone.protocol {
                    Protocol::Mtp => icons::phone(palette, nerd),
                    Protocol::Gphoto2 => icons::camera(palette, nerd),
                },
                phone.name.clone(),
                phone.detail(),
                None,
                Some(phone.root.clone()),
            )
        }),
        // Every share listed is mounted — gvfs lists nothing else — so what it
        // says after its name is where it is: its URL, and not gvfs-fuse's
        // spec-named directory, which is nobody's idea of where a share is.
        Item::Share(i) => card.shares.get(i).map(|share| {
            (
                icons::network(palette, nerd),
                share.label.clone(),
                share.url.clone(),
                None,
                Some(share.url.clone()),
            )
        }),
        // The glyph a remote place wears: it is the same kind of thing —
        // somewhere that is not this disk — and the detail says which service
        // in words.
        Item::Cloud(i) => card.clouds.get(i).map(|cloud| {
            (
                icons::network(palette, nerd),
                cloud.name.clone(),
                cloud.service(),
                None,
                None,
            )
        }),
        Item::Connect => Some((
            icons::connect(palette, nerd),
            "Connect to server…".to_string(),
            "smb · sftp · ftp · dav · nfs".to_string(),
            None,
            None,
        )),
        Item::Place(i) => card.places.get(i).map(|place| {
            (
                if place.remote {
                    icons::network(palette, nerd)
                } else {
                    icons::folder(palette, nerd)
                },
                place.name.clone(),
                place.detail.clone(),
                place.key.clone(),
                None,
            )
        }),
    };
    let Some((icon, name, detail, key, identity)) = row else {
        return Face::default();
    };
    let busy = card
        .busy
        .as_ref()
        .filter(|(busy, _)| identity.as_deref() == Some(busy.as_str()));
    let failed = card
        .failed
        .as_ref()
        .filter(|(failed, _)| identity.as_deref() == Some(failed.as_str()));
    // Words, so an accented one is the accent as ink ([`crate::theme::ink`]).
    let (detail, tone) = match (busy, failed) {
        (Some((_, doing)), _) => (
            (*doing).to_string(),
            crate::theme::ink(palette, palette.peach),
        ),
        (None, Some((_, message))) => (message.clone(), crate::theme::ink(palette, palette.red)),
        (None, None) => (detail, palette.quiet),
    };
    Face {
        icon: Some(icon),
        name,
        detail,
        tone,
        key,
    }
}

/// What a row is filled with: the list panes' rule, on the card's plate.
///
/// The cursor is [`crate::theme::cursor_fill`], and a hover lifts whatever
/// the row already is ([`crate::theme::lift`]) — the full step on a plain row,
/// a little on the cursor's, as a pane's rows do — so on a light palette both
/// are the panes' washes of blue rather than a grey slab. `None` for a row the
/// colour of the plate, which is not drawn.
fn row_fill(palette: &crate::theme::Palette, on_cursor: bool, hover: f32) -> Option<egui::Color32> {
    let plate = palette.crust;
    let (base, lift) = if on_cursor {
        (
            crate::theme::cursor_fill(palette),
            crate::ui::CURSOR_HOVER_LIFT,
        )
    } else {
        (plate, crate::ui::HOVER_LIFT)
    };
    let fill = crate::theme::lift(palette, base, hover * lift);
    (fill != plate).then_some(fill)
}

/// Where one row's pieces go.
#[derive(Debug, Clone, PartialEq)]
struct RowLayout {
    /// The glyph's centre, when there is a glyph to draw.
    glyph: Option<egui::Pos2>,
    /// The name's left end, on the row's centre line, and the most it may
    /// take.
    name: (egui::Pos2, f32),
    /// The detail's left end and the room it has.
    detail: (egui::Pos2, f32),
    /// The key chip, when the row has a key.
    chip: Option<egui::Rect>,
}

/// Lay out a row `rect` whose name is `name_width` wide, with a glyph or not,
/// and a key chip `chip_width` wide or none.
///
/// Left to right: the glyph in its column, the name, [`DETAIL_GAP`], the
/// detail, and at the far end the chip. The name has what it needs up to
/// [`NAME_SHARE`] of the text's room and the detail has the rest, so a long
/// path never pushes the name off the row and a short name leaves the path
/// the room.
///
/// The chip sits [`crate::chrome::CHIP_INSET`] in from the row's top, bottom
/// and right, with [`crate::chrome::CHIP_RADIUS`] corners: the row's own
/// radius less that inset, so the chip's corner is concentric with the row
/// plate's round it when the row is under the cursor (`delightful-ui` §15).
fn row_layout(
    rect: egui::Rect,
    glyph: bool,
    name_width: f32,
    chip_width: Option<f32>,
) -> RowLayout {
    let y = rect.center().y;
    let mut left = rect.left() + ROW_PAD;
    let glyph = glyph.then(|| {
        let at = egui::pos2(left + ICON_COLUMN / 2.0, y);
        left += ICON_COLUMN + crate::chrome::ICON_GAP;
        at
    });
    let inset = crate::chrome::CHIP_INSET;
    let chip = chip_width.map(|width| {
        egui::Rect::from_min_max(
            egui::pos2(rect.right() - inset - width, rect.top() + inset),
            egui::pos2(rect.right() - inset, rect.bottom() - inset),
        )
    });
    let right = chip.map_or(rect.right() - ROW_PAD, |chip| chip.left() - DETAIL_GAP);
    let room = (right - left).max(0.0);
    let name = name_width.min(room * NAME_SHARE).max(0.0);
    let detail_left = left + name + DETAIL_GAP;
    RowLayout {
        glyph,
        name: (egui::pos2(left, y), name),
        detail: (egui::pos2(detail_left, y), (right - detail_left).max(0.0)),
        chip,
    }
}

/// The detail as it fits `room`, `measure` saying how wide a text is: whole
/// when it fits, its middle taken out when not — a path's start says whose it
/// is and its end which, and both matter — and nothing at all when there is
/// less room than [`MIN_DETAIL`].
fn fitted_detail(detail: &str, room: f32, measure: impl Fn(&str) -> f32) -> String {
    if room < MIN_DETAIL {
        return String::new();
    }
    crate::chrome::elide_middle_with(detail, |candidate| measure(candidate) <= room)
}

/// Draw it, over the scrim the app lays for it.
pub fn paint(
    paint: &crate::ui::Painting<'_>,
    card: &Card,
    geometry: &Geometry,
    hovers: &crate::hover::Hovers<crate::ui::Control>,
    ripples: &crate::ripple::Ripples<crate::ui::Control>,
) {
    let palette = paint.palette;
    let painter = paint.painter;
    crate::chrome::card(paint, geometry.card, 1.0);

    let left = geometry.card.left() + PAD;
    // "Places" over the three sections, the way the help sheet's "Keys" sits
    // over its groups: every row on the card is somewhere to go that is not
    // this folder.
    painter.text(
        egui::pos2(left, geometry.card.top() + PAD + TITLE / 2.0),
        egui::Align2::LEFT_CENTER,
        "Places",
        egui::FontId::proportional(FONT + 2.0),
        palette.text,
    );
    if let Some(close) = geometry.close {
        crate::chrome::close_button(paint, close, hovers, ripples);
    }
    // The keys are *not* repeated here: they are on the hint strip along the
    // bottom, where every other overlay puts them
    // ([`crate::chrome::hint_rect`]). Saying them twice on one card taught the
    // eye that the title row was a second place to look, and the two copies
    // did not even agree — the strip knew about `u`, this line did not.

    let clipped = painter.with_clip_rect(geometry.body);
    let selected = card.selected();
    let mut row = 0;
    for (line, rect) in &geometry.lines {
        match *line {
            Line::Section(name) => {
                // The help sheet's group title — its colour, its size, and its
                // seat low in the line, so the name belongs to the rows under
                // it rather than to the ones above.
                clipped.text(
                    egui::pos2(rect.left(), rect.center().y + 2.0),
                    egui::Align2::LEFT_CENTER,
                    name,
                    egui::FontId::proportional(crate::chrome::FONT),
                    crate::theme::ink(palette, palette.blue),
                );
            }
            Line::Empty(message) => {
                // In the names' column, where the rows' names would be, when
                // there are glyphs in front of them.
                let indent = if paint.nerd {
                    ICON_COLUMN + crate::chrome::ICON_GAP
                } else {
                    0.0
                };
                clipped.text(
                    egui::pos2(rect.left() + ROW_PAD + indent, rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    message,
                    egui::FontId::proportional(FONT),
                    palette.faint,
                );
            }
            Line::Item(item) => {
                // The offset among the drawn rows, not the item's index:
                // `Geometry::row_at` and `rect_of` both index
                // `geometry.rows`, so the offset is what the hit test produces
                // and the hover key has to match it.
                let key = crate::ui::Control::PanelRow(row);
                row += 1;
                let hover = hovers.hover(key);
                // Mounting a disk is one of the more consequential clicks in
                // the program, and it was the one row in the crate that gave no
                // feedback at all under the finger (`delightful-ui` §4).
                let rect = crate::hover::pressed_rect(*rect, hovers.press(key));
                if let Some(fill) = row_fill(palette, selected == Some(item), hover) {
                    clipped.rect_filled(rect, crate::ui::ROW_RADIUS, fill);
                }
                for splash in ripples.splashes(key, paint.now) {
                    clipped.circle_filled(
                        splash.center,
                        splash.radius,
                        crate::theme::splash(palette, splash.alpha),
                    );
                }
                paint_face(
                    &clipped,
                    rect,
                    &face(card, item, palette, paint.nerd),
                    palette,
                );
            }
        }
    }
    // On its own line under the rows, at their text's right edge — never in
    // a row, where it would sit over the key chip a place row ends in.
    let more = card.more();
    if let Some(line) = geometry.more.filter(|_| more > 0) {
        painter.with_clip_rect(line).text(
            egui::pos2(line.right() - ROW_PAD, line.center().y),
            egui::Align2::RIGHT_CENTER,
            format!("+{more} more"),
            egui::FontId::proportional(FONT - 1.0),
            palette.faint,
        );
    }
    if let Some(bar) = bar(geometry, card) {
        crate::scrollbar::paint_card(
            paint,
            &bar,
            crate::scrollbar::Surface::Mounts,
            hovers,
            card.scrolled_at(),
            1.0,
        );
    }
}

/// The face a key chip's key is set in: the which-key card's, a step down to
/// sit inside a row.
fn chip_font() -> egui::FontId {
    crate::chrome::key_font(FONT - 1.5)
}

/// One row's glyph, name, detail and key, where [`row_layout`] puts them.
fn paint_face(
    painter: &egui::Painter,
    rect: egui::Rect,
    face: &Face,
    palette: &crate::theme::Palette,
) {
    let font = egui::FontId::proportional(FONT);
    let detail_font = egui::FontId::proportional(DETAIL_FONT);
    // Without a patched font there is no glyph to draw — the file rows go
    // without one too — and then no column either, so the name is not
    // indented by a blank.
    let icon = face.icon.filter(|icon| icon.glyph != ' ');
    let name_width = crate::chrome::text_width(painter, &face.name, font.clone());
    let chip_width = face
        .key
        .as_ref()
        .map(|keys| crate::chrome::text_width(painter, keys, chip_font()) + CHIP_PAD * 2.0);
    let layout = row_layout(rect, icon.is_some(), name_width, chip_width);
    if let (Some(icon), Some(at)) = (icon, layout.glyph) {
        painter.text(
            at,
            egui::Align2::CENTER_CENTER,
            icon.glyph,
            egui::FontId::proportional(FONT + 1.0),
            icon.color,
        );
    }
    let (name_at, name_room) = layout.name;
    crate::chrome::truncated_in(painter, name_at, &face.name, palette.text, name_room, font);
    let (detail_at, detail_room) = layout.detail;
    let detail = fitted_detail(&face.detail, detail_room, |candidate| {
        crate::chrome::text_width(painter, candidate, detail_font.clone())
    });
    if !detail.is_empty() {
        painter.text(
            detail_at,
            egui::Align2::LEFT_CENTER,
            detail,
            detail_font,
            face.tone,
        );
    }
    // The key the way the which-key card draws one — monospace, in the
    // yellow's ink — on a chip of its own tint, so it reads as a key to press
    // and not as more of the path beside it.
    if let (Some(keys), Some(chip)) = (&face.key, layout.chip) {
        painter.rect_filled(
            chip,
            crate::chrome::CHIP_RADIUS,
            crate::theme::mix(palette.crust, palette.yellow, crate::chrome::CHIP_TINT),
        );
        painter.text(
            chip.center(),
            egui::Align2::CENTER_CENTER,
            keys,
            chip_font(),
            crate::theme::ink(palette, palette.yellow),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Same rule as the yank tray, which this card got wrong in the other
    /// direction — a 14 pt inset against a 10-derived radius, so the gap
    /// *widened* around each corner (`delightful-ui` §15).
    #[test]
    fn the_card_radii_are_concentric() {
        assert_eq!(
            crate::ui::ROW_RADIUS as f32 + PAD,
            crate::chrome::CARD_RADIUS as f32
        );
    }
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

    // ── gvfs ────────────────────────────────────────────────────────────────

    /// `gio mount -l` on the development machine (gio 2.88, gvfs 1.60), taken
    /// with four FTP mounts up against a local test server: two anonymous, and
    /// two whose user names need escaping. The drives and volumes above them
    /// are udisks2's, which the Network section must not repeat.
    const LISTING: &str = "\
Drive(0): Samsung SSD 960 EVO 500GB
  Type: GProxyDrive (GProxyVolumeMonitorUDisks2)
  Volume(0): 499 GB Volume
    Type: GProxyVolume (GProxyVolumeMonitorUDisks2)
Drive(1): Samsung SSD 990 EVO Plus 4TB
  Type: GProxyDrive (GProxyVolumeMonitorUDisks2)
Drive(2): SanDisk SDSSDH32000G
  Type: GProxyDrive (GProxyVolumeMonitorUDisks2)
  Volume(0): Media
    Type: GProxyVolume (GProxyVolumeMonitorUDisks2)
Drive(3): INTEL SSDSC2BW240A4
  Type: GProxyDrive (GProxyVolumeMonitorUDisks2)
  Volume(0): Dev
    Type: GProxyVolume (GProxyVolumeMonitorUDisks2)
Drive(4): hp      DVD A  DH16AAL
  Type: GProxyDrive (GProxyVolumeMonitorUDisks2)
Drive(5): Multiple Card  Reader
  Type: GProxyDrive (GProxyVolumeMonitorUDisks2)
Mount(0): b o on localhost:2121 -> ftp://b%20o@localhost:2121/
  Type: GDaemonMount
Mount(1): me@example.com on 127.0.0.1:2121 -> ftp://me%40example.com@127.0.0.1:2121/
  Type: GDaemonMount
Mount(2): 127.0.0.1:2121 -> ftp://127.0.0.1:2121/
  Type: GDaemonMount
Mount(3): 127.0.0.1:2121 -> ftp://anonymous@127.0.0.1:2121/
  Type: GDaemonMount
";

    /// …and what `/run/user/1000/gvfs` held at the same moment.
    const ENTRIES: &[&str] = &[
        "ftp:host=127.0.0.1,port=2121",
        "ftp:host=127.0.0.1,port=2121,user=anonymous",
        "ftp:host=127.0.0.1,port=2121,user=me%40example.com",
        "ftp:host=localhost,port=2121,user=b%20o",
    ];

    fn entries(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    /// The captured listing: the four mounts at the margin and nothing from
    /// the drives above them.
    #[test]
    fn the_listing_yields_the_mounts_at_the_margin() {
        let mounts = gio_mounts(LISTING);
        assert_eq!(
            mounts,
            vec![
                (
                    "b o on localhost:2121".to_string(),
                    "ftp://b%20o@localhost:2121/".to_string()
                ),
                (
                    "me@example.com on 127.0.0.1:2121".to_string(),
                    "ftp://me%40example.com@127.0.0.1:2121/".to_string()
                ),
                (
                    "127.0.0.1:2121".to_string(),
                    "ftp://127.0.0.1:2121/".to_string()
                ),
                (
                    "127.0.0.1:2121".to_string(),
                    "ftp://anonymous@127.0.0.1:2121/".to_string()
                ),
            ]
        );
    }

    /// Each captured mount finds the directory gvfs-fuse really made for it,
    /// escapes and all, and is labelled `user@host`.
    #[test]
    fn each_share_finds_its_gvfs_directory() {
        let root = Path::new("/run/user/1000/gvfs");
        let shares = shares_from(LISTING, root, &entries(ENTRIES));
        let rows: Vec<(&str, &str, PathBuf)> = shares
            .iter()
            .map(|s| (s.label.as_str(), s.scheme.as_str(), s.path.clone()))
            .collect();
        assert_eq!(
            rows,
            vec![
                (
                    "127.0.0.1",
                    "ftp",
                    root.join("ftp:host=127.0.0.1,port=2121")
                ),
                (
                    "anonymous@127.0.0.1",
                    "ftp",
                    root.join("ftp:host=127.0.0.1,port=2121,user=anonymous")
                ),
                (
                    "b o@localhost",
                    "ftp",
                    root.join("ftp:host=localhost,port=2121,user=b%20o")
                ),
                (
                    "me@example.com@127.0.0.1",
                    "ftp",
                    root.join("ftp:host=127.0.0.1,port=2121,user=me%40example.com")
                ),
            ],
            "sorted by label, each on its own directory"
        );
        // The rebuilt names are the real ones, so a machine whose gvfs-fuse
        // has not caught up yet still gets the right path.
        let rebuilt = shares_from(LISTING, root, &[]);
        assert_eq!(
            rebuilt.iter().map(|s| &s.path).collect::<Vec<_>>(),
            shares.iter().map(|s| &s.path).collect::<Vec<_>>()
        );
    }

    /// The other backends, in gio's own format: an SMB share (whose server
    /// gvfs-fuse case-folds), an SFTP login, a Nextcloud WebDAV share with a
    /// prefix — beside a disk mounted through udisks2, nested under its
    /// volume, and an fstab mount at the margin, neither of which is a share.
    #[test]
    fn smb_sftp_and_dav_shares_are_named_like_gvfs_names_them() {
        let listing = "\
Drive(0): SanDisk Cruzer
  Type: GProxyDrive (GProxyVolumeMonitorUDisks2)
  Volume(0): PHOTOS
    Type: GProxyVolume (GProxyVolumeMonitorUDisks2)
    Mount(0): PHOTOS -> file:///run/media/me/PHOTOS
      Type: GProxyMount (GProxyVolumeMonitorUDisks2)
Mount(0): media on nas -> smb://NAS/Media/
  Type: GDaemonMount
Mount(1): me on example.org -> sftp://me@example.org/
  Type: GDaemonMount
Mount(2): WebDAV on cloud.example.org -> davs://me@cloud.example.org/remote.php/webdav/
  Type: GDaemonMount
Mount(3): backup -> file:///mnt/backup
  Type: GUnixMount
";
        let root = Path::new("/run/user/1000/gvfs");
        let present = entries(&[
            "smb-share:server=nas,share=media",
            "sftp:host=example.org,user=me",
            "dav:host=cloud.example.org,ssl=true,user=me,prefix=%2Fremote.php%2Fwebdav",
        ]);
        let shares = shares_from(listing, root, &present);
        let rows: Vec<(&str, &str, PathBuf)> = shares
            .iter()
            .map(|s| (s.label.as_str(), s.scheme.as_str(), s.path.clone()))
            .collect();
        assert_eq!(
            rows,
            vec![
                (
                    "me@cloud.example.org",
                    "davs",
                    root.join(
                        "dav:host=cloud.example.org,ssl=true,user=me,prefix=%2Fremote.php%2Fwebdav"
                    )
                ),
                (
                    "me@example.org",
                    "sftp",
                    root.join("sftp:host=example.org,user=me")
                ),
                (
                    "NAS/Media",
                    "smb",
                    root.join("smb-share:server=nas,share=media")
                ),
            ]
        );
        // With nothing to match against, the names are rebuilt the same way.
        let rebuilt: Vec<PathBuf> = shares_from(listing, root, &[])
            .into_iter()
            .map(|s| s.path)
            .collect();
        assert_eq!(rebuilt, rows.into_iter().map(|r| r.2).collect::<Vec<_>>());
    }

    /// A machine without gvfs — no gio, or a gio with nothing mounted — is an
    /// empty section, not an error.
    #[test]
    fn no_gvfs_is_no_shares() {
        assert!(shares_from("", Path::new("/nowhere"), &[]).is_empty());
        let disks_only = "Drive(0): Disk\n  Type: GProxyDrive (GProxyVolumeMonitorUDisks2)\n";
        assert!(shares_from(disks_only, Path::new("/nowhere"), &[]).is_empty());
    }

    /// The address grammar: users with an `@` of their own, ports, IPv6
    /// literals, and no path at all.
    #[test]
    fn addresses_come_apart() {
        let a = Address::parse("SFTP://me@host:2222/srv/www").expect("parses");
        assert_eq!(a.scheme, "sftp");
        assert_eq!(a.user.as_deref(), Some("me"));
        assert_eq!(a.host, "host");
        assert_eq!(a.port, Some(2222));
        assert_eq!(a.path, "/srv/www");

        let a = Address::parse("ftp://me%40example.com@127.0.0.1:2121/").expect("parses");
        assert_eq!(a.user.as_deref(), Some("me@example.com"));
        assert_eq!(a.host, "127.0.0.1");

        let a = Address::parse("sftp://[::1]:22").expect("parses");
        assert_eq!(a.host, "::1");
        assert_eq!(a.port, Some(22));
        assert_eq!(a.path, "/");

        let a = Address::parse("smb://nas/My%20Share?x=1#y").expect("parses");
        assert_eq!(a.path, "/My Share");
        assert_eq!(a.user, None);

        assert!(Address::parse("nas/share").is_none(), "no scheme");
        assert!(Address::parse("1smb://nas").is_none(), "not a scheme");
        assert!(Address::parse("ftp://host:port/").is_none(), "not a port");
    }

    /// What a share is called: `host/share` for SMB, `user@host` for the rest.
    #[test]
    fn labels_name_the_share_or_the_account() {
        let label = |url: &str| Address::parse(url).expect("parses").label();
        assert_eq!(label("smb://nas/media/films"), "nas/media");
        assert_eq!(label("smb://nas"), "nas");
        assert_eq!(label("sftp://me@example.org/srv"), "me@example.org");
        assert_eq!(label("dav://example.org/"), "example.org");
        assert_eq!(label("nfs://filer/export/home"), "filer");
        assert_eq!(
            label("ftp://anonymous@ftp.example.org"),
            "anonymous@ftp.example.org"
        );
    }

    /// The spec a directory is named after, both ways round.
    #[test]
    fn specs_are_written_and_read_the_way_gvfs_does() {
        let spec = |url: &str| Spec::of(&Address::parse(url).expect("parses"));
        assert_eq!(
            spec("smb://WORK;me@NAS:4455/Media").dir_name(),
            "smb-share:domain=WORK,port=4455,server=nas,share=media,user=me",
            "sorted keys, folded server and share, the domain split off"
        );
        assert_eq!(spec("smb://nas/").dir_name(), "smb-server:server=nas");
        assert_eq!(
            spec("davs://me@cloud/remote.php/webdav").dir_name(),
            "dav:host=cloud,ssl=true,user=me,prefix=%2Fremote.php%2Fwebdav",
            "the prefix last, its slashes escaped"
        );
        assert_eq!(
            spec("ftp://b%20o@localhost:2121/").dir_name(),
            "ftp:host=localhost,port=2121,user=b%20o"
        );
        // UTF-8 and gvfs's own allowed punctuation go through unescaped.
        assert_eq!(
            spec("sftp://josé+1@host/").dir_name(),
            "sftp:host=host,user=josé+1"
        );

        let read = Spec::from_dir_name("sftp:host=example.org,user=me%40corp").expect("a spec");
        assert!(read.same_server(&spec("sftp://me%40corp@Example.org/")));
        assert!(!read.same_server(&spec("sftp://other@example.org/")));
        assert!(!read.same_server(&spec("ftp://me%40corp@example.org/")));
        assert!(
            Spec::from_dir_name("not a spec").is_none(),
            "a stray name in the directory is ignored"
        );
    }

    /// The prompt's check: the six schemes, a server, and nothing else.
    #[test]
    fn the_connect_prompt_takes_network_shares_only() {
        assert_eq!(
            connect_url("  smb://nas/media "),
            Ok("smb://nas/media".to_string())
        );
        assert_eq!(
            connect_url("SFTP://me@host/srv"),
            Ok("sftp://me@host/srv".to_string()),
            "the scheme is lower-cased, the rest kept as typed"
        );
        for url in [
            "ftp://ftp.example.org",
            "dav://example.org/files",
            "davs://cloud.example.org/remote.php/webdav",
            "nfs://filer/export",
        ] {
            assert_eq!(connect_url(url), Ok(url.to_string()), "{url}");
        }

        let error = |text: &str| connect_url(text).expect_err(text);
        assert!(error("").contains("smb://"));
        assert!(error("nas/media").starts_with("not an address"));
        assert!(error("http://example.org").starts_with("http:// is not a network share"));
        assert!(error("file:///home").starts_with("file:// is not"));
        assert!(error("smb://").contains("no server"));
        assert!(error("sftp://me@/srv").contains("no server"));
    }

    /// What gio said, read. The authentication case is the captured one: the
    /// prompt on stdout, nothing on stderr, exit 2.
    #[test]
    fn a_failed_mount_is_read_for_what_it_wanted() {
        assert_eq!(attempt(true, "", ""), Attempt::Mounted);
        // Captured: the share was up before the prompt was typed into.
        assert_eq!(
            attempt(
                false,
                "",
                "gio: ftp://anonymous@127.0.0.1:2121/sub/dir: Location is already mounted\n"
            ),
            Attempt::Mounted
        );

        let asked = "Authentication Required\nEnter user and password for \u{201c}127.0.0.1:2121\u{201d}:\nUser: \n";
        assert_eq!(attempt(false, asked, ""), Attempt::NeedsTerminal);
        assert_eq!(
            attempt(false, "", "gio: smb://nas/x/: Password dialog cancelled\n"),
            Attempt::NeedsTerminal
        );
        assert_eq!(
            attempt(false, "", "gio: sftp://h/: No mount operation available\n"),
            Attempt::NeedsTerminal
        );
        // An unknown host key is a question on stdout too, with no keyword.
        assert_eq!(
            attempt(false, "The identity of the remote computer is unknown.\n[1] Log In Anyway\n[2] Cancel Login\n", ""),
            Attempt::NeedsTerminal
        );

        // Captured, as they come back from a machine with no server running.
        assert_eq!(
            attempt(
                false,
                "",
                "gio: smb://127.0.0.1/nothing/: Failed to mount Windows share: Connection refused\n"
            ),
            Attempt::Failed(
                "gio: smb://127.0.0.1/nothing/: Failed to mount Windows share: Connection refused"
                    .to_string()
            )
        );
        assert_eq!(
            attempt(false, "", "gio: bogus://x/: Location is not mountable\n"),
            Attempt::Failed("gio: bogus://x/: Location is not mountable".to_string())
        );
        assert!(matches!(attempt(false, "", ""), Attempt::Failed(_)));
    }

    /// Where a connected address lands: the share it is on, and the path it
    /// named inside it.
    #[test]
    fn a_connected_address_lands_inside_its_share() {
        let share = |url: &str, dir: &str| Share {
            url: url.to_string(),
            label: String::new(),
            scheme: String::new(),
            path: PathBuf::from("/gvfs").join(dir),
        };
        let shares = vec![
            share("smb://nas/media/", "smb-share:server=nas,share=media"),
            share("sftp://me@host/", "sftp:host=host,user=me"),
            share("davs://me@cloud/", "dav:host=cloud,ssl=true,user=me"),
            share(
                "davs://me@cloud/remote.php/webdav/",
                "dav:host=cloud,ssl=true,user=me,prefix=%2Fremote.php%2Fwebdav",
            ),
        ];
        let land = |url: &str| landing(&shares, &Address::parse(url).expect("parses"));

        assert_eq!(
            land("smb://NAS/Media/films/2024"),
            Some((
                PathBuf::from("/gvfs/smb-share:server=nas,share=media"),
                PathBuf::from("films/2024")
            ))
        );
        assert_eq!(
            land("sftp://me@host"),
            Some((
                PathBuf::from("/gvfs/sftp:host=host,user=me"),
                PathBuf::new()
            ))
        );
        assert_eq!(
            land("sftp://me@host/srv/../www"),
            Some((
                PathBuf::from("/gvfs/sftp:host=host,user=me"),
                PathBuf::from("srv/www")
            )),
            "no walking out of the share"
        );
        assert_eq!(
            land("davs://me@cloud/remote.php/webdav/Photos"),
            Some((
                PathBuf::from(
                    "/gvfs/dav:host=cloud,ssl=true,user=me,prefix=%2Fremote.php%2Fwebdav"
                ),
                PathBuf::from("Photos")
            )),
            "the deepest share it is inside"
        );
        assert_eq!(land("sftp://someone-else@host"), None);
        assert_eq!(land("smb://nas/other"), None);
    }

    // ── Phones and cameras ──────────────────────────────────────────────────

    /// `gio mount -li` on the development machine (gio 2.88, gvfs 1.60 with
    /// gvfs-mtp), with nothing plugged in: three of its nine drives, as
    /// captured — gio numbers what it lists, so the numbers skip. Everything
    /// here is udisks2's.
    const UNPLUGGED: &str = "\
Drive(1): Samsung SSD 960 EVO 500GB
  Type: GProxyDrive (GProxyVolumeMonitorUDisks2)
  ids:
   unix-device: '/dev/nvme1n1'
  themed icons:  [drive-harddisk-solidstate]  [drive-harddisk]  [drive]  [drive-harddisk-solidstate-symbolic]  [drive-harddisk-symbolic]  [drive-symbolic]
  symbolic themed icons:  [drive-harddisk-solidstate-symbolic]  [drive-harddisk-symbolic]  [drive-symbolic]  [drive-harddisk-solidstate]  [drive-harddisk]  [drive]
  is_removable=0
  is_media_removable=0
  has_media=1
  is_media_check_automatic=1
  can_poll_for_media=0
  can_eject=0
  can_start=0
  can_stop=0
  start_stop_type=shutdown
  sort_key=00coldplug/00fixed/nvme1
  Volume(0): 499 GB Volume
    Type: GProxyVolume (GProxyVolumeMonitorUDisks2)
    ids:
     class: 'device'
     unix-device: '/dev/nvme1n1p4'
     uuid: 'E4D4FCBAD4FC8FD2'
    uuid=E4D4FCBAD4FC8FD2
    themed icons:  [drive-harddisk-solidstate]  [drive-harddisk]  [drive]  [drive-harddisk-solidstate-symbolic]  [drive-harddisk-symbolic]  [drive-symbolic]
    symbolic themed icons:  [drive-harddisk-solidstate-symbolic]  [drive-harddisk-symbolic]  [drive-symbolic]  [drive-harddisk-solidstate]  [drive-harddisk]  [drive]
    can_mount=1
    can_eject=0
    should_automount=0
    sort_key=gvfs.time_detected_usec.1790345348083171
Drive(4): hp      DVD A  DH16AAL
  Type: GProxyDrive (GProxyVolumeMonitorUDisks2)
  ids:
   unix-device: '/dev/sr0'
  themed icons:  [drive-optical]  [drive]  [drive-optical-symbolic]  [drive-symbolic]
  symbolic themed icons:  [drive-optical-symbolic]  [drive-symbolic]  [drive-optical]  [drive]
  is_removable=1
  is_media_removable=1
  has_media=0
  is_media_check_automatic=1
  can_poll_for_media=0
  can_eject=1
  can_start=0
  can_stop=0
  start_stop_type=shutdown
  sort_key=00coldplug/11removable/sr0
Drive(8): Generic STORAGE DEVICE
  Type: GProxyDrive (GProxyVolumeMonitorUDisks2)
  ids:
   unix-device: '/dev/sde'
  themed icons:  [drive-removable-media-flash-sd]  [drive-removable-media-flash]  [drive-removable-media]  [drive-removable]  [drive]  [drive-removable-media-flash-sd-symbolic]  [drive-removable-media-flash-symbolic]  [drive-removable-media-symbolic]  [drive-removable-symbolic]  [drive-symbolic]
  symbolic themed icons:  [drive-removable-media-symbolic]  [drive-removable-symbolic]  [drive-symbolic]  [drive-removable-media]  [drive-removable]  [drive]
  is_removable=1
  is_media_removable=1
  has_media=1
  is_media_check_automatic=1
  can_poll_for_media=0
  can_eject=1
  can_start=0
  can_stop=0
  start_stop_type=shutdown
  sort_key=01hotplug/1790456998993800
  Volume(0): 64 GB Volume
    Type: GProxyVolume (GProxyVolumeMonitorUDisks2)
    ids:
     class: 'device'
     unix-device: '/dev/sde1'
    themed icons:  [media-flash-sd]  [media-flash]  [media]  [media-flash-sd-symbolic]  [media-flash-symbolic]  [media-symbolic]
    symbolic themed icons:  [media-flash-symbolic]  [media-symbolic]  [media-flash]  [media]
    can_mount=0
    can_eject=1
    should_automount=0
    sort_key=gvfs.time_detected_usec.1790456999250486
    Mount(0): 64 GB Volume -> file:///run/media/brian/disk
      Type: GProxyMount (GProxyVolumeMonitorUDisks2)
      default_location=file:///run/media/brian/disk
      themed icons:  [media-flash-sd]  [media-flash]  [media]  [media-flash-sd-symbolic]  [media-flash-symbolic]  [media-symbolic]
      symbolic themed icons:  [media-flash-symbolic]  [media-symbolic]  [media-flash]  [media]
      x_content_types: x-content/image-dcf
      can_unmount=1
      can_eject=1
      is_shadowed=0
      sort_key=gvfs.time_detected_usec.1790457433685306
";

    /// A phone's volume, as gvfs-mtp reports one — **not captured**: no phone
    /// could be plugged into the machine these were written on, so this is
    /// gio's format as its source prints a volume (`list_volumes`, glib
    /// 2.88's `gio-tool-mount.c`), with the activation root gvfs-mtp builds
    /// from the device's udev serial.
    const PHONE_VOLUME: &str = "\
Volume(0): Pixel 10a
  Type: GProxyVolume (GProxyVolumeMonitorMTP)
  ids:
   unix-device: '/dev/bus/usb/003/012'
  activation_root=mtp://Google_Pixel_10a_4B021FDAQ00123/
  themed icons:  [phone]  [phone-symbolic]
  symbolic themed icons:  [phone-symbolic]  [phone]
  can_mount=1
  can_eject=0
  should_automount=1
  sort_key=gvfs.time_detected_usec.1790460000000000
";

    /// …once it is mounted: gvfs's shadow of the mount nested under the
    /// volume, and the daemon's own mount at the margin. Written the same way.
    ///
    /// Its first line is on the `const`'s own line because a `\` line break
    /// in a Rust string takes the next line's indentation with it.
    const PHONE_MOUNTS: &str = "  Mount(0): Pixel 10a -> mtp://Google_Pixel_10a_4B021FDAQ00123/
    Type: GProxyShadowMount (GProxyVolumeMonitorMTP)
    default_location=mtp://Google_Pixel_10a_4B021FDAQ00123/
    themed icons:  [phone]  [phone-symbolic]
    symbolic themed icons:  [phone-symbolic]  [phone]
    x_content_types: x-content/image-dcf
    can_unmount=1
    can_eject=0
    is_shadowed=0
Mount(0): Pixel 10a -> mtp://Google_Pixel_10a_4B021FDAQ00123/
  Type: GDaemonMount
  default_location=mtp://Google_Pixel_10a_4B021FDAQ00123/
  themed icons:  [phone]  [phone-symbolic]
  symbolic themed icons:  [phone-symbolic]  [phone]
  can_unmount=1
  can_eject=0
  is_shadowed=1
";

    const PIXEL: &str = "mtp://Google_Pixel_10a_4B021FDAQ00123/";
    const PIXEL_DIR: &str = "mtp:host=Google_Pixel_10a_4B021FDAQ00123";

    /// Unplugged, plugged in, and mounted: no phone, then the phone not
    /// mounted, then the phone at its gvfs-fuse directory — and never a
    /// share, though its daemon's mount is at the margin where shares are.
    #[test]
    fn a_phone_is_the_mtp_monitors_volume_and_its_mount() {
        let root = Path::new("/run/user/1000/gvfs");
        assert!(phones_from(UNPLUGGED, root, &[]).is_empty());

        let plugged = format!("{UNPLUGGED}{PHONE_VOLUME}");
        assert_eq!(
            phones_from(&plugged, root, &[]),
            vec![Phone {
                root: PIXEL.to_string(),
                name: "Pixel 10a".to_string(),
                protocol: Protocol::Mtp,
                mount: None,
            }]
        );
        assert!(shares_from(&plugged, root, &[]).is_empty());

        let mounted = format!("{UNPLUGGED}{PHONE_VOLUME}{PHONE_MOUNTS}");
        let phones = phones_from(&mounted, root, &entries(&[PIXEL_DIR]));
        assert_eq!(phones.len(), 1, "one phone, though gio lists it twice");
        assert_eq!(phones[0].mount, Some(root.join(PIXEL_DIR)));
        assert_eq!(
            phones[0].detail(),
            format!("{}", root.join(PIXEL_DIR).display())
        );
        assert!(
            shares_from(&mounted, root, &entries(&[PIXEL_DIR])).is_empty(),
            "the daemon's mount at the margin is the phone, not a share"
        );
        // Before gvfs-fuse has caught up, the name it will use.
        assert_eq!(
            phones_from(&mounted, root, &[])[0].mount,
            Some(root.join(PIXEL_DIR))
        );
        // Nested only, or at the margin only: mounted either way.
        let shadow: String = PHONE_MOUNTS
            .lines()
            .take_while(|line| line.starts_with(' '))
            .map(|line| format!("{line}\n"))
            .collect();
        let nested = format!("{PHONE_VOLUME}{shadow}");
        assert!(phones_from(&nested, root, &[])[0].is_mounted());
        let margin =
            format!("{PHONE_VOLUME}Mount(0): Pixel 10a -> {PIXEL}\n  Type: GDaemonMount\n");
        assert!(phones_from(&margin, root, &[])[0].is_mounted());
    }

    /// A camera over gphoto2, from an older gvfs that roots it at its USB
    /// address, and a phone mounted by address with no volume monitor to
    /// report it: both rows, each at the directory gvfs names it.
    #[test]
    fn a_camera_and_a_phone_without_a_volume_are_rows_too() {
        let listing = "\
Volume(0): Canon Digital Camera
  Type: GProxyVolume (GProxyVolumeMonitorGPhoto2)
  activation_root=gphoto2://[usb:001,004]/
  Mount(0): Canon Digital Camera -> gphoto2://[usb:001,004]/
    Type: GProxyShadowMount (GProxyVolumeMonitorGPhoto2)
Volume(1): Card Reader
  Type: GProxyVolume (GProxyVolumeMonitorUDisks2)
Mount(0): Galaxy S24 -> mtp://SAMSUNG_Galaxy_S24_R5CX/
  Type: GDaemonMount
";
        let root = Path::new("/run/user/1000/gvfs");
        let phones = phones_from(listing, root, &[]);
        let rows: Vec<(&str, Protocol, Option<PathBuf>)> = phones
            .iter()
            .map(|p| (p.name.as_str(), p.protocol, p.mount.clone()))
            .collect();
        assert_eq!(
            rows,
            vec![
                (
                    "Canon Digital Camera",
                    Protocol::Gphoto2,
                    Some(root.join("gphoto2:host=%5Busb%3A001%2C004%5D"))
                ),
                (
                    "Galaxy S24",
                    Protocol::Mtp,
                    Some(root.join("mtp:host=SAMSUNG_Galaxy_S24_R5CX"))
                ),
            ],
            "sorted by name; the udisks2 volume is not a phone"
        );
        // A volume that says what it is but not how to mount it is no row.
        let rootless = "Volume(0): Pixel\n  Type: GProxyVolume (GProxyVolumeMonitorMTP)\n";
        assert!(phones_from(rootless, root, &[]).is_empty());
    }

    /// The spec bridge takes a device's root to gvfs's name for it, both ways
    /// round.
    #[test]
    fn a_devices_spec_is_its_host() {
        let spec = |url: &str| Spec::of(&Address::parse(url).expect("parses"));
        assert_eq!(spec(PIXEL).dir_name(), PIXEL_DIR);
        assert_eq!(
            spec("gphoto2://[usb:001,004]/").dir_name(),
            "gphoto2:host=%5Busb%3A001%2C004%5D"
        );
        let read = Spec::from_dir_name(PIXEL_DIR).expect("a spec");
        assert!(read.same_server(&spec(PIXEL)));
    }

    /// gio's monitor, as `--detail` prints it — **written from its source**
    /// (`gio-tool-mount.c`'s `monitor_*` callbacks), not captured, for the
    /// reason the phone's listing was not: a phone plugged in, mounted,
    /// the card reader's eject button pressed, the phone unmounted and pulled.
    const MONITOR: &str = "\
Volume added:       'Pixel 10a'
  Volume(0): Pixel 10a
    Type: GProxyVolume (GProxyVolumeMonitorMTP)
    ids:
     unix-device: '/dev/bus/usb/003/012'
    activation_root=mtp://Google_Pixel_10a_4B021FDAQ00123/
    themed icons:  [phone]  [phone-symbolic]
    symbolic themed icons:  [phone-symbolic]  [phone]
    can_mount=1
    can_eject=0
    should_automount=1

Mount added:        'Pixel 10a'
  Mount(0): Pixel 10a -> mtp://Google_Pixel_10a_4B021FDAQ00123/
    Type: GDaemonMount
    default_location=mtp://Google_Pixel_10a_4B021FDAQ00123/
    can_unmount=1
    can_eject=0
    is_shadowed=0

Drive eject button: 'Multiple Card  Reader'
Mount removed:      'nas/media'
  Mount(0): media on nas -> smb://nas/media/
    Type: GDaemonMount
    can_unmount=1
    can_eject=0
    is_shadowed=0

Volume removed:     'Pixel 10a'
  Volume(0): Pixel 10a
    Type: GProxyVolume (GProxyVolumeMonitorMTP)
    activation_root=mtp://Google_Pixel_10a_4B021FDAQ00123/
    can_mount=1
    can_eject=0
    should_automount=1

";

    /// Fed a line at a time, as the reader thread feeds it: each event is
    /// handed over the moment its last line arrives — the blank line under
    /// its detail, or its own line when it has none — and read for what
    /// happened, to what, and what it is.
    #[test]
    fn the_monitor_hands_over_each_event_as_it_ends() {
        let mut blocks = Blocks::default();
        let mut events = Vec::new();
        for (at, line) in MONITOR.lines().enumerate() {
            for block in blocks.feed(&format!("{line}\n")) {
                events.push((at, event_from(&block).expect("an event")));
            }
        }
        assert_eq!(blocks.finish(), None, "nothing held back at the end");
        type Heard<'a> = (usize, Change, &'a str, Option<Protocol>, Option<&'a str>);
        let got: Vec<Heard> = events
            .iter()
            .map(|(at, e)| {
                (
                    *at,
                    e.change,
                    e.name.as_str(),
                    e.protocol,
                    e.root.as_deref(),
                )
            })
            .collect();
        assert_eq!(
            got,
            vec![
                (
                    11,
                    Change::VolumeAdded,
                    "Pixel 10a",
                    Some(Protocol::Mtp),
                    Some(PIXEL)
                ),
                (
                    19,
                    Change::MountAdded,
                    "Pixel 10a",
                    Some(Protocol::Mtp),
                    Some(PIXEL)
                ),
                (20, Change::Other, "Multiple Card  Reader", None, None),
                (
                    27,
                    Change::MountRemoved,
                    "nas/media",
                    None,
                    Some("smb://nas/media/")
                ),
                (
                    35,
                    Change::VolumeRemoved,
                    "Pixel 10a",
                    Some(Protocol::Mtp),
                    Some(PIXEL)
                ),
            ],
            "each on the line that ended it"
        );

        // A stream that stops mid-event gives up what it had; a line that is
        // no event's is dropped; an event line closes the one before it.
        let mut blocks = Blocks::default();
        assert!(blocks.feed("gio: something of its own\n").is_empty());
        assert!(blocks.feed("Volume changed:     'Pixel 10a'\n").is_empty());
        let closed = blocks.feed("Mount pre-unmount:  'Pixel 10a'\n");
        assert_eq!(
            closed,
            vec![vec!["Volume changed:     'Pixel 10a'".to_string()]]
        );
        let last = blocks.finish().expect("the open block");
        assert_eq!(
            event_from(&last).map(|e| (e.change, e.name)),
            Some((Change::Other, "Pixel 10a".to_string()))
        );
    }

    /// The reader thread's loop over gio's words: every event handed over,
    /// the event loop rung once for each, and the loop over when the stream
    /// is.
    #[test]
    fn the_reader_rings_once_per_event_and_ends_with_the_stream() {
        let rung = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let bell = std::sync::Arc::clone(&rung);
        let (tx, rx) = unbounded::<Event>();
        listen(
            MONITOR.as_bytes(),
            tx,
            std::sync::Arc::new(move || {
                bell.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }),
        );
        let heard: Vec<Change> = rx.try_iter().map(|event| event.change).collect();
        assert_eq!(
            heard,
            [
                Change::VolumeAdded,
                Change::MountAdded,
                Change::Other,
                Change::MountRemoved,
                Change::VolumeRemoved,
            ]
        );
        assert_eq!(rung.load(std::sync::atomic::Ordering::SeqCst), 5);
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
        let monitor = Monitor::spawn(
            command,
            std::sync::Arc::new(move || {
                bell.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }),
        )
        .expect("sh starts");
        let deadline = Instant::now() + Duration::from_secs(10);
        while !monitor.gone() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(monitor.gone(), "the pipe ended and nobody noticed");
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

    /// The worker's answers come with the request they answer, and what is
    /// still out is known after the card that asked for it has gone: the
    /// oldest request about a row, until its own answer comes back — a
    /// listing's answer in between does not count as it.
    #[test]
    fn the_worker_knows_what_is_still_out() {
        let (mut worker, asked, answers) = Mounts::detached();
        assert_eq!(worker.in_flight(), None);
        worker.ask(Request::List);
        assert_eq!(worker.in_flight(), None, "a listing is about no row");
        worker.ask(Request::Eject {
            object: "/block/sdb1".to_string(),
            drive: "/drives/usb".to_string(),
        });
        assert_eq!(worker.in_flight(), Some(("/block/sdb1", "ejecting…")));
        assert_eq!(asked.try_iter().count(), 2);

        answers
            .send(Answer {
                to: Request::List,
                reply: Reply::Failed("gone".to_string()),
            })
            .expect("listening");
        let heard = worker.drain();
        assert_eq!(heard.len(), 1);
        assert_eq!(heard[0].to, Request::List);
        assert_eq!(worker.in_flight(), Some(("/block/sdb1", "ejecting…")));

        answers
            .send(Answer {
                to: Request::Eject {
                    object: "/block/sdb1".to_string(),
                    drive: "/drives/usb".to_string(),
                },
                reply: Reply::Ejected,
            })
            .expect("listening");
        worker.drain();
        assert_eq!(worker.in_flight(), None);
        assert_eq!(
            Request::GioUnmount(PIXEL.to_string()).row(),
            Some(PIXEL),
            "a phone's or a share's is its URL"
        );
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

    // ── The card ────────────────────────────────────────────────────────────

    fn share_rows(n: usize) -> Vec<Share> {
        (0..n)
            .map(|i| Share {
                url: format!("sftp://me@host{i}/"),
                label: format!("me@host{i}"),
                scheme: "sftp".to_string(),
                path: PathBuf::from(format!("/run/user/1000/gvfs/sftp:host=host{i},user=me")),
            })
            .collect()
    }

    fn many_devices(n: usize) -> Vec<Device> {
        (0..n)
            .map(|i| Device {
                object: format!("/block/{i}"),
                drive: Some("/drives/x".into()),
                node: format!("/dev/sd{i}"),
                label: format!("Disk {i}"),
                fs: "ext4".into(),
                size: 1 << 30,
                mount: (i % 2 == 0).then(|| PathBuf::from("/run/media/x")),
                removable: i % 3 == 0,
                ejectable: true,
                hardware: "Acme Widget".into(),
            })
            .collect()
    }

    /// The phone of the fixtures above, mounted or not.
    fn pixel(mounted: bool) -> Phone {
        Phone {
            root: PIXEL.to_string(),
            name: "Pixel 10a".to_string(),
            protocol: Protocol::Mtp,
            mount: mounted.then(|| PathBuf::from("/run/user/1000/gvfs").join(PIXEL_DIR)),
        }
    }

    fn palette() -> crate::theme::Palette {
        let theme = df_core::config::Theme::default();
        crate::theme::Palette::from_theme(&theme, df_core::config::Appearance::Dark)
    }

    /// The card lays out and paints in every state without panicking, with a
    /// patched font and without one.
    #[test]
    fn the_card_paints_in_every_state() {
        let theme = df_core::config::Theme::default();
        let palette = palette();
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(Default::default(), |ui| {
            for nerd in [false, true] {
                let painting = crate::ui::Painting {
                    tips: None,
                    held: None,
                    painter: ui.painter(),
                    palette: &palette,
                    theme: &theme,
                    tags: &crate::tags::BUILT_IN,
                    nerd,
                    show_symlink: true,
                    now: std::time::Instant::now(),
                };
                let hovers = crate::hover::Hovers::new();
                for area in [
                    egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, 900.0)),
                    egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 90.0)),
                ] {
                    let draw = |card: &Card| {
                        paint(
                            &painting,
                            card,
                            &geometry(area, card),
                            &hovers,
                            &crate::ripple::Ripples::new(),
                        );
                    };
                    // Still loading, empty, populated, scrolled, busy, failed.
                    let mut card = Card::new();
                    draw(&card);
                    card.update(Vec::new(), Vec::new(), Vec::new());
                    draw(&card);
                    card.set_places(places(3));
                    draw(&card);
                    card.update(
                        many_devices(20),
                        vec![pixel(false), pixel(true)],
                        share_rows(3),
                    );
                    draw(&card);
                    card.set_clouds(clouds());
                    draw(&card);
                    card.move_cursor(21);
                    card.start("sftp://me@host1/", "unmounting…");
                    draw(&card);
                    card.move_cursor(-3);
                    card.start(PIXEL, "mounting…");
                    draw(&card);
                    card.fail(UNLOCK);
                    draw(&card);
                }
            }
        });
    }

    /// The geometry hit-tests to the rows it drew, and only to rows: every
    /// row one line tall, the height of a row in the panes, and every heading
    /// its own smaller line.
    #[test]
    fn the_card_hit_tests_its_one_line_rows_and_not_its_headings() {
        let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, 900.0));
        let mut card = Card::new();
        card.update(devices_from(&objects()), vec![pixel(false)], share_rows(1));
        let g = geometry(area, &card);
        // Two disks, the phone, one share and the connect row.
        assert_eq!(g.rows.len(), 5);
        assert_eq!(
            card.visible_items(),
            vec![
                Item::Disk(0),
                Item::Disk(1),
                Item::Phone(0),
                Item::Share(0),
                Item::Connect
            ]
        );
        for (i, rect) in g.rows.iter().enumerate() {
            assert_eq!(g.row_at(rect.center()), Some(i));
            assert!(g.body.contains_rect(*rect));
            assert_eq!(
                rect.height(),
                crate::ui::ROW_HEIGHT,
                "one line, a pane's row"
            );
            // No gap between rows for a pointer to fall through, and none on
            // top of another.
            if let Some(next) = g.rows.get(i + 1) {
                assert!(next.top() >= rect.bottom() - 0.01);
            }
        }
        assert!(g.row_at(egui::pos2(0.0, 0.0)).is_none());
        // A heading is drawn, is its own height, and is not a row.
        let (heading, rect) = g.lines[0];
        assert_eq!(heading, Line::Section("Devices"));
        assert_eq!(rect.height(), SECTION_ROW);
        assert!(g.row_at(rect.center()).is_none());

        // An empty card still has its three headings, every empty state, and
        // the connect row.
        card.update(Vec::new(), Vec::new(), Vec::new());
        let g = geometry(area, &card);
        assert_eq!(g.rows.len(), 1);
        assert_eq!(
            g.lines.iter().map(|(line, _)| *line).collect::<Vec<_>>(),
            vec![
                Line::Section("Devices"),
                Line::Empty("no removable filesystems"),
                Line::Section("Network"),
                Line::Empty("nothing mounted"),
                Line::Item(Item::Connect),
                Line::Section("Places"),
                Line::Empty(PLACES_EMPTY),
            ]
        );
        for (line, rect) in &g.lines {
            if let Line::Empty(_) = line {
                assert_eq!(rect.height(), ROW, "an empty state is one line too");
            }
        }
    }

    /// Glyph, name and detail on one centre line, left to right, with the key
    /// chip at the far end: inset from the row's top, bottom and end by one
    /// gap, its corner concentric with the row plate's round it.
    #[test]
    fn a_row_lays_its_name_its_detail_and_its_key_along_one_line() {
        let row = egui::Rect::from_min_size(egui::pos2(0.0, 100.0), egui::vec2(500.0, ROW));
        let y = row.center().y;
        let layout = row_layout(row, true, 60.0, Some(40.0));
        assert_eq!(
            layout.glyph,
            Some(egui::pos2(ROW_PAD + ICON_COLUMN / 2.0, y))
        );
        let name_left = ROW_PAD + ICON_COLUMN + crate::chrome::ICON_GAP;
        assert_eq!(layout.name, (egui::pos2(name_left, y), 60.0));
        let (detail_at, detail_room) = layout.detail;
        assert_eq!(detail_at, egui::pos2(name_left + 60.0 + DETAIL_GAP, y));

        let chip = layout.chip.expect("a key, so a chip");
        let inset = crate::chrome::CHIP_INSET;
        assert_eq!(chip.top() - row.top(), inset);
        assert_eq!(row.bottom() - chip.bottom(), inset);
        assert_eq!(
            row.right() - chip.right(),
            inset,
            "equal gaps on every side"
        );
        assert_eq!(chip.width(), 40.0);
        assert_eq!(
            crate::chrome::CHIP_RADIUS + inset as u8,
            crate::ui::ROW_RADIUS,
            "the chip's corner is concentric with the row's"
        );
        assert_eq!(
            detail_at.x + detail_room,
            chip.left() - DETAIL_GAP,
            "the detail stops a gap short of the chip"
        );

        // A name longer than its share leaves the detail the rest; no chip is
        // the row's padding at the end; no glyph is no column.
        let long = row_layout(row, true, 900.0, None);
        let room = row.right() - ROW_PAD - name_left;
        assert_eq!(long.name.1, room * NAME_SHARE);
        assert_eq!(long.chip, None);
        assert!((long.detail.0.x + long.detail.1 - (row.right() - ROW_PAD)).abs() < 1e-3);
        let bare = row_layout(row, false, 60.0, None);
        assert_eq!(bare.glyph, None);
        assert_eq!(bare.name.0.x, ROW_PAD);
        // Squeezed to nothing, nothing is negative.
        let tiny = row_layout(
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(20.0, ROW)),
            true,
            60.0,
            Some(40.0),
        );
        assert!(tiny.name.1 >= 0.0 && tiny.detail.1 >= 0.0);
    }

    /// A path too long for the room it has loses its middle, keeping whose it
    /// is and which it is; one that fits is whole; and a detail with no room
    /// to speak of is left out rather than squeezed to a lone `…`.
    #[test]
    fn a_long_detail_loses_its_middle() {
        let seven = |text: &str| text.chars().count() as f32 * 7.0;
        let path = "/run/user/1000/gvfs/mtp:host=Google_Pixel_10a_4B021FDAQ00123";
        let fitted = fitted_detail(path, 20.0 * 7.0, seven);
        assert!(fitted.chars().count() <= 20, "{fitted}");
        assert!(fitted.starts_with("/run/user"), "{fitted}");
        assert!(fitted.ends_with("DAQ00123"), "{fitted}");
        assert!(fitted.contains('…'));
        assert_eq!(fitted_detail("~/Work", 200.0, seven), "~/Work");
        assert_eq!(fitted_detail(path, MIN_DETAIL - 1.0, seven), "");
    }

    /// Each kind of row says what it is after its name: a disk its size,
    /// filesystem and mount point; a phone where it is or that it is not
    /// mounted; a share its URL; a remote its service by name; a place its
    /// path, with its key as a chip.
    #[test]
    fn every_row_says_what_it_is_after_its_name() {
        let palette = palette();
        let mut card = Card::with_places(places(2));
        card.update(
            many_devices(1),
            vec![pixel(false), pixel(true)],
            share_rows(1),
        );
        card.set_clouds(clouds());
        let said = |item: Item| {
            let face = face(&card, item, &palette, true);
            (face.name, face.detail, face.key)
        };
        let glyph =
            |card: &Card, item: Item| face(card, item, &palette, true).icon.map(|i| i.glyph);
        assert_eq!(
            said(Item::Disk(0)),
            ("Disk 0".into(), "1.0 GB · ext4 · /run/media/x".into(), None)
        );
        assert_eq!(
            said(Item::Phone(0)),
            ("Pixel 10a".into(), "not mounted".into(), None)
        );
        assert_eq!(
            said(Item::Phone(1)).1,
            format!("/run/user/1000/gvfs/{PIXEL_DIR}")
        );
        assert_eq!(
            said(Item::Share(0)),
            ("me@host0".into(), "sftp://me@host0/".into(), None)
        );
        assert_eq!(said(Item::Cloud(0)), ("r2".into(), "S3".into(), None));
        assert_eq!(said(Item::Cloud(1)).1, "rclone");
        assert_eq!(
            said(Item::Place(0)),
            ("place0".into(), "~/place0".into(), Some("g w".into())),
            "the folder, where it is, and its key"
        );
        assert_eq!(said(Item::Place(1)).2, None, "a keyless pin has no chip");
        for item in card.items() {
            let face = face(&card, item, &palette, true);
            assert_eq!(face.tone, palette.quiet, "{item:?}: the detail is quiet");
            assert!(face.icon.is_some(), "{item:?}: every row has its glyph");
        }
        assert_eq!(
            glyph(&card, Item::Phone(0)),
            Some(crate::icons::phone(&palette, true).glyph)
        );
        assert_eq!(
            glyph(&card, Item::Place(1)),
            Some(crate::icons::network(&palette, true).glyph),
            "a place on another machine wears the server"
        );
        let camera = Phone {
            protocol: Protocol::Gphoto2,
            ..pixel(false)
        };
        card.update(Vec::new(), vec![camera], Vec::new());
        assert_eq!(
            glyph(&card, Item::Phone(0)),
            Some(crate::icons::camera(&palette, true).glyph)
        );
        assert_eq!(
            Cloud {
                name: "gdrive".into(),
                provider: "drive".into()
            }
            .service(),
            "Google Drive"
        );
    }

    /// A row is filled as a pane's row is — the cursor's fill, a hover's
    /// lift — and its words in an accent are that accent as ink, on either
    /// side: on the light one the cursor is the panes' wash of blue, and the
    /// amber and red of a status are darkened until they read.
    #[test]
    fn a_row_is_filled_and_inked_as_the_panes_rows_are() {
        let theme = df_core::config::Theme::default();
        for side in [
            df_core::config::Appearance::Dark,
            df_core::config::Appearance::Light,
        ] {
            let palette = crate::theme::Palette::from_theme(&theme, side);
            assert_eq!(row_fill(&palette, false, 0.0), None, "a plain row");
            assert_eq!(
                row_fill(&palette, true, 0.0),
                Some(crate::theme::cursor_fill(&palette)),
                "{side:?}: the cursor"
            );
            assert_eq!(
                row_fill(&palette, false, 1.0),
                Some(crate::theme::lift(
                    &palette,
                    palette.crust,
                    crate::ui::HOVER_LIFT
                )),
                "{side:?}: a hover"
            );
            assert_eq!(
                row_fill(&palette, true, 1.0),
                Some(crate::theme::lift(
                    &palette,
                    crate::theme::cursor_fill(&palette),
                    crate::ui::CURSOR_HOVER_LIFT
                )),
                "{side:?}: a hover on the cursor"
            );

            let mut card = Card::new();
            card.update(many_devices(1), Vec::new(), Vec::new());
            card.start("/block/0", "mounting…");
            let busy = face(&card, Item::Disk(0), &palette, false);
            assert_eq!(busy.tone, crate::theme::ink(&palette, palette.peach));
            card.fail("refused");
            let failed = face(&card, Item::Disk(0), &palette, false);
            assert_eq!(failed.tone, crate::theme::ink(&palette, palette.red));
        }
    }

    /// While a call is out on a row, what it is doing is said where its
    /// detail was, in amber; when it fails, the failure is, in red, until the
    /// next call; and no other row changes.
    #[test]
    fn a_rows_status_takes_its_details_place() {
        let palette = palette();
        let mut card = Card::new();
        card.update(many_devices(2), vec![pixel(false)], Vec::new());
        let seen = |card: &Card, item: Item| {
            let face = face(card, item, &palette, false);
            (face.detail, face.tone)
        };
        card.start("/block/1", "mounting…");
        assert_eq!(
            seen(&card, Item::Disk(1)),
            (
                "mounting…".to_string(),
                crate::theme::ink(&palette, palette.peach)
            )
        );
        assert_eq!(seen(&card, Item::Disk(0)).1, palette.quiet, "not its row");
        assert!(card.is_busy("/block/1"));

        card.fail("Not authorized to perform operation");
        assert!(card.busy.is_none());
        assert_eq!(
            seen(&card, Item::Disk(1)),
            (
                "Not authorized to perform operation".to_string(),
                crate::theme::ink(&palette, palette.red)
            )
        );
        // The next call is a fresh start: the failure is over.
        card.start(PIXEL, "mounting…");
        assert_eq!(seen(&card, Item::Disk(1)).1, palette.quiet);
        assert_eq!(
            seen(&card, Item::Phone(0)),
            (
                "mounting…".to_string(),
                crate::theme::ink(&palette, palette.peach)
            )
        );
        card.finish();
        assert_eq!(
            seen(&card, Item::Phone(0)),
            ("not mounted".to_string(), palette.quiet)
        );
        // A failure with nothing out is nobody's.
        card.fail("stray");
        assert!(card.failed.is_none());
    }

    /// A card with more places than it shows says how many more on a line
    /// of its own: under the last row drawn, over the hint strip, inside the
    /// height the window allowed — and so clear of the last row's key chip,
    /// which it used to be drawn over. The line stays, empty, when the view
    /// reaches the end, so the card keeps its size; a card that shows every
    /// line has no such line and keeps no room for one.
    #[test]
    fn more_is_said_on_its_own_line_under_the_last_row() {
        let now = std::time::Instant::now();
        let keyed: Vec<Place> = places(30)
            .into_iter()
            .enumerate()
            .map(|(i, place)| Place {
                key: Some(format!("g {}", i % 10)),
                ..place
            })
            .collect();
        for height in (300..=900).step_by(50) {
            let area =
                egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, height as f32));
            let mut card = Card::with_places(keyed.clone());
            card.update(many_devices(1), Vec::new(), Vec::new());
            card.fit(window(area), now);
            card.select(Item::Place(3));
            let g = geometry(area, &card);
            assert!(card.overflows() && card.more() > 0, "in {height}");
            let line = g.more.expect("the count has its own line");
            assert_eq!(line.height(), MORE_LINE, "in {height}");
            assert!(area.contains_rect(g.card), "past the window in {height}");
            assert!(g.card.contains_rect(line));
            assert!(
                line.bottom() <= g.card.bottom() - PAD - crate::chrome::HINT_ROW + 0.01,
                "over the hint strip in {height}"
            );
            // The last row drawn is a place with its key, whole in the body,
            // and its chip is above the count's line.
            let (last, rect) = *g.lines.last().expect("lines");
            let Line::Item(Item::Place(i)) = last else {
                panic!("the last line in {height} is {last:?}");
            };
            assert!(card.places[i].key.is_some());
            assert!(g.body.contains_rect(rect));
            let chip = row_layout(rect, true, 40.0, Some(30.0))
                .chip
                .expect("a key, so a chip");
            assert!(g.body.contains_rect(chip), "the chip is cut in {height}");
            assert!(chip.bottom() <= line.top(), "the count is over the chip");
            assert!(rect.bottom() <= line.top() + 0.01, "the count is in a row");
            assert_eq!(g.row_at(line.center()), None, "the line is not a row");

            // At the end: nothing more to say, and the card the same size.
            card.jump(Jump::Bottom);
            let end = geometry(area, &card);
            assert_eq!(card.more(), 0);
            assert!(end.more.is_some());
            assert_eq!(end.card, g.card, "the card changed size at the end");
        }

        let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, 900.0));
        let mut few = Card::with_places(places(2));
        few.update(many_devices(1), Vec::new(), Vec::new());
        few.fit(window(area), now);
        let g = geometry(area, &few);
        assert!(!few.overflows());
        assert_eq!(g.more, None);
        assert_eq!(g.card.height(), FIXED + few.body_height());
    }

    /// …and the card is tall enough for the lines it draws, the heading over
    /// them and the hint strip under them — the strip the keys now live in
    /// alone, having been in the title row as well.
    #[test]
    fn the_card_is_as_tall_as_what_it_draws() {
        let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, 900.0));
        let mut card = Card::new();
        card.update(devices_from(&objects()), Vec::new(), Vec::new());
        let g = geometry(area, &card);
        let (_, last) = *g.lines.last().expect("lines");
        assert_eq!(last.height(), ROW);
        assert!(
            g.card.bottom() - last.bottom() >= crate::chrome::HINT_ROW + PAD - 0.01,
            "the hint strip would be drawn over the last row"
        );
        // Everything sits under the title and no two lines overlap.
        assert!(g.lines[0].1.top() >= g.card.top() + PAD + TITLE);
        for pair in g.lines.windows(2) {
            assert!(pair[1].1.top() >= pair[0].1.bottom() - 0.01);
        }
    }

    /// An empty reply is a normal machine, not a failure.
    #[test]
    fn a_machine_with_no_disks_produces_no_rows() {
        assert!(devices_from(&[]).is_empty());
    }

    /// The page keys clamp, and a refresh keeps the cursor on the row it was
    /// on.
    #[test]
    fn the_cursor_holds_its_place_across_a_refresh() {
        let mut card = Card::new();
        assert_eq!(card.devices_empty(), Some("asking udisks2…"));
        assert_eq!(card.shares_empty(), Some("asking gvfs…"));
        assert_eq!(card.selected(), Some(Item::Connect), "the only row so far");
        card.update(devices_from(&objects()), Vec::new(), share_rows(1));
        assert_eq!(card.devices_empty(), None);
        assert_eq!(card.shares_empty(), None);
        assert_eq!(
            card.selected(),
            Some(Item::Disk(0)),
            "the first listing puts the cursor on the first disk, not on the connect row it started on"
        );

        card.move_cursor(1);
        assert_eq!(
            card.selected_device().map(|d| d.node.as_str()),
            Some("/dev/nvme0n1p2")
        );
        // A page key stops at the end, where the arrows wrap: all four rows
        // are on screen, so a page from the second is past the bottom.
        card.jump(Jump::Page(1));
        assert_eq!(card.selected(), Some(Item::Connect));
        card.jump(Jump::Page(-1));
        assert_eq!(card.cursor, 0);

        // The USB stick is now mounted and sorts the same way; the cursor is on
        // it before and after.
        card.move_cursor(1);
        let on = card.selected_device().map(|d| d.object.clone());
        let mut again = devices_from(&objects());
        again.reverse();
        card.update(again, Vec::new(), share_rows(1));
        assert_eq!(card.selected_device().map(|d| d.object.clone()), on);

        // A share keeps the cursor across a refresh that adds a disk and a
        // phone above it.
        card.select(Item::Share(0));
        card.update(many_devices(3), vec![pixel(false)], share_rows(1));
        assert_eq!(card.selected(), Some(Item::Share(0)));
        assert_eq!(card.cursor, 4);
        // …and a phone keeps it across a refresh that mounts it.
        card.select(Item::Phone(0));
        card.update(many_devices(1), vec![pixel(true)], share_rows(1));
        assert_eq!(card.selected(), Some(Item::Phone(0)));
        assert!(card.selected_phone().is_some_and(Phone::is_mounted));

        // A listing with nothing leaves the connect row, and does not panic.
        card.update(Vec::new(), Vec::new(), Vec::new());
        assert!(card.selected_device().is_none());
        assert_eq!(card.selected(), Some(Item::Connect));
        assert_eq!(card.devices_empty(), Some("no removable filesystems"));
        assert_eq!(card.shares_empty(), Some("nothing mounted"));
        // A phone alone is not "no removable filesystems".
        card.update(Vec::new(), vec![pixel(false)], Vec::new());
        assert_eq!(card.devices_empty(), None);
    }

    fn places(n: usize) -> Vec<Place> {
        (0..n)
            .map(|i| Place {
                name: format!("place{i}"),
                detail: format!("~/place{i}"),
                key: (i == 0).then(|| "g w".to_string()),
                target: PathBuf::from(format!("/home/me/place{i}")),
                remote: i == 1,
                pinned: i != 2,
            })
            .collect()
    }

    /// Devices on top, then the network, then the places; the card opens with
    /// the cursor on its first row, where the first device will be — the
    /// first network row while the listing is out, the first device once it
    /// lands, the index never moving. The places are one `↑` away round the
    /// top, and a place coming off the list leaves the cursor at the same
    /// height.
    #[test]
    fn the_card_opens_on_the_devices_with_the_places_below() {
        let mut card = Card::with_places(places(3));
        assert_eq!(
            card.items(),
            [
                Item::Connect,
                Item::Place(0),
                Item::Place(1),
                Item::Place(2)
            ]
        );
        assert_eq!(card.cursor, 0, "the first row");
        assert_eq!(
            card.selected(),
            Some(Item::Connect),
            "until a device is there"
        );
        card.update(devices_from(&objects()), vec![pixel(false)], share_rows(1));
        assert_eq!(card.cursor, 0, "no jump when the listing lands");
        assert_eq!(
            card.selected(),
            Some(Item::Disk(0)),
            "the first disk filled it"
        );
        assert_eq!(card.first, 0);
        assert_eq!(
            card.lines(),
            [
                Line::Section("Devices"),
                Line::Item(Item::Disk(0)),
                Line::Item(Item::Disk(1)),
                Line::Item(Item::Phone(0)),
                Line::Section("Network"),
                Line::Item(Item::Share(0)),
                Line::Item(Item::Connect),
                Line::Section("Places"),
                Line::Item(Item::Place(0)),
                Line::Item(Item::Place(1)),
                Line::Item(Item::Place(2)),
            ],
            "disks, then phones, then the network, then the places"
        );
        card.move_cursor(-1);
        assert_eq!(card.selected(), Some(Item::Place(2)), "one ↑ away");
        assert_eq!(
            card.selected_place().map(|p| p.name.as_str()),
            Some("place2")
        );
        // A place off the list: the cursor stays at its height, on the row
        // that moved into the gap.
        card.select(Item::Place(1));
        card.set_places(places(2));
        assert_eq!(card.selected(), Some(Item::Place(1)));
        card.set_places(Vec::new());
        assert_eq!(card.selected(), Some(Item::Connect), "the last row now");
        assert!(card.selected_place().is_none());

        // No disks: the phone fills the first row. Nothing plugged in: the
        // first network row does.
        let mut phone = Card::with_places(places(2));
        phone.update(Vec::new(), vec![pixel(false)], share_rows(1));
        assert_eq!(phone.selected(), Some(Item::Phone(0)));
        let mut bare = Card::with_places(places(2));
        bare.update(Vec::new(), Vec::new(), share_rows(1));
        assert_eq!(bare.selected(), Some(Item::Share(0)));
        // A cursor moved round into the places before the listing lands is
        // somebody's choice, and stays.
        let mut early = Card::with_places(places(3));
        early.move_cursor(-2);
        assert_eq!(early.selected(), Some(Item::Place(1)));
        early.update(devices_from(&objects()), Vec::new(), Vec::new());
        assert_eq!(early.selected(), Some(Item::Place(1)));

        // Thirty places under the devices: the card opens at its top, and the
        // last place, reached by End, is whole at the bottom — what is drawn
        // is whole lines, so no half row sits under the "+N more".
        let mut long = Card::with_places(places(30));
        long.update(devices_from(&objects()), Vec::new(), share_rows(1));
        assert_eq!(long.selected(), Some(Item::Disk(0)));
        assert_eq!(long.visible()[0].0, Line::Section("Devices"));
        assert!(long.visible().len() < long.lines().len());
        long.jump(Jump::Bottom);
        assert_eq!(long.selected(), Some(Item::Place(29)));
        let (last, top) = *long.visible().last().expect("lines");
        assert_eq!(last, Line::Item(Item::Place(29)));
        assert!(top + last.height() <= WINDOW + 0.01, "a row is cut off");
    }

    /// A long card scrolls to keep the cursor's row whole, brings a section's
    /// heading back with its first row, and never scrolls past its end.
    #[test]
    fn a_long_card_scrolls_to_the_cursor() {
        let mut card = Card::new();
        card.update(many_devices(30), Vec::new(), share_rows(30));
        assert_eq!(card.first, 0);

        // Down to the connect row, the last row: it is wholly inside the
        // window, and the view went no further down than that took.
        card.jump(Jump::Bottom);
        assert_eq!(card.selected(), Some(Item::Connect));
        let (_, top) = card
            .visible()
            .into_iter()
            .find(|(line, _)| *line == Line::Item(Item::Connect))
            .expect("the cursor's row is drawn");
        assert!(top + ROW <= WINDOW + 0.01, "the cursor's row is cut off");
        assert!(card.first > 0);
        let heights: Vec<f32> = card.lines().iter().map(|line| line.height()).collect();
        let at = card
            .lines()
            .iter()
            .position(|line| *line == Line::Item(Item::Connect))
            .expect("the connect row");
        assert!(
            heights[card.first - 1..=at].iter().sum::<f32>() > WINDOW,
            "scrolled further than the cursor needed"
        );

        // Up, a row at a time, to the first share: the Network heading comes
        // into view over it.
        for _ in 0..40 {
            if card.selected() == Some(Item::Share(0)) {
                break;
            }
            card.move_cursor(-1);
        }
        assert_eq!(card.selected(), Some(Item::Share(0)));
        assert_eq!(
            card.visible()[0].0,
            Line::Section("Network"),
            "the share at the top has its section's name over it"
        );

        // All the way up: the card's top is back.
        card.jump(Jump::Top);
        assert_eq!(card.first, 0);
        assert_eq!(card.visible()[0].0, Line::Section("Devices"));

        // A short list never scrolls.
        assert_eq!(
            scroll(
                3,
                1,
                &[Line::Section("Devices"), Line::Item(Item::Connect)],
                WINDOW
            ),
            0
        );
    }

    /// `↑` on the first row is the last place and `↓` on the last place is
    /// the first row, with the view following the cursor round either way.
    #[test]
    fn the_arrows_wrap_at_both_ends() {
        let mut card = Card::with_places(places(2));
        card.update(devices_from(&objects()), Vec::new(), share_rows(1));
        let last = card.items().len() - 1;
        assert_eq!(card.selected(), Some(Item::Disk(0)));

        card.move_cursor(-1);
        assert_eq!(card.cursor, last, "↑ on the first row is the last");
        assert_eq!(card.selected(), Some(Item::Place(1)));
        card.move_cursor(1);
        assert_eq!(card.cursor, 0, "↓ on the last row is the first");
        assert_eq!(card.selected(), Some(Item::Disk(0)));

        // A stride longer than the list goes round as many times as it says.
        card.move_cursor(last as isize + 1);
        assert_eq!(card.cursor, 0);
        card.move_cursor(-(last as isize + 2));
        assert_eq!(card.cursor, last);

        // On a card that scrolls, the view goes round with the cursor: the
        // last row whole at the bottom, then the top of the card back.
        let mut long = Card::with_places(places(3));
        long.update(many_devices(12), Vec::new(), share_rows(12));
        assert_eq!(long.selected(), Some(Item::Disk(0)));
        long.move_cursor(-1);
        assert_eq!(long.selected(), Some(Item::Place(2)));
        let (bottom, top) = *long.visible().last().expect("lines");
        assert_eq!(bottom, Line::Item(Item::Place(2)));
        assert!(top + ROW <= WINDOW + 0.01, "the cursor's row is cut off");
        assert!(long.first > 0);
        long.move_cursor(1);
        assert_eq!(long.selected(), Some(Item::Disk(0)));
        assert_eq!(long.first, 0);
        assert_eq!(long.visible()[0].0, Line::Section("Devices"));
    }

    /// The card has no empty case: the connect row is always there, before
    /// udisks2 has answered and after it has answered nothing. That one row
    /// is both ends, so every key leaves the cursor on it.
    #[test]
    fn a_card_with_one_row_stays_on_it() {
        let mut waiting = Card::new();
        let mut empty = Card::new();
        empty.update(Vec::new(), Vec::new(), Vec::new());
        for card in [&mut waiting, &mut empty] {
            assert_eq!(card.items(), [Item::Connect]);
            for delta in [1, -1, 2, -7] {
                card.move_cursor(delta);
                assert_eq!(card.cursor, 0, "{delta}");
            }
            for jump in [
                Jump::Page(1),
                Jump::Page(-1),
                Jump::HalfPage(1),
                Jump::HalfPage(-1),
                Jump::Top,
                Jump::Bottom,
            ] {
                card.jump(jump);
                assert_eq!(card.cursor, 0, "{jump:?}");
            }
            assert_eq!(card.selected(), Some(Item::Connect));
        }
    }

    /// A page is the item rows on screen at the moment of the press — the
    /// headings and empty states between them take room and are not rows —
    /// and half a page is half that. Neither goes past an end, and the view
    /// follows every one.
    #[test]
    fn the_page_keys_stride_by_the_rows_on_screen_and_stop_at_the_ends() {
        let mut card = Card::new();
        card.update(many_devices(30), Vec::new(), share_rows(3));
        let last = card.items().len() - 1;
        assert_eq!(card.cursor, 0);
        // Under the Devices heading, twenty-two one-line rows fill the
        // window.
        assert_eq!(card.visible_items().len(), 22);

        let page = card.visible_items().len();
        card.jump(Jump::Page(1));
        assert_eq!(card.cursor, page);
        let on_screen = |card: &Card| {
            let item = card.selected().expect("a row");
            card.visible_items().contains(&item)
        };
        assert!(on_screen(&card), "the view follows a page down");

        let page = card.visible_items().len();
        let from = card.cursor;
        card.jump(Jump::HalfPage(1));
        assert_eq!(card.cursor, (from + page / 2).min(last));
        assert!(on_screen(&card));

        let page = card.visible_items().len();
        let from = card.cursor;
        card.jump(Jump::HalfPage(-1));
        assert_eq!(card.cursor, from - page / 2);
        assert!(on_screen(&card));

        let page = card.visible_items().len();
        let from = card.cursor;
        card.jump(Jump::Page(-1));
        assert_eq!(card.cursor, from.saturating_sub(page));

        card.jump(Jump::Bottom);
        assert_eq!(card.cursor, last);
        assert_eq!(card.selected(), Some(Item::Connect));
        assert!(on_screen(&card));
        // Clamped at the bottom, where `↓` would have wrapped.
        card.jump(Jump::Page(1));
        assert_eq!(card.cursor, last);
        card.jump(Jump::HalfPage(1));
        assert_eq!(card.cursor, last);

        card.jump(Jump::Top);
        assert_eq!(card.cursor, 0);
        assert_eq!(card.first, 0);
        // …and at the top.
        card.jump(Jump::Page(-1));
        assert_eq!(card.cursor, 0);
        card.jump(Jump::HalfPage(-1));
        assert_eq!(card.cursor, 0);

        // A page from near the bottom stops on the last row, not past it.
        card.jump(Jump::Bottom);
        card.move_cursor(-2);
        card.jump(Jump::Page(1));
        assert_eq!(card.cursor, last);
    }

    /// A short window gives the body as many rows as fit between the
    /// heading and the hint strip, under the three section headings, and the
    /// cursor's row is always among the lines drawn; a tall one gives it all
    /// [`ROWS`]. The bar is there only while there are lines the body does
    /// not show.
    #[test]
    fn a_short_window_scrolls_the_mount_cards_body() {
        let now = std::time::Instant::now();
        let screen = |height: f32| {
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, height))
        };
        let (tall, short) = (screen(900.0), screen(300.0));
        let headings = SECTIONS.len() as f32 * SECTION_ROW;
        assert_eq!(window(tall), WINDOW, "all twenty rows");
        assert!(window(short) < WINDOW);
        assert_eq!(
            (window(short) - headings) % ROW,
            0.0,
            "whole rows under the headings"
        );
        assert_eq!(
            window(screen(60.0)),
            ROW + headings,
            "never less than a row"
        );

        let mut card = Card::with_places(places(3));
        card.update(many_devices(20), Vec::new(), share_rows(3));
        card.fit(window(tall), now);
        let g = geometry(tall, &card);
        assert!(tall.contains_rect(g.card));
        assert!(
            bar(&g, &card).is_some(),
            "twenty disks and more past the window"
        );
        assert!(g.band.is_some());

        card.fit(window(short), now);
        let g = geometry(short, &card);
        assert!(short.contains_rect(g.card), "{:?}", g.card);
        assert!(!g.rows.is_empty());
        assert!(g
            .lines
            .iter()
            .all(|(_, rect)| rect.bottom() <= g.body.bottom() + 1e-3));
        assert!(bar(&g, &card).is_some());
        for _ in 0..30 {
            card.move_cursor(1);
            let g = geometry(short, &card);
            let item = card.selected().expect("a row");
            assert!(
                g.lines.iter().any(|(line, _)| *line == Line::Item(item)),
                "the cursor's row is off the card"
            );
        }

        // A window shorter than the heading and the hint strip: the body has
        // no height rather than a negative one, and nothing on it — no row,
        // no bar, no band — for a pointer to find.
        let tiny = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(60.0, 60.0));
        card.fit(window(tiny), now);
        let g = geometry(tiny, &card);
        assert!(g.body.height() >= 0.0, "{:?}", g.body);
        assert_eq!(g.band, None);
        assert_eq!(bar(&g, &card), None);
        for (_, rect) in &g.lines {
            assert_eq!(g.row_at(rect.center()), None);
        }

        // A short list in a tall window has nothing to scroll.
        let mut few = Card::new();
        few.update(Vec::new(), Vec::new(), Vec::new());
        few.fit(window(tall), now);
        let g = geometry(tall, &few);
        assert_eq!(bar(&g, &few), None);
        assert_eq!(g.band, None);
    }

    /// The thumb is one length wherever the view starts, whatever mix of
    /// headings, empty sections and rows is in it; at the last view its
    /// bottom is the track's; and pulled all the way down it lands on the
    /// last line a view can start at — in windows of every height, some of
    /// which leave the last view's lines well short of the window's foot.
    #[test]
    fn the_mount_cards_thumb_is_one_length_all_the_way_down() {
        let now = std::time::Instant::now();
        for height in (300..=560).step_by(5) {
            let area =
                egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, height as f32));
            let mut card = Card::with_places(places(3));
            card.update(Vec::new(), Vec::new(), share_rows(24));
            card.fit(window(area), now);
            let heights: Vec<f32> = card.lines().iter().map(|line| line.height()).collect();
            let last = deepest(&heights, card.window);
            assert!(last > 0, "the card does not scroll in {area:?}");
            let mut lengths = Vec::new();
            for first in 0..=last {
                card.first = first;
                let g = geometry(area, &card);
                let bar = bar(&g, &card).expect("the lines overflow");
                lengths.push(bar.thumb.height());
                if first == last {
                    assert!((bar.thumb.bottom() - bar.track.bottom()).abs() < 1e-3);
                }
            }
            assert!(
                lengths
                    .windows(2)
                    .all(|pair| (pair[0] - pair[1]).abs() < 1e-3),
                "the thumb breathes in {height}: {lengths:?}"
            );

            card.first = 0;
            let g = geometry(area, &card);
            let top = bar(&g, &card).expect("the lines overflow");
            let bottom = top.first_at(top.track.bottom() - top.thumb.height());
            assert!(card.scroll_to(bottom, now));
            assert_eq!(
                card.first, last,
                "pulled down in {height}, the thumb stopped short"
            );
        }
    }

    /// A listing that lands while the card is scrolled moves the view to
    /// wherever the cursor's row went, and that is the list rebuilt under the
    /// card, not a scroll: the bar has nothing to linger for.
    #[test]
    fn a_refresh_under_the_card_is_not_a_scroll() {
        let t0 = std::time::Instant::now();
        let short = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, 300.0));
        let mut card = Card::with_places(places(3));
        card.update(many_devices(20), Vec::new(), share_rows(3));
        card.fit(window(short), t0);
        assert_eq!(card.scrolled_at(), None, "opening is not a scroll");

        let t1 = t0 + std::time::Duration::from_millis(16);
        card.jump(Jump::Bottom);
        card.fit(window(short), t1);
        assert_eq!(card.selected(), Some(Item::Place(2)));
        assert_eq!(card.scrolled_at(), Some(t1), "the End key scrolled it");

        // Fewer disks, and the last place, which the cursor stays on, further
        // up the list: the view follows it.
        let before = card.first;
        card.update(many_devices(8), Vec::new(), share_rows(3));
        let t2 = t1 + std::time::Duration::from_secs(5);
        card.fit(window(short), t2);
        assert_ne!(card.first, before, "the view moved with the refresh");
        assert_eq!(card.scrolled_at(), None, "and nobody scrolled it");
    }

    fn clouds() -> Vec<Cloud> {
        vec![
            Cloud {
                name: "r2".into(),
                provider: "s3".into(),
            },
            Cloud {
                name: "gdrive".into(),
                provider: "rclone".into(),
            },
        ]
    }

    /// The rclone services sit under gvfs's shares and over the connect row,
    /// go to `rclone://<name>`, wear the remote glyph with their service after
    /// the name, and take the mount verbs off the hint strip while the cursor
    /// is on one.
    #[test]
    fn cloud_remotes_follow_the_shares_in_the_network_section() {
        let mut card = Card::with_places(places(1));
        card.set_clouds(clouds());
        // While udisks2 is still being asked, the cursor waits where the
        // first device will be — on the first cloud row, for now…
        assert_eq!(card.selected(), Some(Item::Cloud(0)));
        card.update(many_devices(1), Vec::new(), share_rows(1));
        // …and the disk arrives over it.
        assert_eq!(card.selected(), Some(Item::Disk(0)));
        let network = |card: &Card| -> Vec<Line> {
            card.lines()
                .into_iter()
                .skip_while(|line| *line != Line::Section("Network"))
                .take_while(|line| *line != Line::Section("Places"))
                .collect()
        };
        assert_eq!(
            network(&card),
            [
                Line::Section("Network"),
                Line::Item(Item::Share(0)),
                Line::Item(Item::Cloud(0)),
                Line::Item(Item::Cloud(1)),
                Line::Item(Item::Connect),
            ]
        );
        // With no shares gvfs's sentence stays, and the remotes follow it.
        card.update(many_devices(1), Vec::new(), Vec::new());
        assert_eq!(
            network(&card),
            [
                Line::Section("Network"),
                Line::Empty("nothing mounted"),
                Line::Item(Item::Cloud(0)),
                Line::Item(Item::Cloud(1)),
                Line::Item(Item::Connect),
            ]
        );

        // A refresh keeps the cursor on the remote it was on.
        card.select(Item::Cloud(1));
        card.update(many_devices(2), Vec::new(), share_rows(2));
        let on = card.selected_cloud().expect("still on a remote");
        assert_eq!(on.name, "gdrive");
        assert_eq!(on.target(), PathBuf::from("rclone://gdrive"));

        let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, 900.0));
        let g = geometry(area, &card);
        assert!(g.cloud, "the strip drops m, u and e here");
        assert!(!g.unpin);
        card.select(Item::Connect);
        assert!(!geometry(area, &card).cloud);

        let palette = palette();
        let row = face(&card, Item::Cloud(0), &palette, true);
        assert_eq!(row.name, "r2");
        assert_eq!(row.detail, "S3", "the service, by the name people know");
        assert_eq!(row.tone, palette.quiet, "nothing is mounted, so no state");
        assert_eq!(
            row.icon.map(|icon| icon.glyph),
            Some(crate::icons::network(&palette, true).glyph),
            "the glyph a remote place wears"
        );
    }
}
