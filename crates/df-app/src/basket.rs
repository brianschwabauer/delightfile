//! The selection basket — PLAN §7.1's "collect files from multiple
//! directories (`b` to toss in), shown as a floating tray; paste or drag the
//! whole basket as one payload".
//!
//! ## Why a selection is not enough
//!
//! A selection belongs to a listing. Walk out of the directory and it is gone,
//! because the rows it was about are gone — which is correct, and which is
//! exactly the wrong behaviour when the job is "gather the six photographs that
//! are in four different folders and put them somewhere". The basket is the
//! second, *durable* container: paths, not rows, kept across navigation and
//! across tabs, until the program exits.
//!
//! **Not across restarts.** A basket is a scratch pad for the next few minutes;
//! one restored from disk a week later would be a list of paths that half exist,
//! offering to paste files the user has forgotten choosing. The clipboard is not
//! persisted either, and for the same reason.
//!
//! ## Precedence, spelled out
//!
//! Three things can answer `p`, and they are tried in this order:
//!
//! 1. **The internal clipboard** (`y` / `x`). It always wins. Yanking is a
//!    deliberate, recent act with a visible mark on the rows, and a `p` that
//!    pasted something *else* while those marks were on screen would be
//!    unforgivable.
//! 2. **The basket**, when the clipboard is empty. Also deliberate, also
//!    visible — the tray is on screen the whole time it has anything in it,
//!    which is what earns it this place.
//! 3. **The system clipboard** (PLAN §7.4's `wl-paste` fallback), when both are
//!    empty. Last because it belongs to another application: it is the answer
//!    to "I copied this in Firefox", not to anything done in here.
//!
//! [`Precedence::of`] is that rule as a pure function, so it is a test rather
//! than three `if`s spread through the paste path.

use std::path::{Path, PathBuf};

/// How many paths the basket will hold.
///
/// A thousand. The tray is a *hand-picked* collection — the gesture is one key
/// per file — so this is far past any real use and exists only so that a
/// held-down `b` on a 200 000-file directory cannot grow an unbounded vector.
pub const CAPACITY: usize = 1000;

/// How many rows the expanded tray shows before it scrolls.
pub const ROWS: usize = 8;

/// The collected paths, in the order they were tossed in.
///
/// Insertion order, not sorted: the basket is a record of *what you picked*, and
/// re-ordering it under the user would break the one thing a person tracks about
/// a pile they made by hand — that the last thing they added is at the bottom.
#[derive(Debug, Default, Clone)]
pub struct Basket {
    paths: Vec<PathBuf>,
}

/// What one `b` press did, so the toast can say it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tossed {
    pub added: usize,
    pub removed: usize,
    /// The toss was refused because the basket is full.
    pub full: bool,
}

impl Basket {
    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    pub fn len(&self) -> usize {
        self.paths.len()
    }

    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    pub fn contains(&self, path: &Path) -> bool {
        self.paths.iter().any(|held| held == path)
    }

    /// `b` over a selection, or over one row.
    ///
    /// The gesture is a *toggle*, and the whole batch goes the same way: if
    /// every path offered is already in the basket the press takes them all out,
    /// and otherwise it puts the missing ones in. Deciding per path would mean a
    /// `b` on a selection of five, three of which are already in, doing two
    /// different things at once — and pressing it twice would not get you back
    /// where you started.
    pub fn toss(&mut self, paths: &[PathBuf]) -> Tossed {
        if paths.is_empty() {
            return Tossed {
                added: 0,
                removed: 0,
                full: false,
            };
        }
        if paths.iter().all(|path| self.contains(path)) {
            let before = self.paths.len();
            self.paths.retain(|held| !paths.contains(held));
            return Tossed {
                added: 0,
                removed: before - self.paths.len(),
                full: false,
            };
        }
        let mut added = 0;
        let mut full = false;
        for path in paths {
            if self.contains(path) {
                continue;
            }
            if self.paths.len() >= CAPACITY {
                full = true;
                break;
            }
            self.paths.push(path.clone());
            added += 1;
        }
        Tossed {
            added,
            removed: 0,
            full,
        }
    }

    /// Take one path out, by its row in the tray.
    pub fn remove(&mut self, index: usize) -> Option<PathBuf> {
        (index < self.paths.len()).then(|| self.paths.remove(index))
    }

    pub fn clear(&mut self) {
        self.paths.clear();
    }

    /// Drop paths that are no longer there.
    ///
    /// Called when the tray is opened rather than on a timer: a basket is a
    /// list of files somebody chose, and quietly shrinking it in the background
    /// would be the tray changing while nobody was looking at it. Opening it is
    /// the moment the list is about to be read, which is the moment it has to be
    /// true.
    pub fn prune(&mut self) -> usize {
        let before = self.paths.len();
        self.paths.retain(|path| df_core::ops::exists(path));
        before - self.paths.len()
    }
}

/// Which source answers `p`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Precedence {
    /// `y` / `x` — the internal clipboard.
    Clipboard,
    /// The basket, when the clipboard is empty.
    Basket,
    /// Another application's clipboard.
    System,
}

impl Precedence {
    /// The rule in the module essay, as a function.
    pub fn of(clipboard: bool, basket: bool) -> Precedence {
        if clipboard {
            Precedence::Clipboard
        } else if basket {
            Precedence::Basket
        } else {
            Precedence::System
        }
    }
}

// ── The tray ────────────────────────────────────────────────────────────────

/// The chip's height, and the height of one row in the expanded card.
const CHIP_HEIGHT: f32 = 26.0;
const TRAY_ROW: f32 = 22.0;
/// The tray's inner padding.
///
/// `chrome::CARD_PAD`, not a number of its own. The tray's plate is
/// [`crate::chrome::card`], whose radius is `CARD_ROW_RADIUS + CARD_PAD`, so
/// the padding that sets the gap between a row and that plate has to be the
/// same `CARD_PAD` the radius was derived from or the corners stop being
/// concentric (`delightful-ui` §15). It was 8 against a 10-derived radius,
/// which pinched the gap by 2 px at every corner of the card.
const TRAY_PAD: f32 = crate::chrome::CARD_PAD;
/// The gap between the tray and the window's edges.
const TRAY_MARGIN: f32 = 10.0;
const TRAY_WIDTH: f32 = 260.0;
const TRAY_FONT: f32 = 12.5;
/// The remove button's box. Small glyph, generous target (`delightful-ui` §1).
const REMOVE: f32 = 24.0;

/// Where the tray's pieces are, this frame.
#[derive(Debug, Clone)]
pub struct Geometry {
    /// The always-visible chip. [`egui::Rect::NOTHING`] when the basket is
    /// empty, which is the same "nowhere" every other geometry in this crate
    /// uses for a piece that is not drawn.
    pub chip: egui::Rect,
    /// The expanded card, when it is out.
    pub card: Option<egui::Rect>,
    /// One rect per visible row, and the remove button inside it.
    pub rows: Vec<egui::Rect>,
    pub removes: Vec<egui::Rect>,
}

impl Default for Geometry {
    fn default() -> Geometry {
        Geometry {
            chip: egui::Rect::NOTHING,
            card: None,
            rows: Vec::new(),
            removes: Vec::new(),
        }
    }
}

impl Geometry {
    pub fn row_at(&self, pos: egui::Pos2) -> Option<usize> {
        self.rows.iter().position(|r| r.contains(pos))
    }

    pub fn remove_at(&self, pos: egui::Pos2) -> Option<usize> {
        self.removes.iter().position(|r| r.contains(pos))
    }

    /// Whether the pointer is anywhere on the tray — the test that stops a
    /// click on it from also landing on the row underneath.
    pub fn contains(&self, pos: egui::Pos2) -> bool {
        self.chip.contains(pos) || self.card.is_some_and(|card| card.contains(pos))
    }
}

/// Lay the tray out against the bottom-right of `area`.
///
/// Bottom-**right** and not anywhere else: the file panes are read left to
/// right and top to bottom, so the bottom-right corner is the one place a
/// persistent floating thing can sit without ever being over the row the eye is
/// on. The card grows *upwards* from the chip for the same reason — a card that
/// pushed the chip up would move the thing the pointer is aiming at
/// (`delightful-ui` §8).
pub fn geometry(area: egui::Rect, basket: &Basket, open: bool, first: usize) -> Geometry {
    if basket.is_empty() {
        return Geometry::default();
    }
    let right = area.right() - TRAY_MARGIN;
    let bottom = area.bottom() - TRAY_MARGIN;
    let chip = egui::Rect::from_min_max(
        egui::pos2(right - CHIP_WIDTH, bottom - CHIP_HEIGHT),
        egui::pos2(right, bottom),
    );
    if !open {
        return Geometry {
            chip,
            ..Default::default()
        };
    }
    let visible = basket.len().saturating_sub(first).min(ROWS);
    let height = TRAY_PAD * 2.0 + TRAY_ROW + visible as f32 * TRAY_ROW;
    let card = egui::Rect::from_min_max(
        egui::pos2(right - TRAY_WIDTH, chip.top() - 6.0 - height),
        egui::pos2(right, chip.top() - 6.0),
    );
    let rows_top = card.top() + TRAY_PAD + TRAY_ROW;
    let rows: Vec<egui::Rect> = (0..visible)
        .map(|i| {
            egui::Rect::from_min_size(
                egui::pos2(card.left() + TRAY_PAD, rows_top + i as f32 * TRAY_ROW),
                egui::vec2(card.width() - TRAY_PAD * 2.0, TRAY_ROW),
            )
        })
        .collect();
    let removes = rows
        .iter()
        .map(|row| {
            egui::Rect::from_center_size(
                // Flush with the row's right edge, not 4 px past it: the
                // target's own inset from the card has to match the row's, or
                // the × sits closer to the plate than the name it removes
                // (`delightful-ui` §15's even insets).
                egui::pos2(row.right() - REMOVE / 2.0, row.center().y),
                egui::vec2(REMOVE, REMOVE.min(row.height() + 6.0)),
            )
        })
        .collect();
    Geometry {
        chip,
        card: Some(card),
        rows,
        removes,
    }
}

/// The chip's width. Fixed, so it does not resize as the count crosses ten and
/// move under a pointer that is already reaching for it.
pub const CHIP_WIDTH: f32 = 62.0;

/// Draw the tray.
pub fn paint(
    paint: &crate::ui::Painting<'_>,
    basket: &Basket,
    geometry: &Geometry,
    first: usize,
    hovers: &crate::hover::Hovers<crate::ui::Control>,
    ripples: &crate::ripple::Ripples<crate::ui::Control>,
) {
    use crate::hover::pressed_rect;
    use crate::ui::Control;
    if basket.is_empty() {
        return;
    }
    let palette = paint.palette;
    let painter = paint.painter;

    if let Some(card) = geometry.card {
        crate::chrome::card(paint, card, 1.0);
        painter.text(
            egui::pos2(
                card.left() + TRAY_PAD,
                card.top() + TRAY_PAD + TRAY_ROW / 2.0,
            ),
            egui::Align2::LEFT_CENTER,
            "Basket",
            egui::FontId::proportional(TRAY_FONT + 1.0),
            palette.text,
        );
        painter.text(
            egui::pos2(
                card.right() - TRAY_PAD,
                card.top() + TRAY_PAD + TRAY_ROW / 2.0,
            ),
            egui::Align2::RIGHT_CENTER,
            "p pastes · B closes",
            egui::FontId::proportional(TRAY_FONT - 1.0),
            palette.overlay0,
        );
        for (i, rect) in geometry.rows.iter().enumerate() {
            let Some(path) = basket.paths().get(first + i) else {
                break;
            };
            let key = Control::BasketRow(i);
            let hover = hovers.hover(key);
            // The row depresses and splashes like every other clickable row in
            // the window (`delightful-ui` §4). It was the only list in the
            // crate that lit on hover and then did nothing under the finger.
            let rect = &pressed_rect(*rect, hovers.press(key));
            if hover > 0.0 {
                painter.rect_filled(
                    *rect,
                    crate::ui::ROW_RADIUS,
                    crate::theme::mix(palette.crust, palette.surface1, hover),
                );
            }
            for splash in ripples.splashes(key, paint.now) {
                painter.circle_filled(
                    splash.center,
                    splash.radius,
                    egui::Color32::from_white_alpha((splash.alpha * 255.0).round() as u8),
                );
            }
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.to_string_lossy().into_owned());
            crate::chrome::truncated(
                painter,
                egui::pos2(rect.left() + 6.0, rect.center().y),
                &name,
                palette.subtext0,
                (rect.width() - REMOVE - 12.0).max(0.0),
            );
            // The remove button: a small glyph in a target twice its size.
            let remove = geometry.removes[i];
            let remove_key = Control::BasketRemove(i);
            let lit = hovers.hover(remove_key);
            for splash in ripples.splashes(remove_key, paint.now) {
                painter.circle_filled(
                    splash.center,
                    splash.radius,
                    egui::Color32::from_white_alpha((splash.alpha * 255.0).round() as u8),
                );
            }
            painter.text(
                pressed_rect(remove, hovers.press(remove_key)).center(),
                egui::Align2::CENTER_CENTER,
                "×",
                egui::FontId::proportional(TRAY_FONT + 3.0),
                crate::theme::mix(palette.overlay0, palette.red, lit),
            );
        }
    }

    // The chip itself, last, so it sits over the card's shadow.
    let key = Control::BasketChip;
    let hover = hovers.hover(key);
    let chip = pressed_rect(geometry.chip, hovers.press(key));
    painter.rect_filled(
        chip,
        crate::ui::ROW_RADIUS + 3,
        crate::theme::mix(palette.crust, palette.teal, 0.20 + hover * 0.18),
    );
    let inside = painter.with_clip_rect(chip);
    for splash in ripples.splashes(key, paint.now) {
        inside.circle_filled(
            splash.center,
            splash.radius,
            egui::Color32::from_white_alpha((splash.alpha * 255.0).round() as u8),
        );
    }
    inside.text(
        egui::pos2(chip.left() + 9.0, chip.center().y),
        egui::Align2::LEFT_CENTER,
        // A plain glyph in the proportional face: the nerd-font icons need a
        // patched font that may not be there, and the chip has to read the same
        // either way.
        "▤",
        egui::FontId::proportional(TRAY_FONT + 1.0),
        palette.teal,
    );
    inside.text(
        egui::pos2(chip.right() - 9.0, chip.center().y),
        egui::Align2::RIGHT_CENTER,
        basket.len().to_string(),
        egui::FontId::proportional(TRAY_FONT + 1.0),
        palette.text,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tray's rows sit inside a `chrome::card` plate, so the padding that
    /// sets their gap and the padding its radius was derived from have to be
    /// the same number (`delightful-ui` §15). They were 8 and 10, which pinched
    /// the gap by 2 px at every corner.
    #[test]
    fn the_tray_radii_are_concentric() {
        assert_eq!(
            crate::ui::ROW_RADIUS as f32 + TRAY_PAD,
            crate::chrome::CARD_RADIUS as f32
        );
    }

    fn paths(names: &[&str]) -> Vec<PathBuf> {
        names.iter().map(PathBuf::from).collect()
    }

    /// One press puts the batch in, the next takes the same batch out — and a
    /// path offered twice is held once.
    #[test]
    fn tossing_is_a_toggle_over_the_whole_batch() {
        let mut basket = Basket::default();
        let batch = paths(&["/a", "/b"]);
        let out = basket.toss(&batch);
        assert_eq!((out.added, out.removed), (2, 0));
        assert_eq!(basket.len(), 2);

        // Already in, all of it: the same press takes it back out.
        let out = basket.toss(&batch);
        assert_eq!((out.added, out.removed), (0, 2));
        assert!(basket.is_empty());

        // A mixed batch goes *in* — the press has to mean one thing.
        basket.toss(&paths(&["/a"]));
        let out = basket.toss(&paths(&["/a", "/c"]));
        assert_eq!((out.added, out.removed), (1, 0));
        assert_eq!(basket.paths(), paths(&["/a", "/c"]).as_slice());
    }

    /// Insertion order, because that is the one thing a person tracks about a
    /// pile they made by hand.
    #[test]
    fn the_order_is_the_order_things_were_added() {
        let mut basket = Basket::default();
        basket.toss(&paths(&["/z"]));
        basket.toss(&paths(&["/a"]));
        basket.toss(&paths(&["/m"]));
        assert_eq!(basket.paths(), paths(&["/z", "/a", "/m"]).as_slice());
        assert_eq!(basket.remove(1), Some(PathBuf::from("/a")));
        assert_eq!(basket.paths(), paths(&["/z", "/m"]).as_slice());
        assert_eq!(basket.remove(9), None);
        basket.clear();
        assert!(basket.is_empty());
    }

    #[test]
    fn the_basket_is_bounded() {
        let mut basket = Basket::default();
        let many: Vec<PathBuf> = (0..CAPACITY + 10)
            .map(|i| PathBuf::from(format!("/f{i}")))
            .collect();
        let out = basket.toss(&many);
        assert_eq!(basket.len(), CAPACITY);
        assert_eq!(out.added, CAPACITY);
        assert!(out.full, "the refusal is reported, not silent");
    }

    /// An empty press does nothing and says nothing.
    #[test]
    fn tossing_nothing_is_a_no_op() {
        let mut basket = Basket::default();
        let out = basket.toss(&[]);
        assert_eq!((out.added, out.removed, out.full), (0, 0, false));
        assert!(basket.is_empty());
    }

    /// The three answers to `p`, in order.
    #[test]
    fn the_clipboard_always_beats_the_basket_and_both_beat_the_system() {
        assert_eq!(Precedence::of(true, true), Precedence::Clipboard);
        assert_eq!(Precedence::of(true, false), Precedence::Clipboard);
        assert_eq!(Precedence::of(false, true), Precedence::Basket);
        assert_eq!(Precedence::of(false, false), Precedence::System);
    }

    /// The tray is bottom-right, the card grows upwards from the chip, and
    /// every row has a target at least 24 points across (`delightful-ui` §1).
    #[test]
    fn the_tray_sits_in_the_corner_and_opens_upwards() {
        let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, 900.0));
        let mut basket = Basket::default();
        // An empty basket has no tray at all — nothing to say, nothing drawn.
        assert_eq!(geometry(area, &basket, true, 0).chip, egui::Rect::NOTHING);
        assert!(!geometry(area, &basket, true, 0).contains(area.center()));

        basket.toss(&paths(&["/a", "/b", "/c"]));
        let closed = geometry(area, &basket, false, 0);
        assert!(closed.card.is_none());
        assert!(closed.chip.right() <= area.right());
        assert!(closed.chip.bottom() <= area.bottom());
        assert!(closed.chip.height() >= 24.0, "the chip is a hit target");

        let open = geometry(area, &basket, true, 0);
        let card = open.card.expect("the card is out");
        // Upwards: the chip does not move when the card opens.
        assert_eq!(open.chip, closed.chip);
        assert!(card.bottom() <= open.chip.top());
        assert_eq!(open.rows.len(), 3);
        for (row, remove) in open.rows.iter().zip(&open.removes) {
            assert!(card.contains_rect(*row));
            assert!(remove.width() >= 24.0 && remove.height() >= 22.0);
            assert_eq!(open.remove_at(remove.center()), open.row_at(row.center()));
        }
        assert!(open.contains(open.chip.center()));
        assert!(open.contains(card.center()));
        assert!(!open.contains(egui::pos2(10.0, 10.0)));
    }

    /// More rows than fit: the card caps, and the scroll offset picks up where
    /// it is told to.
    #[test]
    fn a_long_basket_shows_a_windowful() {
        let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, 900.0));
        let mut basket = Basket::default();
        let many: Vec<PathBuf> = (0..30).map(|i| PathBuf::from(format!("/f{i}"))).collect();
        basket.toss(&many);
        assert_eq!(geometry(area, &basket, true, 0).rows.len(), ROWS);
        // Near the end there are fewer rows left to draw than the cap.
        assert_eq!(geometry(area, &basket, true, 28).rows.len(), 2);
    }

    /// The tray paints in every state without panicking, including in a window
    /// far too small for it.
    #[test]
    fn the_tray_paints_without_panicking() {
        let mut basket = Basket::default();
        basket.toss(&paths(&["/a/one.txt", "/b/two.txt"]));
        let theme = df_core::config::Theme::default();
        let palette = crate::theme::Palette::from_theme(&theme);
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(Default::default(), |ui| {
            let painting = crate::ui::Painting {
                painter: ui.painter(),
                palette: &palette,
                theme: &theme,
                nerd: false,
                show_symlink: true,
                now: std::time::Instant::now(),
            };
            let hovers = crate::hover::Hovers::new();
            let ripples = crate::ripple::Ripples::new();
            for area in [
                egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, 900.0)),
                egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(120.0, 60.0)),
            ] {
                for open in [false, true] {
                    let g = geometry(area, &basket, open, 0);
                    paint(&painting, &basket, &g, 0, &hovers, &ripples);
                }
            }
            // An empty basket draws nothing at all.
            let empty = Basket::default();
            let g = geometry(
                egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 400.0)),
                &empty,
                true,
                0,
            );
            paint(&painting, &empty, &g, 0, &hovers, &ripples);
        });
    }

    #[test]
    fn pruning_drops_what_is_no_longer_there() {
        let mut basket = Basket::default();
        // `/` exists on every machine this runs on; the other two do not.
        basket.toss(&paths(&["/", "/nope-df-basket", "/also-nope-df"]));
        assert_eq!(basket.len(), 3);
        assert_eq!(basket.prune(), 2);
        assert_eq!(basket.paths(), paths(&["/"]).as_slice());
    }
}
