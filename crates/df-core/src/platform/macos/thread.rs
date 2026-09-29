//! A worker thread's place in the scheduler, as macOS has it: a quality of
//! service class rather than a nice value.
//!
//! `setpriority(PRIO_PROCESS, 0, …)`, which is a thread on Linux, is the whole
//! process here, window and all — the opposite of the point. What macOS gives
//! a thread instead is a QoS class, and the one for work a person started and
//! is not waiting on frame by frame is `QOS_CLASS_UTILITY`: the scheduler
//! gives it way to the window's thread (`USER_INTERACTIVE`) and, on Apple
//! Silicon, may run it on the efficiency cores. Every nice level the program
//! asks for (1–19) is that one class; a finer ladder than "not the window"
//! would be precision macOS does not offer through this call.
//!
//! A class can only be lowered from a thread's own default for an
//! unprivileged process, which is all this asks for.

/// Put the calling thread in `QOS_CLASS_UTILITY`. `nice` is 1–19
/// ([`crate::thread::lower_priority`] clamps it), and every value means the
/// same class. Returns whether the kernel took it.
#[allow(unsafe_code)]
pub fn lower_priority(nice: i32) -> bool {
    // SAFETY: two scalars; the call changes only the calling thread's class.
    let rc =
        unsafe { libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_UTILITY, 0) };
    if rc != 0 {
        log::debug!(
            "could not lower this thread to utility for nice {nice}: {}",
            std::io::Error::from_raw_os_error(rc)
        );
    }
    rc == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The class this thread is in, as the system reports it.
    #[allow(unsafe_code)]
    fn class_of_this_thread() -> u32 {
        let mut class = libc::qos_class_t::QOS_CLASS_UNSPECIFIED;
        let mut relative = 0;
        // SAFETY: two live out-pointers for the call to write through, and
        // `pthread_self` names the calling thread.
        let rc = unsafe {
            libc::pthread_get_qos_class_np(libc::pthread_self(), &mut class, &mut relative)
        };
        assert_eq!(rc, 0);
        class as u32
    }

    /// The call is taken, and the thread is in the utility class after it.
    #[test]
    fn a_worker_is_put_in_the_utility_class() {
        std::thread::spawn(|| {
            assert!(lower_priority(crate::thread::NICE_BULK));
            assert_eq!(
                class_of_this_thread(),
                libc::qos_class_t::QOS_CLASS_UTILITY as u32
            );
        })
        .join()
        .expect("the test thread panicked");
    }
}
