//! Which pane the keyboard is in (PLAN §2.1 — the DelightMail model).
//!
//! A dumb reactive enum and three pure functions over it. It lives in its own
//! module rather than as a field and a `match` in [`crate::app`] because every
//! interesting thing about focus is a *decision* — what `→` means, what `Esc`
//! means, which bindings are live — and a decision that can be tested without a
//! window is a decision that stays right.
//!
//! **Focus does not change the keymap context stack.** That is PLAN §2.1's
//! rule, and the reason df-core's registry takes [`WhenFlags`]: the same `↑` is
//! a cursor move in the list, a scroll in the preview and a parent-pane step,
//! and it is one binding table with three predicates rather than three stacks
//! that have to be pushed and popped in the right order.
//!
//! With `hjkl` gone (PLAN's one deliberate break from yazi), the transport keys
//! are global and need no focus at all. Preview focus exists for the *rest*:
//! document scrolling, image pan/zoom, and the secondary transport keys whose
//! letters the list still owns — `,` is the sort chord, `.` toggles hidden
//! files, `m` starts the linemode chord, and in the preview all three are the
//! frame step and the mute.

use std::time::{Duration, Instant};

use df_core::keymap::WhenFlags;

/// Which pane the keyboard is in. `List` is where a file manager lives, so it
/// is the default and every ladder ends there.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Focus {
    Parent,
    #[default]
    List,
    Preview,
}

impl Focus {
    /// Which `when` predicates are true, given what is under the cursor.
    ///
    /// `media_hovered` is deliberately *not* about focus: PLAN §4.3's whole
    /// point is that `k` plays the file the cursor is on whatever pane has the
    /// keyboard. What focus decides is only whether `,`, `.`, `m`, `Space` and
    /// the plain arrows read as the transport's or as the list's.
    pub fn flags(self, media_hovered: bool) -> WhenFlags {
        WhenFlags {
            in_list: self == Focus::List,
            in_preview: self == Focus::Preview,
            in_parent: self == Focus::Parent,
            media_hovered,
        }
    }
}

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
    /// A file has no inside to enter, so `→` goes to the thing that is *showing*
    /// it — which is the pane it is already being drawn in.
    FocusPreview,
    Nothing,
}

pub fn rightward(hovered: Hovered) -> Rightward {
    match hovered {
        Hovered::Directory => Rightward::Enter,
        Hovered::File => Rightward::FocusPreview,
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
    pub focus: Focus,
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
    FocusList,
    /// Everything is already put away and the list already has the keyboard.
    Nothing,
}

/// **One rung at a time**, nearest thing first.
///
/// The order is the plan's, and it is an order rather than a set for a reason:
/// an `Esc` that cleared everything means a stray press throws away a selection
/// built up over a dozen keystrokes, and there is no undo for a selection. The
/// modal card is checked first because it is the nearest thing to the user's
/// eye, and focus is checked *last* because returning to the list is the rung
/// that costs nothing to redo.
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
    if state.focus != Focus::List {
        return EscapeRung::FocusList;
    }
    EscapeRung::Nothing
}

/// How long the focus treatment takes to move between panes.
///
/// PLAN §2.1 says 120 ms ease-out, which is also [`crate::flip`]'s,
/// [`crate::menu`]'s and [`crate::whichkey`]'s state fade — one number for
/// "a piece of chrome changed what it means", so the window never has two
/// speeds for the same kind of event.
pub const FOCUS_FADE: Duration = Duration::from_millis(120);

/// Where the focus treatment *is*, as opposed to where focus is.
///
/// [`Focus`] commits the instant the key is pressed — `delightful-ui` §5's
/// "state commits instantly, animation is presentation only" — and this is the
/// presentation catching up. Without it the 4% tint, the 2 px rule and the
/// ghost-cursor dim all pop on and off, and in a three-column miller layout
/// that is the most frequent transition in the program.
///
/// Both directions are eased, not just the outro. This is not a hover: nothing
/// is under a pointer that could leave, and a tint that snapped on while the
/// old one faded would read as the two panes disagreeing about which of them
/// had the keyboard.
/// The default is [`Focus`]'s own default, already arrived — a window that has
/// just opened is not mid-fade.
#[derive(Debug, Clone, Copy, Default)]
pub struct FocusFade {
    /// Where the treatment is heading.
    to: Focus,
    /// Where it is coming from, and when it left. `None` once it has arrived —
    /// an `Option` that is never `None` is a window that never stops asking for
    /// frames (PLAN §1).
    from: Option<(Focus, Instant)>,
}

impl FocusFade {
    pub fn new() -> FocusFade {
        FocusFade::default()
    }

    /// Point the fade at `focus`, and retire a fade that has arrived.
    ///
    /// Called every frame. A focus change *during* a fade retargets from
    /// wherever the treatment currently is rather than restarting from the
    /// original pane — `delightful-ui` §5's "every animation is interruptible
    /// and retargets mid-flight".
    pub fn tick(&mut self, focus: Focus, now: Instant) {
        if focus != self.to {
            self.from = Some((self.to, now));
            self.to = focus;
        } else if self
            .from
            .is_some_and(|(_, at)| now.saturating_duration_since(at) >= FOCUS_FADE)
        {
            self.from = None;
        }
    }

    /// How focused `pane` looks right now, `0.0..=1.0`.
    pub fn amount(&self, pane: Focus, now: Instant) -> f32 {
        let Some((from, at)) = self.from else {
            return if pane == self.to { 1.0 } else { 0.0 };
        };
        let t = (now.saturating_duration_since(at).as_secs_f32()
            / FOCUS_FADE.as_secs_f32().max(f32::EPSILON))
        .clamp(0.0, 1.0);
        let eased = crate::motion::Easing::OutQuint.apply(t);
        if pane == self.to {
            eased
        } else if pane == from {
            1.0 - eased
        } else {
            0.0
        }
    }

    /// Whether the treatment is still moving. A settled focus is a constant,
    /// and asking for frames to redraw a constant is PLAN §1's whole complaint.
    pub fn animating(&self, now: Instant) -> bool {
        self.from
            .is_some_and(|(_, at)| now.saturating_duration_since(at) < FOCUS_FADE)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// PLAN §2.1's treatment, and PLAN §1's idle rule in the same test: the
    /// amounts cross over, they add up to 1 the whole way across, and the fade
    /// stops asking for frames the moment it has arrived.
    #[test]
    fn the_focus_treatment_crosses_over_and_then_stops() {
        let t0 = Instant::now();
        let mut fade = FocusFade::new();
        fade.tick(Focus::List, t0);
        assert_eq!(fade.amount(Focus::List, t0), 1.0);
        assert_eq!(fade.amount(Focus::Preview, t0), 0.0);
        assert!(!fade.animating(t0));

        // Focus commits instantly; the picture starts moving.
        fade.tick(Focus::Preview, t0);
        assert!(fade.animating(t0));
        let mid = t0 + FOCUS_FADE / 2;
        let (leaving, arriving) = (
            fade.amount(Focus::List, mid),
            fade.amount(Focus::Preview, mid),
        );
        assert!(arriving > 0.0 && arriving < 1.0, "{arriving}");
        // One pane arrives at exactly the rate the other leaves, so the window
        // never looks like two panes both have the keyboard, or neither does.
        assert!((leaving + arriving - 1.0).abs() < 1e-5);
        // The pane that was never involved stays out of it.
        assert_eq!(fade.amount(Focus::Parent, mid), 0.0);

        // Arrived: a settled keyboard is a constant, and a constant costs no
        // frames (PLAN §1).
        let done = t0 + FOCUS_FADE;
        assert!(!fade.animating(done));
        fade.tick(Focus::Preview, done);
        assert_eq!(fade.amount(Focus::Preview, done), 1.0);
        assert_eq!(fade.amount(Focus::List, done), 0.0);
        assert!(!fade.animating(done));
    }

    /// `delightful-ui` §5: an animation retargets mid-flight rather than
    /// restarting. A change of mind halfway across must not snap the treatment
    /// back to the pane it originally left.
    #[test]
    fn a_focus_change_mid_fade_retargets_rather_than_restarting() {
        let t0 = Instant::now();
        let mut fade = FocusFade::new();
        fade.tick(Focus::List, t0);
        fade.tick(Focus::Preview, t0);
        let mid = t0 + FOCUS_FADE / 2;
        // Off to the parent instead, from wherever the treatment now is.
        fade.tick(Focus::Parent, mid);
        assert_eq!(fade.amount(Focus::Parent, mid), 0.0);
        // The pane it *was* heading for is what it now fades away from — not
        // the one it started at, which is already gone.
        assert_eq!(fade.amount(Focus::Preview, mid), 1.0);
        assert_eq!(fade.amount(Focus::List, mid), 0.0);
        let done = mid + FOCUS_FADE;
        assert_eq!(fade.amount(Focus::Parent, done), 1.0);
        assert!(!fade.animating(done));
    }

    /// The ghost bar is part of the same statement as the tint and the rule,
    /// so it travels with them instead of popping (PLAN §2.1).
    #[test]
    fn the_ghost_cursor_rides_the_focus_fade() {
        assert_eq!(crate::ui::ghost_cursor(0.0), crate::ui::GHOST_CURSOR);
        assert_eq!(crate::ui::ghost_cursor(1.0), 1.0);
        let half = crate::ui::ghost_cursor(0.5);
        assert!(half > crate::ui::GHOST_CURSOR && half < 1.0, "{half}");
    }

    /// `→` on a directory enters it (yazi, unchanged); `→` on a *file* moves
    /// the keyboard into the pane already showing it.
    #[test]
    fn rightward_enters_a_directory_and_focuses_a_file() {
        assert_eq!(rightward(Hovered::Directory), Rightward::Enter);
        assert_eq!(rightward(Hovered::File), Rightward::FocusPreview);
        // An empty listing has nothing to the right, and `→` must not focus a
        // pane that is showing nothing.
        assert_eq!(rightward(Hovered::Nothing), Rightward::Nothing);
    }

    /// The predicates are exclusive by construction: exactly one pane has the
    /// keyboard, so exactly one of the three pane flags is true.
    #[test]
    fn exactly_one_pane_flag_is_true_at_a_time() {
        for focus in [Focus::Parent, Focus::List, Focus::Preview] {
            let f = focus.flags(false);
            let count = [f.in_list, f.in_preview, f.in_parent]
                .iter()
                .filter(|on| **on)
                .count();
            assert_eq!(count, 1, "{focus:?}");
        }
        assert_eq!(Focus::default(), Focus::List);
        assert_eq!(Focus::List.flags(false), WhenFlags::LIST);
    }

    /// **The transport does not care where the keyboard is** (PLAN §4.3): `k`
    /// on a hovered clip works from any pane, which is the whole reason `hjkl`
    /// was given up.
    #[test]
    fn media_hovered_is_independent_of_focus() {
        for focus in [Focus::Parent, Focus::List, Focus::Preview] {
            assert!(focus.flags(true).media_hovered, "{focus:?}");
            assert!(!focus.flags(false).media_hovered, "{focus:?}");
        }
        // And the preview-focus extras need both halves.
        use df_core::keymap::When;
        assert!(Focus::Preview.flags(true).allows(When::PreviewMedia));
        assert!(!Focus::Preview.flags(false).allows(When::PreviewMedia));
        assert!(!Focus::List.flags(true).allows(When::PreviewMedia));
        // …while a document in preview focus keeps the plain preview rows.
        assert!(Focus::Preview.flags(false).allows(When::InPreview));
    }

    /// **The flags, through the real registry.** The predicates are only worth
    /// anything if the keymap actually resolves them, and this is the wiring
    /// end to end: one chord, three answers, decided entirely by focus and by
    /// what is under the cursor.
    #[test]
    fn the_same_key_means_three_things_and_the_registry_agrees() {
        use df_core::keymap::{Command, ContextStack, Dispatch, KeymapState, Registry};
        let registry = Registry::defaults();
        let stack = ContextStack::browser();
        let dispatch = |focus: Focus, media: bool, key: &str| {
            let mut state = KeymapState::new();
            let chord = df_core::keymap::parse_chord(key).expect("chord");
            match registry.dispatch(
                &mut state,
                &stack,
                focus.flags(media),
                chord,
                std::time::Instant::now(),
            ) {
                Dispatch::Match(command) => Some(command),
                _ => None,
            }
        };

        // `.` is the hidden-files toggle in the list and the frame step in the
        // preview — and only when there is something with frames in it.
        assert_eq!(
            dispatch(Focus::List, true, "."),
            Some(Command::ToggleHidden)
        );
        assert_eq!(
            dispatch(Focus::Preview, true, "."),
            Some(Command::FrameStepForward)
        );
        assert_eq!(dispatch(Focus::Preview, false, "."), None);
        // `Space` selects, plays, or pages.
        assert_eq!(
            dispatch(Focus::List, false, "space"),
            Some(Command::ToggleSelect)
        );
        assert_eq!(
            dispatch(Focus::Preview, true, "space"),
            Some(Command::PlayPause)
        );
        assert_eq!(
            dispatch(Focus::Preview, false, "space"),
            Some(Command::PreviewPageDown)
        );
        // `↑` moves the cursor, the parent's cursor, the document, or the
        // volume — four readings of one key, and never two at once.
        assert_eq!(dispatch(Focus::List, false, "up"), Some(Command::CursorUp));
        assert_eq!(
            dispatch(Focus::Parent, false, "up"),
            Some(Command::ParentPrev)
        );
        assert_eq!(
            dispatch(Focus::Preview, false, "up"),
            Some(Command::PreviewUp)
        );
        assert_eq!(
            dispatch(Focus::Preview, true, "up"),
            Some(Command::VolumeUp)
        );

        // **And the transport is global** (PLAN §4.3): `k` plays from any pane
        // when the cursor is on a clip, and is inert on anything else — no
        // beep, no surprise, and nothing on the help sheet either.
        for focus in [Focus::Parent, Focus::List, Focus::Preview] {
            assert_eq!(dispatch(focus, true, "k"), Some(Command::PlayPause));
            assert_eq!(dispatch(focus, true, "l"), Some(Command::ShuttleForward));
            assert_eq!(dispatch(focus, true, "j"), Some(Command::ShuttleReverse));
            assert_eq!(dispatch(focus, true, "]"), Some(Command::NextEdge));
            assert_eq!(dispatch(focus, true, "shift+up"), Some(Command::VolumeUp));
            assert_eq!(dispatch(focus, false, "k"), None, "{focus:?}");
            assert_eq!(dispatch(focus, false, "j"), None, "{focus:?}");
            assert_eq!(dispatch(focus, false, "]"), None, "{focus:?}");
        }
        // `K`/`J` stay the list's, which is where yazi put them.
        assert_eq!(
            dispatch(Focus::List, true, "K"),
            Some(Command::SeekPreviewUp)
        );
        assert_eq!(dispatch(Focus::Preview, true, "K"), None);
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
            focus: Focus::Preview,
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
            EscapeRung::FocusList,
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
                EscapeRung::FocusList => state.focus = Focus::List,
                EscapeRung::Nothing => {}
            }
        }
    }

    /// The common case: nothing is open, the keyboard is in the preview, and
    /// `Esc` is the way back — one press, no side effects on the selection.
    #[test]
    fn escape_from_the_preview_only_moves_the_keyboard() {
        let state = EscapeState {
            focus: Focus::Preview,
            ..EscapeState::default()
        };
        assert_eq!(escape_rung(state), EscapeRung::FocusList);
        assert_eq!(
            escape_rung(EscapeState {
                focus: Focus::Parent,
                ..EscapeState::default()
            }),
            EscapeRung::FocusList
        );
        assert_eq!(escape_rung(EscapeState::default()), EscapeRung::Nothing);
        // A selection is worth more than a focus change, so it is asked about
        // first even when the keyboard is somewhere else.
        assert_eq!(
            escape_rung(EscapeState {
                focus: Focus::Preview,
                selection: true,
                ..EscapeState::default()
            }),
            EscapeRung::ClearSelection
        );
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
    }
}
