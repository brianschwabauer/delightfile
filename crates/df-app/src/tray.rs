//! The yank tray — the clipboard, listed. `B` opens it, and so does a click on
//! the top row's `3 yanked` chip: a card hanging under that chip with one row
//! per carried file, an `×` on each, and a `Clear` that does what `X` does —
//! PLAN §7.1's floating tray, with the whole yank pasted or dragged as one
//! payload. The wheel over the card scrolls it a row at a time.
//!
//! ## Why a selection is not enough
//!
//! A selection belongs to a listing. Walk out of the directory and it is gone,
//! because the rows it was about are gone — which is correct, and which is
//! exactly the wrong behaviour when the job is "gather the six photographs that
//! are in four different folders and put them somewhere". The clipboard is the
//! *durable* container: paths, not rows, kept across navigation and across
//! tabs, until it is pasted (a cut), put down (`X`) or the program exits. `b`
//! adds the row under the cursor, or the selection, to it from wherever you
//! are, and the row marks follow the paths rather than the listing — so a file
//! picked up two folders ago wears its teal bar again when you walk back past
//! it.
//!
//! **Not across restarts.** A pile of carried files is a scratch pad for the
//! next few minutes; one restored from disk a week later would be a list of
//! paths that half exist, offering to paste files the user has forgotten
//! choosing.
//!
//! ## Why there is one carried set
//!
//! This card used to belong to a *basket*: a second container beside the
//! clipboard, filled with `b`, with a chip of its own and a rule for which of
//! the two `p` pasted — the clipboard if it held anything, the basket if not,
//! the system clipboard last. The rule was coherent and it was the mistake.
//! Two carried sets meant two chips saying two counts, marks on the rows for
//! one and not the other, an `X` that put down whichever came first on the
//! ladder, and a `p` whose meaning depended on something that might have been
//! yanked twenty minutes earlier. Every one of those is a question the user
//! had to answer before pressing a key, and the answer was never on screen in
//! one place.
//!
//! So `b` builds the clipboard itself. `y` and `x` still *replace* it — they
//! are about the rows in front of you — and `b` adds to it or takes back out of
//! it, keeping whichever verb it already has. There is one chip, one set of row
//! marks, one tooltip and this one card, all describing the same list, and `p`
//! has two rungs instead of three: what this program is carrying, and failing
//! that, what another program put on the system clipboard. The one thing a
//! single set costs — a `y` can now throw away a pile somebody spent a minute
//! building — is said out loud by the `y` that does it (see
//! `App::set_clipboard`).
//!
//! The rules themselves — the toggle, the kept verb, the insertion order —
//! are [`df_core::ops::Clipboard`]'s, and tested there without a window. This
//! module is the card: where it is, what it draws, and the pruning it does
//! when it opens.

use df_core::ops::Clipboard;

/// How many rows the tray shows before it scrolls. At most: a window too
/// short for eight under the top row gets as many as fit ([`fit`]).
pub const ROWS: usize = 8;

/// The height of the header and of one row.
const TRAY_ROW: f32 = 22.0;
/// How far under the top row the card's top edge sits: the gap the menus
/// leave under the control they drop out of (`menu::BELOW_GAP`), so the card
/// reads as having come out of the chip rather than as a plate glued to the
/// row.
const HANG: f32 = 4.0;
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
/// The `Clear` button's width. Fixed, so the geometry needs no painter to
/// measure one word with; it is wider than the word by a comfortable margin
/// on each side.
const CLEAR_WIDTH: f32 = 52.0;
const CLEAR_LABEL: &str = "Clear";

/// Where the tray's pieces are, this frame.
#[derive(Debug, Clone)]
pub struct Geometry {
    /// The card, when it is out.
    pub card: Option<egui::Rect>,
    /// The header's `Clear`. [`egui::Rect::NOTHING`] when the card is not
    /// out, which is the same "nowhere" every other geometry in this crate
    /// uses for a piece that is not drawn.
    pub clear: egui::Rect,
    /// One rect per visible row, and the remove button inside it.
    pub rows: Vec<egui::Rect>,
    pub removes: Vec<egui::Rect>,
    /// The band the rows' bar is pointed at by, while the tray carries more
    /// than it shows ([`crate::scrollbar::band`]).
    pub band: Option<egui::Rect>,
}

impl Default for Geometry {
    fn default() -> Geometry {
        Geometry {
            card: None,
            clear: egui::Rect::NOTHING,
            rows: Vec::new(),
            removes: Vec::new(),
            band: None,
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
        self.card.is_some_and(|card| card.contains(pos))
    }
}

/// Hang the tray from the yank chip: its top edge [`HANG`] under `row` (the
/// top row the chip sits on), its right edge on the chip's right edge.
///
/// **Under the thing that was clicked.** The chip is what opens the card, and
/// the card says what the chip counts, so it comes out of the chip the way a
/// menu comes out of its button; a card that appeared in some other corner of
/// the window would leave the eye to go and find it, and to work out that the
/// two were the same list. Right-aligned, because the chip sits at the right
/// end of the row and a card extending left of it stays inside the window; it
/// is slid back inside `area` if the chip is ever too close to either edge
/// for that.
///
/// The card grows *downwards* as it gets longer, so its header — the chip's
/// own label, and `Clear` — stays right under the chip whatever the count,
/// and stops at the window's foot: the rows past what fits there scroll
/// ([`fit`]).
///
/// Nothing at all when the tray is closed or there is nothing to list: the
/// chip that says how much is carried is there whether the card is out or
/// not.
pub fn geometry(
    area: egui::Rect,
    chip: egui::Rect,
    row: egui::Rect,
    clipboard: &Clipboard,
    open: bool,
    first: usize,
) -> Geometry {
    if !open || clipboard.is_empty() {
        return Geometry::default();
    }
    let right = chip.right().min(area.right() - TRAY_MARGIN);
    let left = (right - TRAY_WIDTH).max(area.left() + TRAY_MARGIN);
    // …and never past the window's right edge either, which a card slid in
    // from the left one would otherwise cross in a window narrower than it.
    let width = ((left + TRAY_WIDTH).min(area.right() - TRAY_MARGIN) - left).max(0.0);
    let top = row.bottom() + HANG;
    let visible = clipboard.len().saturating_sub(first).min(fit(area, row));
    let height = TRAY_PAD * 2.0 + TRAY_ROW + visible as f32 * TRAY_ROW;
    let card = egui::Rect::from_min_size(egui::pos2(left, top), egui::vec2(width, height));
    // The header's button sits in the card's corner, so it is inset by the
    // same padding on its top and its right as the rows are on their sides,
    // and rounds like a row: concentric with the card (`delightful-ui` §15).
    let header_top = card.top() + TRAY_PAD;
    let clear = egui::Rect::from_min_max(
        egui::pos2(card.right() - TRAY_PAD - CLEAR_WIDTH, header_top),
        egui::pos2(card.right() - TRAY_PAD, header_top + TRAY_ROW),
    );
    let rows_top = header_top + TRAY_ROW;
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
    let band = rows.first().zip(rows.last()).and_then(|(top, bottom)| {
        crate::scrollbar::band(
            card,
            egui::Rect::from_min_max(top.min, bottom.max),
            rows.len() as f32,
            clipboard.len() as f32,
        )
    });
    Geometry {
        card: Some(card),
        clear,
        rows,
        removes,
        band,
    }
}

/// How many rows the tray has room for, hanging from `row`: [`ROWS`], or as
/// many as fit between its header and the window's foot, and never none.
pub fn fit(area: egui::Rect, row: egui::Rect) -> usize {
    let top = row.bottom() + HANG + TRAY_PAD * 2.0 + TRAY_ROW;
    let room = (area.bottom() - TRAY_MARGIN - top).max(0.0);
    ((room / TRAY_ROW).floor() as usize).clamp(1, ROWS)
}

/// A wheel roll over the card, as the tray's new first row, in a tray
/// showing `visible` rows ([`fit`]).
///
/// Whole rows at a time: the tray is a short list of names, and a row half out
/// of the card would be half a name. A trackpad's fractions of a row are kept
/// in `carry` until they add up to one, so a slow two-finger drag still gets
/// there instead of rounding to nothing on every frame. Clamped to the list —
/// the last windowful is as far as it goes — and the carry is dropped at
/// either end, so the first notch back the other way moves at once rather than
/// paying off a debt rolled up against the stop.
pub fn scroll(first: usize, len: usize, visible: usize, carry: &mut f32, points: f32) -> usize {
    let rows = crate::mouse::wheel_rows(points, TRAY_ROW);
    crate::mouse::roll(first, len.saturating_sub(visible), carry, rows)
}

/// The tray's bar, beside its rows, while it carries more than it shows.
pub fn bar(geometry: &Geometry, first: usize, len: usize) -> Option<crate::scrollbar::Geometry> {
    let card = geometry.card?;
    let (top, bottom) = (geometry.rows.first()?, geometry.rows.last()?);
    crate::scrollbar::card(
        card,
        egui::Rect::from_min_max(top.min, bottom.max),
        first as f32,
        geometry.rows.len() as f32,
        len as f32,
    )
}

/// Drop paths that are no longer there, and say how many went.
///
/// Called when the tray is opened rather than on a timer: the clipboard is a
/// list of files somebody chose, and quietly shrinking it in the background
/// would be the list changing while nobody was looking at it. Opening the tray
/// is the moment the list is about to be read, which is the moment it has to
/// be true.
///
/// A remote path is kept without asking. `y` in a remote tab carries
/// `sftp://…` display paths, which no local `stat` can answer for, and a tray
/// that emptied a download-in-waiting because it could not see the server
/// would be the tray deleting the user's choice.
pub fn prune(clipboard: &mut Clipboard) -> usize {
    let before = clipboard.len();
    clipboard
        .paths
        .retain(|path| crate::remote::is_remote(path) || df_core::ops::exists(path));
    before - clipboard.len()
}

/// Draw the tray.
pub fn paint(
    paint: &crate::ui::Painting<'_>,
    clipboard: &Clipboard,
    geometry: &Geometry,
    first: usize,
    hovers: &crate::hover::Hovers<crate::ui::Control>,
    ripples: &crate::ripple::Ripples<crate::ui::Control>,
) {
    use crate::hover::pressed_rect;
    use crate::ui::Control;
    let Some(card) = geometry.card else {
        return;
    };
    let palette = paint.palette;
    let painter = paint.painter;
    let splash = |painter: &egui::Painter, key: Control| {
        for splash in ripples.splashes(key, paint.now) {
            painter.circle_filled(
                splash.center,
                splash.radius,
                egui::Color32::from_white_alpha((splash.alpha * 255.0).round() as u8),
            );
        }
    };

    crate::chrome::card(paint, card, 1.0);
    // The header says what the chip says, in the chip's colour: the card is
    // the chip, opened.
    let cut = clipboard.mode == df_core::ops::PasteMode::Cut;
    painter.text(
        egui::pos2(
            card.left() + TRAY_PAD,
            card.top() + TRAY_PAD + TRAY_ROW / 2.0,
        ),
        egui::Align2::LEFT_CENTER,
        crate::chrome::yank_label(clipboard.len(), cut),
        egui::FontId::proportional(TRAY_FONT + 1.0),
        crate::chrome::yank_color(palette, cut),
    );
    // `Clear`: a word, with a row's plate under the pointer and nothing at
    // rest — the picker's quiet `Cancel`, at the tray's size. Quiet because
    // it throws the whole list away, and the loudest thing on a card should
    // not be the thing that empties it.
    {
        let key = Control::YankClear;
        let hover = hovers.hover(key);
        let rect = pressed_rect(geometry.clear, hovers.press(key));
        if hover > 0.0 {
            painter.rect_filled(
                rect,
                crate::ui::ROW_RADIUS,
                crate::theme::mix(palette.crust, palette.surface1, hover),
            );
        }
        let inside = painter.with_clip_rect(rect);
        splash(&inside, key);
        inside.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            CLEAR_LABEL,
            egui::FontId::proportional(TRAY_FONT),
            crate::theme::mix(palette.subtext0, palette.text, hover),
        );
    }
    for (i, rect) in geometry.rows.iter().enumerate() {
        let Some(path) = clipboard.paths.get(first + i) else {
            break;
        };
        let key = Control::YankRow(i);
        let hover = hovers.hover(key);
        // The row depresses and splashes like every other clickable row in
        // the window (`delightful-ui` §4).
        let rect = &pressed_rect(*rect, hovers.press(key));
        if hover > 0.0 {
            painter.rect_filled(
                *rect,
                crate::ui::ROW_RADIUS,
                crate::theme::mix(palette.crust, palette.surface1, hover),
            );
        }
        splash(painter, key);
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
        let remove_key = Control::YankRemove(i);
        let lit = hovers.hover(remove_key);
        splash(painter, remove_key);
        painter.text(
            pressed_rect(remove, hovers.press(remove_key)).center(),
            egui::Align2::CENTER_CENTER,
            "×",
            egui::FontId::proportional(TRAY_FONT + 3.0),
            crate::theme::mix(palette.overlay0, palette.red, lit),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

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

    fn carrying(names: &[&str]) -> Clipboard {
        Clipboard::yank(names.iter().map(PathBuf::from))
    }

    fn area() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, 900.0))
    }

    /// A top row across the window, and a yank chip near its right-hand end —
    /// where the cluster puts it, left of the counter.
    fn row() -> egui::Rect {
        egui::Rect::from_min_max(egui::pos2(0.0, 30.0), egui::pos2(1400.0, 62.0))
    }

    fn chip() -> egui::Rect {
        egui::Rect::from_min_max(egui::pos2(1220.0, 34.0), egui::pos2(1300.0, 58.0))
    }

    /// The tray hangs under the chip, right edges together, and grows
    /// downwards; `Clear` sits in the card's corner with even insets; every
    /// row has a target at least 24 points across (`delightful-ui` §1).
    #[test]
    fn the_tray_hangs_under_the_chip_and_grows_downwards() {
        // Closed, or with nothing carried, there is no tray at all.
        let empty = Clipboard::default();
        assert!(geometry(area(), chip(), row(), &empty, true, 0)
            .card
            .is_none());
        let clip = carrying(&["/a", "/b", "/c"]);
        let closed = geometry(area(), chip(), row(), &clip, false, 0);
        assert!(closed.card.is_none());
        assert_eq!(closed.clear, egui::Rect::NOTHING);
        assert!(!closed.contains(area().center()));

        let open = geometry(area(), chip(), row(), &clip, true, 0);
        let card = open.card.expect("the card is out");
        assert!(area().contains_rect(card));
        assert_eq!(card.right(), chip().right(), "right edges together");
        assert_eq!(card.top(), row().bottom() + HANG, "just under the top row");
        assert!(!card.intersects(row()), "the card covers none of the row");
        // Downwards: a longer list moves the bottom, never the top.
        let longer = geometry(
            area(),
            chip(),
            row(),
            &carrying(&["/a", "/b", "/c", "/d"]),
            true,
            0,
        );
        let longer = longer.card.expect("out");
        assert_eq!(longer.top(), card.top());
        assert!(longer.bottom() > card.bottom());

        assert!(card.contains_rect(open.clear));
        assert_eq!(
            open.clear.top() - card.top(),
            card.right() - open.clear.right()
        );
        assert!(open.clear.height() >= 22.0 && open.clear.width() >= 24.0);
        assert_eq!(open.rows.len(), 3);
        for (row, remove) in open.rows.iter().zip(&open.removes) {
            assert!(card.contains_rect(*row));
            assert!(row.top() >= open.clear.bottom(), "a row under the header");
            assert!(remove.width() >= 24.0 && remove.height() >= 22.0);
            assert_eq!(open.remove_at(remove.center()), open.row_at(row.center()));
        }
        assert!(open.contains(card.center()));
        assert!(!open.contains(egui::pos2(10.0, 10.0)));
    }

    /// A chip too close to either edge for the card to hang from it whole:
    /// the card slides back inside the window rather than off it.
    #[test]
    fn the_tray_stays_inside_the_window() {
        let clip = carrying(&["/a"]);
        let at = |left: f32, right: f32| {
            let chip = egui::Rect::from_min_max(egui::pos2(left, 34.0), egui::pos2(right, 58.0));
            geometry(area(), chip, row(), &clip, true, 0)
                .card
                .expect("out")
        };
        let hard_right = at(1380.0, 1400.0);
        assert_eq!(hard_right.right(), area().right() - TRAY_MARGIN);
        let hard_left = at(4.0, 60.0);
        assert_eq!(hard_left.left(), area().left() + TRAY_MARGIN);
        assert_eq!(hard_left.width(), TRAY_WIDTH);
        for card in [hard_right, hard_left] {
            assert!(area().contains_rect(card));
        }
    }

    /// More rows than fit: the card caps, and the scroll offset picks up where
    /// it is told to.
    #[test]
    fn a_long_yank_shows_a_windowful() {
        let clip = Clipboard::yank((0..30).map(|i| PathBuf::from(format!("/f{i}"))));
        assert_eq!(
            geometry(area(), chip(), row(), &clip, true, 0).rows.len(),
            ROWS
        );
        // Near the end there are fewer rows left to draw than the cap.
        assert_eq!(
            geometry(area(), chip(), row(), &clip, true, 28).rows.len(),
            2
        );
    }

    /// A short window: the card stops at the window's foot with the rows that
    /// fit there, never none, and scrolls the rest, its bar there only while
    /// rows are left out; a narrow one keeps the card's right edge inside it.
    /// A tall window shows the old windowful.
    #[test]
    fn a_short_window_scrolls_the_tray() {
        let clip = Clipboard::yank((0..30).map(|i| PathBuf::from(format!("/f{i}"))));
        let few = carrying(&["/a", "/b"]);
        assert_eq!(fit(area(), row()), ROWS, "the old windowful");
        assert!(bar(&geometry(area(), chip(), row(), &clip, true, 0), 0, 30).is_some());
        assert_eq!(
            bar(&geometry(area(), chip(), row(), &few, true, 0), 0, 2),
            None
        );

        let short = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, 200.0));
        let rows = fit(short, row());
        assert!((1..ROWS).contains(&rows), "{rows}");
        let g = geometry(short, chip(), row(), &clip, true, 0);
        let card = g.card.expect("out");
        assert!(short.contains_rect(card), "{card:?}");
        assert_eq!(g.rows.len(), rows);
        assert!(g.rows.iter().all(|rect| card.contains_rect(*rect)));
        assert!(bar(&g, 0, 30).is_some());
        assert!(g.band.is_some_and(|band| card.contains_rect(band)));
        assert_eq!(geometry(short, chip(), row(), &few, true, 0).band, None);
        let mut carry = 0.0;
        let last = (0..20).fold(0, |first, _| {
            scroll(first, 30, rows, &mut carry, -3.0 * TRAY_ROW)
        });
        assert_eq!(last, 30 - rows, "the last windowful of the rows that fit");
        assert_eq!(
            fit(
                egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, 80.0)),
                row()
            ),
            1,
            "never none"
        );

        let narrow = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, 900.0));
        let chip = egui::Rect::from_min_max(egui::pos2(120.0, 34.0), egui::pos2(180.0, 58.0));
        let card = geometry(narrow, chip, row(), &few, true, 0)
            .card
            .expect("out");
        assert!(narrow.contains_rect(card), "{card:?}");
    }

    /// The wheel moves whole rows, keeps a trackpad's fractions until they add
    /// up, and stops at either end of the list without owing anything back.
    #[test]
    fn the_wheel_scrolls_the_tray_a_row_at_a_time() {
        let down = |rows: f32| -rows * TRAY_ROW;
        let mut carry = 0.0;
        // Three rows down, then back up two.
        assert_eq!(scroll(0, 30, ROWS, &mut carry, down(3.0)), 3);
        assert_eq!(scroll(3, 30, ROWS, &mut carry, down(-2.0)), 1);
        // A trackpad's four tenths of a row, three times: nothing, nothing, one.
        let mut carry = 0.0;
        assert_eq!(scroll(0, 30, ROWS, &mut carry, down(0.4)), 0);
        assert_eq!(scroll(0, 30, ROWS, &mut carry, down(0.4)), 0);
        assert_eq!(scroll(0, 30, ROWS, &mut carry, down(0.4)), 1);
        // The last windowful is as far as it goes…
        let last = 30 - ROWS;
        let mut carry = 0.0;
        assert_eq!(scroll(last - 1, 30, ROWS, &mut carry, down(5.0)), last);
        // …and pushing on against the stop leaves no debt: the first row back
        // up moves at once.
        assert_eq!(scroll(last, 30, ROWS, &mut carry, down(0.9)), last);
        assert_eq!(scroll(last, 30, ROWS, &mut carry, down(-1.0)), last - 1);
        // The top holds the same way, and a list that fits does not move.
        let mut carry = 0.0;
        assert_eq!(scroll(0, 30, ROWS, &mut carry, down(-4.0)), 0);
        assert_eq!(scroll(0, ROWS, ROWS, &mut carry, down(3.0)), 0);
    }

    /// The tray paints in every state without panicking, including in a window
    /// far too small for it.
    #[test]
    fn the_tray_paints_without_panicking() {
        let theme = df_core::config::Theme::default();
        let palette = crate::theme::Palette::from_theme(&theme);
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(Default::default(), |ui| {
            let painting = crate::ui::Painting {
                tips: None,
                held: None,
                painter: ui.painter(),
                palette: &palette,
                theme: &theme,
                tags: &crate::tags::BUILT_IN,
                nerd: false,
                show_symlink: true,
                now: std::time::Instant::now(),
            };
            let hovers = crate::hover::Hovers::new();
            let ripples = crate::ripple::Ripples::new();
            for clip in [
                carrying(&["/a/one.txt", "/b/two.txt"]),
                Clipboard::cut([PathBuf::from("/a/one.txt")]),
                Clipboard::default(),
            ] {
                for (area, chip, row) in [
                    (area(), chip(), row()),
                    (
                        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(120.0, 60.0)),
                        egui::Rect::from_min_max(egui::pos2(60.0, 4.0), egui::pos2(110.0, 20.0)),
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(120.0, 24.0)),
                    ),
                ] {
                    for open in [false, true] {
                        let g = geometry(area, chip, row, &clip, open, 0);
                        paint(&painting, &clip, &g, 0, &hovers, &ripples);
                    }
                }
            }
        });
    }

    /// What is gone goes; what is on a server stays, because a local `stat`
    /// cannot see it.
    #[test]
    fn pruning_drops_what_is_no_longer_there() {
        // `/` exists on every machine this runs on; the next two do not. The
        // display path is set down as-is, the way a remote row spells it.
        let mut clip = Clipboard {
            mode: df_core::ops::PasteMode::Copy,
            paths: [
                "/",
                "/nope-df-tray",
                "/also-nope-df",
                "sftp://host/home/me/far.txt",
            ]
            .map(PathBuf::from)
            .to_vec(),
        };
        assert_eq!(clip.len(), 4);
        assert_eq!(prune(&mut clip), 2);
        assert_eq!(
            clip.paths,
            [
                PathBuf::from("/"),
                PathBuf::from("sftp://host/home/me/far.txt")
            ]
        );
    }
}
