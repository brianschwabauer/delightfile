//! Where a Nerd Font is looked for on Linux: the places one is installed on
//! Arch (`/usr/share/fonts`) and the user's own font directories.

use std::path::PathBuf;

/// Directories scanned for a patched font, in order. Deliberately short — this
/// runs on the cold-start path (PLAN §6), and a full recursive walk of
/// `/usr/share/fonts` is the tens of milliseconds delightviewer's own font
/// loader is careful to keep off it.
const FONT_DIRS: &[&str] = &[
    "/usr/share/fonts/TTF",
    "/usr/share/fonts/truetype",
    "/usr/share/fonts/OTF",
    "/usr/share/fonts/nerd-fonts",
    "/usr/local/share/fonts",
];

/// The directories scanned for a patched face, in order
/// ([`crate::icons::install`]). Each is listed once, not walked.
pub fn dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = FONT_DIRS.iter().map(PathBuf::from).collect();
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(&home).join(".local/share/fonts"));
        dirs.push(PathBuf::from(home).join(".fonts"));
    }
    dirs
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The system's directories first, in their order, then the user's two.
    #[test]
    fn the_system_fonts_come_first_and_then_the_users() {
        let dirs = dirs();
        let system: Vec<PathBuf> = FONT_DIRS.iter().map(PathBuf::from).collect();
        assert_eq!(dirs[..system.len()], system[..]);
        if let Some(home) = std::env::var_os("HOME") {
            let home = PathBuf::from(home);
            assert_eq!(
                dirs[system.len()..],
                [home.join(".local/share/fonts"), home.join(".fonts")]
            );
        }
    }
}
