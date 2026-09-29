//! Running a snippet on macOS: the shell body Linux and macOS share
//! ([`crate::platform::unix::open`]), detached from the window by a process
//! group of its own.
//!
//! macOS has no `setsid` program, so the argv is left as it is
//! ([`detached_argv`]) and the command itself is told to start a new process
//! group ([`detach`]): the terminal delightfile was started from, if any,
//! signals delightfile's group and not the editor's, and quitting delightfile
//! sends nothing to either. Nothing forks in between, so the child is
//! delightfile's own until it exits, and a zombie after that unless somebody
//! collects it; [`release`] hands it to a thread that does.

use std::os::unix::process::CommandExt;
use std::process::{Child, Command};

pub use crate::platform::unix::open::*;

/// The argv as it is: the detaching is [`detach`]'s.
pub fn detached_argv(argv: Vec<String>) -> Vec<String> {
    argv
}

/// Start the child as the leader of a process group of its own.
pub fn detach(command: &mut Command) {
    command.process_group(0);
}

/// Collect the child when it exits, on a thread of its own, so that an
/// editor closed an hour later leaves no zombie behind while delightfile
/// runs on.
pub fn release(mut child: Child) {
    let spawned = std::thread::Builder::new()
        .name("opener-reaper".to_string())
        .spawn(move || {
            let _ = child.wait();
        });
    if let Err(error) = spawned {
        log::warn!("could not start a thread to collect a launched program: {error}");
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    /// What `ps` says about a process, trimmed; empty once it is gone.
    fn ps(field: &str, pid: &str) -> String {
        let output = Command::new("ps")
            .args(["-o", &format!("{field}="), "-p", pid])
            .output()
            .expect("ps runs");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    /// A launched program leads a process group of its own — not
    /// delightfile's, so a signal to delightfile's group (a terminal's Ctrl+C
    /// or hangup, a quit) does not reach it — outlives the call that started
    /// it, and is collected when it exits rather than left a zombie.
    #[test]
    fn a_launched_program_leads_its_own_group_and_is_collected() {
        let dir = std::env::temp_dir().join(format!("df-open-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let note = dir.join("pid");
        spawn_detached(
            r#"echo $$ > "$1.tmp" && mv "$1.tmp" "$1"; sleep 1"#,
            std::slice::from_ref(&note),
            &dir,
        )
        .expect("spawned");

        let started = Instant::now();
        let pid = loop {
            if let Ok(text) = std::fs::read_to_string(&note) {
                break text.trim().to_string();
            }
            assert!(started.elapsed() < Duration::from_secs(10), "never ran");
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(ps("pgid", &pid), pid, "its own group, led by itself");
        assert_ne!(ps("pgid", &std::process::id().to_string()), pid, "not ours");
        assert!(!ps("stat", &pid).is_empty(), "still running after the call");

        // `sleep 1`, then the reaper's `wait`: gone from the table, not a
        // zombie in it.
        let gone = loop {
            let stat = ps("stat", &pid);
            if stat.is_empty() {
                break true;
            }
            if started.elapsed() > Duration::from_secs(10) {
                break false;
            }
            std::thread::sleep(Duration::from_millis(50));
        };
        assert!(gone, "collected once it exited: {}", ps("stat", &pid));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
