//! The Places card's rows on macOS, from the volumes the system has mounted,
//! and how its connect prompt is answered there.
//!
//! macOS mounts disks itself, as they are plugged in, and mounts a server's
//! share through Finder's own connect flow; what the card can do is list what
//! is mounted, put a volume away, and hand an address to Finder. The AppKit
//! half — asking `NSFileManager` for the volumes, `statfs` for where each came
//! from, `NSWorkspace` to unmount and to connect — is
//! `platform::macos::mounts`. What the answers become is decided here, as pure
//! functions over plain values, compiled on macOS and in every target's
//! tests, so the rules are checked on the machine they are edited on.

use std::ffi::OsString;
use std::path::PathBuf;

use crate::mounts::{Address, Device, Share};

/// One mounted volume, as the system describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Volume {
    /// Where it is mounted: `/`, `/Volumes/STICK`.
    pub path: PathBuf,
    /// Its name as Finder shows it; empty when it has none.
    pub name: String,
    /// `APFS`, `MS-DOS (FAT32)`, `SMB (SMB2)` — the system's own words.
    pub format: String,
    pub size: u64,
    pub removable: bool,
    pub ejectable: bool,
    /// On a disk of this machine rather than a server.
    pub local: bool,
    /// What it was mounted from (`statfs`'s `f_mntfromname`): a device node
    /// for a disk, `//user@host/share` for a share.
    pub from: String,
    /// The address a share can be mounted again from (`smb://host/share`),
    /// when the system knows one.
    pub remount: Option<String>,
    /// The filesystem's type (`statfs`'s `f_fstypename`): `apfs`, `smbfs`.
    pub fs_type: String,
}

/// The card's disks and shares, in the order the system listed them: a local
/// volume is a disk, anything else a share.
pub fn rows(volumes: Vec<Volume>) -> (Vec<Device>, Vec<Share>) {
    let mut devices = Vec::new();
    let mut shares = Vec::new();
    for volume in volumes {
        if volume.local {
            devices.push(device(volume));
        } else {
            shares.push(share(volume));
        }
    }
    (devices, shares)
}

/// A local volume as a disk row. The mount point is its identity — what an
/// unmount or an eject is asked for — and, when it can be ejected, also the
/// "drive" the eject names, since macOS ejects a volume by where it is.
fn device(volume: Volume) -> Device {
    let object = volume.path.to_string_lossy().into_owned();
    Device {
        drive: volume.ejectable.then(|| object.clone()),
        object,
        node: volume.from,
        label: name_of(&volume.name, &volume.path),
        fs: if volume.format.is_empty() {
            volume.fs_type
        } else {
            volume.format
        },
        size: volume.size,
        mount: Some(volume.path),
        removable: volume.removable,
        ejectable: volume.ejectable,
        hardware: String::new(),
    }
}

/// A server's volume as a share row: its address where the system knows
/// one, and where it was mounted from where it does not.
fn share(volume: Volume) -> Share {
    let url = volume.remount.unwrap_or(volume.from);
    let scheme = match url.split_once("://") {
        Some((scheme, _)) => scheme.to_ascii_lowercase(),
        None => scheme_of_type(&volume.fs_type).to_string(),
    };
    let label = Address::parse(&url)
        .map(|address| address.label())
        .unwrap_or_else(|| name_of(&volume.name, &volume.path));
    Share {
        url,
        label,
        scheme,
        path: volume.path,
    }
}

/// The scheme a network filesystem type is reached by.
fn scheme_of_type(fs_type: &str) -> &str {
    match fs_type {
        "smbfs" => "smb",
        "afpfs" => "afp",
        "webdav" => "dav",
        "nfs" => "nfs",
        "ftp" => "ftp",
        other => other,
    }
}

/// A row has to be called something: the volume's name, or failing that the
/// last part of where it is mounted.
fn name_of(name: &str, path: &std::path::Path) -> String {
    if !name.is_empty() {
        return name.to_string();
    }
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// What the connect prompt's address comes to on macOS.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Route {
    /// Handed to Finder, which asks for any credentials in its own dialog
    /// and mounts the share under `/Volumes`.
    Finder,
    /// Not something Finder mounts from an address: this is why.
    Refused(&'static str),
}

/// Where an address the prompt accepted (`crate::mounts::connect_url`)
/// goes. Finder mounts `smb://`, `nfs://` and `ftp://` addresses handed to
/// it; it has no `sftp://` at all, and it mounts WebDAV only from its own
/// Connect to Server, where the address is an `http(s)://` one that
/// anything else would open in a browser.
pub fn route(url: &str) -> Route {
    let scheme = url
        .split_once("://")
        .map(|(scheme, _)| scheme.to_ascii_lowercase())
        .unwrap_or_default();
    match scheme.as_str() {
        "sftp" => Route::Refused("use the sftp: bookmark instead"),
        "dav" | "davs" => Route::Refused(
            "Finder mounts WebDAV from its own Connect to Server (⌘K), with an https:// address",
        ),
        _ => Route::Finder,
    }
}

/// The entry of `/Volumes` that was not there `before`, if one has appeared.
pub fn appeared(before: &[OsString], now: &[OsString]) -> Option<OsString> {
    now.iter().find(|name| !before.contains(name)).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn volume(path: &str) -> Volume {
        Volume {
            path: PathBuf::from(path),
            name: String::new(),
            format: String::new(),
            size: 0,
            removable: false,
            ejectable: false,
            local: true,
            from: String::new(),
            remount: None,
            fs_type: String::new(),
        }
    }

    /// The boot volume, a USB stick and an SMB share, as the system lists
    /// them: two disks in order and a share, each with what its row needs.
    #[test]
    fn local_volumes_are_disks_and_the_rest_are_shares() {
        let boot = Volume {
            name: "Macintosh HD".into(),
            format: "APFS".into(),
            size: 494_384_795_648,
            from: "/dev/disk3s1s1".into(),
            fs_type: "apfs".into(),
            ..volume("/")
        };
        let stick = Volume {
            name: "STICK".into(),
            format: "MS-DOS (FAT32)".into(),
            size: 16_000_000_000,
            removable: true,
            ejectable: true,
            from: "/dev/disk4s1".into(),
            fs_type: "msdos".into(),
            ..volume("/Volumes/STICK")
        };
        let nas = Volume {
            name: "media".into(),
            format: "SMB (SMB3)".into(),
            local: false,
            ejectable: true,
            from: "//brian@nas._smb._tcp.local/media".into(),
            remount: Some("smb://brian@nas._smb._tcp.local/media".into()),
            fs_type: "smbfs".into(),
            ..volume("/Volumes/media")
        };
        let (devices, shares) = rows(vec![boot, stick, nas]);

        assert_eq!(devices.len(), 2);
        let root = &devices[0];
        assert_eq!(root.object, "/");
        assert_eq!(root.label, "Macintosh HD");
        assert_eq!(root.fs, "APFS");
        assert_eq!(root.node, "/dev/disk3s1s1");
        assert_eq!(root.mount.as_deref(), Some(std::path::Path::new("/")));
        assert_eq!(root.drive, None, "the boot volume is not ejected");
        let usb = &devices[1];
        assert_eq!(usb.object, "/Volumes/STICK");
        assert_eq!(usb.drive.as_deref(), Some("/Volumes/STICK"));
        assert!(usb.removable && usb.ejectable);
        assert_eq!(usb.detail(), "14.9 GB · MS-DOS (FAT32) · /Volumes/STICK");

        assert_eq!(
            shares,
            vec![Share {
                url: "smb://brian@nas._smb._tcp.local/media".into(),
                label: "nas._smb._tcp.local/media".into(),
                scheme: "smb".into(),
                path: PathBuf::from("/Volumes/media"),
            }]
        );
    }

    /// A share the system knows no address for keeps where it was mounted
    /// from, and takes its scheme from its filesystem's type.
    #[test]
    fn a_share_without_an_address_is_named_by_its_source() {
        let (devices, shares) = rows(vec![Volume {
            name: "exports".into(),
            local: false,
            from: "nas:/exports".into(),
            fs_type: "nfs".into(),
            ..volume("/Volumes/exports")
        }]);
        assert!(devices.is_empty());
        assert_eq!(shares[0].url, "nas:/exports");
        assert_eq!(shares[0].scheme, "nfs");
        assert_eq!(shares[0].label, "exports");
    }

    /// A volume with no name is called by its mount point, and one with no
    /// format by its filesystem's type.
    #[test]
    fn a_row_is_always_called_something() {
        let (devices, _) = rows(vec![Volume {
            fs_type: "hfs".into(),
            ..volume("/Volumes/Untitled 1")
        }]);
        assert_eq!(devices[0].label, "Untitled 1");
        assert_eq!(devices[0].fs, "hfs");
    }

    #[test]
    fn finder_is_handed_what_it_mounts_and_nothing_else() {
        assert_eq!(route("smb://nas/media"), Route::Finder);
        assert_eq!(route("nfs://nas/exports"), Route::Finder);
        assert_eq!(route("ftp://files.example.org/"), Route::Finder);
        assert_eq!(
            route("sftp://me@host/srv"),
            Route::Refused("use the sftp: bookmark instead")
        );
        assert!(matches!(
            route("davs://cloud/remote.php"),
            Route::Refused(_)
        ));
        assert!(matches!(route("dav://cloud/"), Route::Refused(_)));
    }

    #[test]
    fn a_new_entry_under_volumes_is_the_one_that_appeared() {
        let before = vec![OsString::from("Macintosh HD"), OsString::from("STICK")];
        let mut now = before.clone();
        assert_eq!(appeared(&before, &now), None);
        now.push(OsString::from("media"));
        assert_eq!(appeared(&before, &now), Some(OsString::from("media")));
    }
}
