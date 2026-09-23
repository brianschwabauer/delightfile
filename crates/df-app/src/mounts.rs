//! The mount manager — PLAN §7.4's "`M`: udisks2 over hand-rolled D-Bus —
//! list/mount/unmount/eject" — and, under the disks, the network shares gvfs
//! has mounted and the way to connect to another one.
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
//! the task panel like any other long thing.
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
//! or from [`connect`] saying `gio` is missing.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crossbeam_channel::{unbounded, Receiver, Sender};

use crate::dbus::{Bus, Interfaces, Value};

/// The udisks2 names, in one place.
const SERVICE: &str = "org.freedesktop.UDisks2";
const MANAGER_PATH: &str = "/org/freedesktop/UDisks2";
const OBJECT_MANAGER: &str = "org.freedesktop.DBus.ObjectManager";
const BLOCK: &str = "org.freedesktop.UDisks2.Block";
const FILESYSTEM: &str = "org.freedesktop.UDisks2.Filesystem";
const DRIVE: &str = "org.freedesktop.UDisks2.Drive";

/// How many rows the card shows before it scrolls. A machine with more than
/// this many mountable filesystems and shares is a server, and a server is not
/// what `M` is for.
pub const ROWS: usize = 10;

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
    /// "SanDisk Cruzer" — the hardware, for the second line of the row.
    pub hardware: String,
}

impl Device {
    pub fn is_mounted(&self) -> bool {
        self.mount.is_some()
    }

    /// The right-hand text: where it is, or what it is.
    pub fn status(&self) -> String {
        match &self.mount {
            Some(path) => path.to_string_lossy().into_owned(),
            None => "not mounted".to_string(),
        }
    }

    /// The dim second line: size, filesystem, device node, and whether it comes
    /// out.
    pub fn detail(&self) -> String {
        let mut parts = vec![crate::format::human_size(self.size)];
        if !self.fs.is_empty() {
            parts.push(self.fs.clone());
        }
        parts.push(self.node.clone());
        if !self.hardware.is_empty() {
            parts.push(self.hardware.clone());
        }
        if self.removable {
            parts.push("removable".to_string());
        }
        parts.join(" · ")
    }
}

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
    pub fn of(address: &Address) -> Spec {
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
        .filter_map(|line| line.strip_prefix("Mount("))
        .filter_map(|line| {
            let (_, rest) = line.split_once("): ")?;
            // From the right: the URL is escaped and has no spaces in it; the
            // name is whatever the backend called the mount.
            let (name, url) = rest.rsplit_once(" -> ")?;
            Some((name.to_string(), url.trim().to_string()))
        })
        .filter(|(_, url)| !url.starts_with("file://"))
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
pub fn shares_from(listing: &str, root: &Path, entries: &[String]) -> Vec<Share> {
    let present: Vec<(&String, Spec)> = entries
        .iter()
        .filter_map(|name| Some((name, Spec::from_dir_name(name)?)))
        .collect();
    let mut shares: Vec<Share> = gio_mounts(listing)
        .into_iter()
        .filter_map(|(name, url)| {
            let address = Address::parse(&url)?;
            let spec = Spec::of(&address);
            let dir = present
                .iter()
                .find(|(_, there)| spec.same_server(there) && spec.prefix == there.prefix)
                .map(|(dir, _)| (*dir).clone())
                .unwrap_or_else(|| spec.dir_name());
            // A backend that is not a server (an archive, a phone) is called
            // what gio calls it; a URL-shaped label for it would be noise.
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

/// Where gvfs-fuse shows its mounts: `/run/user/<uid>/gvfs`.
pub fn gvfs_root() -> PathBuf {
    PathBuf::from(format!("/run/user/{}/gvfs", df_core::ops::trash::uid()))
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

/// `gio mount -l`, started and not yet waited for.
///
/// In two halves because gio spends half a second of every listing waiting for
/// gvfs's volume monitors to report in, whatever there is to report. So the
/// worker starts it, asks udisks2 its question while gio waits, and collects
/// both: one round trip for the card, with the quicker half hidden inside the
/// slower one.
struct ShareListing(Option<std::process::Child>);

impl ShareListing {
    fn start() -> ShareListing {
        let child = Command::new("gio")
            .args(["mount", "-l"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn();
        ShareListing(match child {
            Ok(child) => Some(child),
            Err(e) => {
                log::debug!("gio mount -l did not start: {e}");
                None
            }
        })
    }

    /// No gio, or a gio that failed, is no shares: a machine without gvfs has
    /// nothing mounted through it.
    fn finish(self) -> Vec<Share> {
        let Some(child) = self.0 else {
            return Vec::new();
        };
        let Ok(output) = child.wait_with_output() else {
            return Vec::new();
        };
        let root = gvfs_root();
        shares_from(
            &String::from_utf8_lossy(&output.stdout),
            &root,
            &gvfs_entries(&root),
        )
    }
}

/// Every share gvfs has mounted, now. Blocks for as long as gio does.
pub fn list_shares() -> Vec<Share> {
    ShareListing::start().finish()
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

/// `u` on a share: `gio mount -u`, which is what a desktop's own eject button
/// beside a share does.
fn unmount_share(url: &str) -> Reply {
    let output = Command::new("gio")
        .args(["mount", "-u", url])
        .stdin(Stdio::null())
        .output();
    match output {
        Ok(output) if output.status.success() => Reply::Unmounted,
        Ok(output) => Reply::Failed(
            first_line(&String::from_utf8_lossy(&output.stderr))
                .unwrap_or("gio could not unmount it")
                .to_string(),
        ),
        Err(e) => Reply::Failed(gio_error(&e)),
    }
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
pub fn connect(url: &str) -> Connected {
    let output = match Command::new("gio")
        .args(["mount", url])
        .stdin(Stdio::null())
        .output()
    {
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
#[derive(Debug, Clone)]
pub enum Request {
    /// Both halves of the card: udisks2's disks and gvfs's shares.
    List,
    Mount(String),
    Unmount(String),
    Eject(String),
    /// A share's URL, for `gio mount -u`.
    UnmountShare(String),
}

/// What comes back.
#[derive(Debug, Clone)]
pub enum Reply {
    /// The card's contents, both sections from one request — so the card never
    /// shows the disks of one moment beside the shares of another.
    Listing {
        devices: Vec<Device>,
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
    replies: Receiver<Reply>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Mounts {
    /// Start the worker. `notify` is rung once per reply.
    pub fn start(notify: df_core::fs::Notifier) -> Mounts {
        let (tx, rx) = unbounded::<Request>();
        let (reply_tx, reply_rx) = unbounded::<Reply>();
        let handle = std::thread::Builder::new()
            .name("df-mounts".to_string())
            .spawn(move || run(rx, reply_tx, notify));
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
        }
    }

    pub fn ask(&self, request: Request) {
        if let Some(requests) = &self.requests {
            let _ = requests.send(request);
        }
    }

    pub fn drain(&self) -> Vec<Reply> {
        self.replies.try_iter().collect()
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
fn run(requests: Receiver<Request>, replies: Sender<Reply>, notify: df_core::fs::Notifier) {
    let mut bus: Option<Bus> = None;
    for request in requests {
        let reply = match &request {
            // gio, not the system bus: a share is put away whether or not
            // udisks2 is there to be asked.
            Request::UnmountShare(url) => unmount_share(url),
            _ => udisks(&mut bus, &request),
        };
        let _ = replies.send(reply);
        notify();
    }
}

/// One request that needs udisks2, over the kept connection.
fn udisks(bus: &mut Option<Bus>, request: &Request) -> Reply {
    if bus.is_none() {
        match Bus::connect() {
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
            // gio first, and collected last: see [`ShareListing`]. Collected
            // whatever udisks2 said, so a failed call does not leave the child
            // behind unreaped.
            let shares = ShareListing::start();
            let devices = list_devices(bus);
            let shares = shares.finish();
            Ok(Reply::Listing {
                devices: devices?,
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
        Request::Eject(drive) => {
            let mut args = Vec::new();
            crate::dbus::marshal_no_options(&mut args);
            bus.call(SERVICE, drive, DRIVE, "Eject", Some("a{sv}"), &args)?;
            Ok(Reply::Ejected)
        }
        Request::UnmountShare(url) => Ok(unmount_share(url)),
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

// ── The card's state ────────────────────────────────────────────────────────

/// Something the cursor can be on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    /// An index into [`Card::devices`].
    Disk(usize),
    /// An index into [`Card::shares`].
    Share(usize),
    /// The Network section's last row, always there.
    Connect,
}

/// One line of the card, from top to bottom.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Line {
    /// A section's name: `Disks`, `Network`.
    Section(&'static str),
    /// A section with nothing in it, and why.
    Empty(&'static str),
    Item(Item),
}

impl Line {
    fn height(self) -> f32 {
        match self {
            Line::Section(_) => SECTION_ROW,
            Line::Empty(_) => EMPTY_ROW,
            Line::Item(_) => ROW,
        }
    }
}

/// The card's own state, while it is open.
pub struct Card {
    pub devices: Vec<Device>,
    pub shares: Vec<Share>,
    /// Which of [`Card::items`] the cursor is on.
    pub cursor: usize,
    /// The first of [`Card::lines`] drawn.
    pub first: usize,
    /// A call is in flight, so the card is drawn busy and takes no new one.
    /// One at a time: two mounts of the same device is one of them failing with
    /// `AlreadyMounted`, and a card that let you start it is a card that
    /// produced an error you caused by being allowed to. Holds the busy row's
    /// identity: a block object's path, or a share's URL.
    pub busy: Option<String>,
    /// Nothing has come back yet.
    pub loading: bool,
}

impl Card {
    pub fn new() -> Card {
        Card {
            devices: Vec::new(),
            shares: Vec::new(),
            cursor: 0,
            first: 0,
            busy: None,
            loading: true,
        }
    }

    /// Everything the cursor can land on, in order: the disks, the shares, and
    /// the connect row. Never empty — the connect row is always there.
    pub fn items(&self) -> Vec<Item> {
        (0..self.devices.len())
            .map(Item::Disk)
            .chain((0..self.shares.len()).map(Item::Share))
            .chain(std::iter::once(Item::Connect))
            .collect()
    }

    /// Every line, headings and empty states included.
    pub fn lines(&self) -> Vec<Line> {
        let mut lines = vec![Line::Section("Disks")];
        match self.disks_empty() {
            Some(message) => lines.push(Line::Empty(message)),
            None => lines.extend((0..self.devices.len()).map(|i| Line::Item(Item::Disk(i)))),
        }
        lines.push(Line::Section("Network"));
        match self.shares_empty() {
            Some(message) => lines.push(Line::Empty(message)),
            None => lines.extend((0..self.shares.len()).map(|i| Line::Item(Item::Share(i)))),
        }
        lines.push(Line::Item(Item::Connect));
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

    pub fn selected_share(&self) -> Option<&Share> {
        match self.selected()? {
            Item::Share(i) => self.shares.get(i),
            _ => None,
        }
    }

    /// `↑`/`↓`, clamping. A list of disks has a top and a bottom, and running
    /// off the end of it would be the cursor landing on a different disk from
    /// the one the eye is on.
    pub fn move_cursor(&mut self, delta: isize) {
        let last = self.items().len().saturating_sub(1);
        self.cursor = (self.cursor as isize + delta).clamp(0, last as isize) as usize;
        self.follow();
    }

    /// Put the cursor on `item`: a click.
    pub fn select(&mut self, item: Item) {
        if let Some(index) = self.items().iter().position(|i| *i == item) {
            self.cursor = index;
            self.follow();
        }
    }

    /// Scroll so the cursor's row is in view.
    fn follow(&mut self) {
        let lines = self.lines();
        let Some(item) = self.selected() else { return };
        if let Some(at) = lines.iter().position(|line| *line == Line::Item(item)) {
            self.first = scroll(self.first, at, &lines);
        }
    }

    /// The lines drawn from [`Card::first`], each with its top measured from
    /// the top of the body, stopping at the bottom of the window.
    pub fn visible(&self) -> Vec<(Line, f32)> {
        let mut top = 0.0;
        let mut out = Vec::new();
        for line in self.lines().into_iter().skip(self.first) {
            if top >= WINDOW {
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
            .min(WINDOW)
    }

    /// What a row is across a refresh: the block object, the share's URL, or
    /// the connect row.
    fn identity(&self, item: Item) -> Option<String> {
        match item {
            Item::Disk(i) => self.devices.get(i).map(|d| format!("disk {}", d.object)),
            Item::Share(i) => self.shares.get(i).map(|s| format!("share {}", s.url)),
            Item::Connect => Some("connect".to_string()),
        }
    }

    /// Take a new listing without losing the user's place.
    ///
    /// Keyed on each row's identity, so a refresh that arrives after a mount
    /// leaves the cursor on the disk that was just mounted rather than on
    /// whatever now sorts into that row (`delightful-ui` §8). The *first*
    /// listing is the exception: before it the only row is the connect row,
    /// and the cursor staying there would be the card opening on its last line
    /// instead of on the first disk.
    pub fn update(&mut self, devices: Vec<Device>, shares: Vec<Share>) {
        let on = self.selected().and_then(|item| self.identity(item));
        let first_listing = self.loading;
        self.devices = devices;
        self.shares = shares;
        self.loading = false;
        let items = self.items();
        let kept = on.filter(|_| !first_listing).and_then(|identity| {
            items
                .iter()
                .position(|item| self.identity(*item).as_deref() == Some(identity.as_str()))
        });
        self.cursor = match kept {
            Some(at) => at,
            None if first_listing => 0,
            None => self.cursor,
        }
        .min(items.len().saturating_sub(1));
        self.follow();
    }

    /// What the disks section says when it has no rows, or `None` when it
    /// has some.
    ///
    /// Two different nothings, and they must not look alike
    /// (`delightful-ui` §11): still asking, and nothing to manage.
    pub fn disks_empty(&self) -> Option<&'static str> {
        if !self.devices.is_empty() {
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

/// The first line to draw so that line `at` is wholly inside the window.
///
/// [`crate::viewport::first_visible`]'s job for a list whose lines are not all
/// one height: the view moves only when the cursor would leave it, a section's
/// heading comes back into view with the first row under it — a row at the top
/// of the card with no name over it is a row whose section you have to
/// remember — and the list never scrolls past its own end.
fn scroll(first: usize, at: usize, lines: &[Line]) -> usize {
    let heights: Vec<f32> = lines.iter().map(|line| line.height()).collect();
    if heights.iter().sum::<f32>() <= WINDOW {
        return 0;
    }
    let mut first = first.min(at);
    if first == at && at > 0 && matches!(lines[at - 1], Line::Section(_)) {
        first = at - 1;
    }
    while first < at && heights[first..=at].iter().sum::<f32>() > WINDOW {
        first += 1;
    }
    // The furthest the view goes: the last lines exactly filling it.
    let mut deepest = heights.len();
    let mut tail = 0.0;
    while deepest > 0 && tail + heights[deepest - 1] <= WINDOW {
        deepest -= 1;
        tail += heights[deepest];
    }
    first.min(deepest)
}

// ── The card ────────────────────────────────────────────────────────────────

/// One row. Two lines: the name, then the detail under it.
///
/// Tall enough for both of them and the air between. It was 34 — one line of
/// [`FONT`] plus its padding — so the second line was drawn *through* the
/// first: the name and the detail shared a baseline and the row read as one
/// smudge.
const ROW: f32 = 46.0;
/// Where the name's centre sits in the row, and the detail's under it.
const NAME_LINE: f32 = 15.0;
const DETAIL_LINE: f32 = 31.0;
/// A section's heading line: the help sheet's group-title line
/// ([`crate::chrome::HELP_ROW`]) with a few points more above it, so the
/// second section reads as starting rather than as continuing the first.
const SECTION_ROW: f32 = 26.0;
/// A section with nothing in it: one line of text, not a two-line row.
const EMPTY_ROW: f32 = 30.0;
/// The most the rows may take before the card scrolls: [`ROWS`] rows and both
/// headings.
const WINDOW: f32 = ROWS as f32 * ROW + 2.0 * SECTION_ROW;
/// The row's own left/right inset, inside the card's [`PAD`].
const ROW_PAD: f32 = 10.0;
/// The most of a row the right-hand status may take before it is ellipsised.
/// A mount point is a path and paths are long; the name is what the row is
/// *about*, so it keeps the majority.
const STATUS_SHARE: f32 = 0.42;
/// The card's inner padding — `chrome::CARD_PAD`, not a number of its own.
///
/// The plate is [`crate::chrome::card`], whose radius is
/// `CARD_ROW_RADIUS + CARD_PAD`; a row inset by anything else stops being
/// concentric with it (`delightful-ui` §15). This was 14 against a 10-derived
/// radius, so the gap *widened* by 4 px as it turned each corner — the same
/// mistake the basket tray made in the other direction.
const PAD: f32 = crate::chrome::CARD_PAD;
const TITLE: f32 = 20.0;
/// The title row and the air under it. It used to be two title rows' worth,
/// which was the space the keys were repeated in; with them gone to the hint
/// strip the heading is one line, and the rows start under it rather than
/// under a band of nothing.
const HEADING: f32 = TITLE + 8.0;
const MAX_WIDTH: f32 = 560.0;
const FONT: f32 = 13.0;

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

/// Lay the card out, centred and biased above true centre
/// (`delightful-ui` §16).
pub fn geometry(area: egui::Rect, card: &Card) -> Geometry {
    let height = PAD * 2.0 + HEADING + card.body_height() + crate::chrome::HINT_ROW;
    let width = (area.width() - 40.0).clamp(0.0, MAX_WIDTH);
    let height = height.min((area.height() - 40.0).max(0.0));
    let top = area.top() + (area.height() - height).max(0.0) * crate::chrome::OPTICAL_CENTRE;
    let rect = egui::Rect::from_min_size(
        egui::pos2(area.center().x - width / 2.0, top),
        egui::vec2(width, height),
    );
    let body_top = rect.top() + PAD + HEADING;
    let body = egui::Rect::from_min_max(
        egui::pos2(rect.left() + PAD, body_top),
        egui::pos2(
            rect.right() - PAD,
            rect.bottom() - PAD - crate::chrome::HINT_ROW,
        ),
    );
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
    Geometry {
        card: rect,
        body,
        lines,
        rows,
    }
}

/// What one row says.
#[derive(Default)]
struct Face {
    name: String,
    detail: String,
    /// The right-hand text and its colour; `None` for a row with no state.
    status: Option<(String, egui::Color32)>,
}

fn face(card: &Card, item: Item, palette: &crate::theme::Palette) -> Face {
    let busy = |identity: &str| card.busy.as_deref() == Some(identity);
    let working = || ("working…".to_string(), palette.peach);
    match item {
        Item::Disk(i) => {
            let Some(device) = card.devices.get(i) else {
                return Face::default();
            };
            // A mounted device is the palette's own "this is live" colour; an
            // unmounted one is plain text. The busy one is amber and says so, so
            // a polkit prompt behind the window is not read as the card having
            // hung.
            let status = if busy(&device.object) {
                working()
            } else if device.is_mounted() {
                (device.status(), palette.green)
            } else {
                (device.status(), palette.overlay1)
            };
            Face {
                name: device.label.clone(),
                detail: device.detail(),
                status: Some(status),
            }
        }
        Item::Share(i) => {
            let Some(share) = card.shares.get(i) else {
                return Face::default();
            };
            // Every share listed is mounted — gvfs lists nothing else — so it
            // wears the mounted disk's green, and says what kind of server it is
            // where a disk says where it is: the path would be gvfs-fuse's
            // spec-named directory, which is nobody's idea of where a share is.
            let status = if busy(&share.url) {
                working()
            } else {
                (share.scheme.clone(), palette.green)
            };
            Face {
                name: share.label.clone(),
                detail: share.url.clone(),
                status: Some(status),
            }
        }
        Item::Connect => Face {
            name: "Connect to server…".to_string(),
            detail: "smb · sftp · ftp · dav · nfs".to_string(),
            status: None,
        },
    }
}

/// Draw it.
pub fn paint(
    paint: &crate::ui::Painting<'_>,
    area: egui::Rect,
    card: &Card,
    geometry: &Geometry,
    hovers: &crate::hover::Hovers<crate::ui::Control>,
    ripples: &crate::ripple::Ripples<crate::ui::Control>,
) {
    let palette = paint.palette;
    let painter = paint.painter;
    painter.rect_filled(
        area,
        0,
        egui::Color32::from_black_alpha(crate::chrome::HELP_SCRIM),
    );
    crate::chrome::card(paint, geometry.card, 1.0);

    let left = geometry.card.left() + PAD;
    // "Mounts" over the two sections, the way the help sheet's "Keys" sits
    // over its groups: the card is about both, and "Disks" is now the name of
    // its first half.
    painter.text(
        egui::pos2(left, geometry.card.top() + PAD + TITLE / 2.0),
        egui::Align2::LEFT_CENTER,
        "Mounts",
        egui::FontId::proportional(FONT + 2.0),
        palette.text,
    );
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
                    palette.blue,
                );
            }
            Line::Empty(message) => {
                clipped.text(
                    egui::pos2(rect.left() + ROW_PAD, rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    message,
                    egui::FontId::proportional(FONT),
                    palette.overlay0,
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
                let on_cursor = selected == Some(item);
                if on_cursor || hover > 0.0 {
                    clipped.rect_filled(
                        rect,
                        crate::ui::ROW_RADIUS,
                        crate::theme::mix(
                            palette.crust,
                            palette.surface1,
                            if on_cursor { 1.0 } else { hover * 0.6 },
                        ),
                    );
                }
                for splash in ripples.splashes(key, paint.now) {
                    clipped.circle_filled(
                        splash.center,
                        splash.radius,
                        egui::Color32::from_white_alpha((splash.alpha * 255.0).round() as u8),
                    );
                }
                paint_face(&clipped, rect, &face(card, item, palette), palette);
            }
        }
    }
    let items = card.items();
    let shown = card
        .visible_items()
        .last()
        .and_then(|last| items.iter().position(|item| item == last))
        .map_or(0, |at| at + 1);
    let more = items.len().saturating_sub(shown);
    if more > 0 {
        painter.text(
            egui::pos2(geometry.body.right(), geometry.body.bottom()),
            egui::Align2::RIGHT_BOTTOM,
            format!("+{more} more"),
            egui::FontId::proportional(FONT - 1.0),
            palette.overlay0,
        );
    }
}

/// One row's two lines and its status.
fn paint_face(
    painter: &egui::Painter,
    rect: egui::Rect,
    face: &Face,
    palette: &crate::theme::Palette,
) {
    // The status first, because it is right-aligned and the two lines beside
    // it are given whatever it leaves.
    let status_width = face.status.as_ref().map_or(0.0, |(status, colour)| {
        right_aligned(
            painter,
            egui::pos2(rect.right() - ROW_PAD, rect.center().y),
            status,
            *colour,
            (rect.width() * STATUS_SHARE - ROW_PAD).max(0.0),
            egui::FontId::proportional(FONT - 1.0),
        )
    });
    let left = rect.left() + ROW_PAD;
    let text_width = (rect.right() - ROW_PAD - status_width - crate::ui::GAP - left).max(0.0);
    crate::chrome::truncated_in(
        painter,
        egui::pos2(left, rect.top() + NAME_LINE),
        &face.name,
        palette.text,
        text_width,
        egui::FontId::proportional(FONT),
    );
    // The detail line: dimmer *and* smaller, so it reads as a caption under
    // the name rather than as a second name (`ui-anti-slop`: hierarchy by
    // size, not by decoration).
    crate::chrome::truncated_in(
        painter,
        egui::pos2(left, rect.top() + DETAIL_LINE),
        &face.detail,
        palette.subtext0,
        text_width,
        egui::FontId::proportional(FONT - 1.5),
    );
}

/// One ellipsised line, right-aligned on `right` and centred on its `y`,
/// returning how wide it ended up.
///
/// [`crate::chrome::truncated`]'s twin. A left-aligned line can be drawn where
/// it is told; a right-aligned one has to be laid out before it knows where it
/// starts, and the caller wants that width anyway — it is what is left for the
/// columns beside it.
fn right_aligned(
    painter: &egui::Painter,
    right: egui::Pos2,
    text: &str,
    color: egui::Color32,
    max_width: f32,
    font: egui::FontId,
) -> f32 {
    use egui::text::{LayoutJob, TextFormat, TextWrapping};
    let mut job = LayoutJob::single_section(
        text.to_string(),
        TextFormat {
            font_id: font,
            color,
            ..Default::default()
        },
    );
    job.wrap = TextWrapping {
        max_width,
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    let galley = painter.layout_job(job);
    let size = galley.size();
    painter.galley(
        egui::pos2(right.x - size.x, right.y - size.y / 2.0),
        galley,
        color,
    );
    size.x
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Same rule as the basket tray, which this card got wrong in the other
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
        assert_eq!(usb.status(), "not mounted");
        assert!(usb.detail().contains("vfat"));
        assert!(usb.detail().contains("/dev/sdb1"));
        assert!(usb.detail().contains("removable"));
        assert_eq!(usb.drive.as_deref(), Some("/drives/usb"));

        // No label: the device node's last component, which is what `lsblk`
        // would have shown.
        let root = &devices[1];
        assert_eq!(root.label, "nvme0n1p2");
        assert!(root.is_mounted());
        assert_eq!(root.status(), "/");
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

    /// The card lays out and paints in every state without panicking.
    #[test]
    fn the_card_paints_in_every_state() {
        let theme = df_core::config::Theme::default();
        let palette = crate::theme::Palette::from_theme(&theme);
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(Default::default(), |ui| {
            let painting = crate::ui::Painting {
                tips: None,
                painter: ui.painter(),
                palette: &palette,
                theme: &theme,
                nerd: false,
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
                        area,
                        card,
                        &geometry(area, card),
                        &hovers,
                        &crate::ripple::Ripples::new(),
                    );
                };
                // Still loading, empty, populated, scrolled, and busy.
                let mut card = Card::new();
                draw(&card);
                card.update(Vec::new(), Vec::new());
                draw(&card);
                card.update(many_devices(20), share_rows(3));
                draw(&card);
                card.move_cursor(21);
                card.busy = Some("sftp://me@host1/".to_string());
                draw(&card);
                card.move_cursor(-3);
                card.busy = Some("/block/16".to_string());
                draw(&card);
            }
        });
    }

    /// The geometry hit-tests to the rows it drew, and only to rows.
    #[test]
    fn the_card_hit_tests_its_rows_and_not_its_headings() {
        let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, 900.0));
        let mut card = Card::new();
        card.update(devices_from(&objects()), share_rows(1));
        let g = geometry(area, &card);
        // Two disks, one share, and the connect row.
        assert_eq!(g.rows.len(), 4);
        assert_eq!(
            card.visible_items(),
            vec![Item::Disk(0), Item::Disk(1), Item::Share(0), Item::Connect]
        );
        for (i, rect) in g.rows.iter().enumerate() {
            assert_eq!(g.row_at(rect.center()), Some(i));
            assert!(g.body.contains_rect(*rect));
        }
        assert!(g.row_at(egui::pos2(0.0, 0.0)).is_none());
        // A heading is drawn, and is not a row.
        let (_, heading) = g.lines[0];
        assert_eq!(g.lines[0].0, Line::Section("Disks"));
        assert!(g.row_at(heading.center()).is_none());

        // An empty card still has its two headings, both empty states, and
        // the connect row.
        card.update(Vec::new(), Vec::new());
        let g = geometry(area, &card);
        assert_eq!(g.rows.len(), 1);
        assert_eq!(
            g.lines.iter().map(|(line, _)| *line).collect::<Vec<_>>(),
            vec![
                Line::Section("Disks"),
                Line::Empty("no removable filesystems"),
                Line::Section("Network"),
                Line::Empty("nothing mounted"),
                Line::Item(Item::Connect),
            ]
        );
    }

    /// A row is two lines, and they do not sit on top of each other.
    ///
    /// They did: the name was drawn from the row's top and the detail centred
    /// on its bottom edge, in a row one line tall, so the two overlapped by
    /// most of their height and the card read as a column of smudges. The
    /// check is the geometry rather than the pixels — the two baselines are a
    /// line apart, and the row is tall enough to hold both with air left over.
    #[test]
    fn a_row_has_room_for_both_of_its_lines() {
        // A line of text is about its point size plus its leading; the two
        // faces here are FONT and FONT - 1.5.
        let name = FONT * 1.3;
        let detail = (FONT - 1.5) * 1.3;
        assert!(
            DETAIL_LINE - NAME_LINE >= (name + detail) / 2.0,
            "the two lines overlap: {NAME_LINE} then {DETAIL_LINE}"
        );
        assert!(
            NAME_LINE - name / 2.0 > 0.0,
            "the name is cut off at the top"
        );
        assert!(
            ROW - (DETAIL_LINE + detail / 2.0) > 0.0,
            "the detail is cut off at the bottom"
        );
    }

    /// …and the card is tall enough for the lines it draws, the heading over
    /// them and the hint strip under them — the strip the keys now live in
    /// alone, having been in the title row as well.
    #[test]
    fn the_card_is_as_tall_as_what_it_draws() {
        let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, 900.0));
        let mut card = Card::new();
        card.update(devices_from(&objects()), Vec::new());
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

    /// The cursor clamps, and a refresh keeps it on the row it was on.
    #[test]
    fn the_cursor_holds_its_place_across_a_refresh() {
        let mut card = Card::new();
        assert_eq!(card.disks_empty(), Some("asking udisks2…"));
        assert_eq!(card.shares_empty(), Some("asking gvfs…"));
        assert_eq!(card.selected(), Some(Item::Connect), "the only row so far");
        card.update(devices_from(&objects()), share_rows(1));
        assert_eq!(card.disks_empty(), None);
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
        // Clamped, not wrapped: a list of disks has a bottom.
        card.move_cursor(9);
        assert_eq!(card.selected(), Some(Item::Connect));
        card.move_cursor(-9);
        assert_eq!(card.cursor, 0);

        // The USB stick is now mounted and sorts the same way; the cursor is on
        // it before and after.
        card.move_cursor(1);
        let on = card.selected_device().map(|d| d.object.clone());
        let mut again = devices_from(&objects());
        again.reverse();
        card.update(again, share_rows(1));
        assert_eq!(card.selected_device().map(|d| d.object.clone()), on);

        // A share keeps the cursor across a refresh that adds a disk above it.
        card.select(Item::Share(0));
        card.update(many_devices(3), share_rows(1));
        assert_eq!(card.selected(), Some(Item::Share(0)));
        assert_eq!(card.cursor, 3);

        // A listing with nothing leaves the connect row, and does not panic.
        card.update(Vec::new(), Vec::new());
        assert!(card.selected_device().is_none());
        assert_eq!(card.selected(), Some(Item::Connect));
        assert_eq!(card.disks_empty(), Some("no removable filesystems"));
        assert_eq!(card.shares_empty(), Some("nothing mounted"));
    }

    /// A long card scrolls to keep the cursor's row whole, brings a section's
    /// heading back with its first row, and never scrolls past its end.
    #[test]
    fn a_long_card_scrolls_to_the_cursor() {
        let mut card = Card::new();
        card.update(many_devices(12), share_rows(12));
        assert_eq!(card.first, 0);

        // Down to the connect row, the last line: it is wholly inside the
        // window, and the view went no further than that.
        card.move_cursor(100);
        assert_eq!(card.selected(), Some(Item::Connect));
        let (last, top) = *card.visible().last().expect("lines");
        assert_eq!(last, Line::Item(Item::Connect));
        assert!(top + ROW <= WINDOW + 0.01, "the cursor's row is cut off");
        assert!(top + ROW > WINDOW - ROW, "scrolled past the end");
        assert!(card.first > 0);

        // Up, a row at a time, to the first share: the Network heading comes
        // into view over it.
        for _ in 0..30 {
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

        // All the way up: the Disks heading is back at the top.
        card.move_cursor(-100);
        assert_eq!(card.first, 0);
        assert_eq!(card.visible()[0].0, Line::Section("Disks"));

        // A short list never scrolls.
        assert_eq!(
            scroll(3, 1, &[Line::Section("Disks"), Line::Item(Item::Connect)]),
            0
        );
    }
}
