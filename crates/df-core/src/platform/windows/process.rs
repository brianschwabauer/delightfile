//! The parts of running another program that differ by platform, as Windows
//! has them: `NUL`, no stopping a child, an exit code that is always there,
//! executables known by their extension, tools that carry `.exe`, and a tool
//! run with no console window of its own.
//!
//! delightfile is a window, not a console program, so every console tool it
//! starts — `git`, `7z`, `tar`, `ssh`, `rclone` — would get a console of its
//! own, and its window would flash up over the app for as long as the tool
//! runs. [`quiet`] starts one with `CREATE_NO_WINDOW` instead, which is what
//! a headless tool with piped or null stdio wants. It is never for an opener:
//! a `cmd` or a `wt` that a person asked for needs the console it gets.
//!
//! A child that must not outlive delightfile — the `rclone rcd` daemon, which
//! has no stdin whose end would tell it — is put in a job object created with
//! `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE` ([`tie`]), whose one handle the
//! owning worker holds in the [`Tie`]. When the last handle to a job closes
//! the system ends every process in it, and a process's handles close however
//! it ends — a crash, `TerminateProcess`, a logoff — so the daemon ends with
//! delightfile as it does on Linux by `PR_SET_PDEATHSIG`. The child is put in
//! the job just after it starts (`std` cannot create a process inside one), so
//! a delightfile killed in those microseconds leaves it running.
//!
//! The `unsafe` is the three job calls on a handle the [`Tie`] owns from the
//! moment it exists, and the child's handle, which its `Child` owns; each
//! return is checked, and nothing unsafe escapes the file.

#![allow(unsafe_code)] // CreateJobObjectW, SetInformationJobObject, AssignProcessToJobObject

use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, ExitStatus};

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
    SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
    JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
};
use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;

/// Nothing before the start: Windows puts a process in a job after it has
/// started, which is [`tie`]'s.
pub fn tie_to_this_thread(_command: &mut Command) {}

/// The job object a tied child runs in, which ends it when this is dropped —
/// or when this process ends, however it ends. Hold it for as long as the
/// child should live.
#[derive(Debug)]
pub struct Tie {
    _job: OwnedHandle,
}

/// Put the just-started `child` in a job of its own that ends it when the
/// returned [`Tie`] closes: dropped, or closed by the system as this process
/// dies.
pub fn tie(child: &Child) -> io::Result<Tie> {
    // SAFETY: no security attributes and no name: an unnamed job only this
    // process can reach, owned by the `OwnedHandle` from here on.
    let raw = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if raw == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: `raw` is a handle this call just created and nothing else holds.
    let job = unsafe { OwnedHandle::from_raw_handle(raw as _) };
    let job_handle = job.as_raw_handle() as HANDLE;
    // SAFETY: an all-zero limit structure is its "no limits" value.
    let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    // SAFETY: the job handle is `job`'s; `limits` is a local of the struct the
    // information class names, and the size passed is its size.
    let ok = unsafe {
        SetInformationJobObject(
            job_handle,
            JobObjectExtendedLimitInformation,
            std::ptr::addr_of!(limits).cast(),
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: the job handle is `job`'s, and the process handle `child`'s,
    // open with every right for as long as the `Child` is.
    let ok = unsafe { AssignProcessToJobObject(job_handle, child.as_raw_handle() as HANDLE) };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(Tie { _job: job })
}

/// The file that discards what is written to it and reads as empty.
pub const NULL_DEVICE: &str = "NUL";

/// No `rsync` on Windows, ever: it is not a Windows tool, and its `host:path`
/// syntax collides with drive letters. The remote sync is not offered.
pub const HAS_RSYNC: bool = false;

/// Nothing in place of the bare "rsync": no rsync is ever asked for here.
pub const RSYNC_HINT: &str = "";

/// Refused: Windows has no `SIGSTOP`, and suspending another process's
/// threads one by one is not a pause worth offering (W4.2).
pub fn pause(_child: &Child) -> io::Result<()> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

/// Refused, as [`pause`].
pub fn resume(_child: &Child) -> io::Result<()> {
    Err(io::Error::from(io::ErrorKind::Unsupported))
}

/// Stop `child`: `TerminateProcess`, since Windows has no gentler signal a
/// console-less child would see. A child already reaped is left alone.
pub fn terminate(child: &mut Child) -> io::Result<()> {
    if child.try_wait()?.is_some() {
        return Ok(());
    }
    child.kill()
}

/// What [`quiet`] sets: `CREATE_NO_WINDOW`, and no other creation flag.
const QUIET: u32 = CREATE_NO_WINDOW;

/// Run the tool `command` starts without a console window. The only creation
/// flag df-core sets, so it overwrites none. Returns `command` for chaining.
pub fn quiet(command: &mut Command) -> &mut Command {
    command.creation_flags(QUIET)
}

/// The number a finished child exited with. A Windows process always has
/// one — there are no signals to end it without — so the 1 is never reached;
/// it stands for a failure all the same, never for a success.
pub fn exit_code(status: &ExitStatus) -> i32 {
    status.code().unwrap_or(1)
}

/// Whether `path` is a file whose extension is one `PATHEXT` names
/// (`.COM;.EXE;.BAT;.CMD` when it is unset), in any case: Windows has no
/// execute bit, and the extension is what decides whether a name runs.
pub fn is_executable(path: &Path) -> bool {
    if !std::fs::metadata(path).is_ok_and(|m| m.is_file()) {
        return false;
    }
    let Some(ext) = path.extension().and_then(|ext| ext.to_str()) else {
        return false;
    };
    let pathext = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_string());
    pathext
        .split(';')
        .filter_map(|known| known.strip_prefix('.'))
        .any(|known| known.eq_ignore_ascii_case(ext))
}

/// The file names a tool called `name` may have on `PATH`: `name.exe` first,
/// then the name as given. `bsdtar` is also looked for as `tar`, because
/// Windows 10 and later ship libarchive's bsdtar as `tar.exe` (where the GNU
/// tar flags a Linux `tar` would need never come up). `7z` is also looked for
/// as `7za`, 7-Zip's standalone console build, which takes the same commands
/// and switches but reads fewer formats (no rar): a machine with only that
/// one still extracts a 7z and writes one.
pub fn candidates(name: &str) -> Vec<String> {
    let mut names = vec![format!("{name}.exe"), name.to_string()];
    match name {
        "bsdtar" => names.extend(["tar.exe".to_string(), "tar".to_string()]),
        "7z" => names.extend(["7za.exe".to_string(), "7za".to_string()]),
        _ => {}
    }
    names
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

    use super::*;

    /// `std` cannot read a command's creation flags back, and a console that
    /// never appears cannot be seen headlessly; so the flag is checked by
    /// value, and a quiet tool is run to show its pipes still carry its
    /// output.
    #[test]
    fn a_quiet_tool_has_no_window_and_still_talks() {
        assert_eq!(QUIET, 0x0800_0000, "CREATE_NO_WINDOW");
        let out = quiet(&mut Command::new("cmd"))
            .args(["/C", "echo", "quiet"])
            .output()
            .unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "quiet");
    }

    #[test]
    fn a_tool_is_looked_for_with_its_extension_first() {
        assert_eq!(candidates("7z"), ["7z.exe", "7z", "7za.exe", "7za"]);
        assert_eq!(candidates("git"), ["git.exe", "git"]);
        assert_eq!(
            candidates("bsdtar"),
            ["bsdtar.exe", "bsdtar", "tar.exe", "tar"]
        );
    }

    /// Which part [`the_tie_helper`] plays in a copy of this test binary.
    const ROLE: &str = "DF_TIE_ROLE";
    const HELPER: &str = "platform::windows::process::tests::the_tie_helper";

    /// A copy of this test binary running only [`the_tie_helper`], as `role`.
    fn helper(role: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args(["--exact", HELPER, "--nocapture", "--test-threads=1"])
            .env(ROLE, role);
        quiet(&mut command);
        command
    }

    /// Nothing, in the suite. In a copy started by the test below it is the
    /// parent — which starts and ties a child, names it on stdout and waits to
    /// be killed — or that child, which waits to outlive it or not.
    #[test]
    #[allow(clippy::zombie_processes)] // the job ends the child; that is the point
    fn the_tie_helper() {
        use std::io::Write;
        use std::time::Duration;
        match std::env::var(ROLE).as_deref() {
            Ok("parent") => {
                let mut command = helper("child");
                command
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null());
                tie_to_this_thread(&mut command);
                let child = command.spawn().unwrap();
                let _tie = tie(&child).unwrap();
                println!("child-pid {}", child.id());
                std::io::stdout().flush().unwrap();
                std::thread::sleep(Duration::from_secs(120));
            }
            Ok("child") => std::thread::sleep(Duration::from_secs(120)),
            _ => {}
        }
    }

    /// A tied child ends when its parent is ended with `TerminateProcess`,
    /// which runs no destructor: the system closes the parent's job handle.
    #[test]
    fn a_tied_child_dies_with_its_parent() {
        use std::io::BufRead;
        use windows_sys::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
        use windows_sys::Win32::System::Threading::{
            OpenProcess, TerminateProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION,
            PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
        };
        let mut parent = helper("parent")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let out = std::io::BufReader::new(parent.stdout.take().unwrap());
        let pid: u32 = out
            .lines()
            .find_map(|line| line.ok()?.strip_prefix("child-pid ")?.trim().parse().ok())
            .expect("the parent never named its child");
        // SAFETY: a pid read from the parent, opened for waiting on and, if
        // the test fails, ending; the handle is owned from here on.
        let raw = unsafe {
            OpenProcess(
                PROCESS_SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE,
                0,
                pid,
            )
        };
        assert_ne!(raw, 0, "{}", io::Error::last_os_error());
        // SAFETY: `raw` was just opened and nothing else holds it.
        let child = unsafe { OwnedHandle::from_raw_handle(raw as _) };
        let child_handle = child.as_raw_handle() as HANDLE;
        // SAFETY: a process handle `child` owns, open for waiting.
        let alive = unsafe { WaitForSingleObject(child_handle, 0) };
        assert_eq!(alive, WAIT_TIMEOUT, "the child is running before");

        parent.kill().unwrap();
        parent.wait().unwrap();
        // SAFETY: as above; up to twenty seconds, for a loaded runner.
        let ended = unsafe { WaitForSingleObject(child_handle, 20_000) };
        if ended != WAIT_OBJECT_0 {
            // SAFETY: the same handle, opened with the right to end it.
            unsafe { TerminateProcess(child_handle, 1) };
            panic!("the child outlived its parent");
        }
    }
}
