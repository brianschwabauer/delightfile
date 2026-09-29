//! What `gio` prints, read: the mounts at the margin of `gio mount -li`, the
//! shares and the phones among them with the directories gvfs-fuse shows them
//! as, what a failed `gio mount` wanted, where a connected address lands, and
//! the events `gio mount --monitor` prints.
//!
//! Moved here from `crate::mounts`, whose header says why gvfs is asked
//! through `gio` and how a listing's URL finds its directory through a gvfs
//! mount spec ([`Spec`]): nothing but Linux runs `gio`, so its readers live
//! beside the worker that runs it (`platform::linux::mounts`), and a build
//! for another platform has none of them to leave unused. They are pure, as
//! they were, and their tests came with them.

use std::path::{Path, PathBuf};

use crossbeam_channel::Sender;

use crate::mounts::{decode, Address, Change, Event, Phone, Protocol, Share, Spec, SCHEMES};

impl Spec {
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

impl Protocol {
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

/// The protocol of a URL whose scheme is a phone's or a camera's.
fn device_scheme(url: &str) -> Option<Protocol> {
    Protocol::of_scheme(url.split_once("://")?.0)
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

/// The first line of `text` with anything on it.
pub fn first_line(text: &str) -> Option<&str> {
    text.lines().map(str::trim).find(|line| !line.is_empty())
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

/// The reader thread: gio's stdout, line by line, until it ends. Any reader,
/// so a test can hand it gio's words without a gio.
pub fn listen(stdout: impl std::io::Read, events: Sender<Event>, notify: df_core::fs::Notifier) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mounts::tests::{PIXEL, PIXEL_DIR};
    use crossbeam_channel::unbounded;

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
        // The same root with its brackets escaped: one pair, not two.
        assert_eq!(
            spec("gphoto2://%5Busb%3A001%2C004%5D/").dir_name(),
            "gphoto2:host=%5Busb%3A001%2C004%5D"
        );
        assert_eq!(
            spec("mtp://%5Busb%3A003%2C012%5D/").dir_name(),
            "mtp:host=%5Busb%3A003%2C012%5D"
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
}
