//! Where a worker thread sits in the scheduler's queue (PLAN §1's idle
//! discipline, from the other end).
//!
//! Every worker in this program exists so the UI thread does not have to do its
//! work — but a machine with four cores and thirty ready threads will happily
//! preempt the one thread a person can *see*. Niceness is the cheapest fix:
//! the workers keep every core they can get while nothing is being asked of
//! the window, and they yield the moment a keystroke needs one.
//!
//! Best effort by construction. `setpriority` can only ever *lower* a
//! thread's priority for an unprivileged process, which is all this asks for;
//! if the kernel refuses (a container with a locked-down rlimit) the thread
//! simply runs at the default and nothing else changes.

/// Lower the **calling thread**'s scheduling priority to `nice` (1–19; higher
/// is nicer, 0 is the default and negative values need privileges we do not
/// have and do not want).
///
/// On Linux, the only platform where `setpriority(PRIO_PROCESS, 0, …)` means
/// *this thread* rather than the whole process, that is the nice value; on
/// macOS it is the thread's QoS class, utility for every nice level; on
/// Windows nothing changes ([`crate::platform::thread`]). Call it from inside
/// the thread it is meant for; calling it from the spawner would nice the
/// spawner.
///
/// Returns whether the kernel took it, so a caller that cares can log. Nothing
/// in the program treats a refusal as an error.
pub fn lower_priority(nice: i32) -> bool {
    // Never raise: a caller that passes 0 or a negative is asking for
    // something this function deliberately does not offer, and clamping is
    // quieter than an `Err` nobody would handle.
    let nice = nice.clamp(0, 19);
    if nice == 0 {
        return false;
    }
    crate::platform::thread::lower_priority(nice)
}

/// How nice a **bulk** worker should be: the copy/move/delete pool, twenty
/// threads deep, whose work is measured in seconds and whose progress the user
/// reads off a bar rather than feels under the cursor.
pub const NICE_BULK: i32 = 10;

/// How nice an **interactive** worker should be: preview, decode, document
/// rasterising, probing. A person is waiting on these with their eyes on the
/// pane, so they give way to the paint thread and to nothing else.
pub const NICE_INTERACTIVE: i32 = 3;

#[cfg(test)]
mod tests {
    use super::*;

    /// The clamp is the contract: this function cannot be used to make a
    /// thread *more* important, whatever it is handed.
    #[test]
    fn it_refuses_to_raise_priority() {
        assert!(!lower_priority(0));
        assert!(!lower_priority(-5));
    }

    /// And a sane request is accepted by any ordinary Linux — the test runner
    /// included. Asserted loosely: a sandbox that forbids it must not fail the
    /// suite, because the program itself does not care either.
    #[test]
    fn it_lowers_this_thread() {
        std::thread::spawn(|| {
            let _ = lower_priority(NICE_INTERACTIVE);
        })
        .join()
        .expect("the test thread panicked");
    }
}
