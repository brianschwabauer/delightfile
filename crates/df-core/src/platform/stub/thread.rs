//! No per-thread niceness: stands in on macOS until M2.5 (a QoS class, which
//! changes scheduling class rather than niceness and needs a live check) and
//! on Windows. `setpriority(PRIO_PROCESS, 0, …)` on macOS would nice the
//! whole process, UI thread included, which is the opposite of the point.

/// Nothing is changed; returns `false`, "not taken", which no caller treats as
/// an error.
pub fn lower_priority(_nice: i32) -> bool {
    false
}
