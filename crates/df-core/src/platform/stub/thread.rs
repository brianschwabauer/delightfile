//! No per-thread niceness: stands in on Windows. (macOS puts the thread in a
//! QoS class instead, `platform/macos/thread.rs`.)

/// Nothing is changed; returns `false`, "not taken", which no caller treats as
/// an error.
pub fn lower_priority(_nice: i32) -> bool {
    false
}
