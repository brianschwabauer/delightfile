//! Running a snippet on Linux: the shell body Linux and macOS share
//! ([`crate::platform::unix::open`]), detached through `setsid`.
//!
//! Detaching (PLAN §6's "launched detached") is `setsid`: the opener commands
//! ported from the yazi config already say `setsid uwsm-app --` themselves, and
//! a bare `;` command should be no less free of delightfile's process group —
//! quit the file manager and what you started stays up. It is a real program
//! rather than a `pre_exec` closure because the workspace lints
//! `unsafe_code = "warn"`, and this does not need unsafe to be correct.

use std::path::PathBuf;

pub use crate::platform::unix::open::*;

/// Wrap an argv so the child leaves delightfile's process group.
///
/// `--fork` is not optional: without it `setsid` *execs* the program in place
/// whenever the caller is not already a process-group leader, and delightfile
/// usually is not — so the "detached" child would be this very process's child
/// and the `wait` in [`spawn_detached`] would block the UI thread for as long
/// as the editor stayed open. With `--fork` the direct child exits immediately
/// and the grandchild is the compositor's.
///
/// `setsid` is util-linux and is on every machine this targets; if it is
/// somehow not there the command still runs — just parented to us, which is
/// worse than detached and much better than not opening the file.
pub fn detached_argv(argv: Vec<String>) -> Vec<String> {
    if which("setsid").is_none() {
        return argv;
    }
    let mut out = vec!["setsid".to_string(), "--fork".to_string()];
    out.extend(argv);
    out
}

/// Nothing to set on the command: `setsid` in the argv is the detaching
/// ([`detached_argv`]).
pub fn detach(_command: &mut std::process::Command) {}

/// A child that was not detached — `setsid` missing — is left as it always
/// was: running, parented to the window.
pub fn release(_child: std::process::Child) {}

/// Is `name` on `$PATH`?
fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detaching_only_prefixes_what_it_can_find() {
        let argv = shell_argv("/bin/sh", "true", &[]);
        let detached = detached_argv(argv.clone());
        if which("setsid").is_some() {
            assert_eq!(
                &detached[..2],
                &["setsid".to_string(), "--fork".to_string()]
            );
            assert_eq!(&detached[2..], &argv[..]);
        } else {
            assert_eq!(detached, argv, "no setsid: still open the file");
        }
    }
}
