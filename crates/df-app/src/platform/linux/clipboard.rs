//! The clipboard without a data device, on Linux: wl-clipboard's `wl-copy`
//! and `wl-paste`, run as clients of their own — the fallback for a session
//! the Wayland thread cannot copy in (X11, a compositor without the
//! protocol, a thread that has stopped) and for a copy the compositor
//! refused.
//!
//! Moved here unchanged from `clipboard.rs`, which keeps everything that
//! decides *what* goes on the clipboard (the branches, the mime tables, the
//! `text/uri-list` spelling) and re-exports these four so its callers did not
//! move. [`copy`] hands back the `--foreground` server it started, which the
//! window owns until a newer copy or the quit retires it.

use std::io::Write;
use std::process::{Child, Command, Stdio};

use crate::clipboard::ClipError;

/// What [`ClipError::Missing`] says here: which tool is not on `PATH`, and
/// the package that has it.
pub fn missing(tool: &str) -> String {
    format!("{tool} is not installed — install wl-clipboard")
}

/// Put `bytes` on the clipboard, offered as `mime` (or as plain text when it is
/// `None`) — the **fallback** copy, for a session with no data device.
///
/// Returns the running `wl-copy`, which the caller owns and must eventually
/// [`reap`]. `--foreground` is the whole point: without it `wl-copy` forks a
/// server and the parent exits zero *before* that server has taken the
/// selection, so a successful `wait` here says nothing at all about whether the
/// copy happened — which is exactly the false "Copied" this program used to
/// show. With it, the process that is serving the selection is the process this
/// function hands back, and it being alive a moment later is evidence.
pub fn copy(mime: Option<&str>, bytes: &[u8]) -> Result<Option<Child>, ClipError> {
    let mut command = Command::new("wl-copy");
    command.arg("--foreground");
    if let Some(mime) = mime {
        command.arg("--type").arg(mime);
    }
    // **No `--trim-newline`.** The bytes handed over are the bytes offered:
    // trimming would eat the final newline of a copied file (changing its
    // contents) and the terminating CRLF of a `text/uri-list` (making it
    // invalid). The scripts this is a port of do not trim either.
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => ClipError::Missing("wl-copy"),
            _ => ClipError::Failed(e.to_string()),
        })?;
    // Every failure from here on has a process attached to it, and each one
    // reaps it: an error return that left `wl-copy` running would be a stranger
    // holding the clipboard, and one that left it exited but unwaited would be
    // a zombie for the rest of the session.
    let Some(mut stdin) = child.stdin.take() else {
        // Piped a line ago, so this cannot happen — and a copy that silently
        // succeeded with nothing written is the one way it could go wrong that
        // the user would never see, so it is an error rather than an `if let`
        // with no `else`.
        reap(&mut child);
        return Err(ClipError::Failed(
            "wl-copy gave us nothing to write to".to_string(),
        ));
    };
    let written = stdin.write_all(bytes);
    // Closed before anything waits on the process, or `wl-copy` sits reading a
    // pipe nobody is going to close.
    drop(stdin);
    if let Err(e) = written {
        reap(&mut child);
        return Err(ClipError::Failed(e.to_string()));
    }
    Ok(Some(child))
}

/// Stop a `wl-copy` we are done with and collect it.
///
/// Both halves. `kill` alone leaves a zombie until this process exits, and
/// `wait` alone would block forever on a `--foreground` server that is doing
/// exactly what it was asked to do. Called when a newer copy replaces this one
/// and when the window quits.
pub fn reap(child: &mut Child) {
    let _ = child.kill();
    let _ = child.wait();
}

/// The mime types the clipboard is currently offering, most specific first —
/// `wl-paste --list-types`. The fallback for `platform::linux::wayland`'s own mirror of
/// the selection's offer.
pub fn offered_types() -> Result<Vec<String>, ClipError> {
    let output = Command::new("wl-paste")
        .arg("--list-types")
        .output()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => ClipError::Missing("wl-paste"),
            _ => ClipError::Failed(e.to_string()),
        })?;
    if !output.status.success() {
        // An empty clipboard is an exit code, not a crash: "nothing to paste"
        // is a legitimate answer and the caller says so in a notice.
        return Ok(Vec::new());
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

/// The clipboard's bytes, as `mime`.
pub fn paste(mime: &str) -> Result<Vec<u8>, ClipError> {
    let output = Command::new("wl-paste")
        .arg("--no-newline")
        .arg("--type")
        .arg(mime)
        .output()
        .map_err(|e| match e.kind() {
            std::io::ErrorKind::NotFound => ClipError::Missing("wl-paste"),
            _ => ClipError::Failed(e.to_string()),
        })?;
    if !output.status.success() {
        return Err(ClipError::Failed("nothing on the clipboard".to_string()));
    }
    Ok(output.stdout)
}
