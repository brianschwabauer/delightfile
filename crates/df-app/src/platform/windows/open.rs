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
//! the line is run after `pushd` into it, which maps the share to a letter,
//! and `popd` gives the letter back after it, the line's status kept
//! ([`typed`]).
//!
//! **`builtin:shell-open` is `ShellExecuteW(…, "open", …)`**: what a
//! double-click in Explorer does, the file's associated program or the
//! "How do you want to open this?" choice, and for a folder an Explorer
//! window. The shell may hand the verb to an extension that is a COM object,
//! so the calling thread has COM for the length of the call: the window's
//! thread has it already (winit starts OLE there), and a pool thread — the
//! connect prompt's — is given it and has it taken away again.
//!
//! **`builtin:font-install` is the shell's `install` verb** (W4.43), through
//! `ShellExecuteExW`, the one that can be told to wait: Explorer's "Install"
//! on a font, which installs it for this user. A Windows without the verb
//! opens the font in the Font Viewer instead, and the window says which
//! happened.
#![allow(unsafe_code)] // COM's per-thread start, ShellExecuteW and ShellExecuteExW, on wide strings this module builds and keeps alive for the call

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
///
/// In a share the line is wrapped: `(pushd "<cwd>" || exit 1) & <line> &
/// (call set DF_STATUS=%^ERRORLEVEL%) & popd & call exit %^DF_STATUS%`. The
/// parentheses keep the `||` to `pushd` whatever the line's own `&&` and
/// `||` are, so a share that cannot be entered runs nothing; `&` is `cmd`'s
/// loosest joint, so the tail runs however the line ends. The tail is there
/// because the letter `pushd` maps stays mapped, for the rest of the
/// session, unless `popd` gives it back, and `popd` must not stand in for
/// the line's status: `%^ERRORLEVEL%` reaches `call` as `%ERRORLEVEL%` (the
/// caret is gone by then, and on a command line an unknown name is left as
/// written), so it is read after the line has run rather than when `cmd`
/// read the whole.
fn typed(line: &str, paths: &[PathBuf], cwd: &Path) -> Command {
    let share = cwd.as_os_str().to_string_lossy().starts_with(r"\\");
    let mut whole = String::from("/S /C \"");
    if share {
        whole.push_str(&format!("(pushd \"{}\" || exit 1) & ", cwd.display()));
    }
    whole.push_str(line);
    for path in paths {
        whole.push_str(" \"");
        whole.push_str(&path.to_string_lossy());
        whole.push('"');
    }
    if share {
        whole.push_str(" & (call set DF_STATUS=%^ERRORLEVEL%) & popd & call exit %^DF_STATUS%");
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
    shell_execute(target, "open")
}

/// Windows' own "Open with" chooser for `target`: the shell's `openas`
/// verb (`builtin:shell-open-with`, W4.41). Not `rundll32
/// shell32.dll,OpenAs_RunDLL "$1"`, the first form: on the VM it showed
/// nothing for a file whose name has a space, the path reaching it quoted.
pub fn shell_open_with(target: &OsStr) -> io::Result<()> {
    shell_execute(target, "openas")
}

/// `verb` on `target` through `ShellExecuteW`.
fn shell_execute(target: &OsStr, verb: &str) -> io::Result<()> {
    let wide = |text: &OsStr| -> Vec<u16> { text.encode_wide().chain(Some(0)).collect() };
    let verb = wide(OsStr::new(verb));
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

/// Whether `builtin:font-install` is something this platform does: yes, the
/// shell's `install` verb (W4.43).
pub const INSTALLS_FONTS: bool = true;

/// How long the verb is given to finish: its own process, when it starts
/// one, and otherwise its work on this thread's apartment.
const INSTALL_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// The shell's `install` verb on `path` — what Explorer's "Install" on a
/// font does, which installs it for this user (into
/// `%LOCALAPPDATA%\Microsoft\Windows\Fonts`, no administrator asked) — run
/// and seen through: the thread keeps its apartment and pumps its messages
/// until `done` says the font is in, the verb's process has ended, or
/// [`INSTALL_WAIT`] has passed. `Ok(false)` when the file's type has no
/// `install` verb (`SE_ERR_NOASSOC`, `ERROR_NO_ASSOCIATION`): nothing was
/// done.
///
/// The pumping is the point: the verb is a handler the shell runs in this
/// process, on this thread, and it does its work through the thread's
/// message queue after `ShellExecuteExW` returns — on the VM, a call that
/// gave the apartment up at once (the first build of W4.43) installed
/// nothing. Called on a task worker ([`crate::open::install_font`]): the
/// verb may put up the shell's own "already installed — replace it?" and
/// wait on it.
pub fn install_verb(path: &Path, done: &dyn Fn() -> bool) -> io::Result<bool> {
    match shell_verb(path.as_os_str(), "install", INSTALL_WAIT, done) {
        Ok(()) => Ok(true),
        Err(VerbError::NoVerb) => Ok(false),
        Err(VerbError::Io(error)) => Err(error),
    }
}

/// Why a verb did not run.
enum VerbError {
    /// The file's type has no such verb.
    NoVerb,
    Io(io::Error),
}

/// `verb` on `target` through `ShellExecuteExW`, seen through: the call
/// (`SEE_MASK_NOASYNC`), then this thread's apartment kept and its messages
/// pumped until `done`, the end of a process the verb started, or `wait`.
/// The shell's own error dialogs are off (`SEE_MASK_FLAG_NO_UI`): the window
/// says what went wrong.
fn shell_verb(
    target: &OsStr,
    verb: &str,
    wait: std::time::Duration,
    done: &dyn Fn() -> bool,
) -> Result<(), VerbError> {
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_NO_ASSOCIATION, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::System::Threading::WaitForSingleObject;
    use windows_sys::Win32::UI::Shell::{
        ShellExecuteExW, SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SEE_MASK_NOCLOSEPROCESS,
        SHELLEXECUTEINFOW,
    };
    let wide = |text: &OsStr| -> Vec<u16> { text.encode_wide().chain(Some(0)).collect() };
    let verb = wide(OsStr::new(verb));
    let file = wide(target);
    // SAFETY: as in `shell_open`: a per-thread start, balanced below.
    let com = unsafe {
        CoInitializeEx(
            std::ptr::null(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
        )
    };
    // SAFETY: all-zero is the structure's empty value: null strings and
    // handles, no flags.
    let mut info: SHELLEXECUTEINFOW = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
    info.fMask = SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI | SEE_MASK_NOCLOSEPROCESS;
    info.lpVerb = verb.as_ptr();
    info.lpFile = file.as_ptr();
    info.nShow = SW_SHOWNORMAL;
    // SAFETY: `info` is sized and filled above; its strings are
    // NUL-terminated and outlive the call.
    let ok = unsafe { ShellExecuteExW(&mut info) } != 0;
    // SAFETY: read at once on the failing thread.
    let error = (!ok).then(|| unsafe { GetLastError() });
    if ok {
        let start = std::time::Instant::now();
        while !done() && start.elapsed() < wait {
            // SAFETY: a handle the call returned, asked without waiting.
            if info.hProcess != 0
                && unsafe { WaitForSingleObject(info.hProcess, 0) } == WAIT_OBJECT_0
            {
                break;
            }
            pump(std::time::Duration::from_millis(50));
        }
        if info.hProcess != 0 {
            // SAFETY: the process handle the call returned, closed once.
            unsafe { CloseHandle(info.hProcess) };
        }
    }
    if com >= 0 {
        // SAFETY: balances the start counted above, on the same thread.
        unsafe { CoUninitialize() };
    }
    match error {
        None => Ok(()),
        Some(code) if code == ERROR_NO_ASSOCIATION || info.hInstApp == 31 => Err(VerbError::NoVerb),
        Some(code) => Err(VerbError::Io(io::Error::from_raw_os_error(code as i32))),
    }
}

/// This thread's messages, dispatched, for up to `wait` while none come.
fn pump(wait: std::time::Duration) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        DispatchMessageW, MsgWaitForMultipleObjects, PeekMessageW, TranslateMessage, MSG,
        PM_REMOVE, QS_ALLINPUT,
    };
    let millis = u32::try_from(wait.as_millis()).unwrap_or(u32::MAX);
    // SAFETY: no handles to wait on, a timeout, every kind of input; it
    // returns when one comes or the time is up.
    unsafe { MsgWaitForMultipleObjects(0, std::ptr::null(), 0, millis, QS_ALLINPUT) };
    // SAFETY: an all-zero `MSG` is a valid one to be filled; each message
    // taken off this thread's queue is translated and handed to its window
    // procedure, as any message loop does.
    let mut message: MSG = unsafe { std::mem::zeroed() };
    while unsafe { PeekMessageW(&mut message, 0, 0, 0, PM_REMOVE) } != 0 {
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
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
        // It runs in the folder it is given: `cd` there prints the folder.
        let here = dir.join("here.txt");
        run_typed(&format!("cd > \"{}\"", here.display()), &[], &dir).expect("ran");
        let said = std::fs::read_to_string(&here).expect("cmd wrote");
        assert_eq!(
            PathBuf::from(said.trim_end()).canonicalize().expect("real"),
            dir.canonicalize().expect("real")
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// In a share's folder a line runs there, through the letter `pushd`
    /// maps, its own `&` and relative names included; its status is its
    /// own, not `popd`'s; and the letter is given back. The share is the
    /// temp folder through the machine's own `C$`, which a runner's
    /// elevated account reaches; elsewhere, with no such share, the test
    /// says so and stops, and on a runner it fails instead.
    #[test]
    fn a_typed_line_in_a_share_runs_there_and_gives_its_letter_back() {
        use windows_sys::Win32::Storage::FileSystem::GetLogicalDrives;
        let dir = std::env::temp_dir().join(format!("df-typed-share-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let dir = dir.canonicalize().expect("real");
        let text = dir.to_string_lossy().into_owned();
        let rest = text
            .trim_start_matches(r"\\?\")
            .strip_prefix(r"C:\")
            .map(str::to_string);
        let share = rest.map(|rest| PathBuf::from(format!(r"\\localhost\C$\{rest}")));
        let Some(share) = share.filter(|share| share.is_dir()) else {
            assert!(
                std::env::var_os("CI").is_none(),
                "no administrative share to run in at {text}"
            );
            eprintln!("skipped: {text} has no administrative share here");
            let _ = std::fs::remove_dir_all(&dir);
            return;
        };
        // SAFETY: takes nothing and returns a bit mask of the letters in use.
        let letters = || unsafe { GetLogicalDrives() };
        let before = letters();

        let there = dir.join("there.txt");
        let code = run_typed(&format!("cd > \"{}\"", there.display()), &[], &share).expect("ran");
        assert_eq!(code, 0);
        let said = std::fs::read_to_string(&there).expect("cmd wrote");
        let said = said.trim_end();
        assert!(
            !said.eq_ignore_ascii_case(r"C:\Windows"),
            "cmd started in the Windows folder: {said}"
        );
        let tail = text.trim_start_matches(r"\\?\").trim_start_matches(r"C:\");
        assert!(
            said.to_ascii_lowercase()
                .ends_with(&tail.to_ascii_lowercase()),
            "{said} is not the share's folder"
        );

        let code = run_typed("echo one> a.txt & echo two> b.txt", &[], &share).expect("ran");
        assert_eq!(code, 0);
        assert!(dir.join("a.txt").is_file() && dir.join("b.txt").is_file());

        assert_eq!(run_typed("cmd /c exit 4", &[], &share).expect("ran"), 4);
        assert_eq!(letters(), before, "a letter pushd mapped is still mapped");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// What the shell has for `verb` on files of `extension`, as
    /// `AssocQueryStringW` reads it: `what` is a command line or a
    /// `DelegateExecute` class.
    fn association(extension: &str, verb: &str, what: i32) -> Option<String> {
        use windows_sys::Win32::UI::Shell::{AssocQueryStringW, ASSOCF_NONE};
        let wide =
            |text: &str| -> Vec<u16> { OsStr::new(text).encode_wide().chain(Some(0)).collect() };
        let (extension, verb) = (wide(extension), wide(verb));
        let mut out = vec![0u16; 2048];
        let mut len = out.len() as u32;
        // SAFETY: both strings are NUL-terminated; `out` holds `len` units.
        let result = unsafe {
            AssocQueryStringW(
                ASSOCF_NONE,
                what,
                extension.as_ptr(),
                verb.as_ptr(),
                out.as_mut_ptr(),
                &mut len,
            )
        };
        (result >= 0).then(|| {
            let end = out.iter().position(|&u| u == 0).unwrap_or(out.len());
            String::from_utf16_lossy(&out[..end])
        })
    }

    /// The shell has an `install` verb for a TrueType font — a command or a
    /// handler class — which is what `builtin:font-install` runs (W4.43).
    /// What it is, is printed, for the plan's record.
    #[test]
    fn the_shell_has_an_install_verb_for_fonts() {
        use windows_sys::Win32::UI::Shell::{ASSOCSTR_COMMAND, ASSOCSTR_DELEGATEEXECUTE};
        let command = association(".ttf", "install", ASSOCSTR_COMMAND);
        let handler = association(".ttf", "install", ASSOCSTR_DELEGATEEXECUTE);
        let open = association(".ttf", "open", ASSOCSTR_COMMAND);
        eprintln!(".ttf install: command {command:?}, handler {handler:?}; open: {open:?}");
        // A Windows that opens a font at all installs it too; the runner's
        // Server image has neither (W4.43), which is said and not failed.
        if open.is_some() {
            assert!(
                command.is_some() || handler.is_some(),
                "a font opens here but has no install verb"
            );
        } else {
            eprintln!("this Windows has no association for .ttf at all");
        }
    }

    /// The verb installs a font for this user: a stock face no Windows has
    /// (egui's Hack), under a name of this run's own, lands in the
    /// per-user fonts folder. On a runner only (`CI`), whose profile is
    /// thrown away after the job: a person's own machine keeps what a test
    /// installs.
    #[test]
    fn a_font_installs_for_this_user() {
        if std::env::var_os("CI").is_none() {
            eprintln!("skipped: installs a font into this profile; runs on a runner only");
            return;
        }
        use windows_sys::Win32::UI::Shell::ASSOCSTR_COMMAND;
        if association(".ttf", "open", ASSOCSTR_COMMAND).is_none() {
            eprintln!(
                "skipped: this Windows has no association for .ttf (the runner's Server image)"
            );
            return;
        }
        let definitions = egui::FontDefinitions::default();
        let mono = &definitions.families[&egui::FontFamily::Monospace][0];
        let hack = &definitions.font_data[mono];
        let dir = std::env::temp_dir().join(format!("df-font-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let font = dir.join(format!("delightfile-test-{}.ttf", std::process::id()));
        std::fs::write(&font, &hack.font[..]).expect("the font");
        let outcome = crate::open::install_font(&font).expect("the verb ran");
        eprintln!("install_font: {outcome:?}");
        assert_eq!(outcome, crate::open::FontInstall::Installed);
        let local = std::env::var_os("LOCALAPPDATA").expect("LOCALAPPDATA");
        let installed = PathBuf::from(local)
            .join(r"Microsoft\Windows\Fonts")
            .join(font.file_name().expect("a name"));
        assert!(installed.is_file(), "{} is not there", installed.display());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
