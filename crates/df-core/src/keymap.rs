//! The keymap engine: one registry of `(context, chord, when-predicate) →
//! Command`.
//!
//! It exists once and feeds four surfaces (PLAN §4) — dispatch, the which-key
//! card, the `?` help browser, and the command palette — so the keymap
//! documents itself instead of drifting from three hand-written lists.
//! Contexts stack most-specific-wins; declaration order is preserved because
//! which-key ordering is editorial, not alphabetical; and the transport keys
//! `j k l [ ]` are hard-reserved (PLAN §4.3) so a user override that would take
//! them is rejected rather than silently honoured.
