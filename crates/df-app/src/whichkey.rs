//! The which-key card's timing (PLAN §4, §8) — ported from delightviewer's
//! `ui/chords.rs`, whose delay this shares.
//!
//! The card lists what could finish a half-typed chord. df-core already decides
//! *when* it is due — [`KeymapState::which_key_due`] is
//! `pending_started + WHICH_KEY_DELAY` — so all that is left here is the small
//! state machine around that instant, and it exists for one reason: **the card
//! must not vanish the moment the chord resolves.** `g` `g` typed at speed never
//! shows a card at all, which is the point of the delay; but a card that *did*
//! appear and then disappeared between two frames would read as a flicker
//! rather than as an answer, so it fades.
//!
//! Two rules the tests pin:
//!
//! - **Instant in, animated out** (`delightful-ui` §3), which is the same rule
//!   the hover system runs on. Waiting 175 ms and then *also* fading in would be
//!   two delays stacked, and the card would arrive after the hesitation it was
//!   supposed to answer.
//! - **A settled card asks for no frames.** [`WhichKey::deadline`] hands the
//!   event loop exactly one instant — the moment the card is due, or the moment
//!   its fade ends — and nothing polls in between (PLAN §1).
//!
//! [`KeymapState::which_key_due`]: df_core::keymap::KeymapState::which_key_due

use std::time::{Duration, Instant};

/// How long the card takes to leave once the chord has resolved or been
/// abandoned.
///
/// 120 ms is PLAN §8's state-fade duration, and an outro should be quicker than
/// its intro — the card's "intro" is a 175 ms wait and then nothing, so this is
/// already the slower half of the pair. Long enough not to be a cut, short
/// enough that the card is gone before the command it explained has finished
/// happening.
pub const FADE_OUT: Duration = Duration::from_millis(120);

/// Whether the card is up, and how far through leaving it is.
#[derive(Debug, Clone, Copy, Default)]
pub struct WhichKey {
    /// The card is fully on screen.
    shown: bool,
    /// When the fade-out started, while one is running.
    fading_since: Option<Instant>,
}

impl WhichKey {
    pub fn new() -> WhichKey {
        WhichKey::default()
    }

    /// Advance the state one frame.
    ///
    /// `due` is [`KeymapState::which_key_due`]: `Some` while a chord is pending,
    /// `None` the instant it resolves, matches nothing, or is cancelled.
    ///
    /// [`KeymapState::which_key_due`]: df_core::keymap::KeymapState::which_key_due
    pub fn update(&mut self, due: Option<Instant>, now: Instant) {
        match due {
            Some(at) => {
                // A new chord while the old card is still fading takes the
                // screen back immediately rather than finishing the fade — the
                // card is about the chord in the hand, not the one that ended.
                self.fading_since = None;
                self.shown = now >= at;
            }
            None if self.shown => {
                self.shown = false;
                self.fading_since = Some(now);
            }
            None => {
                if self
                    .fading_since
                    .is_some_and(|from| now.saturating_duration_since(from) >= FADE_OUT)
                {
                    self.fading_since = None;
                }
            }
        }
    }

    /// 0…1. Full while shown; a quadratic ease-*in* falloff on the way out — it
    /// holds and then goes, the same shape [`crate::ripple`]'s alpha uses, so
    /// the two pieces of transient chrome leave the same way.
    pub fn alpha(&self, now: Instant) -> f32 {
        if self.shown {
            return 1.0;
        }
        let Some(from) = self.fading_since else {
            return 0.0;
        };
        let t = (now.saturating_duration_since(from).as_secs_f32()
            / FADE_OUT.as_secs_f32().max(f32::EPSILON))
        .clamp(0.0, 1.0);
        1.0 - t * t
    }

    pub fn visible(&self, now: Instant) -> bool {
        self.alpha(now) > 0.0
    }

    /// Whether the card is mid-fade and therefore needs frames back to back. A
    /// card that is *up* is not animating — it is a static rectangle, and asking
    /// for 60 frames a second to redraw it is how a hint costs a battery.
    pub fn fading(&self) -> bool {
        self.fading_since.is_some()
    }

    /// When the next frame is owed, or `None` when the card is settled — either
    /// fully up (a static card costs nothing) or fully gone.
    pub fn deadline(&self, due: Option<Instant>) -> Option<Instant> {
        if let Some(from) = self.fading_since {
            return Some(from + FADE_OUT);
        }
        // Waiting for the card to become due: exactly one wake-up, at the
        // instant it does.
        match due {
            Some(at) if !self.shown => Some(at),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use df_core::keymap::WHICH_KEY_DELAY;

    /// The whole point of the delay: a chord finished before it elapses never
    /// puts a card on screen, and never asks for a frame to take one off.
    #[test]
    fn a_fast_typist_never_sees_the_card() {
        let t0 = Instant::now();
        let mut card = WhichKey::new();
        let quick = t0 + Duration::from_millis(90);
        card.update(Some(t0 + WHICH_KEY_DELAY), quick);
        assert!(!card.visible(quick));
        // …and the second key of the chord resolves it.
        card.update(None, quick);
        assert!(!card.visible(quick));
        assert_eq!(card.deadline(None), None);
    }

    /// A hand that stopped gets the card, at once, at the moment it is due.
    #[test]
    fn a_hesitation_shows_the_card_instantly_when_it_is_due() {
        let t0 = Instant::now();
        let due = t0 + WHICH_KEY_DELAY;
        let mut card = WhichKey::new();

        card.update(Some(due), t0);
        assert!(!card.visible(t0));
        assert_eq!(card.deadline(Some(due)), Some(due), "one scheduled wake-up");

        card.update(Some(due), due);
        assert_eq!(card.alpha(due), 1.0, "instant in, no fade-in");
        assert_eq!(card.deadline(Some(due)), None, "a held card costs nothing");
    }

    /// Resolving a chord the card was up for fades it rather than cutting it.
    #[test]
    fn resolving_a_shown_chord_fades_the_card_out() {
        let t0 = Instant::now();
        let due = t0 + WHICH_KEY_DELAY;
        let mut card = WhichKey::new();
        card.update(Some(due), due);

        card.update(None, due);
        assert_eq!(card.alpha(due), 1.0, "the fade starts from full");
        assert_eq!(card.deadline(None), Some(due + FADE_OUT));
        let mid = due + FADE_OUT / 2;
        let a = card.alpha(mid);
        assert!(a > 0.0 && a < 1.0, "got {a}");

        let end = due + FADE_OUT;
        assert_eq!(card.alpha(end), 0.0);
        card.update(None, end);
        assert!(!card.visible(end));
        assert_eq!(card.deadline(None), None, "and then it is asleep");
    }

    /// A second chord started mid-fade takes the card back rather than waiting
    /// for the first one's exit to finish.
    #[test]
    fn a_new_chord_interrupts_the_fade() {
        let t0 = Instant::now();
        let mut card = WhichKey::new();
        card.update(Some(t0), t0);
        card.update(None, t0);
        let later = t0 + FADE_OUT / 2;
        let next_due = later + WHICH_KEY_DELAY;
        card.update(Some(next_due), later);
        assert_eq!(card.alpha(later), 0.0, "the old card is gone at once");
        assert_eq!(card.deadline(Some(next_due)), Some(next_due));
    }
}
