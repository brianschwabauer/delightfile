//! What the keys mean at the edges of a listing (PLAN §2.1, §4.1).
//!
//! This module used to hold a `Pane` enum and the machinery that moved the
//! keyboard between the three miller columns. **It does not any more: the list
//! is the keyboard, always.** A file manager is a list of files with two
//! columns of context beside it, and a three-way focus meant every key had to
//! be read twice — once for what it does, once for where you were when you
//! pressed it. The columns beside the list are still fully live under the
//! pointer (click a parent row to go there, wheel or scrub the preview), and
//! the preview's own keys are now modified keys that act on whatever the cursor
//! is standing on (PLAN §4.3) — so nothing was lost except the question.
//!
//! What is left is the two *decisions* that were about focus and are now about
//! the list: what `→` does at the right-hand edge of a row, and what `Esc`
//! does. Both live here rather than as a `match` in [`crate::app`] because a
//! decision that can be tested without a window is a decision that stays right.

/// What the cursor is standing on, as far as `→` is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hovered {
    Directory,
    File,
    /// An empty (or wholly filtered) listing: there is nothing to go right into.
    Nothing,
}

/// What `→` does from the list (PLAN §2.1's "rightward").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rightward {
    /// yazi's `enter`: a directory is a place, and `→` goes there.
    Enter,
    /// A file has no inside to enter — **unless it is an archive**, which
    /// PLAN §7.3 says is a place after all. Only the caller knows whether the
    /// archive reader can reach this particular file (not inside another
    /// archive, not on a remote service), so the last word is its.
    ///
    /// When it is not an archive, `→` does nothing at all. Deliberately not
    /// "open": `→` is a navigation key, and a navigation key that could launch
    /// a video player is a key you stop pressing.
    MaybeArchive,
    Nothing,
}

pub fn rightward(hovered: Hovered) -> Rightward {
    match hovered {
        Hovered::Directory => Rightward::Enter,
        Hovered::File => Rightward::MaybeArchive,
        Hovered::Nothing => Rightward::Nothing,
    }
}

/// Everything `Esc` looks at, in one struct so the ladder is a function.
#[derive(Debug, Clone, Copy, Default)]
pub struct EscapeState {
    /// A modal card — confirm, conflict, opener picker, task panel.
    pub overlay_open: bool,
    /// A chord is half-typed (`g`, `m`, `,`).
    pub chord_pending: bool,
    pub prompt_open: bool,
    /// The help overlay is open **and** something has been typed into its
    /// filter. Its own rung, above closing the overlay: the query is the more
    /// recent thing, and the binding text promises "clear the filter, or
    /// close".
    pub help_filter: bool,
    pub help_open: bool,
    pub visual: bool,
    pub selection: bool,
    pub filter: bool,
}

/// One rung of the `Esc` ladder (PLAN §4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EscapeRung {
    CloseOverlay,
    CancelChord,
    ClosePrompt,
    ClearHelpFilter,
    CloseHelp,
    LeaveVisual,
    ClearFilter,
    ClearSelection,
    /// Everything is already put away.
    Nothing,
}

/// **One rung at a time**, nearest thing first.
///
/// The order is the plan's, and it is an order rather than a set for a reason:
/// an `Esc` that cleared everything means a stray press throws away a selection
/// built up over a dozen keystrokes, and there is no undo for a selection. The
/// modal card is checked first because it is the nearest thing to the user's
/// eye.
///
/// Two rungs are ordered by how *recent* the thing is rather than by how big:
/// a help filter is undone before the overlay it narrows, and a file filter is
/// undone before a selection. The filter is the thing you typed a second ago
/// and can see in the bar; the selection may be a dozen `Space`s old and is
/// the one state in the program with no undo, so it is the later rung of the
/// two — `Esc` on a filtered listing must not throw it away.
pub fn escape_rung(state: EscapeState) -> EscapeRung {
    if state.overlay_open {
        return EscapeRung::CloseOverlay;
    }
    if state.chord_pending {
        return EscapeRung::CancelChord;
    }
    if state.prompt_open {
        return EscapeRung::ClosePrompt;
    }
    if state.help_filter {
        return EscapeRung::ClearHelpFilter;
    }
    if state.help_open {
        return EscapeRung::CloseHelp;
    }
    if state.visual {
        return EscapeRung::LeaveVisual;
    }
    if state.filter {
        return EscapeRung::ClearFilter;
    }
    if state.selection {
        return EscapeRung::ClearSelection;
    }
    EscapeRung::Nothing
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `→` on a directory enters it (yazi, unchanged); `→` on a *file* is the
    /// caller's question, because the only file `→` may go into is an archive.
    #[test]
    fn rightward_enters_a_directory_and_asks_about_a_file() {
        assert_eq!(rightward(Hovered::Directory), Rightward::Enter);
        assert_eq!(rightward(Hovered::File), Rightward::MaybeArchive);
        // An empty listing has nothing to the right at all.
        assert_eq!(rightward(Hovered::Nothing), Rightward::Nothing);
    }

    /// **The transport does not care what is on screen** (PLAN §4.3): the only
    /// predicate left is what the cursor is standing on, and it is the same
    /// answer wherever the pointer happens to be.
    #[test]
    fn the_only_predicate_left_is_what_is_hovered() {
        use df_core::keymap::{When, WhenFlags};
        assert!(WhenFlags::MEDIA.allows(When::MediaHovered));
        assert!(!WhenFlags::NONE.allows(When::MediaHovered));
        assert!(WhenFlags::NONE.allows(When::Always));
    }

    /// **The preview's keys, through the real registry.** The list keeps every
    /// plain key it ever had, and the preview's versions are the modified ones
    /// — which is the whole of PLAN §2.1's simplification, end to end.
    #[test]
    fn the_list_keeps_the_plain_keys_and_the_preview_takes_the_modified_ones() {
        use df_core::keymap::{Command, ContextStack, Dispatch, KeymapState, Registry, WhenFlags};
        let registry = Registry::defaults();
        let stack = ContextStack::browser();
        let dispatch = |media: bool, key: &str| {
            let mut state = KeymapState::new();
            let chord = df_core::keymap::parse_chord(key).expect("chord");
            let flags = if media {
                WhenFlags::MEDIA
            } else {
                WhenFlags::NONE
            };
            match registry.dispatch(&mut state, &stack, flags, chord, std::time::Instant::now()) {
                Dispatch::Match(command) => Some(command),
                _ => None,
            }
        };

        // The four keys that used to mean two things each now mean one, and it
        // is the list's meaning — whatever is in the preview pane.
        assert_eq!(dispatch(true, "."), Some(Command::ToggleHidden));
        assert_eq!(dispatch(true, "space"), Some(Command::ToggleSelect));
        assert_eq!(dispatch(true, "up"), Some(Command::CursorUp));
        assert_eq!(dispatch(true, "down"), Some(Command::CursorDown));
        // `m` is still the linemode chord and nothing else — it does not
        // complete, it waits for its second key, even on a clip.
        assert_eq!(dispatch(true, "m"), None);
        assert_eq!(dispatch(true, "ctrl+m"), Some(Command::Mute));
        assert_eq!(dispatch(false, "ctrl+m"), None);

        // Ctrl+arrow is the preview's arrow family, and the sideways pair
        // splits on what is under the cursor exactly as the transport does.
        assert_eq!(dispatch(false, "ctrl+up"), Some(Command::PreviewUp));
        assert_eq!(dispatch(true, "ctrl+up"), Some(Command::PreviewUp));
        assert_eq!(dispatch(false, "ctrl+right"), Some(Command::PreviewRight));
        assert_eq!(
            dispatch(true, "ctrl+right"),
            Some(Command::FrameStepForward)
        );
        assert_eq!(dispatch(true, "ctrl+left"), Some(Command::FrameStepBack));
        assert_eq!(
            dispatch(false, "shift+space"),
            Some(Command::PreviewPageDown)
        );
        // The zoom family is on Ctrl, and the bare keys are the view-scale
        // ladder — the same split, one more time: a key you press on the thing
        // you are looking *at* keeps the plain spelling.
        assert_eq!(dispatch(false, "ctrl+-"), Some(Command::PreviewZoomOut));
        assert_eq!(dispatch(false, "ctrl+="), Some(Command::PreviewZoomIn));
        assert_eq!(dispatch(false, "ctrl+0"), Some(Command::PreviewZoomReset));
        assert_eq!(dispatch(false, "-"), Some(Command::ViewScaleDown));
        assert_eq!(dispatch(false, "="), Some(Command::ViewScaleUp));

        // **And the transport is still global** (PLAN §4.3): `k` plays the file
        // the cursor is on, and is inert on anything else — no beep, no
        // surprise, and nothing on the help sheet either.
        assert_eq!(dispatch(true, "k"), Some(Command::PlayPause));
        assert_eq!(dispatch(true, "l"), Some(Command::ShuttleForward));
        assert_eq!(dispatch(true, "j"), Some(Command::ShuttleReverse));
        assert_eq!(dispatch(true, "]"), Some(Command::NextEdge));
        assert_eq!(dispatch(true, "shift+up"), Some(Command::VolumeUp));
        assert_eq!(dispatch(false, "k"), None);
        assert_eq!(dispatch(false, "j"), None);
        assert_eq!(dispatch(false, "]"), None);
        // `K`/`J` stay the list's nudge of the preview, which is where yazi
        // put them.
        assert_eq!(dispatch(true, "K"), Some(Command::SeekPreviewUp));
    }

    /// The ladder, from the top: every rung is reached in order and none is
    /// skipped, **including with a dialog up** — a confirm dialog is the
    /// nearest thing to the user and `Esc` belongs to it before it belongs to a
    /// half-typed chord or a selection underneath it.
    #[test]
    fn escape_climbs_down_one_rung_at_a_time() {
        let mut state = EscapeState {
            overlay_open: true,
            chord_pending: true,
            prompt_open: true,
            help_filter: true,
            help_open: true,
            visual: true,
            selection: true,
            filter: true,
        };
        let order = [
            EscapeRung::CloseOverlay,
            EscapeRung::CancelChord,
            EscapeRung::ClosePrompt,
            EscapeRung::ClearHelpFilter,
            EscapeRung::CloseHelp,
            EscapeRung::LeaveVisual,
            EscapeRung::ClearFilter,
            EscapeRung::ClearSelection,
            EscapeRung::Nothing,
        ];
        for expected in order {
            let rung = escape_rung(state);
            assert_eq!(rung, expected);
            match rung {
                EscapeRung::CloseOverlay => state.overlay_open = false,
                EscapeRung::CancelChord => state.chord_pending = false,
                EscapeRung::ClosePrompt => state.prompt_open = false,
                EscapeRung::ClearHelpFilter => state.help_filter = false,
                EscapeRung::CloseHelp => state.help_open = false,
                EscapeRung::LeaveVisual => state.visual = false,
                EscapeRung::ClearSelection => state.selection = false,
                EscapeRung::ClearFilter => state.filter = false,
                EscapeRung::Nothing => {}
            }
        }
    }

    /// A committed filter — `f`, type, `Enter` — is undone by `Esc` before the
    /// selection underneath it, and a help filter before the help itself.
    #[test]
    fn a_committed_filter_is_undone_before_the_selection() {
        assert_eq!(
            escape_rung(EscapeState {
                filter: true,
                selection: true,
                ..EscapeState::default()
            }),
            EscapeRung::ClearFilter
        );
        assert_eq!(
            escape_rung(EscapeState {
                help_open: true,
                help_filter: true,
                ..EscapeState::default()
            }),
            EscapeRung::ClearHelpFilter
        );
        assert_eq!(
            escape_rung(EscapeState {
                help_open: true,
                ..EscapeState::default()
            }),
            EscapeRung::CloseHelp
        );
        // With nothing open and nothing marked, `Esc` is a no-op — it does not
        // reach for something to undo (PLAN §4.1).
        assert_eq!(escape_rung(EscapeState::default()), EscapeRung::Nothing);
    }
}
