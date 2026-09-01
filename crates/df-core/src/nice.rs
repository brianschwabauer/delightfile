//! Making a worker thread yield to the one the user is looking at.
//!
//! PLAN §1's rule is that nothing the user did not ask for may cost them a
//! frame. Thread *count* is how the workspace has said that so far — two du
//! walkers, ten task workers — but a count only bounds how many threads exist,
//! not what the kernel does when they all want the CPU at once. A recursive
//! walk of `~` is a thread that will happily consume a core for a minute, and
//! on a busy machine it is scheduled against the UI thread as an equal.
//!
//! Linux nice values are **per thread**, not per process: `setpriority` with
//! `PRIO_PROCESS` and a `who` of 0 applies to the calling thread alone, which
//! is exactly the granularity wanted here and is why this is three lines rather
//! than a scheduling policy.
//!
//! ## Best effort, and silent about it
//!
//! Lowering a priority never needs a capability, so this call effectively
//! cannot fail — but if it does (a seccomp filter, a container with an
//! unusual rlimit, a kernel that disagrees), the walk still runs and the user
//! still gets their sizes, a little less politely. That is a debug line, not an
//! error: there is nothing for anyone to do about it.
//!
//! Deliberately one-way. There is no `raise` here, because raising a niceness
//! back down *does* need a capability on Linux, and an API that half works is
//! worse than one that says what it does.

/// How much a background worker steps aside, in nice units.
///
/// 10 is half the range and the number `nice(1)` uses when you do not give it
/// one. Under Linux's CFS a +10 thread gets roughly a tenth of the CPU share of
/// a nice-0 thread when the two compete — so a du walk still finishes in about
/// the time it would have on an idle machine, and gets out of the way entirely
/// on a busy one. Higher (19) starves the walk on a loaded laptop for no
/// visible gain; lower is not really stepping aside.
pub const BACKGROUND_NICE: i32 = 10;

/// Step the **calling thread** down to `nice`, so it loses to the UI thread
/// whenever the two compete. Returns whether the kernel took it.
///
/// Call it as the first thing a worker does, once — the value sticks for the
/// life of the thread.
#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
pub fn be_background(nice: i32) -> bool {
    // SAFETY: `setpriority` takes three scalars and writes nothing back. `who`
    // of 0 means "the caller", which under Linux's thread model is this thread
    // and not the whole process.
    let ok = unsafe { libc::setpriority(libc::PRIO_PROCESS, 0, nice) } == 0;
    if !ok {
        log::debug!(
            "could not set thread niceness to {nice}: {}",
            std::io::Error::last_os_error()
        );
    }
    ok
}

/// Everywhere else this is a no-op that says so, so a caller does not have to
/// carry its own `cfg`.
#[cfg(not(target_os = "linux"))]
pub fn be_background(_nice: i32) -> bool {
    false
}

/// [`be_background`] at [`BACKGROUND_NICE`] — what every caller in the
/// workspace actually wants.
pub fn step_aside() -> bool {
    be_background(BACKGROUND_NICE)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lowering a priority is unprivileged, so on the machines this is built
    /// for it works. The test asserts the *call*, not a number — reading the
    /// value back would need `getpriority` and would only be restating the
    /// line above it.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_thread_can_step_aside() {
        // In its own thread, so the test runner's other threads keep their
        // priority — niceness is per thread and does not come back.
        let handle = std::thread::spawn(|| (step_aside(), be_background(BACKGROUND_NICE + 5)));
        let (first, second) = handle.join().expect("worker thread");
        assert!(first, "setpriority refused a step down");
        assert!(second, "a second step down should also be allowed");
    }
}
