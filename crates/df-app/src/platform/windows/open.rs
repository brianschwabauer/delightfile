//! Running things on Windows (`plans/other-platforms/04-windows.md` W4.3):
//! openers without a shell, a typed line through `cmd`, and the shell's own
//! "open" for the program a file is associated with.
//!
//! **An opener is an argument list** ([`crate::platform::argv`]): the
//! program and its words, the paths substituted for `$1`, `$@` and `$dir`,
//! and started directly. It is detached from the window by a process group
//! of its own (`CREATE_NEW_PROCESS_GROUP`) — a Ctrl+C in the terminal that
//! started delightfile is not the editor's — and never with
//! `DETACHED_PROCESS` or `CREATE_NO_WINDOW`, which would deny a console to
//! the openers that are one (`wt`, `cmd /K`) (`00-ground-rules.md` §5).
//!
//! **A typed `;` or `:` line is `cmd`'s**: `%COMSPEC% /S /C "<line> <paths…>"`,
//! the line exactly as typed and each path quoted after it. `/S` makes `cmd`
//! strip only the outer pair of quotes, so a line that begins with a quoted
//! program keeps its own. What `cmd` does inside double quotes is its
//! business: a `%NAME%` in a path is expanded there, as it would be typed at
//! its prompt. `cmd` runs with no window of its own (`CREATE_NO_WINDOW`), as
//! a `;` line runs with no terminal on Linux: a program the line starts that
//! has a window of its own shows it, and a console one's output goes where
//! delightfile's does. And `cmd` cannot stand in a share's folder — handed
//! `\\server\share\…` as its directory it says so and starts in the Windows
//! folder, where a relative name in the line would then act — so in a share
//! the line is run after `pushd` into it, which maps the share to a letter
//! for the length of the line.
//!
//! **`builtin:shell-open` is `ShellExecuteW(…, "open", …)`**: what a
//! double-click in Explorer does, the file's associated program or the
//! "How do you want to open this?" choice, and for a folder an Explorer
//! window. The shell may hand the verb to an extension that is a COM object,
//! so the calling thread has COM for the length of the call: the window's
//! thread has it already (winit starts OLE there), and a pool thread — the
//! connect prompt's — is given it and has it taken away again.
#![allow(unsafe_code)] // COM's per-thread start and ShellExecuteW, on wide strings this function builds and keeps alive for the call

use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use windows_sys::Win32::System::Com::{
    CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE,
};
use windows_sys::Win32::System::Threading::{CREATE_NEW_PROCESS_GROUP, CREATE_NO_WINDOW};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

use crate::platform::argv::argv_from;

/// `cmd`, as the system names it (`%COMSPEC%`), else by name.
pub fn shell_program() -> String {
    std::env::var("COMSPEC")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "cmd.exe".to_string())
}

/// An opener's command as a `Command`, from `cwd`; an error for a command
/// with no program in it.
fn opener(command: &str, paths: &[PathBuf], cwd: &Path) -> io::Result<Command> {
    let argv = argv_from(command, paths);
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| io::Error::other("empty command"))?;
    let mut command = Command::new(program);
    command.args(args).current_dir(cwd);
    Ok(command)
}

/// Start an opener and do not wait for it: its own process group, no
/// standard streams.
pub fn spawn_detached(command: &str, paths: &[PathBuf], cwd: &Path) -> io::Result<()> {
    let mut command = opener(command, paths, cwd)?;
    command
        .creation_flags(CREATE_NEW_PROCESS_GROUP)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Nothing waits for it: a Windows process leaves no zombie, and the
    // handle is closed when `Child` is dropped.
    command.spawn()?;
    Ok(())
}

/// Run an opener to completion (`block = true`), on a task worker.
pub fn run_blocking(command: &str, paths: &[PathBuf], cwd: &Path) -> io::Result<i32> {
    let status = opener(command, paths, cwd)?.status()?;
    Ok(df_core::platform::process::exit_code(&status))
}

/// A typed line as `cmd` runs it, from `cwd`.
fn typed(line: &str, paths: &[PathBuf], cwd: &Path) -> Command {
    let mut whole = String::from("/S /C \"");
    if cwd.as_os_str().to_string_lossy().starts_with(r"\\") {
        whole.push_str(&format!("pushd \"{}\" && ", cwd.display()));
    }
    whole.push_str(line);
    for path in paths {
        whole.push_str(" \"");
        whole.push_str(&path.to_string_lossy());
        whole.push('"');
    }
    whole.push('"');
    let mut command = Command::new(shell_program());
    command
        .raw_arg(whole)
        .current_dir(cwd)
        .creation_flags(CREATE_NO_WINDOW);
    command
}

/// Start a typed `;` line and do not wait for it.
pub fn spawn_typed(line: &str, paths: &[PathBuf], cwd: &Path) -> io::Result<()> {
    typed(line, paths, cwd)
        .creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}

/// Run a typed `:` line to completion, on a task worker.
pub fn run_typed(line: &str, paths: &[PathBuf], cwd: &Path) -> io::Result<i32> {
    let status = typed(line, paths, cwd).status()?;
    Ok(df_core::platform::process::exit_code(&status))
}

/// What a double-click in Explorer does to `target`: a file opens in its
/// associated program, a folder or a `shell:` name in an Explorer window.
pub fn shell_open(target: &OsStr) -> io::Result<()> {
    let wide = |text: &OsStr| -> Vec<u16> { text.encode_wide().chain(Some(0)).collect() };
    let verb = wide(OsStr::new("open"));
    let file = wide(target);
    // SAFETY: a per-thread start with no pointer but the reserved null. It
    // answers S_OK or S_FALSE (already started, the window's thread) when
    // it counted a start, which the CoUninitialize below takes back; a
    // thread already in another model answers an error and is left as it
    // is.
    let com = unsafe {
        CoInitializeEx(
            std::ptr::null(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
        )
    };
    // SAFETY: both strings are NUL-terminated and outlive the call; the
    // parameters and directory are null, which the call takes as none.
    let code = unsafe {
        ShellExecuteW(
            0,
            verb.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    if com >= 0 {
        // SAFETY: balances the start counted above, on the same thread.
        unsafe { CoUninitialize() };
    }
    // Above 32 is success; at or below it one of the documented errors.
    if code > 32 {
        return Ok(());
    }
    Err(shell_error(code))
}

/// `ShellExecuteW`'s answer, in words a toast can carry.
fn shell_error(code: isize) -> io::Error {
    match code {
        // SE_ERR_NOASSOC, SE_ERR_ASSOCINCOMPLETE
        31 | 27 => io::Error::other("no program is set to open it"),
        // ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND
        2 | 3 => io::Error::from(io::ErrorKind::NotFound),
        // SE_ERR_ACCESSDENIED
        5 => io::Error::from(io::ErrorKind::PermissionDenied),
        // SE_ERR_OOM, 0 (out of memory or resources)
        0 | 8 => io::Error::from(io::ErrorKind::OutOfMemory),
        code => io::Error::other(format!("the shell could not open it (error {code})")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An opener is started as its program with the paths as arguments,
    /// through no shell: a `.cmd` stub writes what it was handed and where
    /// it ran.
    #[test]
    fn an_opener_runs_its_program_with_the_paths_as_arguments() {
        let dir = std::env::temp_dir().join(format!("df-open-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let stub = dir.join("stub.cmd");
        let out = dir.join("out.txt");
        std::fs::write(
            &stub,
            format!("@(echo %~1& echo %~2& cd) > \"{}\"\r\n", out.display()),
        )
        .expect("the stub");
        let paths = vec![dir.join("a b.txt"), dir.join("c.txt")];
        let code =
            run_blocking(&format!("\"{}\" \"$@\"", stub.display()), &paths, &dir).expect("ran");
        assert_eq!(code, 0);
        let text = std::fs::read_to_string(&out).expect("the stub wrote");
        let lines: Vec<&str> = text.lines().map(str::trim_end).collect();
        assert_eq!(lines[0], paths[0].to_string_lossy());
        assert_eq!(lines[1], paths[1].to_string_lossy());
        assert_eq!(
            PathBuf::from(lines[2]).canonicalize().expect("real"),
            dir.canonicalize().expect("real")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A typed line is `cmd`'s, with the paths after it.
    #[test]
    fn a_typed_line_runs_in_cmd_with_the_paths_after_it() {
        let dir = std::env::temp_dir().join(format!("df-typed-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let out = dir.join("out.txt");
        let line = format!("echo said > \"{}\" & echo", out.display());
        let code = run_typed(&line, &[dir.join("x y.txt")], &dir).expect("ran");
        assert_eq!(code, 0);
        let text = std::fs::read_to_string(&out).expect("cmd wrote");
        assert_eq!(text.trim_end(), "said");
        assert_eq!(run_typed("exit 3", &[], &dir).expect("ran"), 3);
        // It runs in the folder it is given, a share's included: `cd` there
        // prints the folder, and in a share a letter `pushd` mapped to it.
        let here = dir.join("here.txt");
        run_typed(&format!("cd > \"{}\"", here.display()), &[], &dir).expect("ran");
        let said = std::fs::read_to_string(&here).expect("cmd wrote");
        assert_eq!(
            PathBuf::from(said.trim_end()).canonicalize().expect("real"),
            dir.canonicalize().expect("real")
        );
        // The same folder through the administrative share, where the runner
        // may reach it (an elevated account on the machine's own C$).
        let text = dir.to_string_lossy().into_owned();
        if let Some(rest) = text.strip_prefix(r"C:\") {
            let share = PathBuf::from(format!(r"\\localhost\C$\{rest}"));
            if share.is_dir() {
                let there = dir.join("there.txt");
                run_typed(&format!("cd > \"{}\"", there.display()), &[], &share).expect("ran");
                let said = std::fs::read_to_string(&there).expect("cmd wrote");
                assert!(
                    !said.trim_end().eq_ignore_ascii_case(r"C:\Windows"),
                    "cmd started in the Windows folder: {said}"
                );
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
