//! The Places card's rows on Windows, from the drives the system has
//! lettered, and where its connect prompt's address goes there
//! (`plans/other-platforms/04-windows.md` W4.18, W4.19).
//!
//! Windows letters a disk as it is plugged in and a share as it is mapped;
//! what the card can do is list the letters, eject a removable one, put a
//! mapped share away, and hand a server's address to Explorer, which asks
//! for the password in its own dialog. The Win32 half — `GetLogicalDrives`,
//! `GetDriveTypeW`, `GetVolumeInformationW`, `GetDiskFreeSpaceExW`,
//! `WNetGetConnectionW`, the eject's `DeviceIoControl`s — is
//! `platform::windows::mounts`. What the answers become is decided here, as
//! pure functions over plain values, compiled on Windows and in every
//! target's tests.

use std::path::PathBuf;

use crate::mounts::{Address, Device, Share};

/// What kind of drive a letter is, as `GetDriveTypeW` says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Fixed,
    Removable,
    CdRom,
    /// A mapped network share.
    Remote,
}

impl Kind {
    /// The kind `GetDriveTypeW` answered, or `None` for one the card does
    /// not list: no root, unknown, a RAM disk.
    pub fn of(drive_type: u32) -> Option<Kind> {
        match drive_type {
            2 => Some(Kind::Removable),
            3 => Some(Kind::Fixed),
            4 => Some(Kind::Remote),
            5 => Some(Kind::CdRom),
            _ => None,
        }
    }
}

/// One lettered drive, as the system describes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Drive {
    /// `C`.
    pub letter: char,
    pub kind: Kind,
    /// The volume's label; empty when it has none.
    pub label: String,
    /// `NTFS`, `FAT32`, `exFAT`, `CDFS`…; empty when the system would not say.
    pub fs: String,
    /// The volume's size in bytes; 0 when unknown.
    pub size: u64,
    /// A mapped share's address, `\\server\share`.
    pub unc: Option<String>,
}

impl Drive {
    /// `C:`, what a drive is asked about by and put away as.
    pub fn name(&self) -> String {
        format!("{}:", self.letter)
    }

    /// `C:\`, its root.
    pub fn root(&self) -> PathBuf {
        PathBuf::from(format!("{}:\\", self.letter))
    }
}

/// The card's disks and shares, in letter order: a mapped share is a share,
/// everything else a disk.
pub fn rows(drives: Vec<Drive>) -> (Vec<Device>, Vec<Share>) {
    let mut devices = Vec::new();
    let mut shares = Vec::new();
    for drive in drives {
        match drive.kind {
            Kind::Remote => shares.push(share(drive)),
            _ => devices.push(device(drive)),
        }
    }
    (devices, shares)
}

/// A local drive as a disk row. Its letter is its identity — what an eject is
/// asked for, and the "drive" an eject names — and its root is where it is
/// mounted, which on Windows every lettered drive is.
fn device(drive: Drive) -> Device {
    let ejectable = matches!(drive.kind, Kind::Removable | Kind::CdRom);
    let name = drive.name();
    let root = drive.root();
    let label = if drive.label.is_empty() {
        match drive.kind {
            Kind::Removable => "USB Drive",
            Kind::CdRom => "CD Drive",
            _ => "Local Disk",
        }
        .to_string()
    } else {
        drive.label
    };
    Device {
        drive: ejectable.then(|| name.clone()),
        object: name.clone(),
        node: root.to_string_lossy().into_owned(),
        label: format!("{label} ({name})"),
        fs: drive.fs,
        size: drive.size,
        mount: Some(root),
        removable: drive.kind == Kind::Removable,
        ejectable,
        hardware: String::new(),
    }
}

/// A mapped share as a share row: its address, what it is lettered as, and
/// its root, which is where `Enter` goes.
fn share(drive: Drive) -> Share {
    let name = drive.name();
    let unc = drive.unc.clone().unwrap_or_else(|| name.clone());
    Share {
        label: format!("{name} → {unc}"),
        url: unc,
        scheme: "smb".to_string(),
        path: drive.root(),
    }
}

/// Where a connect prompt's address goes on Windows: `smb://server/share`
/// (and anything under it) is the UNC path `\\server\share\…`, which
/// Explorer opens and asks the password for; any other scheme is refused in
/// words that say what is taken.
pub fn unc_of(url: &str) -> Result<String, &'static str> {
    let address = Address::parse(url).ok_or("That is not an address")?;
    if address.scheme != "smb" {
        return Err("Windows opens smb:// shares only");
    }
    if address.host.is_empty() {
        return Err("An smb:// address names a server");
    }
    let mut unc = format!(r"\\{}", address.host);
    for segment in address.path.split('/').filter(|s| !s.is_empty()) {
        unc.push('\\');
        unc.push_str(segment);
    }
    Ok(unc)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drive(letter: char, kind: Kind, label: &str) -> Drive {
        Drive {
            letter,
            kind,
            label: label.to_string(),
            fs: "NTFS".to_string(),
            size: 512 << 30,
            unc: None,
        }
    }

    /// Every lettered drive is a row: fixed and removable ones are disks,
    /// mounted at their roots, a removable one and a CD drive ejectable by
    /// their letters, and a mapped share a share.
    #[test]
    fn the_drives_become_the_cards_rows() {
        let mut nas = drive('Z', Kind::Remote, "");
        nas.unc = Some(r"\\nas\media".to_string());
        let (devices, shares) = rows(vec![
            drive('C', Kind::Fixed, ""),
            drive('E', Kind::Removable, "STICK"),
            drive('F', Kind::CdRom, ""),
            nas,
        ]);
        assert_eq!(devices.len(), 3);
        let c = &devices[0];
        assert_eq!(c.label, "Local Disk (C:)");
        assert_eq!(c.object, "C:");
        assert_eq!(c.mount, Some(PathBuf::from(r"C:\")));
        assert!(!c.ejectable && !c.removable && c.drive.is_none());
        let e = &devices[1];
        assert_eq!(e.label, "STICK (E:)");
        assert!(e.ejectable && e.removable);
        assert_eq!(e.drive.as_deref(), Some("E:"));
        assert_eq!(devices[2].label, "CD Drive (F:)");
        assert!(devices[2].ejectable && !devices[2].removable);

        assert_eq!(
            shares,
            [Share {
                url: r"\\nas\media".to_string(),
                label: r"Z: → \\nas\media".to_string(),
                scheme: "smb".to_string(),
                path: PathBuf::from(r"Z:\"),
            }]
        );
    }

    #[test]
    fn the_drive_types_listed_are_the_four_with_a_root() {
        assert_eq!(Kind::of(3), Some(Kind::Fixed));
        assert_eq!(Kind::of(2), Some(Kind::Removable));
        assert_eq!(Kind::of(5), Some(Kind::CdRom));
        assert_eq!(Kind::of(4), Some(Kind::Remote));
        for other in [0, 1, 6] {
            assert_eq!(Kind::of(other), None, "{other}");
        }
    }

    /// `smb://` becomes the UNC path Explorer opens; nothing else is taken.
    #[test]
    fn a_share_address_is_its_unc_path() {
        assert_eq!(unc_of("smb://nas/media"), Ok(r"\\nas\media".to_string()));
        assert_eq!(
            unc_of("smb://me@nas/media/films/"),
            Ok(r"\\nas\media\films".to_string())
        );
        assert_eq!(
            unc_of("sftp://host/srv"),
            Err("Windows opens smb:// shares only")
        );
        assert!(unc_of("nonsense").is_err());
    }
}
