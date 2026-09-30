//! The console, on Windows (`plans/other-platforms/04-windows.md` W4.1).
//!
//! The executable is built for the *windows* subsystem (`main.rs`), so a
//! double-click in Explorer opens the window and nothing else: a console
//! program would have the system open a console window beside it, empty,
//! for as long as the program ran. The price is that a program of that
//! subsystem is given no console when a terminal starts it either, so
//! `delightfile --version` typed into `cmd` would print nowhere.
//! [`attach_parent_console`] pays it back: when the parent has a console —
//! a terminal started us — the process joins it, and standard output and
//! error that point nowhere are pointed at it. A parent without one
//! (Explorer, the Start menu) leaves the process with none, which is the
//! point of the subsystem.
//!
//! Streams the parent redirected — to a file, or to the pipe a CI job or a
//! wrapper script reads — are already real handles and are left exactly as
//! they are; only an absent one is replaced. The terminal does not wait for
//! a windows-subsystem program, so what is printed lands after its prompt
//! has come back: the known cost of this design, and the reason `--help`
//! and `--version` are the only output worth it.
#![allow(unsafe_code)] // three kernel32 calls on the process's own standard handles

use std::os::windows::io::IntoRawHandle;

use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::Storage::FileSystem::{GetFileType, FILE_TYPE_UNKNOWN};
use windows_sys::Win32::System::Console::{
    AttachConsole, GetStdHandle, SetStdHandle, ATTACH_PARENT_PROCESS, STD_ERROR_HANDLE, STD_HANDLE,
    STD_OUTPUT_HANDLE,
};

/// Join the console of the process that started this one, if it has one,
/// and point standard output and error at it where they point nowhere.
/// Called first thing in `main`, before anything is written.
pub fn attach_parent_console() {
    // SAFETY: takes a process id (the parent's, by the constant) and touches
    // no memory of ours; failure — no parent console — is a plain FALSE.
    if unsafe { AttachConsole(ATTACH_PARENT_PROCESS) } == 0 {
        return;
    }
    for stream in [STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        if !points_nowhere(stream) {
            continue;
        }
        let Ok(console) = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open("CONOUT$")
        else {
            return;
        };
        // The handle is the stream's from here on, for the life of the
        // process, so it is given up rather than closed.
        let handle = console.into_raw_handle() as isize;
        // SAFETY: `handle` is an open console handle this process owns and
        // never closes; SetStdHandle only records it.
        unsafe { SetStdHandle(stream, handle) };
    }
}

/// Whether a standard stream has nothing behind it: no handle, or one the
/// system cannot say the kind of.
fn points_nowhere(stream: STD_HANDLE) -> bool {
    // SAFETY: GetStdHandle reads the process's own table; GetFileType is
    // asked only of a handle it returned, and answers UNKNOWN for a bad one.
    unsafe {
        let handle = GetStdHandle(stream);
        handle == 0 || handle == INVALID_HANDLE_VALUE || GetFileType(handle) == FILE_TYPE_UNKNOWN
    }
}
