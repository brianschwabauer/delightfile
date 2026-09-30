//! `delightfile --version` and `--help` from the built executable, run the
//! way a script or a CI step runs it: what they print reaches whoever
//! started the program.
//!
//! On Windows the executable is a windows-subsystem program that joins its
//! parent's console for these two (`plans/other-platforms/04-windows.md`
//! W4.1), and this is its piped case — the one CI's smoke test and a wrapper
//! script have. A console typed into is the live check
//! (`07-verification.md` §5.1).

#![allow(clippy::unwrap_used)] // tests: a broken fixture should panic

use std::process::Command;

#[test]
fn version_and_help_reach_whoever_started_the_program() {
    let exe = env!("CARGO_BIN_EXE_delightfile");
    let version = Command::new(exe).arg("--version").output().unwrap();
    assert!(version.status.success(), "{version:?}");
    assert_eq!(
        String::from_utf8_lossy(&version.stdout),
        format!("delightfile {}\n", env!("CARGO_PKG_VERSION"))
    );
    let help = Command::new(exe).arg("--help").output().unwrap();
    assert!(help.status.success(), "{help:?}");
    let help = String::from_utf8_lossy(&help.stdout);
    assert!(
        help.starts_with("delightfile — a keyboard-first file manager"),
        "{help}"
    );
    assert!(help.ends_with("show the version\n"), "{help}");
}
