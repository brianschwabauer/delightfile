//! FLIP re-sorts: rows and tiles travel to their new places instead of
//! teleporting (PLAN §2, §8).
//!
//! Press `, m` and the directory re-sorts by modified time. Without this, forty
//! rows are somewhere else on the next frame and the only way to find the file
//! you were looking at is to read the whole column again. With it, you *watch*
//! it move, and the sort is a thing that happened to a list you already knew
//! rather than a new list that replaced it.
//!
//! The name is the web technique, and the mechanics are the same three steps:
//!
//! - **First** — before the change, record where everything is.
//! - **Last** — after it, work out where everything now is.
//! - **Invert & Play** — draw each item at `new + (old - new) × (1 − eased t)`,
//!   so frame one puts everything exactly where it already was and the
//!   animation carries it to the truth.
//!
//! Two properties that follow from writing it that way, and both matter here:
//! the **state has already committed** before a pixel moves (`delightful-ui`
//! §5 — the cursor is on the right row the instant the key lands, whatever the
//! rows are doing), and the whole thing is a [`Tween`] sampled at an `Instant`,
//! so it is pure, interruptible, and stops asking for frames the moment it
//! arrives (PLAN §1).
//!
//! ## What it is deliberately not for
//!
//! **Navigation.** Entering a directory replaces every row with an unrelated
//! row, and animating between two different listings is animating between two
//! different things — it would read as the rows melting rather than moving.
//! Only a *same-directory* reorder animates, which is what [`reorders`] is: a
//! small, tested list of the commands that shuffle rows you are already looking
//! at.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use df_core::keymap::Command;

use crate::motion::{Easing, Tween};

/// How long a row takes to reach its new place.
///
/// 220 ms on `OutQuint`. Shorter than about 180 and the eye cannot follow a row
/// that crossed half the pane — which is the whole point of the animation —
/// while past about 260 the list is still settling when the next `,` chord
/// arrives and the column feels sticky. Because `OutQuint` is front-loaded,
/// the rows are visually in place after roughly 80 ms and the rest is the
/// settle.
pub const TRAVEL: Duration = Duration::from_millis(220);

/// How long a row that appeared or vanished takes to fade.
///
/// 120 ms, delightviewer's state-fade number (PLAN §8). A row that has no old
/// position cannot travel, so it fades — and it does so faster than the
/// travelling rows so that the movement is what the eye follows and the
/// arrivals are texture around it.
pub const FADE: Duration = Duration::from_millis(120);

/// The furthest a row is allowed to appear to have come from, in points.
///
/// A row that was three hundred rows above the fold has a mathematically
/// correct old position a screen and a half away, and honouring it would fling
/// something across the pane at enormous speed to say nothing. Clamping the
/// *displacement* — not dropping the row — keeps it moving in the right
/// direction from just off the edge, which is what the eye reads as "it came
/// from up there" anyway. 600 points is a little under a full pane at the sizes
/// this program runs at.
pub const MAX_TRAVEL: f32 = 600.0;

/// Where everything was, or is: a key to a rectangle.
///
/// Keyed by path rather than by index, because index is the thing that
/// *changed*. Only the rows the frame actually laid out are in here — the
/// visible ones plus the margin the painter draws either side of them — which
/// is also the cap on how many participants an animation can have.
pub type Snapshot = HashMap<PathBuf, egui::Rect>;

/// One re-sort in flight.
pub struct Flip {
    /// Per key, how far it has to travel: `old.min - new.min`. Applied as a
    /// *displacement* so the caller's layout stays the source of truth and this
    /// only ever offsets it.
    travel: HashMap<PathBuf, egui::Vec2>,
    /// Keys that were not there before, and are now.
    entering: Vec<PathBuf>,
    /// Where the keys that vanished used to be, so they can be drawn fading out
    /// of the place they left.
    leaving: Vec<(PathBuf, egui::Rect)>,
    tween: Tween,
    fade: Tween,
}

impl Flip {
    /// The animation from `before` to `after`, or `None` when nothing moved.
    ///
    /// `None` rather than a zero-length animation, so the caller's `Option`
    /// stays empty and the window is not asked for a frame to draw a list that
    /// is already correct.
    pub fn begin(before: &Snapshot, after: &Snapshot, now: Instant) -> Option<Flip> {
        let mut travel = HashMap::new();
        for (key, new) in after {
            let Some(old) = before.get(key) else {
                continue;
            };
            let delta = clamp_travel(old.min - new.min);
            // Sub-pixel movement is a resize, not a re-sort, and animating it
            // would mean the list quietly asking for frames after every window
            // change.
            if delta.length() < 0.5 {
                continue;
            }
            travel.insert(key.clone(), delta);
        }
        let entering: Vec<PathBuf> = after
            .keys()
            .filter(|key| !before.contains_key(*key))
            .cloned()
            .collect();
        let leaving: Vec<(PathBuf, egui::Rect)> = before
            .iter()
            .filter(|(key, _)| !after.contains_key(*key))
            .map(|(key, rect)| (key.clone(), *rect))
            .collect();
        if travel.is_empty() && entering.is_empty() && leaving.is_empty() {
            return None;
        }
        Some(Flip {
            travel,
            entering,
            leaving,
            tween: Tween::new(1.0, 0.0, TRAVEL, Easing::OutQuint, now),
            fade: Tween::new(0.0, 1.0, FADE, Easing::OutQuint, now),
        })
    }

    /// How far to displace `key` from where the layout says it goes.
    ///
    /// Zero for anything that did not move, which is most rows most of the
    /// time — so a caller can ask for every row it draws without branching.
    pub fn offset(&self, key: &Path, now: Instant) -> egui::Vec2 {
        let Some(delta) = self.travel.get(key) else {
            return egui::Vec2::ZERO;
        };
        *delta * self.tween.value(now)
    }

    /// How solidly to draw `key`: 1 for a row that was already there, and a
    /// fade-in for one that has just arrived.
    pub fn alpha(&self, key: &Path, now: Instant) -> f32 {
        if self.entering.iter().any(|held| held == key) {
            self.fade.value(now)
        } else {
            1.0
        }
    }

    /// The rows that are on their way out, with where they were and how solid
    /// they still are. Drawn by the caller on top of the layout, because the
    /// layout no longer has anywhere to put them.
    pub fn ghosts(&self, now: Instant) -> Vec<(&Path, egui::Rect, f32)> {
        let alpha = 1.0 - self.fade.value(now);
        if alpha <= 0.0 {
            return Vec::new();
        }
        self.leaving
            .iter()
            .map(|(key, rect)| (key.as_path(), *rect, alpha))
            .collect()
    }

    /// Is it still moving? The `animating()` half of PLAN §1's idle-cost rule:
    /// the caller drops the `Flip` the moment this goes false, and a settled
    /// list stops asking for frames.
    pub fn finished(&self, now: Instant) -> bool {
        self.tween.finished(now) && self.fade.finished(now)
    }
}

/// Keep a displacement inside [`MAX_TRAVEL`] without turning it.
///
/// Scaled rather than component-clamped: clamping x and y separately would bend
/// a diagonal, and a tile that moved down and left has to keep moving down and
/// left.
fn clamp_travel(delta: egui::Vec2) -> egui::Vec2 {
    let length = delta.length();
    if length <= MAX_TRAVEL || length == 0.0 {
        return delta;
    }
    delta * (MAX_TRAVEL / length)
}

/// Does this command reorder the rows you are already looking at?
///
/// The trigger set, and it is a *short* list on purpose. Three groups, and the
/// reasoning for each is what keeps this from growing into "animate
/// everything":
///
/// - **The sorts.** This is what FLIP is for. The rows are the same rows in a
///   different order, which is precisely the change an animation can explain.
/// - **`.`, the hidden toggle.** Rows appear and disappear among rows that
///   stay, so the ones that stay slide and the ones that arrive fade.
/// - **Nothing else.** The linemodes (`m s`, `m p`, …) change what the right
///   hand column *says* and not what order anything is in — animating them
///   would be motion attached to a change that did not move anything. Every
///   navigation command replaces the listing outright, which this module's
///   header explains is the case it must not animate. And the `f` filter runs
///   per keystroke, where a 220 ms travel on every letter is noise rather than
///   explanation.
pub fn reorders(command: Command) -> bool {
    use Command as C;
    matches!(
        command,
        C::SortMtime
            | C::SortMtimeReverse
            | C::SortBtime
            | C::SortBtimeReverse
            | C::SortExtension
            | C::SortExtensionReverse
            | C::SortAlphabetical
            | C::SortAlphabeticalReverse
            | C::SortNatural
            | C::SortNaturalReverse
            | C::SortSize
            | C::SortSizeReverse
            | C::SortRandom
            | C::ToggleHidden
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(y: f32) -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(0.0, y), egui::vec2(100.0, 22.0))
    }

    fn snapshot(items: &[(&str, f32)]) -> Snapshot {
        items
            .iter()
            .map(|(name, y)| (PathBuf::from(name), rect(*y)))
            .collect()
    }

    /// The first frame of a FLIP draws everything exactly where it already was,
    /// and the last frame draws nothing displaced at all. That is the whole
    /// contract; everything else is easing.
    #[test]
    fn the_first_frame_is_the_old_layout_and_the_last_is_the_new_one() {
        let now = Instant::now();
        let before = snapshot(&[("a", 0.0), ("b", 22.0)]);
        let after = snapshot(&[("a", 22.0), ("b", 0.0)]);
        let flip = Flip::begin(&before, &after, now).expect("two rows swapped");
        // At t=0 each row is displaced by exactly the distance it has to cover,
        // so `new + offset` is `old`.
        assert_eq!(flip.offset(Path::new("a"), now), egui::vec2(0.0, -22.0));
        assert_eq!(flip.offset(Path::new("b"), now), egui::vec2(0.0, 22.0));
        // Half way, it is somewhere in between and heading the right way.
        let mid = flip.offset(Path::new("a"), now + TRAVEL / 2).y;
        assert!(mid < 0.0 && mid > -22.0, "got {mid}");
        // At the end, nothing is displaced and nothing is owed a frame.
        assert_eq!(
            flip.offset(Path::new("a"), now + TRAVEL),
            egui::Vec2::ZERO
        );
        assert!(flip.finished(now + TRAVEL));
        assert!(!flip.finished(now));
    }

    /// A row nobody moved is not in the animation at all, and asking about it
    /// is free and answers zero.
    #[test]
    fn a_row_that_did_not_move_is_not_a_participant() {
        let now = Instant::now();
        let before = snapshot(&[("a", 0.0), ("b", 22.0)]);
        let after = snapshot(&[("a", 0.0), ("b", 44.0)]);
        let flip = Flip::begin(&before, &after, now).expect("b moved");
        assert_eq!(flip.travel.len(), 1);
        assert_eq!(flip.offset(Path::new("a"), now), egui::Vec2::ZERO);
        assert_eq!(flip.offset(Path::new("never-seen"), now), egui::Vec2::ZERO);
    }

    /// Nothing moved: no animation, so the caller's `Option` stays `None` and
    /// the window is never asked for a frame to redraw a correct list.
    #[test]
    fn an_unchanged_layout_produces_no_animation() {
        let now = Instant::now();
        let same = snapshot(&[("a", 0.0), ("b", 22.0)]);
        assert!(Flip::begin(&same, &same, now).is_none());
        // A sub-pixel shift is a resize, not a re-sort.
        let nudged = snapshot(&[("a", 0.2), ("b", 22.1)]);
        assert!(Flip::begin(&same, &nudged, now).is_none());
    }

    /// `.` makes rows appear among rows that stay: the stayers travel, the
    /// arrivals fade in, and the departures fade out of where they were.
    #[test]
    fn arrivals_fade_in_and_departures_fade_out_of_where_they_were() {
        let now = Instant::now();
        let before = snapshot(&[("a", 0.0), ("gone", 22.0)]);
        let after = snapshot(&[("a", 0.0), ("dotfile", 22.0)]);
        let flip = Flip::begin(&before, &after, now).expect("one in, one out");
        assert_eq!(flip.alpha(Path::new("a"), now), 1.0, "a was always there");
        assert_eq!(flip.alpha(Path::new("dotfile"), now), 0.0, "starts invisible");
        assert!((flip.alpha(Path::new("dotfile"), now + FADE) - 1.0).abs() < 1e-4);
        let ghosts = flip.ghosts(now);
        assert_eq!(ghosts.len(), 1);
        assert_eq!(ghosts[0].0, Path::new("gone"));
        assert_eq!(ghosts[0].1, rect(22.0), "fades out of where it was");
        assert!((ghosts[0].2 - 1.0).abs() < 1e-4, "starts solid");
        assert!(flip.ghosts(now + FADE).is_empty(), "and then is gone");
    }

    /// A row from far off-screen keeps its direction but not its absurd
    /// distance — otherwise a sort flings something across the pane at speed to
    /// say nothing.
    #[test]
    fn a_row_from_far_away_is_clamped_without_being_turned() {
        let now = Instant::now();
        let before = snapshot(&[("a", -9000.0)]);
        let after = snapshot(&[("a", 0.0)]);
        let flip = Flip::begin(&before, &after, now).expect("it moved");
        let offset = flip.offset(Path::new("a"), now);
        assert!((offset.y + MAX_TRAVEL).abs() < 0.01, "clamped, got {offset:?}");
        assert!(offset.y < 0.0, "still coming from above");
        // A diagonal keeps its direction rather than being bent by a
        // per-component clamp.
        let mut before = Snapshot::new();
        before.insert(
            PathBuf::from("t"),
            egui::Rect::from_min_size(egui::pos2(-3000.0, -4000.0), egui::vec2(10.0, 10.0)),
        );
        let mut after = Snapshot::new();
        after.insert(
            PathBuf::from("t"),
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(10.0, 10.0)),
        );
        let flip = Flip::begin(&before, &after, now).expect("it moved");
        let offset = flip.offset(Path::new("t"), now);
        assert!((offset.length() - MAX_TRAVEL).abs() < 0.01);
        // 3:4 in, 3:4 out.
        assert!((offset.x / offset.y - 0.75).abs() < 1e-4, "{offset:?}");
    }

    /// The trigger set, pinned. Every sort animates, the hidden toggle
    /// animates, and nothing that leaves the order alone does.
    #[test]
    fn only_the_commands_that_reorder_the_listing_animate() {
        use Command as C;
        for command in [
            C::SortMtime,
            C::SortMtimeReverse,
            C::SortBtime,
            C::SortBtimeReverse,
            C::SortExtension,
            C::SortExtensionReverse,
            C::SortAlphabetical,
            C::SortAlphabeticalReverse,
            C::SortNatural,
            C::SortNaturalReverse,
            C::SortSize,
            C::SortSizeReverse,
            C::SortRandom,
            C::ToggleHidden,
        ] {
            assert!(reorders(command), "{} should animate", command.id());
        }
        for command in [
            // The linemodes change what a row *says*, not where it is.
            C::LinemodeSize,
            C::LinemodePermissions,
            C::LinemodeBtime,
            C::LinemodeMtime,
            C::LinemodeOwner,
            C::LinemodeNone,
            // Navigation replaces the listing outright.
            C::Leave,
            C::EnterOrPreview,
            C::HistoryBack,
            C::HistoryForward,
            C::Goto(0),
            C::TabSwitch(1),
            // Moving the cursor, selecting, and the filter prompt.
            C::CursorDown,
            C::ToggleSelect,
            C::SelectAll,
            C::Filter,
            C::FindNext,
            // Everything else.
            C::Quit,
            C::Yank,
            C::Trash,
            C::CommandPalette,
        ] {
            assert!(!reorders(command), "{} should not animate", command.id());
        }
    }

    /// Every sort command in the registry is in the trigger set — the guard
    /// against a fourteenth sort being added and quietly not animating.
    #[test]
    fn no_sort_command_is_missing_from_the_trigger_set() {
        for command in Command::all() {
            if command.id().starts_with("sort-") {
                assert!(reorders(command), "{} is a sort and must animate", command.id());
            }
        }
    }
}
