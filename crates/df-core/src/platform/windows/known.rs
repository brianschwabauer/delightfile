//! Where Windows says a user's folders and installed programs are: the
//! known folders (Downloads, Desktop, Documents, Pictures, Videos, Music)
//! from the shell, and a program's full path from `App Paths`, for the
//! tables a fresh install ships (`defaults`, W4.40 and W4.41).
//!
//! **A known folder is asked of the shell, not built from the profile.** A
//! person can move Documents to another drive, and OneDrive's backup takes
//! Desktop, Documents and Pictures into `OneDrive\…`; `SHGetKnownFolderPath`
//! answers where each is now. Only when it does not answer is the folder
//! looked for where Windows makes it, under `%USERPROFILE%`.
//!
//! **`App Paths` is how the shell finds a program that is not on `PATH`.**
//! Notepad++, VLC and 7-Zip install without touching `PATH` and register
//! their executable under
//! `Software\Microsoft\Windows\CurrentVersion\App Paths\<name>.exe` instead,
//! which is how Run (Win+R) and `start` find them; the current user's key is
//! read before the machine's, as the shell reads them.
//!
//! The FFI's ownership rules: the path `SHGetKnownFolderPath` hands back is
//! the shell's allocation, read to its NUL and freed with `CoTaskMemFree`
//! whether or not the call succeeded, as its documentation asks; the
//! registry is read into a buffer this module owns, sized by a first call
//! and bounded; every string handed in is NUL-terminated and outlives its
//! call.
#![allow(unsafe_code)] // SHGetKnownFolderPath and RegGetValueW, under the rules above

use std::ffi::OsString;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;

use windows_sys::core::{GUID, PWSTR};
use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Com::CoTaskMemFree;
use windows_sys::Win32::System::Registry::{
    RegGetValueW, HKEY, HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ,
};
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

/// Where `App Paths` says `program` (`notepad++`, `vlc`, `7zFM`) is, when
/// that is a file: the current user's registration, then the machine's.
pub fn app_path(program: &str) -> Option<PathBuf> {
    let key = format!(r"Software\Microsoft\Windows\CurrentVersion\App Paths\{program}.exe");
    [HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE]
        .into_iter()
        .filter_map(|root| default_value(root, &key))
        .map(|text| PathBuf::from(text.trim().trim_matches('"')))
        .find(|path| path.is_file())
}

/// The longest value read: a path, and a long one, in UTF-16 bytes.
const MOST: u32 = 64 * 1024;

/// The default value of `root\key` as text, its environment strings
/// expanded; `None` when there is no such key or it is not text.
fn default_value(root: HKEY, key: &str) -> Option<String> {
    let key: Vec<u16> = std::ffi::OsStr::new(key)
        .encode_wide()
        .chain(Some(0))
        .collect();
    let mut bytes: u32 = 0;
    // SAFETY: `key` is NUL-terminated and outlives the call; a null value
    // name is the default value; a null buffer asks only for the size.
    let sized = unsafe {
        RegGetValueW(
            root,
            key.as_ptr(),
            std::ptr::null(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut bytes,
        )
    };
    if sized != ERROR_SUCCESS || bytes == 0 || bytes > MOST {
        return None;
    }
    let mut buffer = vec![0u16; (bytes as usize).div_ceil(2)];
    // SAFETY: the buffer holds `bytes` bytes, which the call is told; it
    // writes at most that many and says how many it wrote.
    let read = unsafe {
        RegGetValueW(
            root,
            key.as_ptr(),
            std::ptr::null(),
            RRF_RT_REG_SZ,
            std::ptr::null_mut(),
            buffer.as_mut_ptr().cast(),
            &mut bytes,
        )
    };
    if read != ERROR_SUCCESS {
        return None;
    }
    buffer.truncate((bytes as usize / 2).min(buffer.len()));
    let end = buffer.iter().position(|&u| u == 0).unwrap_or(buffer.len());
    Some(String::from_utf16_lossy(&buffer[..end]))
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

    /// A program `App Paths` does not name is not found there, and one it
    /// names is a file: the shell's own `wordpad` or `msedge` where this
    /// Windows has them, which it says rather than requires.
    #[test]
    fn app_paths_finds_a_registered_program_and_only_that() {
        assert_eq!(app_path("no-such-program-delightfile"), None);
        for program in ["msedge", "wordpad", "mspaint"] {
            match app_path(program) {
                Some(path) => {
                    assert!(path.is_file(), "{}", path.display());
                    eprintln!("App Paths: {program} is {}", path.display());
                }
                None => eprintln!("App Paths: no {program} here"),
            }
        }
    }
}
