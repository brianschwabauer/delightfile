//! Niceness for the calling thread, which on Linux is what
//! `setpriority(PRIO_PROCESS, 0, …)` means: Linux threads are tasks, and `who
//! = 0` is the calling one.

/// Set the calling thread's nice value to `nice` (already clamped to 1–19 by
/// [`crate::thread::lower_priority`]). Returns whether the kernel took it.
#[allow(unsafe_code)]
pub fn lower_priority(nice: i32) -> bool {
    // SAFETY: `setpriority` takes three scalars and touches no memory of ours.
    // `who = 0` with `PRIO_PROCESS` is Linux's "the calling thread", which is
    // the whole reason this is a syscall rather than a thread-builder option.
    let ok = unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, nice) } == 0;
    if !ok {
        log::debug!("could not nice this thread to {nice}");
    }
    ok
}
