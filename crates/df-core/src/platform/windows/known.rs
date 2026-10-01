//! Where Windows says a user's folders are: the known folders (Downloads,
//! Desktop, Documents, Pictures, Videos, Music) from the shell, for the
//! bookmarks a fresh install ships (`defaults`, W4.40).
//!
//! **A known folder is asked of the shell, not built from the profile.** A
//! person can move Documents to another drive, and OneDrive's backup takes
//! Desktop, Documents and Pictures into `OneDrive\…`; `SHGetKnownFolderPath`
//! answers where each is now. Only when it does not answer is the folder
//! looked for where Windows makes it, under `%USERPROFILE%`.
//!
//! The FFI's ownership rules: the path `SHGetKnownFolderPath` hands back is
//! the shell's allocation, read to its NUL and freed with `CoTaskMemFree`
//! whether or not the call succeeded, as its documentation asks.
#![allow(unsafe_code)] // SHGetKnownFolderPath, under the rule above

use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;

use windows_sys::core::{GUID, PWSTR};
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::UI::Shell::{
    FOLDERID_Desktop, FOLDERID_Documents, FOLDERID_Downloads, FOLDERID_Music, FOLDERID_Pictures,
    FOLDERID_Videos, SHGetKnownFolderPath, KF_FLAG_DEFAULT,
};

/// A folder the shell knows by an id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Known {
    Downloads,
    Desktop,
    Documents,
    Pictures,
    Videos,
    Music,
}

impl Known {
    /// What Explorer calls it, which is also its folder's name under the
    /// profile where Windows makes it.
    pub fn name(self) -> &'static str {
        match self {
            Known::Downloads => "Downloads",
            Known::Desktop => "Desktop",
            Known::Documents => "Documents",
            Known::Pictures => "Pictures",
            Known::Videos => "Videos",
            Known::Music => "Music",
        }
    }

    fn id(self) -> GUID {
        match self {
            Known::Downloads => FOLDERID_Downloads,
            Known::Desktop => FOLDERID_Desktop,
            Known::Documents => FOLDERID_Documents,
            Known::Pictures => FOLDERID_Pictures,
            Known::Videos => FOLDERID_Videos,
            Known::Music => FOLDERID_Music,
        }
    }
}

/// Where `which` is: the shell's answer, else its folder under the profile.
pub fn folder(which: Known) -> Option<PathBuf> {
    from_shell(which).or_else(|| Some(super::dirs::home()?.join(which.name())))
}

/// The shell's answer alone, `None` when it gives none.
pub fn from_shell(which: Known) -> Option<PathBuf> {
    let id = which.id();
    let mut out: PWSTR = std::ptr::null_mut();
    // SAFETY: `id` outlives the call; a null token is the current user;
    // `out` receives the shell's allocation, or null.
    let result = unsafe { SHGetKnownFolderPath(&id, KF_FLAG_DEFAULT as u32, 0, &mut out) };
    let path = (result >= 0 && !out.is_null()).then(|| {
        // SAFETY: on success `out` is a NUL-terminated string the shell
        // allocated; it is read up to its NUL and not past it.
        let len = (0..).take_while(|&i| unsafe { *out.add(i) } != 0).count();
        // SAFETY: `len` units were just read from `out`.
        let units = unsafe { std::slice::from_raw_parts(out, len) };
        PathBuf::from(OsString::from_wide(units))
    });
    // SAFETY: the shell's allocation (or null, which CoTaskMemFree takes),
    // freed once, success or not, and not used after.
    unsafe { CoTaskMemFree(out as *const _) };
    path.filter(|path| path.is_absolute())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The runner's account has every known folder, and the shell answers
    /// for each with a folder that is there.
    #[test]
    fn the_shell_says_where_each_known_folder_is() {
        for which in [
            Known::Downloads,
            Known::Desktop,
            Known::Documents,
            Known::Pictures,
            Known::Videos,
            Known::Music,
        ] {
            let path = from_shell(which).unwrap_or_else(|| panic!("{which:?}"));
            assert!(path.is_absolute(), "{which:?}: {}", path.display());
            let text = path.to_string_lossy();
            assert!(!text.contains('/'), "{which:?}: {text}");
            if std::env::var_os("CI").is_some() {
                assert!(path.is_dir(), "{which:?}: {text} is not a folder");
            }
            assert_eq!(folder(which), Some(path));
        }
    }
}
