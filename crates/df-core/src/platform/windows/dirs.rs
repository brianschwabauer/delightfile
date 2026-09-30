//! Where a user's files of each kind live on Windows
//! (`05-defaults-and-config.md` §1, D5.1): the profile is home, the roaming
//! application data holds configuration, the local application data holds
//! state, data and caches, and the temp folder stands in for a runtime
//! directory Windows does not have.
//!
//! Each function answers the *base*, as the Unix ones do, and the caller adds
//! its own folder under it — `config_dir()` is `%APPDATA%`, and the config
//! file is `%APPDATA%\delightfile\delightfile.toml`; the state file is
//! `%LOCALAPPDATA%\delightfile\state`; zoxide's database is
//! `%LOCALAPPDATA%\zoxide\db.zo`, where zoxide itself keeps it; rclone's
//! `%APPDATA%\rclone\rclone.conf` is where rclone looks.
//!
//! The folders come from the environment variables Windows sets for every
//! session. One that is unset or empty — a process started with a stripped
//! environment — falls back to where Windows puts that folder under the
//! profile (`AppData\Roaming`, `AppData\Local`), so the answer is the same
//! place either way. `HOME` is never read: on Windows it is set, if at all,
//! by a Unix toolchain for its own programs.

use std::ffi::OsString;
use std::path::PathBuf;

/// Where the variables are read from: the process environment, or a test's.
type Env<'a> = &'a dyn Fn(&str) -> Option<OsString>;

fn process(name: &str) -> Option<OsString> {
    std::env::var_os(name)
}

/// `%name%` as a path, unless it is unset or empty.
fn var(env: Env<'_>, name: &str) -> Option<PathBuf> {
    env(name)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn home_in(env: Env<'_>) -> Option<PathBuf> {
    var(env, "USERPROFILE")
}

fn roaming_in(env: Env<'_>) -> Option<PathBuf> {
    var(env, "APPDATA").or_else(|| Some(home_in(env)?.join("AppData").join("Roaming")))
}

fn local_in(env: Env<'_>) -> Option<PathBuf> {
    var(env, "LOCALAPPDATA").or_else(|| Some(home_in(env)?.join("AppData").join("Local")))
}

/// `%USERPROFILE%`.
pub fn home() -> Option<PathBuf> {
    home_in(&process)
}

/// `%APPDATA%`, the roaming application data: configuration follows a
/// roaming profile from machine to machine.
pub fn config_dir() -> Option<PathBuf> {
    roaming_in(&process)
}

/// yazi's configuration folder, whose `vfs.toml` is read before ours:
/// `%APPDATA%\yazi\config` — yazi keeps one level more on Windows than the
/// `~/.config/yazi` it uses elsewhere.
pub fn yazi_config_dir() -> Option<PathBuf> {
    Some(config_dir()?.join("yazi").join("config"))
}

/// `%LOCALAPPDATA%`, the local application data: the state file is this
/// machine's, not the profile's.
pub fn state_dir() -> Option<PathBuf> {
    local_in(&process)
}

/// `%LOCALAPPDATA%`, where zoxide keeps its database on Windows.
pub fn data_dir() -> Option<PathBuf> {
    local_in(&process)
}

/// `%LOCALAPPDATA%`, where Windows programs keep what can be rebuilt.
pub fn cache_dir() -> Option<PathBuf> {
    local_in(&process)
}

/// The temp folder: Windows has no per-session runtime directory, and the
/// callers want somewhere private to this user, which `%TEMP%` is.
pub fn runtime_dir() -> Option<PathBuf> {
    temp_dir()
}

/// `%TEMP%`, as `std` finds it.
pub fn temp_dir() -> Option<PathBuf> {
    Some(std::env::temp_dir())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<OsString> {
        let map: HashMap<String, OsString> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), OsString::from(v)))
            .collect();
        move |name| map.get(name).cloned()
    }

    #[test]
    fn each_folder_is_the_variable_windows_sets() {
        let env = env_of(&[
            ("USERPROFILE", r"C:\Users\a"),
            ("APPDATA", r"D:\Roam"),
            ("LOCALAPPDATA", r"E:\Local"),
            ("HOME", r"C:\msys\home\a"),
        ]);
        assert_eq!(home_in(&env), Some(PathBuf::from(r"C:\Users\a")));
        assert_eq!(roaming_in(&env), Some(PathBuf::from(r"D:\Roam")));
        assert_eq!(local_in(&env), Some(PathBuf::from(r"E:\Local")));
    }

    /// A stripped environment finds the same folders under the profile, and
    /// no profile at all finds nothing rather than a relative path.
    #[test]
    fn an_unset_or_empty_folder_falls_back_under_the_profile() {
        let env = env_of(&[("USERPROFILE", r"C:\Users\a"), ("APPDATA", "")]);
        assert_eq!(
            roaming_in(&env),
            Some(PathBuf::from(r"C:\Users\a\AppData\Roaming"))
        );
        assert_eq!(
            local_in(&env),
            Some(PathBuf::from(r"C:\Users\a\AppData\Local"))
        );
        let bare = env_of(&[("HOME", r"C:\Users\a")]);
        assert_eq!(home_in(&bare), None, "HOME is not read");
        assert_eq!(roaming_in(&bare), None);
        assert_eq!(local_in(&bare), None);
    }

    /// Where the files land, read off the real environment the runner gives
    /// the tests: the config and state folders under the two AppData
    /// folders, and yazi's `config` level.
    #[test]
    fn the_program_keeps_its_files_under_appdata() {
        let (Some(roaming), Some(local)) = (roaming_in(&process), local_in(&process)) else {
            eprintln!("skipping: no profile in this environment");
            return;
        };
        assert_eq!(
            crate::config::config_dir(),
            Some(roaming.join("delightfile"))
        );
        assert_eq!(
            crate::state::StateStore::state_path(),
            Some(local.join("delightfile").join("state"))
        );
        assert_eq!(yazi_config_dir(), Some(roaming.join("yazi").join("config")));
        if std::env::var_os("_ZO_DATA_DIR").is_none() {
            assert_eq!(
                crate::zoxide::db_path(),
                Some(local.join("zoxide").join("db.zo"))
            );
        }
    }
}
