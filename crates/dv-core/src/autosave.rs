//! Autosave decision logic (§8.2): every completed command marks the project
//! dirty; a flush happens after 1 s of idle. Pure — the caller supplies
//! timestamps and performs the actual `ProjectDb::save`, so this is fully
//! testable and owns no I/O.

pub const DEBOUNCE_MS: u64 = 1_000;

#[derive(Debug, Default)]
pub struct Autosave {
    /// Time of the most recent dirtying command; `None` = clean.
    dirty_at: Option<u64>,
}

impl Autosave {
    pub fn new() -> Autosave {
        Autosave::default()
    }

    /// A command completed (post-coalescing). Restarts the idle window.
    pub fn mark_dirty(&mut self, now_ms: u64) {
        self.dirty_at = Some(now_ms);
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty_at.is_some()
    }

    /// True once the project is dirty and 1 s has passed without further
    /// edits. The caller then saves and calls [`Autosave::flushed`].
    pub fn should_flush(&self, now_ms: u64) -> bool {
        matches!(self.dirty_at, Some(t) if now_ms.saturating_sub(t) >= DEBOUNCE_MS)
    }

    /// Deadline for the next flush check (for repaint scheduling); `None`
    /// when clean.
    pub fn next_deadline_ms(&self) -> Option<u64> {
        self.dirty_at.map(|t| t + DEBOUNCE_MS)
    }

    pub fn flushed(&mut self) {
        self.dirty_at = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debounce_window() {
        let mut a = Autosave::new();
        assert!(!a.should_flush(0));
        a.mark_dirty(100);
        assert!(!a.should_flush(600), "still inside the idle window");
        a.mark_dirty(900); // another edit restarts the window
        assert!(!a.should_flush(1500));
        assert!(a.should_flush(1900));
        a.flushed();
        assert!(!a.is_dirty());
        assert!(!a.should_flush(10_000));
    }
}
