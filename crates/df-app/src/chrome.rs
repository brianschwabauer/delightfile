//! The chrome around the panes: the tab strip, the top row (crumbs, prompt and
//! status cluster), the which-key card and the help browser.
//!
//! All four are painted, like everything else in delightfile, straight onto an
//! [`egui::Painter`] — see [`crate::ui`]'s header for why there are no widgets
//! here. They are in their own file because they are the pieces that *float*:
//! the panes own the window's space and these four are laid over or beside it,
//! and keeping them apart means the pane painter never grows a special case for
//! "unless the help is open".
//!
//! ## The card look
//!
//! One card style, shared (ported from delightviewer's `ui::card`): a
//! near-opaque plate of the palette's darkest ground, a hairline edge to lift it
//! off whatever it covers, and generous rounding. The which-key card and the
//! help overlay are the same surface at two sizes, which is what makes them read
//! as one program rather than two panels somebody drew on different days.
//!
//! Nested rounding is concentric throughout (`delightful-ui` §15): a row inside
//! a card is inset by [`CARD_PAD`] and its radius is the card's minus that
//! inset, so the gap around a highlighted row stays a constant width as it turns
//! the card's corner.

use std::sync::Arc;

use df_core::fs::is_case_sensitive;
use df_core::keymap::{Chord, Command};
use df_core::text::grouped;

use crate::help::{self, Help, HelpLine};
use crate::hover::{pressed_rect, Hovers};
use crate::input::Prompt;
use crate::ripple::Ripples;
use crate::theme::mix;
use crate::ui::{Control, Painting, CHROME_HEIGHT, GAP, ROW_RADIUS, TOP_HEIGHT};

/// The padding inside a card, and the inset its rows get on every adjacent
/// side (`delightful-ui` §15's even insets).
pub const CARD_PAD: f32 = 10.0;

/// A row inside a card rounds like a row inside a pane: they are the same kind
/// of thing at the same size, and two radii for one shape would be two.
pub const CARD_ROW_RADIUS: u8 = ROW_RADIUS;

/// The floating card's corner radius: **its row's plus the padding**, so the
/// gap around a highlighted row stays a constant width as it turns the card's
/// corner. Derived, never picked.
pub const CARD_RADIUS: u8 = CARD_ROW_RADIUS + CARD_PAD as u8;

/// How far a floating card keeps off the window's edge, and off the chrome it
/// sits above.
pub const CARD_MARGIN: f32 = 16.0;

/// How opaque a card's plate is, 0–255.
///
/// Not quite opaque: a hint of the pane beneath is what says the card is a
/// temporary thing lying on top rather than a region of the window that has
/// changed. Below about 220 the file names underneath start showing through the
/// text on the card, which is where "layered" turns into "unreadable".
const CARD_ALPHA: u8 = 242;

/// Body text on the chrome, in logical points. A shade under the pane's row
/// text: this is the frame, not the content.
pub const FONT: f32 = 12.5;

/// A line of a card's list.
pub const CARD_ROW: f32 = 20.0;

/// Horizontal padding inside a bar or a chip.
pub const PAD_X: f32 = 8.0;

/// The space between a chip's icon and the word beside it, in logical points.
///
/// The icon is measured, not assumed: a patched-font glyph can be nearly two
/// characters wide where the plain face's stand-in is one, and a word set a
/// fixed em from the chip's edge butts into the wider one. Six points is a
/// word space at this size — the gap between two words, which is what an icon
/// and its label are.
pub const ICON_GAP: f32 = 6.0;

/// How wide an icon is, in the face it is set in, for laying a label after it.
fn icon_width(painter: &egui::Painter, glyph: &str, font: &egui::FontId) -> f32 {
    text_width(painter, glyph, font.clone())
}

/// A chip's inset inside the top row, on **every** adjacent side
/// (`delightful-ui` §15's even insets).
///
/// Three: a chip has to keep enough of its own plate to read as a pill, so
/// this is about as much as the row can give away and still have two
/// distinguishable surfaces. It is a *fixed* inset rather than a fraction of
/// the row, so a chip grows with the bar it sits in — at [`TOP_HEIGHT`] the
/// chip is the same share of the row it was at [`CHROME_HEIGHT`], which is
/// why the bar getting taller did not need a second number here.
pub const CHIP_INSET: f32 = 3.0;

/// A chip's corner radius: **the row's less its inset**, so the gap between a
/// chip and the row's own corner stays a constant width as it turns
/// (`delightful-ui` §15). Derived, never picked.
pub const CHIP_RADIUS: u8 = ROW_RADIUS - CHIP_INSET as u8;

/// How far a chip's plate is tinted towards its accent at rest.
///
/// The number every count on the top row has always been drawn with, named
/// now that the hover and the fade multiply it: a wash, not a fill — the text
/// on the chip is what is being read, and a plate at much above this starts
/// competing with it.
const CHIP_TINT: f32 = 0.16;

// ── Tab strip (PLAN §2) ─────────────────────────────────────────────────────

/// The gap between two tab chips: **exactly one pigtail wide**.
///
/// It used to be half the window's [`GAP`], which was a number picked for how
/// close two plates should sit. There are no two plates any more — an inactive
/// tab is the window ground (see [`tab_strip`]) — so the only thing this space
/// has to hold is the active tab's flare, and holding it *exactly* is what
/// keeps the flare off its neighbour's text and off any hover plate the
/// neighbour is wearing. Derived from [`TAB_PIGTAIL`], never picked.
const TAB_GAP: f32 = TAB_PIGTAIL;

/// The radius of a tab's top corners, and of its pigtails.
///
/// Its own number rather than [`ROW_RADIUS`]: a tab is the one surface in the
/// window that is drawn as a *shape* — turning in at the top and out at the
/// bottom — and at the rows' six points that shape was too tight to read as
/// one. Eight is where the curve is a curve at strip height and the pigtail's
/// flare is unmistakably a flare. The row the tab joins keeps its own radius
/// on the corners that are its own.
pub const TAB_RADIUS: u8 = 8;

/// The narrowest a tab chip gets when it is sized to its title, in logical
/// points: room for the numeral, a few letters, and the ellipsis a long name
/// ends in. Narrower and the chip is a numeral with a smudge after it.
pub const TAB_MIN_WIDTH: f32 = 64.0;

/// The widest a tab chip gets, in logical points.
///
/// Directory names are short and a strip of nine equal chips across a 1400 pt
/// window would give each one 150 pt of mostly empty plate. Capping the width
/// keeps two tabs looking like two tabs rather than like a segmented control
/// that has taken over the top of the window.
pub const TAB_MAX_WIDTH: f32 = 190.0;

/// The active tab's pigtails: the concave quarter-circles at its bottom
/// corners that flare out past its edges and run into the top row's ground,
/// the way a browser tab's do.
///
/// [`TAB_RADIUS`] — the *same* radius the tab's own top corners wear, so the
/// tab is one shape drawn with one curve: it turns in at the top and out at
/// the bottom by the same amount, which is what makes it read as a folder tab
/// rather than as a rectangle with something happening at its feet. It was
/// [`CHIP_RADIUS`] (three points), on the reasoning that a pigtail must fit
/// inside the gap; at that size it was invisible, and the honest fix is to
/// size the gap off the pigtail rather than the pigtail off the gap.
const TAB_PIGTAIL: f32 = TAB_RADIUS as f32;

/// How far above and below the strip a tab drag still counts as a *reorder*
/// rather than as a tab on its way out of the window.
///
/// Twelve, a little under half [`CHROME_HEIGHT`]: far enough that sliding a
/// chip along a 26 pt strip with a normal hand does not fall out of the
/// gesture, near enough that it is well inside
/// [`crate::window::DETACH_THRESHOLD`]'s 40 pt — so the two gestures cannot
/// both be true, and the band a hand has to cross to get from one to the other
/// is wide enough to be a decision.
pub const TAB_REORDER_BAND: f32 = 12.0;

/// How long the tabs take to slide aside for a chip being carried past them.
///
/// `delightful-ui` §5's reflow: the same order as [`crate::flip::TRAVEL`], and
/// on the same curve, because it is the same event — a list that has changed
/// order under the pointer and is showing the reader where the rows went.
pub const TAB_SLIDE: std::time::Duration = std::time::Duration::from_millis(200);

/// The hairline between two adjacent inactive tabs, as a fraction of the
/// strip's height.
///
/// Short of full height on purpose: a rule that ran the whole way down would
/// draw a grid, and what is wanted is the *hint* of a division between two
/// titles that share a ground. Three fifths is delightstack's separator
/// proportion inside a menu, which is the same problem.
const TAB_SEPARATOR_HEIGHT: f32 = 0.6;

/// How wide that hairline is. One point — a *hair*, not a rule: it is there to
/// be found by an eye already looking for the join between two titles, and
/// anything thicker would compete with the active tab's edge.
const TAB_SEPARATOR_WIDTH: f32 = 1.0;

/// How far a chip in the hand is tinted off the ground while it is carried.
///
/// An inactive tab has no plate at rest; one being dragged needs to look
/// picked up, and this is the plate it borrows for as long as it is off the
/// ground. It fades back out as the chip settles into its slot, so nothing
/// pops at the end of the landing.
const TAB_CARRY_TINT: f32 = 0.8;

/// How wide each tab's chip wants to be: its title's width plus the numeral
/// and the padding, held between [`TAB_MIN_WIDTH`] and [`TAB_MAX_WIDTH`].
///
/// Measured once a frame in the same face the chip is drawn in, and then
/// handed to everything that lays the strip out — the paint, the hit test, the
/// drag — so they are all reading one set of numbers. A chip sized to its name
/// says what it is without wasting a third of the strip on a name that is
/// four letters long.
pub fn tab_widths(painter: &egui::Painter, titles: &[String]) -> Vec<f32> {
    titles
        .iter()
        .map(|title| {
            let text = text_width(painter, title, egui::FontId::proportional(FONT));
            (PAD_X + FONT + text + PAD_X).clamp(TAB_MIN_WIDTH, TAB_MAX_WIDTH)
        })
        .collect()
}

/// Where each tab's chip goes, given how wide each one wants to be
/// ([`tab_widths`]).
///
/// Shared by the paint and the hit test, so a click lands on the chip it looks
/// like it landed on — two functions computing this separately is how a strip
/// grows a one-pixel lie at its edges.
///
/// When the chips together are wider than the strip they are all squeezed by
/// the same factor rather than the last ones being cut off: a tab you cannot
/// see is a tab you cannot switch to with the pointer, and a strip that is
/// tight everywhere is a strip that is honest about being full.
pub fn tab_rects(strip: egui::Rect, widths: &[f32]) -> Vec<egui::Rect> {
    let count = widths.len();
    if count == 0 {
        return Vec::new();
    }
    let chips = tab_room(strip);
    let total_gap = TAB_GAP * (count - 1) as f32;
    let natural: f32 = widths.iter().map(|w| w.max(0.0)).sum();
    let room = (chips.width() - total_gap).max(0.0);
    let squeeze = if natural > room && natural > 0.0 {
        room / natural
    } else {
        1.0
    };
    let mut left = chips.left();
    widths
        .iter()
        .map(|width| {
            let width = (width.max(0.0) * squeeze).max(0.0);
            let rect = egui::Rect::from_min_size(
                egui::pos2(left, strip.top()),
                egui::vec2(width, strip.height()),
            );
            left += width + TAB_GAP;
            rect
        })
        .collect()
}

/// The part of the strip the chips are laid out in: all of it but the `+`'s
/// square at the end and the gap before it.
///
/// Held back even when the chips are nowhere near the end, so that a strip
/// squeezed full still has its `+`: the one way to a new tab that does not
/// need the keyboard.
fn tab_room(strip: egui::Rect) -> egui::Rect {
    let right = (strip.right() - TAB_GAP - CHROME_HEIGHT).max(strip.left());
    egui::Rect::from_min_max(strip.min, egui::pos2(right, strip.bottom()))
}

/// Where the `+` goes ([`Control::TabNew`]): a strip-high square one
/// [`TAB_GAP`] after the last chip, which is where a hand looks for the next
/// tab — and at the strip's far end, in the room [`tab_room`] keeps for it,
/// once the chips are squeezed up to it.
pub fn tab_new_rect(strip: egui::Rect, widths: &[f32]) -> egui::Rect {
    let left = tab_rects(strip, widths)
        .last()
        .map_or(strip.left(), |last| last.right() + TAB_GAP);
    egui::Rect::from_min_size(
        egui::pos2(left, strip.top()),
        egui::vec2(CHROME_HEIGHT, strip.height()),
    )
}

/// The chip's numeral slot, the full height of the chip: where its `×`
/// ([`Control::TabClose`]) is drawn and hit.
///
/// The numeral's own column and nothing wider, so the `×` takes the number's
/// place without the title beside it moving a point. Held inside the chip, for
/// a strip squeezed so tight the column no longer fits.
pub fn tab_close_rect(chip: egui::Rect) -> egui::Rect {
    egui::Rect::from_min_max(
        egui::pos2(chip.left() + PAD_X, chip.top()),
        egui::pos2(chip.left() + PAD_X + FONT, chip.bottom()),
    )
    .intersect(chip)
}

/// What in the strip a point is over, if anything: a chip's `×`, the rest of
/// a chip, or the `+` after them.
pub fn tab_at(strip: egui::Rect, widths: &[f32], pos: egui::Pos2) -> Option<Control> {
    let rects = tab_rects(strip, widths);
    if let Some(index) = rects.iter().position(|rect| rect.contains(pos)) {
        return Some(if tab_close_rect(rects[index]).contains(pos) {
            Control::TabClose(index)
        } else {
            Control::Tab(index)
        });
    }
    tab_new_rect(strip, widths)
        .contains(pos)
        .then_some(Control::TabNew)
}

/// The band a tab drag has to stay inside to be a *reorder*.
///
/// The strip, grown [`TAB_REORDER_BAND`] above and below it and not one point
/// sideways: the strip already spans the window, and a hand that has run off
/// its end is still sliding a chip along it.
pub fn tab_band(strip: egui::Rect) -> egui::Rect {
    strip.expand2(egui::vec2(0.0, TAB_REORDER_BAND))
}

/// Whether a tab drag at `at` is reordering rather than leaving.
///
/// The one question that decides which gesture is live, asked from the pointer
/// alone so that a hand can cross back and forth between the two and the
/// answer changes with it — there is no mode to get stuck in.
pub fn reordering(strip: egui::Rect, at: egui::Pos2) -> bool {
    tab_band(strip).contains(at)
}

/// Where the chip being carried is drawn: its own size, at the pointer, held
/// inside the strip.
///
/// `grab_dx` is where in the chip the button went down, so the chip travels
/// with the point the hand took hold of rather than jumping its centre under
/// the cursor. Clamped to the strip because a chip that could be dragged off
/// the end would be a chip in a slot that does not exist — to the chips' part
/// of it, short of the square [`tab_room`] keeps for the `+`, so a strip
/// squeezed full ends the carry where its last slot ends.
///
/// A strip with room to spare lets the chip on past its last slot and over the
/// `+`, which the chip in the hand is drawn over. Stopping it at the end of the
/// last chip instead would strand a slot: a slot is found from the carried
/// chip's middle ([`tab_slot`]), and a wide chip held at the end of a run of
/// narrow ones has its middle nearer the second-last slot than the last.
pub fn tab_carry(
    strip: egui::Rect,
    widths: &[f32],
    index: usize,
    grab_dx: f32,
    x: f32,
) -> egui::Rect {
    let rects = tab_rects(strip, widths);
    let Some(home) = rects.get(index).copied() else {
        return egui::Rect::NOTHING;
    };
    let room = tab_room(strip);
    let left = (x - grab_dx).clamp(room.left(), (room.right() - home.width()).max(room.left()));
    egui::Rect::from_min_size(egui::pos2(left, home.top()), home.size())
}

/// Which slot a carried chip would drop into, from where its middle is.
///
/// The nearest slot's, rather than a division of the strip's width: "which
/// slot is this chip mostly over" and "which slot centre is it nearest" are
/// the same question, and the nearest one cannot fall off the end however the
/// chips are sized. Measured against the tabs' *home* slots, which are the
/// ones the hand can see the chip passing over — the shifted layout is what
/// the strip is animating towards, not what it is showing.
pub fn tab_slot(strip: egui::Rect, widths: &[f32], centre_x: f32) -> usize {
    tab_rects(strip, widths)
        .iter()
        .enumerate()
        .min_by(|a, b| {
            (a.1.center().x - centre_x)
                .abs()
                .total_cmp(&(b.1.center().x - centre_x).abs())
        })
        .map(|(index, _)| index)
        .unwrap_or(0)
}

/// The tab index sitting in each slot while the tab at `from` is being carried
/// to slot `to` — the order the strip would have if the chip were let go now.
///
/// Remove-then-insert, which is what the drop itself does
/// ([`crate::tabs::Tabs::reorder`]): the two must agree, or the tabs would
/// slide one way during the drag and land another.
pub fn tab_order(count: usize, from: usize, to: usize) -> Vec<usize> {
    let mut order: Vec<usize> = (0..count).collect();
    if from >= count {
        return order;
    }
    let tab = order.remove(from);
    order.insert(to.min(order.len()), tab);
    order
}

/// Where every tab sits while the tab at `from` is carried to slot `to`,
/// indexed **by tab**, not by slot.
///
/// The carried tab's entry is its would-be resting place; the painter draws it
/// at the pointer instead and uses this only as the target the landing
/// animation aims at.
///
/// The slots are laid out again in the new order, with each tab's own width,
/// rather than the tabs being dealt into the old slots: chips are sized to
/// their titles, so a wide tab moved past a narrow one changes where every
/// slot after them begins.
pub fn tab_shifted(strip: egui::Rect, widths: &[f32], from: usize, to: usize) -> Vec<egui::Rect> {
    let count = widths.len();
    let order = tab_order(count, from, to);
    let reordered: Vec<f32> = order
        .iter()
        .map(|tab| widths.get(*tab).copied().unwrap_or(0.0))
        .collect();
    let rects = tab_rects(strip, &reordered);
    let mut shifted = tab_rects(strip, widths);
    for (slot, tab) in order.into_iter().enumerate() {
        if let (Some(dest), Some(rect)) = (rects.get(slot), shifted.get_mut(tab)) {
            *rect = *dest;
        }
    }
    shifted
}

/// A tab chip that is not where its slot is: one in the hand, or one on its
/// way back down into a slot.
///
/// It carries the other tabs' displacement with it because the two are one
/// motion — the chip goes somewhere and the strip opens to receive it — and a
/// painter given only the chip would have to work the rest out again.
#[derive(Debug, Clone)]
pub struct Carry {
    /// Which tab is off the ground.
    pub tab: usize,
    /// Where its chip is drawn this frame.
    pub rect: egui::Rect,
    /// How far each tab has slid from its own slot, by tab index. Empty means
    /// nothing has moved.
    pub offsets: Vec<f32>,
    /// 0 while the chip is in the hand, 1 once it has settled into a slot.
    /// The bottom corners and the pigtails come back over this, so a chip that
    /// has landed does not snap from *floating plate* to *folder tab*.
    pub settle: f32,
}

/// Draw the strip. Only called with two or more tabs (PLAN §2).
///
/// The strip sits **flush** on the top row, and there is exactly one plate on
/// it: the active tab's, which is the top row's own ground carried up over the
/// chip and flared back down into it through two concave [`TAB_PIGTAIL`] arcs.
/// The two are one shape, which is the only honest way to draw "this tab is
/// the path below it" — and it is why the strip is drawn *before* the top row
/// rather than beside it.
///
/// **The inactive tabs have no plate at all.** They used to have a quieter one,
/// lifted slightly off the ground and dropped a couple of points below the
/// active tab's top edge, and it read backwards: a lighter plate beside a
/// darker one says the *lighter* one is on top, so the tab you were looking at
/// was the one that looked recessed. Now they are simply the window ground
/// with a title on it, divided by a hairline where two of them meet, and the
/// active tab is unmistakable because it is the only tab that is a surface.
///
/// `filter` is the committed filter's fade, passed through to [`bar_fill`] so
/// the active tab is tinted by exactly as much as the row it joins.
///
/// `carry` is the chip that is off the ground — one being dragged along the
/// strip, or one settling into the slot it was dropped in — and the sideways
/// displacement the rest of the strip is wearing to make room for it.
///
/// After the chips is the `+` ([`tab_new_rect`]), and a chip under the pointer
/// wears its `×` in its numeral's slot ([`tab_close_rect`]).
#[allow(clippy::too_many_arguments)] // a painter's arguments are its inputs
pub fn tab_strip(
    paint: &Painting<'_>,
    strip: egui::Rect,
    titles: &[String],
    active: usize,
    filter: f32,
    widths: &[f32],
    carry: Option<&Carry>,
    hovers: &Hovers<Control>,
    ripples: &Ripples<Control>,
) {
    let palette = paint.palette;
    let count = titles.len();
    let homes = tab_rects(strip, widths);
    // A carry naming a tab that is no longer there — a scan landed, a `}` went
    // past, the strip shrank — is no carry at all, rather than a panic or a
    // chip drawn for a tab that has gone.
    let carry = carry.filter(|carry| carry.tab < count);
    let rects: Vec<egui::Rect> = homes
        .iter()
        .enumerate()
        .map(|(index, home)| {
            let dx = carry
                .and_then(|carry| carry.offsets.get(index).copied())
                .unwrap_or(0.0);
            home.translate(egui::vec2(dx, 0.0))
        })
        .collect();

    // The hairlines, under everything: they divide two grounds, and the moment
    // either side of one is a *surface* — the active tab, or a chip in the
    // hand — the surface's own edge is already doing the dividing.
    let quiet = |index: usize| index != active && carry.is_none_or(|carry| carry.tab != index);
    for index in 0..count.saturating_sub(1) {
        if !(quiet(index) && quiet(index + 1)) {
            continue;
        }
        let (left, right) = (rects[index], rects[index + 1]);
        let x = (left.right() + right.left()) / 2.0;
        let half = strip.height() * TAB_SEPARATOR_HEIGHT / 2.0;
        paint.painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(x - TAB_SEPARATOR_WIDTH / 2.0, strip.center().y - half),
                egui::pos2(x + TAB_SEPARATOR_WIDTH / 2.0, strip.center().y + half),
            ),
            0,
            crate::theme::hairline(palette),
        );
    }

    // The active tab after the quiet ones, so its pigtails are drawn over the
    // ground beside it rather than under whatever is there; the carried chip
    // after everything, because it is the thing in the hand.
    let seated = (0..count).filter(|index| Some(*index) != carry.map(|c| c.tab));
    let order = seated
        .clone()
        .filter(|index| *index != active)
        .chain(seated.filter(|index| *index == active));
    // A chip actually in the hand takes the hover with it: the hit test is
    // still measuring the *slots*, which the tabs have slid out of, so a
    // highlight left switched on would land on whichever tab the pointer's
    // old slot now belongs to. A chip merely settling into a slot is past
    // that — the pointer is free again and the strip has stopped moving.
    let in_hand = carry.is_some_and(|carry| carry.settle <= 0.0);
    // The chip is lit while the pointer is anywhere on it, its `×` included:
    // moving onto the `×` is still being on the chip, and a chip that went
    // dark under it would take the `×` away with its hover.
    let warm = |index: usize| {
        if in_hand {
            Warmth::default()
        } else {
            let (key, close) = (Control::Tab(index), Control::TabClose(index));
            Warmth {
                hover: hovers.hover(key).max(hovers.hover(close)),
                press: hovers.press(key),
                close: hovers.hover(close),
            }
        }
    };
    for index in order {
        tab_chip(
            paint,
            strip,
            rects[index],
            index,
            &titles[index],
            index == active,
            1.0,
            filter,
            warm(index),
            ripples,
        );
    }
    // The `+` stays where it is while a chip is carried — the chips are
    // trading places, not changing how much room they take — and under the
    // chip in the hand, which may be carried over it.
    let (hover, press) = if in_hand {
        (0.0, 0.0)
    } else {
        (hovers.hover(Control::TabNew), hovers.press(Control::TabNew))
    };
    tab_new(paint, tab_new_rect(strip, widths), (hover, press), ripples);
    if let Some(carry) = carry {
        tab_chip(
            paint,
            strip,
            carry.rect,
            carry.tab,
            &titles[carry.tab],
            carry.tab == active,
            carry.settle,
            filter,
            // A chip off the ground wears its own plate, and a hover under it
            // would be a second one saying the same thing more faintly.
            Warmth::default(),
            ripples,
        );
    }
}

/// How lit a chip is, read once by [`tab_strip`] and handed to [`tab_chip`]
/// whole: the strip decides whether any of it applies (see there), and a chip
/// that read the hovers itself could not be told to ignore them.
#[derive(Debug, Clone, Copy, Default)]
struct Warmth {
    /// The chip's hover, its `×` included: the plate an inactive chip lifts
    /// on, and how far the numeral has turned into the `×`.
    hover: f32,
    /// The chip's own press. Not the `×`'s — that one closes the chip, and an
    /// inset on a chip that is going away is never seen.
    press: f32,
    /// The `×`'s own hover, which brightens the glyph.
    close: f32,
}

/// The `+` after the last chip ([`Control::TabNew`]): a glyph at rest, and the
/// plate an inactive chip lifts on under the pointer — it sits among them and
/// answers the hand the way they do — with the press and ripple every control
/// gets.
fn tab_new(
    paint: &Painting<'_>,
    rect: egui::Rect,
    (hover, press): (f32, f32),
    ripples: &Ripples<Control>,
) {
    let palette = paint.palette;
    let rect = pressed_rect(rect, press);
    if hover > 0.0 {
        paint.painter.rect_filled(
            rect,
            TAB_RADIUS,
            mix(palette.crust, palette.surface0, hover),
        );
    }
    let inside = paint.painter.with_clip_rect(rect);
    for splash in ripples.splashes(Control::TabNew, paint.now) {
        inside.circle_filled(
            splash.center,
            splash.radius,
            crate::theme::splash(palette, splash.alpha),
        );
    }
    inside.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "+",
        egui::FontId::proportional(FONT + 3.0),
        mix(palette.faint, palette.text, hover),
    );
}

/// One chip of the strip: its plate if it has one, its ripples, its number (or,
/// under the pointer, its `×`) and its title.
///
/// `settle` is 1 for a chip in its slot and 0 for one in the hand, and every
/// difference between the two rides on it — the active tab's bottom corners
/// and pigtails come back over it, and the plate a carried inactive chip
/// borrows fades out over it — so a chip landing in a slot arrives as the tab
/// that lives there instead of snapping into it.
#[allow(clippy::too_many_arguments)] // a painter's arguments are its inputs
fn tab_chip(
    paint: &Painting<'_>,
    strip: egui::Rect,
    rect: egui::Rect,
    index: usize,
    title: &str,
    is_active: bool,
    settle: f32,
    filter: f32,
    warmth: Warmth,
    ripples: &Ripples<Control>,
) {
    let palette = paint.palette;
    let key = Control::Tab(index);
    let Warmth {
        hover,
        press,
        close,
    } = warmth;
    let settle = settle.clamp(0.0, 1.0);
    // The press inset is the inactive tabs' alone: shrinking the active one
    // would open a seam between it and the row it is joined to, and pressing
    // the tab you are already on does nothing anyway. A chip in the hand does
    // not take it either — it is already off the ground.
    let rect = if is_active || settle < 1.0 {
        rect
    } else {
        pressed_rect(rect, press)
    };
    if is_active {
        // The top row's own ground: the tab and the path it is about are one
        // surface, and this is the expression that says so.
        let fill = bar_fill(palette, filter);
        // Square at the bottom once it is seated, because that is where it
        // meets the row — a rounded bottom corner would be a gap between two
        // things that are touching. Off the ground it rounds all four ways:
        // nothing is under it to join.
        let bottom = (TAB_RADIUS as f32 * (1.0 - settle)).round() as u8;
        paint.painter.rect_filled(
            rect,
            egui::CornerRadius {
                nw: TAB_RADIUS,
                ne: TAB_RADIUS,
                sw: bottom,
                se: bottom,
            },
            fill,
        );
        // The left pigtail is skipped on the tab in the first slot: its left
        // edge lines up with the top row's, and a flare out over the ground
        // there would hang off the end of the row it is supposed to join —
        // which is also why the row squares that corner (see [`bar_ground`]).
        // Same at the other end, for a tab whose right edge is the row's.
        let radius = TAB_PIGTAIL * settle;
        if rect.left() > strip.left() + 0.5 {
            pigtail(paint, rect, fill, radius, false);
        }
        if rect.right() < strip.right() - 0.5 {
            pigtail(paint, rect, fill, radius, true);
        }
    } else {
        // No plate at rest — the ground *is* the inactive tab. What is drawn
        // here is the hover, and the plate a chip borrows while it is in the
        // hand, which fades back into the ground as the chip settles.
        let plate = mix(palette.crust, palette.surface0, hover);
        let plate = mix(plate, palette.surface1, TAB_CARRY_TINT * (1.0 - settle));
        if hover > 0.0 || settle < 1.0 {
            paint.painter.rect_filled(rect, TAB_RADIUS, plate);
        }
    }

    let inside = paint.painter.with_clip_rect(rect);
    // The `×`'s ripples are the chip's too, clipped to it: the press that made
    // them closed the tab, so what they land on is the chip that has slid into
    // its place under the pointer.
    let splashes = ripples
        .splashes(key, paint.now)
        .chain(ripples.splashes(Control::TabClose(index), paint.now));
    for splash in splashes {
        inside.circle_filled(
            splash.center,
            splash.radius,
            crate::theme::splash(palette, splash.alpha),
        );
    }
    let color = if is_active {
        palette.text
    } else {
        palette.quiet
    };
    // The number is what `1`–`9` press, so it is on the chip rather than in
    // the help sheet: the strip teaches its own shortcut. Under the pointer it
    // gives its slot to the `×`, which closes this tab without switching to it
    // first. The two trade places over the hover's fade, so the number comes
    // back as the pointer leaves rather than snapping in behind it.
    let hover = hover.clamp(0.0, 1.0);
    inside.text(
        egui::pos2(rect.left() + PAD_X, rect.center().y),
        egui::Align2::LEFT_CENTER,
        format!("{}", index + 1),
        key_font(FONT - 1.0),
        fade(palette.faint, 1.0 - hover),
    );
    if hover > 0.0 {
        inside.text(
            tab_close_rect(rect).center(),
            egui::Align2::CENTER_CENTER,
            "×",
            egui::FontId::proportional(FONT + 2.0),
            fade(mix(palette.faint, palette.text, close), hover),
        );
    }
    let text_left = rect.left() + PAD_X + FONT;
    truncated(
        &inside,
        egui::pos2(text_left, rect.center().y),
        title,
        color,
        (rect.right() - PAD_X - text_left).max(0.0),
    );
}

/// One of the active tab's pigtails: the concave quarter-circle that carries
/// its bottom corner outwards and down into the top row's ground.
///
/// Drawn as a filled square of the tab's own colour with a disc of the window
/// ground bitten out of its *outer* corner, rather than as a path: egui fills a
/// closed path by fanning from its first point, which is only correct for a
/// convex outline — and a pigtail is concave by definition. Two primitives and
/// a clip rectangle give the exact shape with no tessellation to get wrong.
///
/// The bite is [`crate::theme::Palette::crust`], the window's own ground, and
/// it can be that unconditionally because the square it is taken out of sits
/// inside [`TAB_GAP`] — which is exactly one pigtail wide, so a neighbour's
/// hover plate can never be underneath it.
fn pigtail(paint: &Painting<'_>, tab: egui::Rect, fill: egui::Color32, radius: f32, right: bool) {
    if radius <= 0.0 {
        return;
    }
    let square = if right {
        egui::Rect::from_min_max(
            egui::pos2(tab.right(), tab.bottom() - radius),
            egui::pos2(tab.right() + radius, tab.bottom()),
        )
    } else {
        egui::Rect::from_min_max(
            egui::pos2(tab.left() - radius, tab.bottom() - radius),
            egui::pos2(tab.left(), tab.bottom()),
        )
    };
    paint.painter.rect_filled(square, 0, fill);
    // The bite: centred on the corner furthest from the tab, so what is left
    // of the square is the quarter that curves away from it. Clipped to the
    // square, because the rest of the disc would eat the tab.
    let centre = egui::pos2(
        if right { square.right() } else { square.left() },
        square.top(),
    );
    paint
        .painter
        .with_clip_rect(square)
        .circle_filled(centre, radius, paint.palette.crust);
}

// ── The breadcrumb path bar (PLAN §2) ───────────────────────────────────────

/// The separator drawn between two crumbs.
///
/// A chevron rather than the platform's `/`: the slash is *in* the path, and a
/// separator that looks like content makes a segment's own name ambiguous the
/// moment a directory has a slash-like character in it.
const CRUMB_SEPARATOR: &str = "›";

/// The separator's column, in logical points. Symmetric padding either side of
/// a one-character glyph.
const CRUMB_SEPARATOR_WIDTH: f32 = 13.0;

/// What is drawn when the path is too long for the bar.
const CRUMB_ELLIPSIS: &str = "…";

/// One clickable segment of the path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crumb {
    /// What is drawn: the directory's own name, or `/` for the root.
    pub label: String,
    /// Where clicking goes.
    pub path: std::path::PathBuf,
    /// Draw this segment as a **chip** — a plate in the accent colour — rather
    /// than as plain text.
    ///
    /// A local path has none: every segment of it is a directory on this
    /// machine and they are all the same kind of thing. The virtual locations
    /// have exactly one, and it is the first (PLAN §7.4, §7.6): the machine a
    /// remote listing is on, or the word `Trash`. Neither is a folder, and a
    /// bar reading `showandtour1 › srv › www` in one colour would look like a
    /// directory called `showandtour1` on this computer — which is precisely
    /// the mistake those two features must not invite.
    pub accent: bool,
}

/// The path, as segments from the root rightwards.
///
/// The last one is the directory you are in. It is still a crumb, shaped and
/// hit-tested like every other; the difference is only in what a click on it
/// does. It cannot go anywhere, so it opens the `Go to:` prompt over the bar
/// with the whole path in it — the app decides that, and this function does
/// not need to know.
pub fn crumbs(path: &std::path::Path) -> Vec<Crumb> {
    use std::path::Component;
    let mut out = Vec::new();
    let mut here = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir => {
                here.push("/");
                out.push(Crumb {
                    label: "/".to_string(),
                    path: here.clone(),
                    accent: false,
                });
            }
            Component::Normal(name) => {
                here.push(name);
                out.push(Crumb {
                    label: name.to_string_lossy().into_owned(),
                    path: here.clone(),
                    accent: false,
                });
            }
            // A relative path's `.`/`..`/prefix components cannot be clicked to
            // anywhere meaningful, so they are pushed onto the accumulator and
            // not offered as segments. In practice every path here is absolute.
            other => here.push(other.as_os_str()),
        }
    }
    out
}

/// The app menu's button, at the top row's leading end: a square as tall as a
/// chip, inset from the row's corner by [`CHIP_INSET`] on both sides it
/// shares with the row.
///
/// Even insets and [`CHIP_RADIUS`] are the same pair every chip on the row
/// wears (`delightful-ui` §15): the gap between the button and the row's own
/// rounded corner stays one width all the way round the turn. Square, because
/// it is one glyph and a hit target; the row's own height less the insets,
/// because that is the size every other plate on the row already is.
///
/// A function of the row alone, so the hit test, the paint, the crumbs laid
/// out after it and the menu that drops out of it all read one rect.
pub fn menu_button_rect(row: egui::Rect) -> egui::Rect {
    let side = (row.height() - CHIP_INSET * 2.0).max(0.0);
    egui::Rect::from_min_size(
        egui::pos2(row.left() + CHIP_INSET, row.top() + CHIP_INSET),
        egui::vec2(side, side),
    )
}

/// Where the path starts on the row: after the menu button and a word space.
///
/// [`ICON_GAP`] rather than a crumb separator's width: the button is not a
/// step of the path, and a chevron's worth of space before the root would read
/// as a step that had gone missing.
fn crumbs_left(row: egui::Rect) -> f32 {
    menu_button_rect(row).right() + ICON_GAP
}

/// The menu button's glyph with the patched font (nf-fa-bars)…
const MENU_ICON: char = '\u{f0c9}';

/// …and without it: the identity sign, the nearest thing to three bars the
/// stock faces draw.
const MENU_GLYPH: &str = "≡";

/// Where each crumb goes, sharing the measurement with the paint so a click
/// lands on the segment it looks like it landed on.
///
/// A crumb that did not fit gets [`egui::Rect::NOTHING`], which contains no
/// point — so an elided segment is simply not hit-testable, and the vector
/// stays index-aligned with `crumbs`.
pub fn crumb_rects(
    painter: &egui::Painter,
    bar: egui::Rect,
    crumbs: &[Crumb],
    reserved_right: f32,
) -> Vec<egui::Rect> {
    let font = egui::FontId::proportional(FONT);
    let widths: Vec<f32> = crumbs
        .iter()
        .map(|crumb| text_width(painter, &crumb.label, font.clone()) + PAD_X * 2.0)
        .collect();
    // The menu button holds the row's leading end, so the path is measured
    // against what is left after it.
    let start = crumbs_left(bar);
    let room = (bar.right() - PAD_X - reserved_right - start).max(0.0);
    // Elide from the *left*: the segment you are in and the ones just above it
    // are what a person is reading, and the root is the part they can guess.
    let mut first = 0;
    loop {
        let shown = &widths[first..];
        let separators = CRUMB_SEPARATOR_WIDTH * shown.len().saturating_sub(1) as f32;
        let ellipsis = if first > 0 {
            text_width(painter, CRUMB_ELLIPSIS, font.clone()) + CRUMB_SEPARATOR_WIDTH
        } else {
            0.0
        };
        if shown.iter().sum::<f32>() + separators + ellipsis <= room || first + 1 >= widths.len() {
            break;
        }
        first += 1;
    }

    let mut rects = vec![egui::Rect::NOTHING; crumbs.len()];
    let mut x = start;
    if first > 0 {
        x += text_width(painter, CRUMB_ELLIPSIS, font.clone()) + CRUMB_SEPARATOR_WIDTH;
    }
    for (index, width) in widths.iter().enumerate().skip(first) {
        if index > first {
            x += CRUMB_SEPARATOR_WIDTH;
        }
        rects[index] = egui::Rect::from_min_size(
            // Inset by the same amount on every adjacent side as every other
            // chip on this row, so the row has one nesting rule and not two.
            egui::pos2(x, bar.top() + CHIP_INSET),
            egui::vec2(*width, (bar.height() - CHIP_INSET * 2.0).max(0.0)),
        );
        x += width;
    }
    rects
}

/// The text of the breadcrumb's git chip: the branch, and the dirty count once
/// a status has landed (PLAN §7.3).
///
/// `main ·3` — the branch, a middle dot, and one number. **One** number, not
/// git's four: the chip is read at a glance while doing something else, and its
/// job is to answer "is there anything uncommitted here", which is a yes or a
/// no with a magnitude. The breakdown lives in the terminal the user is going to
/// type `git status` into anyway.
///
/// The count is omitted entirely when the tree is clean and when no scan has
/// landed yet, and those two are deliberately the same picture: a chip that read
/// `main ·0` for the half-second before the first status came back would be a
/// lie, and one that showed a spinner would be motion asking to be watched. The
/// branch is what the breadcrumb is for; the count arrives when it arrives.
///
/// Pure, so the formatting is a test rather than a repository.
pub fn branch_label(branch: &str, counts: Option<df_core::git::DirtyCounts>) -> String {
    match counts {
        Some(counts) if !counts.is_clean() => {
            format!("{branch} ·{}", grouped(counts.total() as u64))
        }
        _ => branch.to_string(),
    }
}

// ── The top row's right-hand cluster ────────────────────────────────────────

/// What the top row says about the listing, on its right-hand end.
///
/// One row, read right to left: the position counter is the number that is
/// *always* true and so is always in the same place at the far end; the git
/// chip is next because it is about the directory rather than the cursor; and
/// the status chips — the ones that are only there when they have something to
/// say — grow leftwards from those two towards the crumbs. A file dialog with
/// type filters puts its filter's chip between the counter and the git chip,
/// because it says what the counter is counting.
pub struct Cluster<'a> {
    /// How many files are selected (PLAN §4.1's `Space`/`Ctrl+a`/`v`).
    pub selected: usize,
    /// Visual mode, and which kind — the one piece of modal state in the
    /// browser, so it has to be visible somewhere that is not the row colours.
    pub visual: Option<bool>,
    /// What `y` / `x` is holding (PLAN §4.1), if anything.
    pub yank: Option<Yank<'a>>,
    /// The branch, as git spells it, when there is one (PLAN §7.3). The chip's
    /// own text is [`branch_label`] of this and the counts below — the raw fact
    /// travels, and the formatting happens where it is drawn.
    pub branch: Option<&'a str>,
    /// The four numbers behind the chip's one, for its tooltip. `None` until a
    /// status lands, which is a different answer from "clean" and is said as
    /// one (see [`branch_tooltip`]).
    pub dirty: Option<df_core::git::DirtyCounts>,
    /// Where the cursor is, 1-based, and how many rows there are.
    pub position: usize,
    pub rows: usize,
    /// The picker's two buttons, when this window is somebody's file dialog
    /// (`--chooser-file`); `None` in a file manager.
    pub pick: Option<Pick>,
    /// The dialog's type-filter chip, when the dialog offered filters.
    pub types: Option<Types<'a>>,
    /// What the trash holds, while the list pane is showing it (PLAN §7.4).
    pub trash: Option<TrashChip>,
}

/// The trash view's chip: how many things, and what they weigh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrashChip {
    /// `37 items · 1.2 GB`, with the size column's `~` while the walk counts
    /// ([`crate::trashview::weight_text`]).
    pub label: String,
    /// What the pointer on the chip is told: how long the trash keeps things.
    /// `None` when it keeps them for ever, and then the chip is only a label.
    pub tip: Option<String>,
}

/// Which of a file dialog's type filters the listing is narrowed to, as the
/// top row's chip says it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Types<'a> {
    /// The active filter's name, or [`crate::menu::ALL_FILES`].
    pub label: &'a str,
    /// Whether a filter is narrowing the listing. `false` is "All files",
    /// drawn dim: the chip is still there to be clicked, but it is no longer
    /// saying the listing is short of anything.
    pub narrowing: bool,
    /// Its popover is out, so it is held down the way the app menu's button
    /// is while that menu is.
    pub open: bool,
}

/// What a picker session's primary button says, and whether it can be
/// pressed. The quiet `Cancel` beside it has neither to decide: its word
/// never changes, and closing a dialog is always possible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pick {
    /// `Select`, `Select 3`, `Choose folder`, `Save`.
    pub label: String,
    /// Whether there is anything the button could pick. A disabled button is
    /// dimmed and takes no hover and no press — a plate that lit up under the
    /// pointer would be promising a click that does nothing.
    pub enabled: bool,
}

/// The quiet button's word.
const CANCEL_LABEL: &str = "Cancel";

/// How far the primary button's accent is lifted towards the palette's text
/// under the pointer: enough to read as *lit* against a plate that is already
/// the brightest thing on the row, not so much that it stops being the accent.
const PICK_HOVER_LIFT: f32 = 0.25;

/// How strong a disabled primary button's accent plate is, as an opacity over
/// the row: still recognisably the button that will light up, visibly not
/// ready to be pressed.
const PICK_DISABLED_ALPHA: f32 = 0.28;

/// The clipboard, as the top row sees it.
///
/// The row marks in the current directory only answer "is *this* file yanked";
/// a yank made two directories ago is invisible until you paste it somewhere
/// you did not mean to. The chip is the other half of that fact, and it is on
/// the one strip of chrome that is on screen wherever you have wandered to.
pub struct Yank<'a> {
    pub paths: &'a [std::path::PathBuf],
    /// `x` rather than `y` — the same distinction the row marks draw.
    pub cut: bool,
    /// The chip's fade: 1 while there is a clipboard, and its eased way out
    /// after `X` (PLAN §8's instant-in, eased-out).
    pub alpha: f32,
    /// Whether the tray is hanging under the chip ([`crate::tray`]). It lists
    /// the same names in the same place the tooltip would hang, so the
    /// tooltip keeps quiet while it is out rather than covering its header.
    pub tray: bool,
}

/// The most names the yank chip's tooltip lists.
///
/// Six: enough that a yank of a handful is entirely readable, few enough that
/// the card stays something the eye takes in rather than a file listing
/// floating over the file listing. Past that it says how many more there are,
/// which is the question a longer list would have been answering anyway.
const YANK_TOOLTIP_NAMES: usize = 6;

/// Where each piece of the cluster is, measured before the crumbs so they know
/// how much of the row is left (and so a click lands on the chip it looks like
/// it landed on).
#[derive(Debug, Clone, PartialEq)]
pub struct ClusterGeom {
    /// What the crumbs must keep clear on the right, the separating gap
    /// included.
    pub width: f32,
    pub counter: egui::Rect,
    pub git: Option<egui::Rect>,
    pub yank: Option<egui::Rect>,
    pub selected: Option<egui::Rect>,
    pub visual: Option<egui::Rect>,
    /// The picker's primary button, at the row's far right, and the `Cancel`
    /// before it. `None` outside a picker session.
    pub pick: Option<egui::Rect>,
    pub cancel: Option<egui::Rect>,
    /// The type-filter chip, beside the counter. `None` unless the session is
    /// a dialog with filters.
    pub types: Option<egui::Rect>,
    /// The trash's chip, beside the counter. `None` outside the trash view.
    pub trash: Option<egui::Rect>,
    /// The three strings the measuring already built, kept rather than built a
    /// second time by the painter a few lines later. Measuring text means
    /// laying it out, which means having the string; formatting each of them
    /// twice per frame bought nothing but the allocations.
    pub labels: ClusterLabels,
}

/// What the cluster's chips say, formatted once (see [`ClusterGeom::labels`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClusterLabels {
    /// `12 / 340`.
    pub counter: String,
    /// `3 yanked` / `3 cut`, when there is a clipboard.
    pub yank: Option<String>,
    /// `4 selected`, when there is a selection.
    pub selected: Option<String>,
    /// `main ·3`, when the directory is in a repository.
    pub git: Option<String>,
}

/// `12 / 340`, or `0 / 0` for an empty directory.
fn counter_text(cluster: &Cluster<'_>) -> String {
    if cluster.rows == 0 {
        "0 / 0".to_string()
    } else {
        format!("{} / {}", cluster.position, cluster.rows)
    }
}

/// The yank chip's label: `3 yanked` / `3 cut`.
pub fn yank_label(count: usize, cut: bool) -> String {
    format!(
        "{} {}",
        grouped(count as u64),
        if cut { "cut" } else { "yanked" }
    )
}

/// The chip's own colour, matching the mark on the rows it is about — and the
/// tray's header, which is the chip opened ([`crate::tray`]).
pub fn yank_color(palette: &crate::theme::Palette, cut: bool) -> egui::Color32 {
    if cut {
        palette.peach
    } else {
        palette.teal
    }
}

/// Lay the cluster out, right to left.
pub fn cluster_geometry(
    painter: &egui::Painter,
    row: egui::Rect,
    cluster: &Cluster<'_>,
    nerd: bool,
) -> ClusterGeom {
    let font = egui::FontId::proportional(FONT);
    let inner = row.shrink2(egui::vec2(PAD_X, 0.0));
    // A picker's two buttons end the row, the primary one last: it is the
    // answer to the dialog, and the far right is where every dialog puts
    // that. The primary sits in the row's corner, so it is inset from the
    // right edge by exactly what it is inset from the top and bottom, and
    // its radius is the row's less that inset — the gap around its corner
    // is one width all the way round (`delightful-ui` §15).
    let (pick, cancel, end) = match &cluster.pick {
        Some(buttons) => {
            let (top, bottom) = (row.top() + CHIP_INSET, row.bottom() - CHIP_INSET);
            let pick_w = text_width(painter, &buttons.label, font.clone()) + PAD_X * 2.0;
            let pick = egui::Rect::from_min_max(
                egui::pos2(row.right() - CHIP_INSET - pick_w, top),
                egui::pos2(row.right() - CHIP_INSET, bottom),
            );
            let cancel_w = text_width(painter, CANCEL_LABEL, font.clone()) + PAD_X * 2.0;
            let cancel = egui::Rect::from_min_max(
                egui::pos2(pick.left() - GAP - cancel_w, top),
                egui::pos2(pick.left() - GAP, bottom),
            );
            // The counter keeps the chips' spacing from the first button.
            (Some(pick), Some(cancel), cancel.left() - GAP)
        }
        None => (None, None, inner.right()),
    };
    let counter_label = counter_text(cluster);
    let counter_w = text_width(painter, &counter_label, font.clone());
    let counter = egui::Rect::from_min_max(
        egui::pos2(end - counter_w, inner.top()),
        egui::pos2(end, inner.bottom()),
    );
    let mut right = counter.left();
    // A chip is inset from the row on every adjacent side and rounded
    // concentrically with it (`delightful-ui` §15) — see [`CHIP_INSET`].
    let chip_at = |width: f32, right: &mut f32| {
        let rect = egui::Rect::from_min_max(
            egui::pos2(*right - GAP - width, row.top() + CHIP_INSET),
            egui::pos2(*right - GAP, row.bottom() - CHIP_INSET),
        );
        *right = rect.left();
        rect
    };
    // The type filter stands right beside the counter, because it is what the
    // counter is counting — `Images 12 / 40` is the twelfth of forty images —
    // and because it is there for the whole session, so it keeps one place
    // while the chips that come and go grow leftwards past it.
    let types = cluster.types.as_ref().map(|types| {
        let glyph = crate::icons::glyph(nerd, TYPES_ICON, TYPES_GLYPH);
        let width = text_width(painter, types.label, font.clone())
            + PAD_X * 2.0
            + icon_width(painter, &glyph, &font)
            + ICON_GAP;
        chip_at(width, &mut right)
    });
    // The trash's weight, beside the counter for the type filter's reason:
    // it is about the same rows the counter counts — all of them, weighed.
    let trash = cluster.trash.as_ref().map(|chip| {
        let width = text_width(painter, &chip.label, font.clone()) + PAD_X * 2.0;
        chip_at(width, &mut right)
    });
    let mut git_text = None;
    let git = cluster.branch.map(|branch| {
        let label = branch_label(branch, cluster.dirty);
        // The glyph's own measured width, then a word space, then the text.
        let glyph = crate::icons::glyph(nerd, GIT_ICON, GIT_GLYPH);
        let width = text_width(painter, &label, font.clone())
            + PAD_X * 2.0
            + icon_width(painter, &glyph, &font)
            + ICON_GAP;
        git_text = Some(label);
        chip_at(width, &mut right)
    });
    let mut yank_text = None;
    let yank = cluster
        .yank
        .as_ref()
        .filter(|y| !y.paths.is_empty())
        .map(|y| {
            let label = yank_label(y.paths.len(), y.cut);
            let width = text_width(painter, &label, font.clone()) + PAD_X * 2.0;
            yank_text = Some(label);
            chip_at(width, &mut right)
        });
    let mut selected_text = None;
    let selected = (cluster.selected > 0).then(|| {
        let label = format!("{} selected", grouped(cluster.selected as u64));
        let width = text_width(painter, &label, font.clone()) + PAD_X * 2.0;
        selected_text = Some(label);
        chip_at(width, &mut right)
    });
    let visual = cluster.visual.map(|selecting| {
        let width = text_width(painter, visual_label(selecting), font.clone()) + PAD_X * 2.0;
        chip_at(width, &mut right)
    });
    ClusterGeom {
        // The gap between the cluster and the crumbs is the window's own, so
        // the two groups on one row are separated by the same distance as
        // everything else on it.
        width: (row.right() - right + GAP).max(0.0),
        counter,
        git,
        yank,
        selected,
        visual,
        pick,
        cancel,
        types,
        trash,
        labels: ClusterLabels {
            counter: counter_label,
            yank: yank_text,
            selected: selected_text,
            git: git_text,
        },
    }
}

fn visual_label(selecting: bool) -> &'static str {
    if selecting {
        "visual"
    } else {
        "visual unset"
    }
}

/// Draw the cluster into the rectangles [`cluster_geometry`] measured.
fn paint_cluster(
    paint: &Painting<'_>,
    cluster: &Cluster<'_>,
    geom: &ClusterGeom,
    hovers: &Hovers<Control>,
    ripples: &Ripples<Control>,
) {
    let palette = paint.palette;
    let painter = paint.painter;
    // `12 / 340`, right-aligned: the one number that is always true, in the one
    // place it can be found without reading. It brightens under the pointer
    // rather than growing a plate — it is a number, not a chip, and giving it
    // one would make the row read as four chips and no counter — but it does
    // answer a click, with `/` (`delightful-ui` §2).
    let counter_hover = hovers.hover(Control::Counter);
    painter.text(
        egui::pos2(geom.counter.right(), geom.counter.center().y),
        egui::Align2::RIGHT_CENTER,
        &geom.labels.counter,
        egui::FontId::proportional(FONT),
        mix(palette.quiet, palette.text, counter_hover),
    );
    if let (Some(rect), Some(types)) = (geom.types, &cluster.types) {
        type_chip(paint, rect, types, hovers, ripples);
    }
    if let (Some(rect), Some(chip)) = (geom.trash, &cluster.trash) {
        // A statement of fact, quiet as the type chip is at "All files": the
        // trash's weight is worth a glance, not an accent. It lifts under the
        // pointer only when there is a tooltip to answer it with — how long
        // the trash keeps things — and only as far as the branch chip does.
        let hover = if chip.tip.is_some() {
            hovers.hover(Control::TrashChip)
        } else {
            0.0
        };
        plate(paint, rect, palette.overlay1, 1.0 + hover * 0.4);
        painter.with_clip_rect(rect).text(
            egui::pos2(rect.left() + PAD_X, rect.center().y),
            egui::Align2::LEFT_CENTER,
            &chip.label,
            egui::FontId::proportional(FONT),
            mix(palette.subtext0, palette.text, hover),
        );
    }
    if let (Some(rect), Some(branch)) = (geom.git, geom.labels.git.as_deref()) {
        let font = egui::FontId::proportional(FONT);
        let glyph = crate::icons::glyph(paint.nerd, GIT_ICON, GIT_GLYPH);
        // The branch, in the palette's own git colour, on a plate of it — the
        // same chip treatment every count on this row gets. It lifts under the
        // pointer like the rest of them, but only far enough to say "there is
        // something here": what is here is a tooltip, not a verb.
        plate(
            paint,
            rect,
            palette.mauve,
            1.0 + hovers.hover(Control::GitChip) * 0.4,
        );
        let ink = crate::theme::ink(palette, palette.mauve);
        painter.text(
            egui::pos2(rect.left() + PAD_X, rect.center().y),
            egui::Align2::LEFT_CENTER,
            &glyph,
            egui::FontId::proportional(FONT),
            ink,
        );
        painter.text(
            egui::pos2(
                rect.left() + PAD_X + icon_width(painter, &glyph, &font) + ICON_GAP,
                rect.center().y,
            ),
            egui::Align2::LEFT_CENTER,
            branch,
            egui::FontId::proportional(FONT),
            ink,
        );
    }
    if let (Some(rect), Some(yank)) = (geom.yank, &cluster.yank) {
        let accent = yank_color(palette, yank.cut);
        let hover = hovers.hover(Control::YankChip);
        let rect = pressed_rect(rect, hovers.press(Control::YankChip));
        // Lifted a little under the pointer, because it *does* something when
        // clicked: it opens the tray that lists what is carried, the same as
        // `B` ([`crate::tray`]).
        plate(paint, rect, accent, yank.alpha * (1.0 + hover * 0.6));
        let inside = painter.with_clip_rect(rect);
        for splash in ripples.splashes(Control::YankChip, paint.now) {
            inside.circle_filled(
                splash.center,
                splash.radius,
                crate::theme::splash(palette, splash.alpha * yank.alpha),
            );
        }
        inside.text(
            egui::pos2(rect.left() + PAD_X, rect.center().y),
            egui::Align2::LEFT_CENTER,
            geom.labels.yank.as_deref().unwrap_or(""),
            egui::FontId::proportional(FONT),
            fade(crate::theme::ink(palette, accent), yank.alpha),
        );
    }
    if let Some(rect) = geom.selected {
        // The count is in the selection's own colour, on a plate of it: the
        // badge and the yellow bars down the column are visibly the same fact,
        // said twice, in the two places the eye looks. Clicking it clears the
        // selection — the pointer's `Esc`, the same as the yank chip is the
        // pointer's `B`.
        action_chip(
            paint,
            rect,
            geom.labels.selected.as_deref().unwrap_or(""),
            palette.yellow,
            Control::SelectedChip,
            hovers,
            ripples,
        );
    }
    if let (Some(rect), Some(selecting)) = (geom.visual, cluster.visual) {
        // Visual mode is the browser's one piece of modal state, and a mode you
        // cannot see is a mode you get caught in — so the chip that says you
        // are in it is also the way out of it.
        action_chip(
            paint,
            rect,
            visual_label(selecting),
            palette.sky,
            Control::VisualChip,
            hovers,
            ripples,
        );
    }
    if let Some(rect) = geom.cancel {
        cancel_button(paint, rect, hovers, ripples);
    }
    if let (Some(rect), Some(pick)) = (geom.pick, &cluster.pick) {
        pick_button(paint, rect, pick, hovers, ripples);
    }
}

/// The type-filter chip: the name filter's chip in every measure — the inset,
/// the radius, the tint, the icon before the word — because it is the same
/// kind of fact, "you are looking at part of this directory".
///
/// At "All files" it goes quiet rather than away: grey where it was blue, as
/// the listing is no longer short of anything, but still where the hand left
/// it, since it is also the way back to the dialog's filter.
fn type_chip(
    paint: &Painting<'_>,
    rect: egui::Rect,
    types: &Types<'_>,
    hovers: &Hovers<Control>,
    ripples: &Ripples<Control>,
) {
    let palette = paint.palette;
    let painter = paint.painter;
    let key = Control::TypeChip;
    let open = f32::from(types.open);
    // Lit and held while its popover is out, so the card has a visible thing
    // it came out of — [`menu_button`]'s rule.
    let hover = hovers.hover(key).max(open);
    let rect = pressed_rect(rect, hovers.press(key).max(open));
    let (accent, ink) = if types.narrowing {
        (palette.blue, palette.blue)
    } else {
        (
            palette.overlay1,
            mix(palette.quiet, crate::theme::louder(palette), hover),
        )
    };
    plate(paint, rect, accent, 1.0 + hover * 0.9);
    let inside = painter.with_clip_rect(rect);
    for splash in ripples.splashes(key, paint.now) {
        inside.circle_filled(
            splash.center,
            splash.radius,
            crate::theme::splash(palette, splash.alpha),
        );
    }
    let font = egui::FontId::proportional(FONT);
    let glyph = crate::icons::glyph(paint.nerd, TYPES_ICON, TYPES_GLYPH);
    inside.text(
        egui::pos2(rect.left() + PAD_X, rect.center().y),
        egui::Align2::LEFT_CENTER,
        &glyph,
        font.clone(),
        ink,
    );
    inside.text(
        egui::pos2(
            rect.left() + PAD_X + icon_width(painter, &glyph, &font) + ICON_GAP,
            rect.center().y,
        ),
        egui::Align2::LEFT_CENTER,
        types.label,
        font,
        ink,
    );
}

/// The picker's primary button: the accent, filled, with its word in the
/// ground's colour so it reads at a glance as *the* way out of the dialog.
///
/// Drawn the moment the window is — no fade in. It is part of the window a
/// dialog opens as, not something that arrived later.
fn pick_button(
    paint: &Painting<'_>,
    rect: egui::Rect,
    pick: &Pick,
    hovers: &Hovers<Control>,
    ripples: &Ripples<Control>,
) {
    let palette = paint.palette;
    let key = Control::PickButton;
    if !pick.enabled {
        // Dimmed, and deaf to the pointer: no lift, no press, no splash.
        paint
            .painter
            .rect_filled(rect, CHIP_RADIUS, fade(palette.blue, PICK_DISABLED_ALPHA));
        paint.painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            &pick.label,
            egui::FontId::proportional(FONT),
            palette.quiet,
        );
        return;
    }
    let hover = hovers.hover(key);
    let rect = pressed_rect(rect, hovers.press(key));
    paint.painter.rect_filled(
        rect,
        CHIP_RADIUS,
        mix(palette.blue, palette.text, hover * PICK_HOVER_LIFT),
    );
    let inside = paint.painter.with_clip_rect(rect);
    for splash in ripples.splashes(key, paint.now) {
        inside.circle_filled(
            splash.center,
            splash.radius,
            crate::theme::splash(palette, splash.alpha),
        );
    }
    inside.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        &pick.label,
        egui::FontId::proportional(FONT),
        palette.crust,
    );
}

/// The picker's `Cancel`: a word, with the plate a crumb wears under the
/// pointer and nothing at rest. Quiet on purpose — it sits beside the one
/// button that answers the dialog and must not compete with it.
fn cancel_button(
    paint: &Painting<'_>,
    rect: egui::Rect,
    hovers: &Hovers<Control>,
    ripples: &Ripples<Control>,
) {
    let palette = paint.palette;
    let key = Control::CancelButton;
    let hover = hovers.hover(key);
    let rect = pressed_rect(rect, hovers.press(key));
    if hover > 0.0 {
        // Faded by alpha over the row, for the reason a crumb's plate is (see
        // [`path_bar`]).
        paint
            .painter
            .rect_filled(rect, CHIP_RADIUS, fade(palette.surface1, hover));
    }
    let inside = paint.painter.with_clip_rect(rect);
    for splash in ripples.splashes(key, paint.now) {
        inside.circle_filled(
            splash.center,
            splash.radius,
            crate::theme::splash(palette, splash.alpha),
        );
    }
    inside.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        CANCEL_LABEL,
        egui::FontId::proportional(FONT),
        mix(palette.subtext0, palette.text, hover),
    );
}

/// A chip that answers a click: the lift, the press, the ripple and the label,
/// in the one place so every one of them behaves the same.
#[allow(clippy::too_many_arguments)] // a painter's arguments are its inputs
fn action_chip(
    paint: &Painting<'_>,
    rect: egui::Rect,
    label: &str,
    accent: egui::Color32,
    key: Control,
    hovers: &Hovers<Control>,
    ripples: &Ripples<Control>,
) {
    let hover = hovers.hover(key);
    let rect = pressed_rect(rect, hovers.press(key));
    plate(paint, rect, accent, 1.0 + hover * 0.6);
    let inside = paint.painter.with_clip_rect(rect);
    for splash in ripples.splashes(key, paint.now) {
        inside.circle_filled(
            splash.center,
            splash.radius,
            crate::theme::splash(paint.palette, splash.alpha),
        );
    }
    inside.text(
        egui::pos2(rect.left() + PAD_X, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        egui::FontId::proportional(FONT),
        crate::theme::ink(paint.palette, accent),
    );
}

/// The yank chip's tooltip: what is actually on the clipboard.
///
/// Hung off the chip rather than shown in a toast, because it answers a
/// question the user asked by pointing at it — and a yank the user has
/// forgotten the contents of is exactly the yank that pastes the wrong files.
fn yank_tooltip(
    paint: &Painting<'_>,
    area: egui::Rect,
    rect: egui::Rect,
    yank: &Yank<'_>,
    warm: f32,
) {
    if warm <= 0.0 || yank.paths.is_empty() || yank.tray {
        return;
    }
    let mut lines: Vec<String> = yank
        .paths
        .iter()
        .take(YANK_TOOLTIP_NAMES)
        .map(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.display().to_string())
        })
        .collect();
    if yank.paths.len() > YANK_TOOLTIP_NAMES {
        lines.push(format!(
            "and {} more",
            grouped((yank.paths.len() - YANK_TOOLTIP_NAMES) as u64)
        ));
    }
    tip(paint, area, rect, &lines, YANK_TOOLTIP_NAMES, warm);
}

/// The git chip's tooltip: the branch, spelled out, and git's four numbers.
///
/// The chip itself carries **one** number on purpose (see [`branch_label`]) —
/// it is read at a glance. The breakdown is the thing you have to stop and look
/// at, so it lives where stopping and looking is what you did: under the
/// pointer.
///
/// Pure, so the wording is a test rather than a repository.
pub fn branch_tooltip(branch: &str, counts: Option<df_core::git::DirtyCounts>) -> Vec<String> {
    let mut lines = vec![branch.to_string()];
    match counts {
        // No scan has landed. Said out loud rather than left as a blank card:
        // "nothing here yet" and "nothing to report" are different answers.
        None => lines.push("status not in yet".to_string()),
        Some(counts) if counts.is_clean() => lines.push("working tree clean".to_string()),
        Some(counts) => {
            for (n, what) in [
                (counts.staged, "staged"),
                (counts.unstaged, "unstaged"),
                (counts.untracked, "untracked"),
                (counts.conflicted, "conflicted"),
            ] {
                if n > 0 {
                    lines.push(format!("{} {what}", grouped(n as u64)));
                }
            }
        }
    }
    lines
}

/// The shape every tooltip on the chrome takes: a small card of lines, hung off
/// the thing it is about.
///
/// Lines from `dim_from` on are drawn a shade quieter — the yank card's "and 4
/// more" is a footnote about the list, not another name in it.
pub fn tip(
    paint: &Painting<'_>,
    area: egui::Rect,
    rect: egui::Rect,
    lines: &[String],
    dim_from: usize,
    warm: f32,
) {
    if warm <= 0.0 || lines.is_empty() {
        return;
    }
    let painter = paint.painter;
    let palette = paint.palette;
    let font = egui::FontId::proportional(FONT);
    let width = lines.iter().fold(0.0f32, |m, line| {
        m.max(text_width(painter, line, font.clone()))
    }) + CARD_PAD * 2.0;
    let height = lines.len() as f32 * CARD_ROW + CARD_PAD * 2.0;
    let card_rect = tip_rect(area, rect, width, height);
    card(paint, card_rect, warm);
    for (i, line) in lines.iter().enumerate() {
        // Cut short with `…` in a window narrower than the line, rather than
        // wrapped: every line of a tip keeps the one row it was measured for.
        truncated_in(
            painter,
            egui::pos2(
                card_rect.left() + CARD_PAD,
                card_rect.top() + CARD_PAD + i as f32 * CARD_ROW + CARD_ROW / 2.0,
            ),
            line,
            fade(
                if i < dim_from {
                    palette.subtext0
                } else {
                    palette.faint
                },
                warm,
            ),
            (card_rect.width() - CARD_PAD * 2.0).max(0.0),
            font.clone(),
        );
    }
}

/// Where a tip of `width` × `height` goes for the thing at `rect`.
///
/// Right-aligned with what it explains, and under it — hanging the card off
/// the thing's own edge is what keeps it pointing at it. Above instead when
/// there is no room below, which is the only case a mark low in the list ever
/// hits; the top row never does. Either way inside the window: never wider
/// than it less its margins, and a tip turned above something near the top
/// stops at the top margin rather than going off the window's edge.
fn tip_rect(area: egui::Rect, rect: egui::Rect, width: f32, height: f32) -> egui::Rect {
    let width = width.min(area.width() - CARD_MARGIN * 2.0).max(0.0);
    let left = (rect.right() - width).max(area.left() + CARD_MARGIN);
    let below = rect.bottom() + CHIP_INSET;
    let top = if below + height <= area.bottom() - CARD_MARGIN {
        below
    } else {
        (rect.top() - CHIP_INSET - height).max(area.top() + CARD_MARGIN)
    };
    egui::Rect::from_min_size(egui::pos2(left, top), egui::vec2(width, height))
}

// ── The top row ─────────────────────────────────────────────────────────────

/// Everything the top row's geometry has to agree about: where the crumbs go,
/// where the committed filter's chip goes, and where the cluster's chips are.
///
/// Measured once a frame and handed to both the hit test and the paint, for the
/// reason [`tab_rects`] is shared: two functions computing this separately is
/// how a row grows a one-pixel lie at its edges.
pub struct TopGeom {
    /// The app menu's button, at the row's leading end ([`menu_button_rect`]).
    pub menu: egui::Rect,
    pub crumbs: Vec<egui::Rect>,
    /// The leading `…`, when the path did not fit. It is not a crumb — it
    /// stands for several — so it is not in the vector above.
    pub ellipsis: Option<egui::Rect>,
    /// The committed `f` filter's trailing chip, when there is one.
    pub filter: Option<egui::Rect>,
    pub cluster: ClusterGeom,
}

/// The glyph the filter chip wears without the patched font: the key that
/// opens the filter, so the chip teaches its own shortcut the way the tab
/// numerals do. (The lens the plain face was asked for, `⌕`, is not in it.)
const FILTER_GLYPH: &str = "f";

/// …and with it: the funnel.
const FILTER_ICON: char = '\u{f0b0}';

/// The type chip's glyph without the patched font: the glob wildcard the
/// filters it names are written in (`*.png`).
const TYPES_GLYPH: &str = "*";

/// …and with it: an outlined funnel (nf-md-filter_outline). Kin to the name
/// filter's solid one, not the same glyph: the two chips can sit on one row
/// at once, and they are two different filters.
const TYPES_ICON: char = '\u{f0233}';

/// The branch chip's glyph without the patched font: the plain face has no
/// fork of its own (`⑂` came up as a blank box), so the column carries the
/// nearest letter shape instead of nothing.
const GIT_GLYPH: &str = "Y";

/// …and with it: the branch.
const GIT_ICON: char = '\u{f418}';

/// How wide the filter chip is, so the crumbs can be measured against what is
/// left. Zero when nothing is filtered.
fn filter_width(painter: &egui::Painter, filter: &str, nerd: bool) -> f32 {
    if filter.is_empty() {
        return 0.0;
    }
    let font = egui::FontId::proportional(FONT);
    let glyph = crate::icons::glyph(nerd, FILTER_ICON, FILTER_GLYPH);
    text_width(painter, filter, font.clone())
        + PAD_X * 2.0
        + icon_width(painter, &glyph, &font)
        + ICON_GAP
        + CRUMB_SEPARATOR_WIDTH
}

/// Lay the whole top row out.
pub fn top_geometry(
    painter: &egui::Painter,
    row: egui::Rect,
    crumbs: &[Crumb],
    filter: &str,
    cluster: &Cluster<'_>,
    nerd: bool,
) -> TopGeom {
    let cluster_geom = cluster_geometry(painter, row, cluster, nerd);
    let filter_w = filter_width(painter, filter, nerd);
    let reserved = cluster_geom.width + filter_w;
    let rects = crumb_rects(painter, row, crumbs, reserved);
    // After the last crumb that fitted, with a separator's worth of space
    // before it: the committed filter reads as one more step down the path,
    // because that is what it is — `Downloads › ⌕ invoice` is the directory
    // and then the part of it you are looking at (PLAN §7.2).
    //
    // …and when *no* crumb fitted, at the row's own left edge. A path narrow
    // enough to elide entirely is exactly the case where the filter is the only
    // thing on the row worth reading, and hanging the chip off a crumb that is
    // not there made it the one thing that disappeared.
    //
    // …and clipped to the same band the crumbs are, for the same reason. The
    // chip hangs off the last crumb's *measured* right edge, and that edge can
    // be past the room the row had: the elision loop stops with one segment
    // left, so a narrow window leaves a last crumb wider than the band and the
    // chip was placed beyond the end of it — drawn over the counter and the
    // right-hand chips, and winning the hit test there. A chip that cannot fit
    // whole is not shown at all rather than shown cut in half: half a filter is
    // a filter you would misread. The band-left fallback still applies when no
    // crumb fitted, and that placement always fits, because the band is at
    // least as wide as the chip or the chip would not have been measured.
    let band_right = row.right() - PAD_X - cluster_geom.width;
    let filter_rect = (filter_w > 0.0)
        .then(|| {
            let left = rects
                .iter()
                .rev()
                .find(|r| **r != egui::Rect::NOTHING)
                .map(|last| last.right() + CRUMB_SEPARATOR_WIDTH)
                .unwrap_or(crumbs_left(row));
            egui::Rect::from_min_max(
                egui::pos2(left, row.top() + CHIP_INSET),
                egui::pos2(
                    left + filter_w - CRUMB_SEPARATOR_WIDTH,
                    row.bottom() - CHIP_INSET,
                ),
            )
        })
        .filter(|rect| rect.right() <= band_right);
    // …and only *then* are the crumbs clipped to the band they were measured
    // in. The elision loop stops when one segment is left, so on a narrow row
    // the last crumb can be wider than the room it was given and reach under
    // the counter and the chips painted on top of it — where it would win the
    // hit test, because the crumb is asked first. Clipping is right rather than
    // reordering the hit test: a rect that extends under something opaque is
    // wrong wherever it is read, and the drop zones read the same vector.
    //
    // Done after the filter chip is placed, because that chip hangs off the
    // last crumb's *measured* right edge and must not move when the rect it is
    // measured from is trimmed.
    let band_right = band_right - filter_w;
    let rects: Vec<egui::Rect> = rects
        .into_iter()
        .map(|rect| {
            if rect == egui::Rect::NOTHING || rect.left() >= band_right {
                return egui::Rect::NOTHING;
            }
            egui::Rect::from_min_max(
                rect.min,
                egui::pos2(rect.right().min(band_right), rect.max.y),
            )
        })
        .collect();
    // The leading `…`, when anything was elided. Measured here because it is
    // the only handle the pointer has on the part of the path that is not on
    // the row — [`crumb_rects`] gives an elided segment `Rect::NOTHING`, which
    // by design contains no point.
    let elided = rects
        .iter()
        .position(|rect| *rect != egui::Rect::NOTHING)
        .unwrap_or(crumbs.len());
    let ellipsis = (elided > 0 && !crumbs.is_empty()).then(|| {
        let width = text_width(painter, CRUMB_ELLIPSIS, egui::FontId::proportional(FONT));
        let left = crumbs_left(row);
        egui::Rect::from_min_max(
            egui::pos2(left, row.top() + CHIP_INSET),
            egui::pos2(left + width, row.bottom() - CHIP_INSET),
        )
    });
    TopGeom {
        menu: menu_button_rect(row),
        crumbs: rects,
        ellipsis,
        filter: filter_rect,
        cluster: cluster_geom,
    }
}

/// How far the top row's ground is tinted towards the filter's blue while a
/// committed filter is on.
///
/// A tenth: enough that the row reads as *changed* out of the corner of the
/// eye — the listing below it is not the whole directory — and far too little
/// to read as a highlight. The chip beside the crumbs is what says which query
/// is on; this only says that one is.
const BAR_FILTER_TINT: f32 = 0.10;

/// The top row's ground: the window's own, so it reads as part of the frame
/// rather than as a fourth pane.
///
/// `filter` is the committed filter's fade, 0 when there is none — it rides
/// the same eased way out the chip does, so the row settles back to its own
/// colour rather than snapping.
///
/// `joined` is whether the tab strip is sitting on top of this row, and the
/// only thing it changes is the north-west corner — see [`bar_corners`].
fn bar_ground(paint: &Painting<'_>, rect: egui::Rect, joined: bool, filter: f32) -> egui::Rect {
    paint
        .painter
        .rect_filled(rect, bar_corners(joined), bar_fill(paint.palette, filter));
    bar_inner(rect)
}

/// The top row's content box: the row less its padding either side. The one
/// definition, so the prompt's hit test ([`prompt_field_geometry`]) and the
/// ground it is drawn on cannot disagree about where the row's text starts.
fn bar_inner(row: egui::Rect) -> egui::Rect {
    row.shrink2(egui::vec2(PAD_X, 0.0))
}

/// The top row's four corner radii.
///
/// Three of them are always [`ROW_RADIUS`]. The fourth, the north-west, is
/// **square while the first tab is the active one**: that tab has no left
/// pigtail (its left edge is the row's own) and its fill runs straight down
/// into the row, so a rounded corner there would put a curve immediately
/// beside a straight edge that is continuing it — a misalignment rather than
/// one line. Squared, the row's left edge and the first tab's are the same
/// edge, which is what they are. With any other tab active there is nothing
/// joined to that corner but the ground, and it rounds like the other three.
pub fn bar_corners(joined: bool) -> egui::CornerRadius {
    egui::CornerRadius {
        nw: if joined { 0 } else { ROW_RADIUS },
        ne: ROW_RADIUS,
        sw: ROW_RADIUS,
        se: ROW_RADIUS,
    }
}

/// The colour [`bar_ground`] paints, on its own.
///
/// Public because the tab strip needs it: the active tab is drawn *in* it, so
/// the two read as one surface, and a second copy of this expression is how
/// the tab and the row it hangs off drift apart.
pub fn bar_fill(palette: &crate::theme::Palette, filter: f32) -> egui::Color32 {
    let ground = mix(palette.crust, palette.base, 0.5);
    mix(
        ground,
        palette.blue,
        BAR_FILTER_TINT * filter.clamp(0.0, 1.0),
    )
}

/// The app menu's button: the three bars, on a plate that lights under the
/// pointer, sinks under the press and ripples, like every chip on the row.
///
/// `open` holds it pressed while its menu is out. The press is also fed to
/// the hover map for as long as the menu is up (see the app's frame), so when
/// the menu goes the button springs back over the press's own release rather
/// than snapping up.
fn menu_button(
    paint: &Painting<'_>,
    rect: egui::Rect,
    open: bool,
    hovers: &Hovers<Control>,
    ripples: &Ripples<Control>,
) {
    let palette = paint.palette;
    let key = Control::MenuButton;
    let press = hovers.press(key).max(f32::from(open));
    // A held button keeps its plate even with the pointer gone down into the
    // menu: the plate is what says which control the card belongs to.
    let lit = hovers.hover(key).max(press);
    let rect = pressed_rect(rect, press);
    if lit > 0.0 {
        // Faded by alpha rather than mixed up from `crust`, for the reason the
        // crumbs' plate is: the row under it is lighter than `crust`.
        paint
            .painter
            .rect_filled(rect, CHIP_RADIUS, fade(palette.surface1, lit));
    }
    let inside = paint.painter.with_clip_rect(rect);
    for splash in ripples.splashes(key, paint.now) {
        inside.circle_filled(
            splash.center,
            splash.radius,
            crate::theme::splash(palette, splash.alpha),
        );
    }
    inside.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        crate::icons::glyph(paint.nerd, MENU_ICON, MENU_GLYPH),
        egui::FontId::proportional(FONT),
        mix(palette.quiet, palette.text, lit),
    );
}

/// Draw the top row in browse mode: the menu button, the crumbs, the filter
/// chip, and the cluster (PLAN §2, §7.2, §7.3).
#[allow(clippy::too_many_arguments)] // a painter's arguments are its inputs
pub fn path_bar(
    paint: &Painting<'_>,
    area: egui::Rect,
    bar: egui::Rect,
    crumbs: &[Crumb],
    filter: &str,
    // The committed filter chip's fade: 1 while a filter is on, and its eased
    // way out after it is cleared — the same instant-in/eased-out the yank chip
    // an inch to its right rides (PLAN §8).
    filter_alpha: f32,
    // Whether the tab strip is sitting on this row, which squares its
    // north-west corner so the row's left edge and the first tab's are one
    // straight line ([`bar_corners`]).
    joined: bool,
    cluster: &Cluster<'_>,
    geom: &TopGeom,
    // Whether the app menu is out. Its button stays down for as long as it is,
    // so the card below it has a visible thing it came out of.
    menu_open: bool,
    hovers: &Hovers<Control>,
    ripples: &Ripples<Control>,
) {
    let palette = paint.palette;
    let painter = paint.painter;
    bar_ground(paint, bar, joined, filter_alpha);
    let font = egui::FontId::proportional(FONT);
    let rects = &geom.crumbs;

    menu_button(paint, geom.menu, menu_open, hovers, ripples);

    // The leading ellipsis, when the path did not fit. It brightens under the
    // pointer because it answers one — with the segments it is standing in for,
    // which are otherwise nowhere on screen.
    let elided = rects.iter().position(|r| *r != egui::Rect::NOTHING);
    if let Some(rect) = geom.ellipsis {
        painter.text(
            egui::pos2(rect.left(), bar.center().y),
            egui::Align2::LEFT_CENTER,
            CRUMB_ELLIPSIS,
            font.clone(),
            mix(
                palette.faint,
                palette.text,
                hovers.hover(Control::CrumbEllipsis),
            ),
        );
    }

    let last = crumbs.len().saturating_sub(1);
    for (index, (crumb, rect)) in crumbs.iter().zip(rects).enumerate() {
        if *rect == egui::Rect::NOTHING {
            continue;
        }
        let key = Control::Crumb(index);
        let hover = hovers.hover(key);
        let rect = pressed_rect(*rect, hovers.press(key));
        if crumb.accent {
            // The chip that says "this is not a folder on this machine" — the
            // same plate treatment the git branch wears at the other end of the
            // row, so the two read as one kind of ornament (PLAN §7.4, §7.6).
            plate(paint, rect, palette.sky, 1.0 + hover * 0.9);
        } else if hover > 0.0 {
            // Faded by *alpha*, not mixed up from the window ground: the row
            // this sits on is `bar_fill`, a step lighter than `crust`, so a
            // plate mixed from `crust` went *darker* than its ground on the
            // way out — a black flash before it vanished. A translucent plate
            // thins towards whatever is under it.
            painter.rect_filled(rect, CHIP_RADIUS, fade(palette.surface1, hover));
        }
        let inside = painter.with_clip_rect(rect);
        for splash in ripples.splashes(key, paint.now) {
            inside.circle_filled(
                splash.center,
                splash.radius,
                crate::theme::splash(palette, splash.alpha),
            );
        }
        // The directory you are actually in is the bright one; the ancestors
        // are the route you took to it. A chip has its own ink for the same
        // reason it has its own plate.
        let color = if crumb.accent {
            palette.sky
        } else if index == last {
            palette.text
        } else {
            mix(palette.quiet, palette.text, hover)
        };
        inside.text(
            egui::pos2(rect.left() + PAD_X, rect.center().y),
            egui::Align2::LEFT_CENTER,
            &crumb.label,
            font.clone(),
            color,
        );
        let has_next = rects
            .get(index + 1)
            .is_some_and(|next| *next != egui::Rect::NOTHING);
        if (index < last && has_next) || (index == last && geom.filter.is_some()) {
            painter.text(
                egui::pos2(rect.right() + CRUMB_SEPARATOR_WIDTH / 2.0, bar.center().y),
                egui::Align2::CENTER_CENTER,
                CRUMB_SEPARATOR,
                font.clone(),
                palette.faint,
            );
        }
    }

    // The committed filter, as the trailing crumb it behaves like: clicking it
    // re-opens the prompt that set it, which is the only way the pointer has of
    // editing a query the keyboard typed.
    if let Some(rect) = geom.filter {
        let alpha = filter_alpha.clamp(0.0, 1.0);
        let key = Control::FilterChip;
        let hover = hovers.hover(key);
        let rect = pressed_rect(rect, hovers.press(key));
        // The plate's strength carries the fade, exactly as the yank chip's
        // does: `plate` reads `strength` as both tint and opacity, so a chip on
        // its way out thins towards the row rather than blinking off it.
        plate(paint, rect, palette.blue, (1.0 + hover * 0.9) * alpha);
        let inside = painter.with_clip_rect(rect);
        for splash in ripples.splashes(key, paint.now) {
            inside.circle_filled(
                splash.center,
                splash.radius,
                crate::theme::splash(palette, splash.alpha),
            );
        }
        let glyph = crate::icons::glyph(paint.nerd, FILTER_ICON, FILTER_GLYPH);
        inside.text(
            egui::pos2(rect.left() + PAD_X, rect.center().y),
            egui::Align2::LEFT_CENTER,
            &glyph,
            font.clone(),
            fade(palette.blue, alpha),
        );
        inside.text(
            egui::pos2(
                rect.left() + PAD_X + icon_width(painter, &glyph, &font) + ICON_GAP,
                rect.center().y,
            ),
            egui::Align2::LEFT_CENTER,
            filter,
            font.clone(),
            fade(palette.blue, alpha),
        );
    }

    paint_cluster(paint, cluster, &geom.cluster, hovers, ripples);
    // The hidden half of the path, under the `…` that is standing for it. The
    // segments themselves, one per line, rather than the joined path: what the
    // reader lost was the *steps*, and each of them is a place they could have
    // clicked if the row had been wider.
    if let (Some(rect), Some(first)) = (geom.ellipsis, elided) {
        let lines: Vec<String> = crumbs
            .iter()
            .take(first)
            .map(|crumb| crumb.label.clone())
            .collect();
        tip(
            paint,
            area,
            rect,
            &lines,
            lines.len(),
            hovers.hover(Control::CrumbEllipsis),
        );
    }
    if let Some((rect, branch)) = geom.cluster.git.zip(cluster.branch) {
        let lines = branch_tooltip(branch, cluster.dirty);
        tip(paint, area, rect, &lines, 1, hovers.hover(Control::GitChip));
    }
    // The trash's clock, under its weight: the one line a non-empty trash has
    // to say it, since the pane only has room for it while it is empty.
    if let (Some(rect), Some(line)) = (
        geom.cluster.trash,
        cluster.trash.as_ref().and_then(|chip| chip.tip.clone()),
    ) {
        tip(
            paint,
            area,
            rect,
            &[line],
            1,
            hovers.hover(Control::TrashChip),
        );
    }
    if let (Some(rect), Some(yank)) = (geom.cluster.yank, &cluster.yank) {
        // …and the card leaves with the chip it hangs off, rather than
        // outliving the fact it is explaining.
        yank_tooltip(
            paint,
            area,
            rect,
            yank,
            hovers.hover(Control::YankChip) * yank.alpha,
        );
    }
}

/// The top row while something is being typed into it: `f`, `/`, `?`, `a`, `;`
/// and the help browser's own filter (PLAN §4.2).
///
/// The prompt takes the crumbs' place rather than opening a line of its own:
/// the keyboard is in one place at a time, and a row that grew under the panes
/// every time a query was typed would move rows under the pointer
/// (`delightful-ui` §8's spatial stability).
///
/// `joined` is the strip above it, exactly as in [`path_bar`]: the strip is
/// drawn whether or not a prompt has taken the row under it, so the corner it
/// squares has to be squared here too.
pub fn prompt_row(
    paint: &Painting<'_>,
    row: egui::Rect,
    prompt: &Prompt,
    tail: Option<&str>,
    joined: bool,
) {
    let palette = paint.palette;
    // No rule along the top edge: the prompt *is* the indication. A row that
    // has swapped its breadcrumbs for a titled field with a caret in it has
    // already said the keyboard is here, and a second mark saying the same
    // thing is a mark that only ever gets in the way.
    bar_ground(paint, row, joined, 0.0);
    let boxes = prompt_boxes(paint.painter, row, tail);

    // The directory the prompt is about, kept at the far left as context when
    // there is room for it: a filter with no idea what it is filtering is a
    // text field floating in a window.
    if let Some(tail) = tail.filter(|_| boxes.tail) {
        let font = egui::FontId::proportional(FONT);
        let line = boxes.line;
        paint.painter.text(
            egui::pos2(line.left(), line.center().y),
            egui::Align2::LEFT_CENTER,
            tail,
            font.clone(),
            palette.faint,
        );
        paint.painter.text(
            egui::pos2(
                boxes.field.left() - CRUMB_SEPARATOR_WIDTH / 2.0,
                line.center().y,
            ),
            egui::Align2::CENTER_CENTER,
            CRUMB_SEPARATOR,
            font,
            palette.faint,
        );
    }
    prompt_field(paint, boxes.field, prompt, boxes.error);
}

/// How a prompt row is divided up: the line the field is on, the part of that
/// line the field is handed once the directory tail has had its share, and the
/// error's own line when the row grew one.
///
/// Measured apart from the paint so that [`prompt_field_geometry`] can ask
/// the same question the paint does and get the same answer.
struct PromptBoxes {
    /// The first line, inside the row's padding.
    line: egui::Rect,
    /// `line` less the directory tail, when the tail is shown.
    field: egui::Rect,
    /// Whether the tail is shown.
    tail: bool,
    /// The second line, when an error too long for the first asked for one.
    error: Option<egui::Rect>,
}

fn prompt_boxes(painter: &egui::Painter, row: egui::Rect, tail: Option<&str>) -> PromptBoxes {
    let mut line = bar_inner(row);
    // The row grew a second line for an error that would not fit beside the
    // query: the field keeps the first line and the error gets the second.
    let error = (row.height() > TOP_HEIGHT + 1.0).then(|| {
        let split = row.top() + TOP_HEIGHT;
        let second = egui::Rect::from_min_max(
            egui::pos2(line.left(), split),
            egui::pos2(line.right(), row.bottom()),
        );
        line = egui::Rect::from_min_max(line.min, egui::pos2(line.right(), split));
        second
    });
    // The tail is dropped rather than squeezing the field below its minimum,
    // which is the condition [`prompt_lines`] repeats to the constant.
    let tail = tail
        .map(|tail| {
            text_width(painter, tail, egui::FontId::proportional(FONT)) + CRUMB_SEPARATOR_WIDTH
        })
        .filter(|width| line.width() - width >= PROMPT_MIN_WIDTH);
    let field = match tail {
        Some(width) => {
            egui::Rect::from_min_max(egui::pos2(line.left() + width, line.top()), line.max)
        }
        None => line,
    };
    PromptBoxes {
        line,
        field,
        tail: tail.is_some(),
        error,
    }
}

/// Where the text of the prompt on the top row is, laid out as it will be
/// drawn: what the pointer is hit-tested against.
///
/// The same arguments [`prompt_row`] is given, run through the same two
/// functions its paint runs through ([`prompt_boxes`], then [`field_layout`]),
/// so a click lands on the character it is seen to land on. The precedent is
/// [`prompt_rect`], which the floating prompt shares with its hit test for the
/// same reason.
pub fn prompt_field_geometry(
    painter: &egui::Painter,
    row: egui::Rect,
    prompt: &Prompt,
    tail: Option<&str>,
) -> FieldGeom {
    let boxes = prompt_boxes(painter, row, tail);
    field_layout(painter, boxes.field, prompt, boxes.error.is_some()).field
}

/// How far a prompt's text is scrolled to the left, in points, so that the
/// caret is inside a field `width` points wide.
///
/// `caret` and `text` are measured from the text's own start: where the caret
/// is along the line, and how long the line is. `previous` is the scroll the
/// field had, which is the whole reason this takes four numbers rather than
/// three. A scroll worked out from the caret alone would move the text every
/// time the caret moved — a click in the middle of a long path would slide
/// the path out from under the pointer that clicked it, and a drag would chase
/// its own tail. So the text holds still while the caret is in view, and moves
/// only as far as it must when the caret would leave it:
///
/// * past the right edge, the caret is put **at** the right edge. That is the
///   `Go to:` case: the prompt opens on a whole absolute path with the caret
///   at its end, and what shows is the end of the path with the caret against
///   the field's right edge, the rest cut off at the left;
/// * past the left edge, it is put at the left edge;
/// * and never so far that there is empty field after the end of the line.
///   A line that fits is not scrolled at all.
///
/// The caret bar's own [`CARET_WIDTH`] counts as part of the line, so a caret
/// at the end of it is drawn whole rather than clipped to a sliver.
pub fn caret_scroll(width: f32, caret: f32, text: f32, previous: f32) -> f32 {
    let most = (text + CARET_WIDTH - width).max(0.0);
    let mut scroll = previous.clamp(0.0, most);
    if caret + CARET_WIDTH - scroll > width {
        scroll = caret + CARET_WIDTH - width;
    }
    if caret < scroll {
        scroll = caret;
    }
    scroll.clamp(0.0, most)
}

/// Where a prompt's text is, and the text itself, laid out.
///
/// Shared by the paint and the pointer for the reason [`TopGeom`] is, and
/// more so: a click has to put the caret between the two characters it is
/// *seen* to fall between, and the only way to promise that is for both sides
/// to read one layout. The paint draws [`FieldGeom::galley`] at
/// [`FieldGeom::origin`] and puts the caret and the selection where
/// [`FieldGeom::x_of`] says a boundary is; the hit test asks the same galley,
/// through [`FieldGeom::boundary_at`], which boundary is nearest the pointer.
#[derive(Clone)]
pub struct FieldGeom {
    /// The strip the text is drawn in and clipped to: from the end of the
    /// title to the start of whatever sits at the line's right end — the
    /// inline error, or the case indicator. A press inside it is a press in
    /// the field. The title and that furniture are not the field.
    pub rect: egui::Rect,
    /// How far the text is scrolled left to keep the caret in view
    /// ([`caret_scroll`]): 0 while the line fits.
    pub scroll: f32,
    /// The x of the first character's left edge: `rect.left()` less
    /// [`FieldGeom::scroll`]. Off the field to the left when the text is
    /// scrolled — the clip is what hides the part that is not in view.
    pub origin: f32,
    /// The line, laid out in the field's font. Uncoloured
    /// ([`egui::Color32::PLACEHOLDER`]): the ink is the paint's business.
    pub galley: Arc<egui::text::Galley>,
}

impl FieldGeom {
    /// The furthest the text can scroll: its end, caret and all, against the
    /// field's right edge. Zero for a line that fits.
    pub fn most_scroll(&self) -> f32 {
        (self.galley.size().x + CARET_WIDTH - self.rect.width()).max(0.0)
    }

    /// The same field with its text scrolled to `scroll` (clamped to what the
    /// line allows) — what a drag past the field's edge moves the text to
    /// before it asks which character is at that edge now.
    pub fn scrolled(&self, scroll: f32) -> FieldGeom {
        let scroll = scroll.clamp(0.0, self.most_scroll());
        FieldGeom {
            rect: self.rect,
            scroll,
            origin: self.rect.left() - scroll,
            galley: self.galley.clone(),
        }
    }

    /// The character boundary nearest `x`: where a click puts the caret, and
    /// how far a drag has reached. Left of the text is 0 and right of it is the
    /// end. Measured along the whole line, not just the part the field shows:
    /// a line too long for the field goes on past its right edge, and so does
    /// a drag that follows it there.
    pub fn boundary_at(&self, x: f32) -> usize {
        // Asked at the line's own height, whatever the pointer's. The galley
        // reads a point above or below its rows as the start or the end of the
        // text, which is right for a paragraph and wrong for one line: a drag
        // that strayed a few points off the row would snap the selection to
        // one end of it.
        let y = self.galley.size().y / 2.0;
        let cursor = self.galley.cursor_from_pos(egui::vec2(x - self.origin, y));
        cursor.index.0.min(self.len())
    }

    /// The character under `x`, for a double click — the one whose box `x` is
    /// in, not the boundary nearest it: a click on the right half of a letter
    /// is still on that letter. Past the end of the text is the text's length.
    pub fn char_under(&self, x: f32) -> usize {
        let at = self.boundary_at(x);
        if at > 0 && x < self.x_of(at) {
            at - 1
        } else {
            at
        }
    }

    /// The x of boundary `index`: where the caret is drawn, and where a
    /// selection starts or ends.
    pub fn x_of(&self, index: usize) -> f32 {
        let cursor = egui::text::CCursor::new(index.min(self.len()));
        self.origin + self.galley.pos_from_cursor(cursor).min.x
    }

    fn len(&self) -> usize {
        self.galley.text().chars().count()
    }
}

/// Everything on a prompt's line, measured: the title, the furniture at the
/// line's right end, and the field between them.
///
/// The one place those widths are decided. [`prompt_field`] paints from it and
/// [`prompt_field_geometry`] hands its field to the hit test, so the two cannot
/// drift a pixel apart the way two copies of this arithmetic would. Laid out
/// uncoloured, because which ink the paint uses is not geometry.
struct FieldLayout {
    title: Arc<egui::text::Galley>,
    /// What sits at the line's right end, and the x it starts at.
    furniture: Option<(Furniture, f32)>,
    field: FieldGeom,
}

/// The one thing that may sit at the right end of a prompt's line.
enum Furniture {
    /// The inline error, laid out to the room it has.
    Error(Arc<egui::text::Galley>),
    /// The prompt's hint ([`Prompt::hint`]), laid out exactly as the error is
    /// and drawn in a quiet colour instead of the error's — and the room it
    /// was cut to, so an inked hint ([`Prompt::inked`]) can be laid out again
    /// in its colours at exactly the same width.
    Hint(Arc<egui::text::Galley>, f32),
    /// The smart-case indicator on a live prompt, and whether it is lit.
    Case(Arc<egui::text::Galley>, bool),
}

fn field_layout(
    painter: &egui::Painter,
    inner: egui::Rect,
    prompt: &Prompt,
    error_line: bool,
) -> FieldLayout {
    let font = egui::FontId::proportional(FONT);
    let blank = egui::Color32::PLACEHOLDER;
    let title = painter.layout_no_wrap(prompt.title().to_string(), font.clone(), blank);

    // ── The right-hand furniture, measured first so the text knows its room ──
    // There is no mode chip: the editor has no modes to report, and the field
    // itself is the only thing on this line that says the keyboard is here.
    let mut right = inner.right();
    let furniture = match prompt.message() {
        // The row grew for this: the error has a line of its own, under the
        // query it is about, and this line keeps its whole width for the text.
        Some(_) if error_line => None,
        Some((message, error)) => {
            // The error takes the place the case indicator would have had: it
            // is the more urgent thing to say about what has been typed. A
            // hint takes it on the same terms — the filter's "No matches
            // here" is more use than whether the empty result was
            // case-sensitive.
            //
            // Laid out **to the room it has**, with an ellipsis, rather than
            // laid out full width and then drawn from a left edge computed
            // backwards from a clamped width. That older arithmetic moved the
            // text left without making it shorter, so in an anchored popup —
            // which has no second line to grow and passes `error_line: None` —
            // a long message ran back over the title and out through the side
            // of the card.
            let room = (right - inner.left()).max(0.0);
            let mut job = egui::text::LayoutJob::single_section(
                message.to_string(),
                egui::TextFormat::simple(font.clone(), blank),
            );
            job.wrap = egui::text::TextWrapping::truncate_at_width(room);
            let galley = painter.layout_job(job);
            right -= galley.size().x.min(room);
            let furniture = if error {
                Furniture::Error(galley)
            } else {
                Furniture::Hint(galley, room)
            };
            Some((furniture, right))
        }
        None if prompt.kind.is_live() => {
            // The smart-case indicator: lit when the query has a capital in it
            // and is therefore case-*sensitive* (df-core's rule, PLAN §7.2).
            // Dim the rest of the time — it reports a mode nobody chose, so it
            // must not shout.
            let lit = is_case_sensitive(prompt.query());
            let text = if lit { "Aa" } else { "aa" };
            let galley = painter.layout_no_wrap(text.to_string(), key_font(FONT - 0.5), blank);
            right -= galley.size().x;
            Some((Furniture::Case(galley, lit), right))
        }
        None => None,
    };
    if furniture.is_some() {
        right -= PAD_X;
    }

    let text_left = inner.left() + title.size().x + PAD_X;
    let room = (right - text_left).max(0.0);
    let galley = painter.layout_no_wrap(prompt.query().to_string(), font, blank);
    // Scrolled so the caret is in view, from wherever the prompt was scrolled
    // to last ([`crate::input::Prompt::scroll`]). The hit test and the paint
    // both come through here with the same prompt, so they agree on it.
    let caret = galley
        .pos_from_cursor(egui::text::CCursor::new(
            prompt.buffer.cursor().min(galley.text().chars().count()),
        ))
        .min
        .x;
    let scroll = caret_scroll(room, caret, galley.size().x, prompt.scroll);
    let field = FieldGeom {
        rect: egui::Rect::from_min_max(
            egui::pos2(text_left, inner.top()),
            egui::pos2(text_left + room, inner.bottom()),
        ),
        scroll,
        origin: text_left - scroll,
        galley,
    };
    FieldLayout {
        title,
        furniture,
        field,
    }
}

/// How many lines the top row needs while `prompt` is open.
///
/// Two only when there is an error and no room to say it beside the query:
/// an inline error that has been squeezed to three characters is not an error
/// message, and a row that is always two lines tall would cost the panes a
/// line for a state they are usually not in.
///
/// `tail` is the directory chip [`prompt_row`] puts at the far left, and it is
/// passed here for one reason: this function decides how tall the row is and
/// that one paints it, so anything one of them spends and the other does not is
/// a row that grows a second line it does not need — or, worse, does not grow
/// one it does. The condition below is [`prompt_row`]'s, to the constant.
pub fn prompt_lines(
    painter: &egui::Painter,
    prompt: &Prompt,
    width: f32,
    tail: Option<&str>,
) -> usize {
    // A hint is measured exactly as an error is: it sits where one would, and
    // squeezed to three characters it would say as little.
    let Some((message, _)) = prompt.message() else {
        return 1;
    };
    let font = egui::FontId::proportional(FONT);
    let wanted = text_width(painter, prompt.title(), font.clone())
        + PAD_X
        + text_width(painter, prompt.query(), font.clone())
        + PAD_X
        + text_width(painter, message, font.clone())
        + PAD_X;
    let inner = width - PAD_X * 2.0;
    let tail_width = tail
        .map(|tail| text_width(painter, tail, font) + CRUMB_SEPARATOR_WIDTH)
        .unwrap_or(0.0);
    // The tail is dropped rather than squeezing the field below its minimum —
    // so on a narrow window it costs nothing, exactly as it is drawn.
    let spent = if inner - tail_width >= PROMPT_MIN_WIDTH {
        tail_width
    } else {
        0.0
    };
    if wanted > inner - spent {
        2
    } else {
        1
    }
}

/// A chip's plate: the window's darkest ground tinted towards the chip's own
/// accent, at `strength` (1 at rest, more under a pointer, less on the way
/// out).
fn plate(paint: &Painting<'_>, rect: egui::Rect, accent: egui::Color32, strength: f32) {
    let tint = mix(
        paint.palette.crust,
        accent,
        CHIP_TINT * strength.clamp(0.0, 2.0),
    );
    paint
        .painter
        .rect_filled(rect, CHIP_RADIUS, fade(tint, strength.min(1.0)));
}

/// How wide a floating rename prompt is, and how far it may hang past its row.
///
/// Anchored prompts sit over the row they rename (PLAN §4.2's yazi geometry),
/// so the width is the row's — a popup narrower than its own file name would be
/// the one field in the program you cannot see the end of.
const PROMPT_MIN_WIDTH: f32 = 260.0;

/// The floating prompt: `r`, `R`, and the conflict dialog's rename.
///
/// Drawn over `anchor` — the cursor's row — because that is the thing being
/// renamed, and the eye is already there.
pub fn prompt_popup(paint: &Painting<'_>, area: egui::Rect, anchor: egui::Rect, prompt: &Prompt) {
    let rect = prompt_rect(area, anchor);
    card(paint, rect, 1.0);
    prompt_field(paint, rect.shrink2(egui::vec2(CARD_PAD, 0.0)), prompt, None);
}

/// Where a floating prompt goes. Shared with the hit test so a click lands
/// inside the field it looks like it landed in.
pub fn prompt_rect(area: egui::Rect, anchor: egui::Rect) -> egui::Rect {
    let width = anchor
        .width()
        .max(PROMPT_MIN_WIDTH)
        .min(area.width() - CARD_MARGIN * 2.0);
    let height = CHROME_HEIGHT + CARD_PAD;
    let left = anchor
        .left()
        .min(area.right() - CARD_MARGIN - width)
        .max(area.left() + CARD_MARGIN);
    // Centred on the row, so the name being edited does not appear to jump to
    // another line as the popup opens (`delightful-ui` §8).
    let top = (anchor.center().y - height / 2.0).clamp(
        area.top() + CARD_MARGIN,
        (area.bottom() - CARD_MARGIN - height).max(area.top() + CARD_MARGIN),
    );
    egui::Rect::from_min_size(egui::pos2(left, top), egui::vec2(width.max(0.0), height))
}

/// The prompt itself, in whatever box it has been given: title, the text with
/// its selection, the caret, and the inline error.
///
/// `error_line` is the second line the top row grew when the error would not
/// fit beside the query; with `None` the error keeps its place on the line, as
/// it does in an anchored popup.
///
/// Everything is placed by [`field_layout`], the measure the top row's hit
/// test is given too ([`prompt_field_geometry`]).
fn prompt_field(
    paint: &Painting<'_>,
    inner: egui::Rect,
    prompt: &Prompt,
    error_line: Option<egui::Rect>,
) {
    let palette = paint.palette;
    let painter = paint.painter;
    let layout = field_layout(painter, inner, prompt, error_line.is_some());
    let top = |galley: &egui::text::Galley| inner.center().y - galley.size().y / 2.0;

    painter.galley(
        egui::pos2(inner.left(), top(&layout.title)),
        layout.title.clone(),
        palette.blue,
    );

    if let (Some((message, error)), Some(line)) = (prompt.message(), error_line) {
        // The row grew for this: the error gets a line of its own, under the
        // query it is about, rather than being squeezed into three characters
        // beside it.
        match prompt.inked_message() {
            Some(inked) => {
                let galley = painter.layout_job(inked_job(inked, palette, line.width()));
                painter.galley(
                    egui::pos2(line.left(), line.center().y - galley.size().y / 2.0),
                    galley,
                    hint_ink(palette),
                );
            }
            None => {
                painter.text(
                    egui::pos2(line.left(), line.center().y),
                    egui::Align2::LEFT_CENTER,
                    message,
                    egui::FontId::proportional(FONT),
                    if error {
                        palette.red
                    } else {
                        hint_ink(palette)
                    },
                );
            }
        }
    }
    match &layout.furniture {
        Some((Furniture::Error(galley), left)) => {
            painter.galley(egui::pos2(*left, top(galley)), galley.clone(), palette.red);
        }
        Some((Furniture::Hint(galley, room), left)) => {
            // The same words at the same width, so the same place — only the
            // colours differ from the galley the layout measured.
            let galley = match prompt.inked_message() {
                Some(inked) => painter.layout_job(inked_job(inked, palette, *room)),
                None => galley.clone(),
            };
            painter.galley(egui::pos2(*left, top(&galley)), galley, hint_ink(palette));
        }
        Some((Furniture::Case(galley, lit), left)) => {
            let color = if *lit { palette.yellow } else { palette.faint };
            painter.galley(egui::pos2(*left, top(galley)), galley.clone(), color);
        }
        None => {}
    }

    // ── The line ────────────────────────────────────────────────────────────
    let field = &layout.field;
    let painter = painter.with_clip_rect(field.rect);

    if let Some(range) = prompt.buffer.selection() {
        // A selected run is a *region*, so it is drawn as one rather than as
        // differently coloured letters — and drawn the same whichever hand made
        // it, Shift and an arrow or a drag across the field.
        let (from, to) = (field.x_of(range.start), field.x_of(range.end));
        painter.rect_filled(
            egui::Rect::from_min_max(
                egui::pos2(from, inner.top() + 4.0),
                egui::pos2(to.max(from + 2.0), inner.bottom() - 4.0),
            ),
            2,
            mix(paint.palette.crust, palette.mauve, 0.35),
        );
    }

    painter.galley(
        egui::pos2(field.origin, top(&field.galley)),
        field.galley.clone(),
        palette.text,
    );

    // The caret. Always a bar, because the caret always sits *between* two
    // characters now — there is no mode in which it stands on one. **Never
    // blinking**: PLAN §4.2 says no blink, and a blink is an animation that
    // never stops asking for frames (PLAN §1).
    let caret_x = field.x_of(prompt.buffer.cursor());
    painter.rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(caret_x, inner.top() + 5.0),
            egui::pos2(caret_x + CARET_WIDTH, inner.bottom() - 5.0),
        ),
        0,
        palette.blue,
    );
}

/// The colour a prompt's hint is drawn in.
///
/// The directory tail's grey family, a step brighter than the tail itself: the
/// hint is a sentence with an instruction in it and has to be read, where the
/// tail only has to be recognised — but it is about the listing, not a fault
/// in the text, so it stays well clear of the error's red and of the query's
/// own full-strength ink.
fn hint_ink(palette: &crate::theme::Palette) -> egui::Color32 {
    palette.quiet
}

/// An inked hint laid out in its colours, cut to `room` the way a plain hint
/// is. The quiet runs are the hint's own ink; the lit one is the text's, a
/// dimmed one sinks towards the ground as a disabled menu row does, and a
/// warning is the yellow the smart-case indicator lights in.
fn inked_job(
    inked: &crate::input::InkedHint,
    palette: &crate::theme::Palette,
    room: f32,
) -> egui::text::LayoutJob {
    use crate::input::Ink;
    let font = egui::FontId::proportional(FONT);
    let mut job = egui::text::LayoutJob::default();
    for (range, ink) in inked.runs() {
        let color = match ink {
            Ink::Quiet => hint_ink(palette),
            Ink::Strong => palette.text,
            Ink::Absent => palette.surface2,
            Ink::Warn => palette.yellow,
        };
        job.append(
            &inked.text()[range.clone()],
            0.0,
            egui::TextFormat::simple(font.clone(), color),
        );
    }
    job.wrap = egui::text::TextWrapping::truncate_at_width(room);
    job
}

/// The insert caret's width, in points. One-and-a-half rather than one: a
/// hairline caret disappears against a busy line at fractional scaling.
pub const CARET_WIDTH: f32 = 1.5;

// ── An overlay's hints (PLAN §4) ───────────────────────────────────────────

/// The height of the hint strip along the bottom of an overlay card.
///
/// Sixteen: a line of [`HINT_FONT`] plus the air a line of text needs under a
/// list of rows to read as a footnote rather than as one more row.
pub const HINT_ROW: f32 = 16.0;

/// The hints' text size. Under the card's own body text, because the hints are
/// what you can do next and the card is what you are doing now — a footer that
/// matched the body would be a second thing to read (`ui-anti-slop`: hierarchy
/// by size, not by decoration).
const HINT_FONT: f32 = FONT - 1.0;

/// Where a card's hints go: the strip inside its bottom padding.
///
/// Every overlay reserves [`HINT_ROW`] for this in its own geometry, so the
/// strip is *inside* the plate and the rows above it never run under it.
pub fn hint_rect(card: egui::Rect) -> egui::Rect {
    egui::Rect::from_min_max(
        egui::pos2(card.left() + CARD_PAD, card.bottom() - CARD_PAD - HINT_ROW),
        egui::pos2(card.right() - CARD_PAD, card.bottom() - CARD_PAD),
    )
}

/// One entry of a card's hint strip: a key, what it does on that card, and what
/// a press on the hint does ([`Control::Hint`]). `None` is a hint that only
/// describes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Hint {
    pub keys: &'static str,
    pub label: &'static str,
    pub act: Option<HintAct>,
}

/// What a press on a hint does: what its key does on the card, one of two ways.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintAct {
    /// The command the key's row in the card's context runs — the form to
    /// prefer, because it is read off df-core's table, so the pointer's copy of
    /// a verb and the keyboard's cannot drift apart.
    Command(Command),
    /// The key itself, typed into the card as the keyboard would type it: for
    /// the keys a card matches by hand (`overlay_literal`), which have no row
    /// and so no command to run.
    Key(Chord),
}

impl Hint {
    /// A hint the pointer can press: `keys` is one chord, and `command` is what
    /// that chord runs on the card.
    pub const fn new(keys: &'static str, label: &'static str, command: Command) -> Hint {
        Hint {
            keys,
            label,
            act: Some(HintAct::Command(command)),
        }
    }

    /// A hint the pointer presses by typing its key, `chord`, into the card: a
    /// key the card matches by hand, with no command behind it.
    pub const fn key(keys: &'static str, label: &'static str, chord: Chord) -> Hint {
        Hint {
            keys,
            label,
            act: Some(HintAct::Key(chord)),
        }
    }

    /// A hint that describes and does not do: two keys or a range (`↑↓`,
    /// `Tab / Esc`), which no one press could stand for. No hover, no press,
    /// no hand.
    pub const fn inert(keys: &'static str, label: &'static str) -> Hint {
        Hint {
            keys,
            label,
            act: None,
        }
    }
}

/// Where each hint goes along the strip `rect`, in order: its key and label
/// with [`HINT_AIR`] either side, the strip's full height.
///
/// One function for the hit test and the paint, so a press lands on the hint
/// it is seen to. A hint that does not fit is **dropped whole**, and so is
/// everything after it, which is why this can be shorter than `hints`. The
/// strip used to be clipped, which cut the last hint off mid-word — `Enter ope`
/// — and a hint truncated into a different word is worse than no hint, because
/// the reader has no way of telling that is what happened. The hints are in
/// importance order already, so dropping from the end drops the least
/// important thing on the strip.
///
/// The first rect starts at the strip's own edge, which is the card's padding
/// in from the card's: a hover plate there is concentric with the card's
/// corner (`delightful-ui` §15).
pub fn hint_rects(painter: &egui::Painter, rect: egui::Rect, hints: &[Hint]) -> Vec<egui::Rect> {
    let mut rects = Vec::with_capacity(hints.len());
    let mut x = rect.left();
    for hint in hints {
        let width = HINT_AIR
            + text_width(painter, hint.keys, key_font(HINT_FONT))
            + HINT_KEY_GAP
            + text_width(painter, hint.label, egui::FontId::proportional(HINT_FONT))
            + HINT_AIR;
        if x + width > rect.right() {
            break;
        }
        rects.push(egui::Rect::from_min_max(
            egui::pos2(x, rect.top()),
            egui::pos2(x + width, rect.bottom()),
        ));
        x += width + HINT_SEP;
    }
    rects
}

/// What the keys do now, along the bottom of the surface that owns them, at
/// the places [`hint_rects`] measured.
///
/// On the overlay rather than on a strip of window chrome: a hint is about the
/// card it belongs to, and the eye that is reading the card should not have to
/// travel to the other end of the window to find out what `Enter` does there.
///
/// A hint with a command is a button, and wears a row's plate under the
/// pointer, the press and the ripple, with its label brightening towards the
/// body text. An inert one is words, painted as the whole strip always was.
pub fn hints(
    paint: &Painting<'_>,
    hints: &[Hint],
    rects: &[egui::Rect],
    hovers: &Hovers<Control>,
    ripples: &Ripples<Control>,
) {
    let palette = paint.palette;
    for (index, (hint, rect)) in hints.iter().zip(rects).enumerate() {
        let key = Control::Hint(index);
        let (hover, press) = match hint.act {
            Some(_) => (hovers.hover(key), hovers.press(key)),
            None => (0.0, 0.0),
        };
        let plate = pressed_rect(*rect, press);
        if hover > 0.0 {
            paint.painter.rect_filled(
                plate,
                CARD_ROW_RADIUS,
                mix(palette.crust, palette.surface1, hover),
            );
        }
        if hint.act.is_some() {
            let inside = paint.painter.with_clip_rect(plate);
            for splash in ripples.splashes(key, paint.now) {
                inside.circle_filled(
                    splash.center,
                    splash.radius,
                    crate::theme::splash(palette, splash.alpha),
                );
            }
        }
        let painter = paint.painter.with_clip_rect(*rect);
        let label_colour = mix(palette.faint, palette.text, hover);
        let key_galley =
            painter.layout_no_wrap(hint.keys.to_string(), key_font(HINT_FONT), palette.subtext0);
        let label_galley = painter.layout_no_wrap(
            hint.label.to_string(),
            egui::FontId::proportional(HINT_FONT),
            label_colour,
        );
        let x = rect.left() + HINT_AIR;
        painter.galley(
            egui::pos2(x, rect.center().y - key_galley.size().y / 2.0),
            key_galley.clone(),
            palette.subtext0,
        );
        painter.galley(
            egui::pos2(
                x + key_galley.size().x + HINT_KEY_GAP,
                rect.center().y - label_galley.size().y / 2.0,
            ),
            label_galley,
            label_colour,
        );
    }
}

/// Between a hint's key and what it does. Narrower than the gap between two
/// hints, so the strip reads as pairs rather than as a row of words.
const HINT_KEY_GAP: f32 = 6.0;

/// The air either side of a hint's words, inside its rect: half a chip's
/// padding, so a hover plate has room round its text without the strip
/// spreading out.
const HINT_AIR: f32 = PAD_X / 2.0;

/// Between two hints' rects: the words keep the two-gap spacing they always
/// had, less the air each rect now carries inside it.
const HINT_SEP: f32 = GAP * 2.0 - HINT_AIR * 2.0;

// ── The which-key card (PLAN §4, §8) ────────────────────────────────────────

/// Draw the card listing what could finish the pending chord, where
/// [`crate::whichkey::geometry`] laid it out.
///
/// `alpha` is [`crate::whichkey::WhichKey::alpha`] — 1 while the card is up, and
/// its fade on the way out. A row under the pointer lifts as a menu row does,
/// because a click on it presses its key ([`Control::WhichKey`]).
pub fn which_key(
    paint: &Painting<'_>,
    geometry: &crate::whichkey::Geometry,
    rows: &[crate::whichkey::Row],
    alpha: f32,
    hovers: &Hovers<Control>,
    ripples: &Ripples<Control>,
) {
    if rows.is_empty() || alpha <= 0.0 {
        return;
    }
    let palette = paint.palette;
    card(paint, geometry.card, alpha);
    // Inside the card's padding: rows scrolled past it are not drawn, and a
    // label too long for a card squeezed narrow stops at its edge.
    let painter = paint.painter.with_clip_rect(geometry.clip);
    let placed = rows.iter().zip(&geometry.rows).zip(&geometry.labels);
    for (index, ((row, rect), label_x)) in placed.enumerate() {
        if !geometry.clip.contains(rect.center()) {
            continue;
        }
        let key = Control::WhichKey(index);
        let hover = hovers.hover(key);
        let plate = pressed_rect(*rect, hovers.press(key));
        if hover > 0.0 {
            painter.rect_filled(
                plate,
                CARD_ROW_RADIUS,
                fade(mix(palette.crust, palette.surface1, hover), alpha),
            );
        }
        let inside = painter.with_clip_rect(plate.intersect(geometry.clip));
        for splash in ripples.splashes(key, paint.now) {
            inside.circle_filled(
                splash.center,
                splash.radius,
                crate::theme::splash(palette, splash.alpha * alpha),
            );
        }
        let y = rect.center().y;
        painter.text(
            egui::pos2(rect.left() + PAD_X, y),
            egui::Align2::LEFT_CENTER,
            &row.keys,
            key_font(FONT),
            fade(crate::theme::ink(palette, palette.yellow), alpha),
        );
        painter.text(
            egui::pos2(*label_x, y),
            egui::Align2::LEFT_CENTER,
            &row.label,
            egui::FontId::proportional(FONT),
            fade(palette.subtext0, alpha),
        );
    }
}

// ── The help browser (PLAN §4.1's `~` / `F1`) ───────────────────────────────

/// One line of the help sheet, and the height its scrolling is measured in.
pub const HELP_ROW: f32 = 20.0;

/// The keys column's width, in logical points. Fixed rather than measured: the
/// descriptions have to start at the same x on every line or the sheet is a
/// ragged mess, and the widest chord in the shipped table (`Ctrl+Shift+z`) fits
/// inside this.
///
/// It is the *legend* that sets the number, though, not the chords: a legend
/// row spends [`LEGEND_SWATCH`] and its gap on the mark itself before the word
/// for it starts, and `faint green dot` after that lane is the longest thing
/// this column ever carries. One column for both, because the meanings and the
/// descriptions have to start at the same x or the sheet reads as two sheets.
const HELP_KEYS_COLUMN: f32 = 119.0;

/// The legend's swatch column: where the mark itself is drawn, left of the word
/// for it.
///
/// Narrow on purpose. A dot is five pixels and a bar is two and a half, and a
/// wide column would leave each of them adrift in the middle of nothing; this
/// is about the width of the widest of them plus room to sit off the edge.
const LEGEND_SWATCH: f32 = 7.0;

/// Between the swatch and the word it stands for.
const LEGEND_SWATCH_GAP: f32 = 8.0;

/// How far a bar swatch stops short of the row's own top and bottom: the row
/// painter's own inset, so the mark in the legend is the shape the mark on the
/// row is.
const LEGEND_SWATCH_INSET: f32 = crate::ui::SELECT_BAR_INSET;

/// The widest the help card gets. Beyond about this a line of "keys …
/// description … command-id" is three things separated by a desert, and the eye
/// loses the row on the way across (`ui-anti-slop`: line length ≤ ~80
/// characters).
const HELP_MAX_WIDTH: f32 = 860.0;

/// How far the window behind the help is dimmed, 0–255.
///
/// A flat wash, not a gradient: it covers the whole window uniformly, so there
/// is no fade-to-transparent to ease (`delightful-ui` §14 applies to the ones
/// that *do* fade). Enough to push the panes back, little enough that you can
/// still see where you were.
pub const HELP_SCRIM: u8 = 150;

// ── Optical centring (`delightful-ui` §16) ──────────────────────────────────
// Content centred in a large region reads as sitting *low* at the true 50/50
// mark; the fix is to bias it up towards a 60/40 split. One rule, but it is
// applied to two different quantities depending on what is being centred, so
// it is two constants rather than one used two ways — and they live here, in
// the module every surface already imports, rather than as a private constant
// in one card and a bare literal in five others.

/// The share of the *slack* that goes above a centred block — a card in a
/// window, a panel in a pane. Four tenths above, six below.
pub const OPTICAL_CENTRE: f32 = 0.4;

/// The share of a *region's height* at which a single centred line of text
/// sits. Milder than [`OPTICAL_CENTRE`] because it is measuring a different
/// thing: a lone baseline in an otherwise empty pane needs a nudge, not the
/// full 60/40 a whole card wants, and at 0.4 an empty-state label reads as
/// having drifted towards the top rather than as being centred well.
pub const OPTICAL_BASELINE: f32 = 0.42;

/// Where the help card goes: most of the window, from `top` down to `bottom`.
///
/// `top` is the bottom of the row above it rather than the window's own edge.
/// The sheet is drawn *under* the top row on purpose — its filter is typed
/// there, and an overlay that covered its own input would be hiding the answer
/// to the question it is asking — so a card that started at the window edge
/// spent its first thirty points painting behind that row, and grew a second
/// hidden strip whenever an error made the row two lines tall.
pub fn help_rect(area: egui::Rect, top: f32, bottom: f32) -> egui::Rect {
    let width = (area.width() - CARD_MARGIN * 2.0).min(HELP_MAX_WIDTH);
    let top = top.max(area.top() + CARD_MARGIN);
    egui::Rect::from_min_max(
        egui::pos2(area.center().x - width / 2.0, top),
        egui::pos2(
            area.center().x + width / 2.0,
            bottom.max(top + CHROME_HEIGHT),
        ),
    )
}

/// Where the help card's lines are drawn, and clipped: the card less its
/// padding, its heading row and its hint strip.
pub fn help_body(rect: egui::Rect) -> egui::Rect {
    egui::Rect::from_min_max(
        egui::pos2(rect.left() + CARD_PAD, rect.top() + CARD_PAD + CARD_ROW),
        egui::pos2(rect.right() - CARD_PAD, rect.bottom() - CARD_PAD - HINT_ROW),
    )
}

/// How many help lines fit in the card — its own heading row and its hint strip
/// come out of the height first.
pub fn help_page(rect: egui::Rect) -> usize {
    crate::viewport::visible_rows(help_body(rect).height(), HELP_ROW)
}

/// The sheet's bar, down the card's right padding beside its lines, while
/// the `lines` it has run past a page — measured in lines, as its keys and
/// its wheel count them.
pub fn help_bar(rect: egui::Rect, help: &Help, lines: usize) -> Option<crate::scrollbar::Geometry> {
    crate::scrollbar::card(
        rect,
        help_body(rect),
        help.first as f32,
        help_page(rect) as f32,
        lines as f32,
    )
}

/// Where the sheet's bar can be pointed at, while its `lines` run past a
/// page ([`crate::scrollbar::band`]): below the heading, so the `×` in the
/// heading's corner is the `×`'s, and above the hint strip.
pub fn help_band(rect: egui::Rect, lines: usize) -> Option<egui::Rect> {
    crate::scrollbar::band(rect, help_body(rect), help_page(rect) as f32, lines as f32)
}

/// Draw the help overlay: every live binding, grouped by context.
///
/// Deliberately plain (`ui-anti-slop`: no gratuitous chrome). This is a
/// reference sheet — it is read, not admired — so it is a card, a heading per
/// group, and three columns: what to press, what it does, and the id to write in
/// `keymap.toml` if you want to change it.
/// `filter` is what the sheet is narrowed by, drawn in the heading. It used to
/// be typed into the top row, which this card is painted *over*: the one thing
/// on screen that was changing under the fingers was the one thing behind the
/// scrim.
#[allow(clippy::too_many_arguments)] // a painter's arguments are its inputs
pub fn help_overlay(
    paint: &Painting<'_>,
    area: egui::Rect,
    rect: egui::Rect,
    lines: &[HelpLine],
    help: &Help,
    total: usize,
    filter: crate::help::Filter<'_>,
    hovers: &Hovers<Control>,
    ripples: &Ripples<Control>,
) {
    let (query, caret) = (filter.query, filter.caret);
    let painter = paint.painter;
    let palette = paint.palette;
    painter.rect_filled(area, 0, crate::theme::scrim(palette));
    card(paint, rect, 1.0);

    let shown = lines.iter().filter(|l| l.selectable()).count();
    let heading = egui::pos2(
        rect.left() + CARD_PAD,
        rect.top() + CARD_PAD + CARD_ROW / 2.0,
    );
    let title = egui::FontId::proportional(FONT + 3.0);
    painter.text(
        heading,
        egui::Align2::LEFT_CENTER,
        "Keys",
        title.clone(),
        palette.text,
    );
    let count = if shown == total {
        format!("{total} bindings")
    } else {
        format!("{shown} of {total}")
    };
    // The `×` takes the heading's far corner, and the count sits left of it.
    let close = close_button_rect(rect);
    close_button(paint, close, hovers, ripples);
    let count_right = close.left() - GAP;
    let count_width = text_width(painter, &count, egui::FontId::proportional(FONT));
    painter.text(
        egui::pos2(count_right, heading.y),
        egui::Align2::RIGHT_CENTER,
        &count,
        egui::FontId::proportional(FONT),
        palette.faint,
    );

    // The filter, between the title and the count: what has been typed, with
    // the caret in it while the field is open, and an invitation when it is
    // empty. The invitation is in the faint ink and the query in `text`, so the
    // two never read as the same thing.
    let filter_left = heading.x + text_width(painter, "Keys", title) + GAP * 2.0;
    let filter_width = (count_right - count_width - GAP * 2.0 - filter_left).max(0.0);
    let font = egui::FontId::proportional(FONT);
    let clipped = painter.with_clip_rect(egui::Rect::from_min_max(
        egui::pos2(filter_left, rect.top()),
        egui::pos2(filter_left + filter_width, rect.bottom()),
    ));
    // The caret goes at the head of the line while the field is empty, and the
    // invitation steps aside for it rather than being drawn under it.
    let caret_room = if caret.is_some() {
        CARET_WIDTH + 4.0
    } else {
        0.0
    };
    if query.is_empty() {
        truncated_in(
            &clipped,
            egui::pos2(filter_left + caret_room, heading.y),
            "type to filter",
            palette.faint,
            (filter_width - caret_room).max(0.0),
            font.clone(),
        );
    } else {
        truncated_in(
            &clipped,
            egui::pos2(filter_left, heading.y),
            query,
            palette.text,
            filter_width,
            font.clone(),
        );
    }
    if let Some(at) = caret {
        // Floored to a char boundary rather than trusted. The caret is a byte
        // offset into the buffer it was measured on, and a slice that lands
        // inside a multibyte character is a panic — in a *filter box*, where
        // the character before the caret is as likely to be `é` as `e`.
        let mut at = at.min(query.len());
        while at > 0 && !query.is_char_boundary(at) {
            at -= 1;
        }
        let before = &query[..at];
        let x = filter_left + text_width(painter, before, font);
        // A line's half-height, and the palette's caret colour: the field is
        // the same field it was in the top row, so it keeps the same caret.
        let half = FONT * 0.75;
        clipped.rect_filled(
            egui::Rect::from_min_size(
                egui::pos2(x, heading.y - half),
                egui::vec2(CARET_WIDTH, half * 2.0),
            ),
            0,
            palette.blue,
        );
    }

    let content = help_body(rect);
    let painter = painter.with_clip_rect(content);
    let page = help_page(rect);
    for (offset, line) in lines.iter().skip(help.first).take(page + 1).enumerate() {
        let index = help.first + offset;
        let top = content.top() + offset as f32 * HELP_ROW;
        let row = egui::Rect::from_min_size(
            egui::pos2(content.left(), top),
            egui::vec2(content.width(), HELP_ROW),
        );
        match line {
            HelpLine::Group(name) => {
                painter.text(
                    egui::pos2(row.left(), row.center().y + 2.0),
                    egui::Align2::LEFT_CENTER,
                    *name,
                    egui::FontId::proportional(FONT),
                    palette.blue,
                );
            }
            HelpLine::Row(binding) => {
                if index == help.cursor {
                    painter.rect_filled(row, CARD_ROW_RADIUS, palette.surface1);
                }
                painter.text(
                    egui::pos2(row.left() + PAD_X, row.center().y),
                    egui::Align2::LEFT_CENTER,
                    &binding.keys,
                    key_font(FONT),
                    crate::theme::ink(palette, palette.yellow),
                );
                let description_left = row.left() + PAD_X + HELP_KEYS_COLUMN;
                let id_galley = painter.layout_no_wrap(
                    binding.id.clone(),
                    egui::FontId::proportional(FONT - 1.0),
                    palette.faint,
                );
                painter.galley(
                    egui::pos2(
                        row.right() - PAD_X - id_galley.size().x,
                        row.center().y - id_galley.size().y / 2.0,
                    ),
                    id_galley.clone(),
                    palette.faint,
                );
                truncated(
                    &painter,
                    egui::pos2(description_left, row.center().y),
                    &binding.description,
                    palette.subtext0,
                    (row.right() - PAD_X - id_galley.size().x - GAP - description_left).max(0.0),
                );
            }
            // A legend entry, in a binding's geometry: the mark where the keys
            // go and the meaning where the description goes, so the eye tracks
            // one pair of columns down the whole sheet.
            //
            // The mark is `subtext1` rather than the bindings' `yellow` — it is
            // a description of something on screen, not a key you press, and
            // wearing the key colour would invite people to try pressing it.
            // No third column: a mark has no id to write in `keymap.toml`.
            HelpLine::Legend(entry) => {
                let swatch = egui::Rect::from_min_max(
                    egui::pos2(row.left() + PAD_X, row.top() + LEGEND_SWATCH_INSET),
                    egui::pos2(
                        row.left() + PAD_X + LEGEND_SWATCH,
                        row.bottom() - LEGEND_SWATCH_INSET,
                    ),
                );
                legend_swatch(&painter, swatch, entry.swatch, palette);
                let mark_left = swatch.right() + LEGEND_SWATCH_GAP;
                let meaning_left = row.left() + PAD_X + HELP_KEYS_COLUMN;
                let (font, colour) = (egui::FontId::proportional(FONT), palette.subtext1);
                truncated_in(
                    &painter,
                    egui::pos2(mark_left, row.center().y),
                    entry.mark,
                    colour,
                    (meaning_left - GAP - mark_left).max(0.0),
                    font,
                );
                truncated(
                    &painter,
                    egui::pos2(meaning_left, row.center().y),
                    entry.meaning,
                    palette.subtext0,
                    (row.right() - PAD_X - meaning_left).max(0.0),
                );
            }
        }
    }

    if lines.is_empty() {
        // An empty state that says what to do about it, not "no results"
        // (`delightful-ui` §11).
        painter.text(
            egui::pos2(
                content.center().x,
                content.top() + content.height() * OPTICAL_BASELINE,
            ),
            egui::Align2::CENTER_CENTER,
            "No binding matches. Backspace to widen the filter.",
            egui::FontId::proportional(FONT),
            palette.faint,
        );
    }
    if let Some(bar) = help_bar(rect, help, lines.len()) {
        crate::scrollbar::paint_card(
            paint,
            &bar,
            crate::scrollbar::Surface::Help,
            hovers,
            help.scrolled_at(),
            1.0,
        );
    }
}

/// The mark a legend row is about, drawn where the keys column starts.
///
/// Every colour and every dimension is the row painter's own — [`ui::git_dot`]
/// for the dots, [`ui::SELECT_BAR_WIDTH`] and the palette's yellow/teal/peach
/// for the bars — so a swatch cannot say something the pane does not.
///
/// [`ui::git_dot`]: crate::ui::git_dot
/// [`ui::SELECT_BAR_WIDTH`]: crate::ui::SELECT_BAR_WIDTH
fn legend_swatch(
    painter: &egui::Painter,
    rect: egui::Rect,
    swatch: help::Swatch,
    palette: &crate::theme::Palette,
) {
    match swatch {
        help::Swatch::None => {}
        help::Swatch::Dot(status) => {
            if let Some(colour) = crate::ui::git_dot(status, palette) {
                painter.circle_filled(rect.center(), crate::ui::GIT_DOT_RADIUS, colour);
            }
        }
        help::Swatch::Bar(mark) => {
            let colour = match mark {
                help::Mark::Selected => palette.yellow,
                help::Mark::Yanked => palette.teal,
                help::Mark::Cut => palette.peach,
            };
            let bar = egui::Rect::from_min_max(
                egui::pos2(
                    rect.center().x - crate::ui::SELECT_BAR_WIDTH / 2.0,
                    rect.top(),
                ),
                egui::pos2(
                    rect.center().x + crate::ui::SELECT_BAR_WIDTH / 2.0,
                    rect.bottom(),
                ),
            );
            painter.rect_filled(bar, 1, colour);
        }
    }
}

// ── Shared bits ─────────────────────────────────────────────────────────────

/// A card plate and its hairline edge, at `alpha`.
pub fn card(paint: &Painting<'_>, rect: egui::Rect, alpha: f32) {
    let plate = paint.palette.crust;
    let a = (alpha.clamp(0.0, 1.0) * CARD_ALPHA as f32).round() as u8;
    paint.painter.rect_filled(
        rect,
        CARD_RADIUS,
        egui::Color32::from_rgba_unmultiplied(plate.r(), plate.g(), plate.b(), a),
    );
    paint.painter.rect_stroke(
        rect,
        CARD_RADIUS,
        egui::Stroke::new(1.0, fade(crate::theme::hairline(paint.palette), alpha)),
        egui::StrokeKind::Inside,
    );
}

/// Where a card's `×` goes: a [`CARD_ROW`] square tucked into the top-right
/// corner at [`CARD_PAD`], so its hover plate is concentric with the card's
/// corner the way a row's is (`delightful-ui` §15).
pub fn close_button_rect(card: egui::Rect) -> egui::Rect {
    egui::Rect::from_min_max(
        egui::pos2(card.right() - CARD_PAD - CARD_ROW, card.top() + CARD_PAD),
        egui::pos2(card.right() - CARD_PAD, card.top() + CARD_PAD + CARD_ROW),
    )
}

/// A card's `×` ([`Control::Close`]), the pointer's `Esc`: a glyph at rest, a
/// row's plate under the pointer, and the press and ripple every control gets.
/// Neutral rather than red, because it closes the card and destroys nothing.
pub fn close_button(
    paint: &Painting<'_>,
    rect: egui::Rect,
    hovers: &Hovers<Control>,
    ripples: &Ripples<Control>,
) {
    let palette = paint.palette;
    let key = Control::Close;
    let hover = hovers.hover(key);
    let rect = pressed_rect(rect, hovers.press(key));
    if hover > 0.0 {
        paint.painter.rect_filled(
            rect,
            CARD_ROW_RADIUS,
            mix(palette.crust, palette.surface1, hover),
        );
    }
    let inside = paint.painter.with_clip_rect(rect);
    for splash in ripples.splashes(key, paint.now) {
        inside.circle_filled(
            splash.center,
            splash.radius,
            crate::theme::splash(palette, splash.alpha),
        );
    }
    inside.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "×",
        // A size up from the tray's per-row `×`: this one is the only control
        // in its corner, and at the row size it read as a speck beside the
        // heading's count.
        egui::FontId::proportional(FONT + 5.0),
        mix(palette.faint, palette.text, hover),
    );
}

/// The same colour, at `alpha`.
pub fn fade(color: egui::Color32, alpha: f32) -> egui::Color32 {
    let a = (alpha.clamp(0.0, 1.0) * color.a() as f32).round() as u8;
    egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), a)
}

/// The font every keyboard shortcut on the chrome is drawn in — monospace, so
/// a column of chords lines up and `l` and `1` are not the same shape.
pub fn key_font(size: f32) -> egui::FontId {
    egui::FontId::monospace(size)
}

pub fn text_width(painter: &egui::Painter, text: &str, font: egui::FontId) -> f32 {
    painter
        // Measured, never drawn: the colour is no part of the width.
        .layout_no_wrap(text.to_string(), font, egui::Color32::WHITE)
        .size()
        .x
}

/// Left-aligned, vertically centred, ellipsised at `max_width`.
pub fn truncated(
    painter: &egui::Painter,
    pos: egui::Pos2,
    text: &str,
    color: egui::Color32,
    max_width: f32,
) {
    truncated_in(
        painter,
        pos,
        text,
        color,
        max_width,
        egui::FontId::proportional(FONT),
    );
}

/// The same, in a face of your choosing — the legend's `ignored` tag is set in
/// the row's tag type, not the sheet's.
pub fn truncated_in(
    painter: &egui::Painter,
    pos: egui::Pos2,
    text: &str,
    color: egui::Color32,
    max_width: f32,
    font: egui::FontId,
) {
    use egui::text::{LayoutJob, TextFormat, TextWrapping};
    let mut job = LayoutJob::single_section(
        text.to_string(),
        TextFormat {
            font_id: font,
            color,
            ..Default::default()
        },
    );
    job.wrap = TextWrapping {
        max_width,
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    let galley = painter.layout_job(job);
    painter.galley(
        egui::pos2(pos.x, pos.y - galley.size().y / 2.0),
        galley,
        color,
    );
}

/// A `/`-separated path shortened to fit `max_width` in `font`, keeping the
/// part that tells one path from its neighbours: the end.
///
/// The measured half of [`elide_segments`], which makes the decision; this
/// only splits the text and measures the candidates. A trailing `/` (a folder)
/// stays on the last component, so it survives every step of the elision.
pub fn elide_path(
    painter: &egui::Painter,
    text: &str,
    font: egui::FontId,
    max_width: f32,
) -> String {
    let (body, dir) = match text.strip_suffix('/') {
        Some(body) if !body.is_empty() => (body, true),
        _ => (text, false),
    };
    let mut segments: Vec<String> = body.split('/').map(str::to_string).collect();
    if dir {
        if let Some(last) = segments.last_mut() {
            last.push('/');
        }
    }
    let segments: Vec<&str> = segments.iter().map(String::as_str).collect();
    elide_segments(&segments, |candidate| {
        text_width(painter, candidate, font.clone()) <= max_width
    })
}

/// `text` shortened to fit `max_width` in `font` by taking characters out of
/// its middle, `…` standing for them.
///
/// For a sentence whose two ends both matter: the start says what happened
/// and the end which — a file's name, a count — and a line cut at its end
/// keeps the first and loses the second. The measured half of
/// [`elide_middle_with`].
pub fn elide_middle(
    painter: &egui::Painter,
    text: &str,
    font: egui::FontId,
    max_width: f32,
) -> String {
    elide_middle_with(text, |candidate| {
        text_width(painter, candidate, font.clone()) <= max_width
    })
}

/// Which middle-shortening of `text` fits, as `fits` measures it: the whole
/// text when it does, and otherwise as many characters as fit, split either
/// side of `…` with the odd one going to the end. Pure, for the reason
/// [`elide_segments`] is.
pub fn elide_middle_with(text: &str, fits: impl Fn(&str) -> bool) -> String {
    if fits(text) {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let candidate = |keep: usize| {
        let head = keep / 2;
        let mut out: String = chars[..head].iter().collect();
        out.push('…');
        out.extend(&chars[chars.len() - (keep - head)..]);
        out
    };
    // The most characters that fit, by bisection: `fits` is a text layout
    // apiece, as it is for a path.
    let (mut low, mut high) = (0usize, chars.len().saturating_sub(1));
    while low < high {
        let mid = (low + high).div_ceil(2);
        if fits(&candidate(mid)) {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    candidate(low)
}

/// Which shortening of a path fits, as `fits` measures it.
///
/// A listing of paths is a column of shared prefixes — `project/src/preview/`
/// on every other row — and cutting each one at its end, the way a file name
/// is cut, leaves a column of identical beginnings with the names that tell
/// them apart gone. So the path is shortened from the *front*, a whole
/// directory at a time:
///
/// 1. the whole path, if it fits;
/// 2. otherwise leading directories are dropped, one at a time, and `…/`
///    stands for what went (`…/src/preview/paint.rs`, `…/preview/paint.rs`,
///    `…/paint.rs`);
/// 3. only when `…/` and the last component alone still do not fit is the
///    last component itself cut at its end with `…`.
///
/// A last component ending in `/` is a folder: its slash is kept through step
/// 3 too (`…/very-long-fold…/`), so a folder row still reads as one. Pure,
/// and `fits` is the only thing that knows about fonts, so the ladder is a
/// test rather than something checked by eye in a narrow window.
pub fn elide_segments(segments: &[&str], fits: impl Fn(&str) -> bool) -> String {
    let full = segments.join("/");
    if fits(&full) {
        return full;
    }
    for drop in 1..segments.len() {
        let candidate = format!("…/{}", segments[drop..].join("/"));
        if fits(&candidate) {
            return candidate;
        }
    }
    let last = segments.last().copied().unwrap_or_default();
    let prefix = if segments.len() > 1 { "…/" } else { "" };
    let (name, suffix) = match last.strip_suffix('/') {
        Some(name) => (name, "…/"),
        None => (last, "…"),
    };
    // The longest front of the name that fits, found by bisection: `fits` is
    // a text layout apiece, and a sixty-character name would otherwise be
    // sixty of them per row per frame.
    let ends: Vec<usize> = name
        .char_indices()
        .map(|(at, _)| at)
        .chain(std::iter::once(name.len()))
        .collect();
    let candidate = |keep: usize| format!("{prefix}{}{suffix}", &name[..ends[keep]]);
    let (mut low, mut high) = (0usize, ends.len() - 1);
    while low < high {
        let mid = (low + high).div_ceil(2);
        if fits(&candidate(mid)) {
            low = mid;
        } else {
            high = mid - 1;
        }
    }
    candidate(low)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The ladder, measured in characters: the whole path, then leading
    /// directories dropped one at a time behind `…/`, and only then the name
    /// itself cut at its end.
    #[test]
    fn a_long_line_loses_its_middle_and_keeps_its_end() {
        let text = "Copied 1,204 files from the camera card to Photos";
        let at = |width: usize| elide_middle_with(text, |s| s.chars().count() <= width);
        assert_eq!(at(100), text, "a line that fits is left alone");
        let short = at(24);
        assert!(short.chars().count() <= 24, "{short}");
        assert!(short.starts_with("Copied"), "{short}");
        assert!(short.ends_with("to Photos"), "the last word went: {short}");
        assert!(short.contains('…'));
        // Nothing fits at all: the mark alone, for the clip.
        assert_eq!(at(1), "…");
        assert_eq!(at(0), "…");
    }

    /// A tip turned above something near the window's top stops at the top
    /// margin, and one wider than the window is cut to it.
    #[test]
    fn a_tip_stays_inside_the_window() {
        let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 140.0));
        let near_top = egui::Rect::from_min_size(egui::pos2(300.0, 40.0), egui::vec2(60.0, 20.0));
        // No room below, and not enough above: turned up, then stopped at the
        // top margin rather than hung off the window's edge.
        let tip = tip_rect(area, near_top, 200.0, 100.0);
        assert_eq!(tip.top(), area.top() + CARD_MARGIN, "{tip:?}");
        assert!(area.contains_rect(tip), "{tip:?}");
        // With room above, it sits wholly above the thing it is about.
        let lower = near_top.translate(egui::vec2(0.0, 60.0));
        let tip = tip_rect(area, lower, 200.0, 60.0);
        assert!(tip.bottom() <= lower.top(), "{tip:?}");
        let wide = tip_rect(area, near_top, 900.0, 40.0);
        assert!(wide.width() <= area.width() - CARD_MARGIN * 2.0);
        assert!(area.contains_rect(wide), "{wide:?}");
    }

    #[test]
    fn a_path_is_shortened_from_the_front_a_directory_at_a_time() {
        let path = ["delightfile-demo", "src", "preview", "paint.rs"];
        let at = |width: usize| elide_segments(&path, |s| s.chars().count() <= width);
        assert_eq!(at(100), "delightfile-demo/src/preview/paint.rs");
        assert_eq!(at(36), "…/src/preview/paint.rs");
        assert_eq!(at(22), "…/src/preview/paint.rs");
        assert_eq!(at(21), "…/preview/paint.rs");
        assert_eq!(at(18), "…/preview/paint.rs");
        assert_eq!(at(17), "…/paint.rs");
        assert_eq!(at(10), "…/paint.rs");
        // …and only now is the name itself cut, at its end.
        assert_eq!(at(9), "…/paint.…");
        assert_eq!(at(8), "…/paint…");
        assert_eq!(at(4), "…/p…");
        // Nothing fits at all: the shortest mark there is, for the clip.
        assert_eq!(at(0), "…/…");

        // Two rows that differ only in their last component still differ at
        // every width that has room for `…/` and the names.
        let other = ["delightfile-demo", "src", "preview", "mod.rs"];
        for width in 8..40 {
            assert_ne!(
                at(width),
                elide_segments(&other, |s| s.chars().count() <= width),
                "at {width}"
            );
        }
    }

    /// A folder keeps its trailing slash through every step, the cut
    /// included, so a folder row still reads as a folder.
    #[test]
    fn a_folder_keeps_its_slash_while_it_is_shortened() {
        let path = ["project", "assets", "very-long-folder-name/"];
        let at = |width: usize| elide_segments(&path, |s| s.chars().count() <= width);
        assert_eq!(at(100), "project/assets/very-long-folder-name/");
        assert_eq!(at(31), "…/assets/very-long-folder-name/");
        assert_eq!(at(30), "…/very-long-folder-name/");
        assert_eq!(at(24), "…/very-long-folder-name/");
        assert_eq!(at(12), "…/very-lon…/");
    }

    /// A bare name has no directories to drop, so it goes straight to the
    /// cut — with no `…/` in front of a path that never had one.
    #[test]
    fn a_bare_name_is_cut_at_its_end() {
        let at = |width: usize| elide_segments(&["README.md"], |s| s.chars().count() <= width);
        assert_eq!(at(9), "README.md");
        assert_eq!(at(7), "README…");
        assert_eq!(at(0), "…");
        // Multi-byte characters are cut on a character, never inside one.
        assert_eq!(
            elide_segments(&["héllo-wörld"], |s| s.chars().count() <= 6),
            "héllo…"
        );
    }

    /// The measured wrapper: whatever it answers fits the width it was given,
    /// keeps the last component when there is room for it, and splits a
    /// folder's slash onto its last component.
    #[test]
    fn the_measured_path_fits_and_keeps_its_end() {
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(Default::default(), |ui| {
            let painter = ui.painter();
            let font = egui::FontId::proportional(13.5);
            let path = "delightfile-demo/src/preview/paint.rs";
            let whole = text_width(painter, path, font.clone());
            assert_eq!(elide_path(painter, path, font.clone(), whole + 1.0), path);
            for width in [whole * 0.8, whole * 0.5, whole * 0.35] {
                let out = elide_path(painter, path, font.clone(), width);
                assert!(
                    text_width(painter, &out, font.clone()) <= width,
                    "{out:?} is wider than {width}"
                );
                assert!(out.ends_with("paint.rs"), "{out:?} lost the name");
                assert!(out.starts_with("…/"), "{out:?}");
            }
            let folder = "alpha/beta/c/";
            assert_eq!(elide_path(painter, folder, font.clone(), 1000.0), folder);
            let narrow = text_width(painter, "…/c/", font.clone());
            assert_eq!(elide_path(painter, folder, font.clone(), narrow), "…/c/");
        });
    }

    fn strip() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(8.0, 8.0), egui::vec2(1384.0, CHROME_HEIGHT))
    }

    /// The chip is the branch alone until there is something to count, and the
    /// two states that show no number — clean, and not yet scanned — look the
    /// same on purpose.
    #[test]
    fn the_branch_chip_counts_only_when_there_is_something_to_count() {
        use df_core::git::DirtyCounts;
        assert_eq!(branch_label("main", None), "main");
        assert_eq!(branch_label("main", Some(DirtyCounts::default())), "main");
        assert_eq!(
            branch_label(
                "main",
                Some(DirtyCounts {
                    staged: 1,
                    unstaged: 2,
                    ..Default::default()
                })
            ),
            "main ·3"
        );
        // Untracked and conflicted are dirt too — a repository with one
        // unmerged path is not clean.
        assert_eq!(
            branch_label(
                "feature/long-name",
                Some(DirtyCounts {
                    untracked: 4,
                    conflicted: 1,
                    ..Default::default()
                })
            ),
            "feature/long-name ·5"
        );
        // A detached head's short hash goes through unchanged: it is whatever
        // the caller decided to call the place you are standing.
        assert_eq!(branch_label("a1b2c3d", None), "a1b2c3d");
    }

    /// The chip carries one number; the card under it carries git's four, and
    /// says the two "nothing to report" answers apart.
    #[test]
    fn the_branch_tooltip_breaks_the_one_number_back_down() {
        use df_core::git::DirtyCounts;
        // No scan yet is not the same as clean, and the card says which.
        assert_eq!(
            branch_tooltip("main", None),
            vec!["main".to_string(), "status not in yet".to_string()]
        );
        assert_eq!(
            branch_tooltip("main", Some(DirtyCounts::default())),
            vec!["main".to_string(), "working tree clean".to_string()]
        );
        // Only the kinds that have something in them, in git's own order.
        assert_eq!(
            branch_tooltip(
                "main",
                Some(DirtyCounts {
                    staged: 2,
                    untracked: 1,
                    ..Default::default()
                })
            ),
            vec![
                "main".to_string(),
                "2 staged".to_string(),
                "1 untracked".to_string(),
            ]
        );
    }

    /// The `…` is a pointer target only when there is something behind it, and
    /// it never sits on top of a crumb that *did* fit.
    #[test]
    fn the_leading_ellipsis_is_hit_testable_only_when_the_path_elides() {
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(Default::default(), |ui| {
            let path = crumbs(std::path::Path::new("/home/brian/Work/delightfile/crates"));
            let bare = Cluster {
                selected: 0,
                visual: None,
                yank: None,
                branch: None,
                dirty: None,
                position: 0,
                rows: 0,
                pick: None,
                types: None,
                trash: None,
            };
            let wide =
                egui::Rect::from_min_size(egui::pos2(8.0, 8.0), egui::vec2(900.0, TOP_HEIGHT));
            assert!(
                top_geometry(ui.painter(), wide, &path, "", &bare, false)
                    .ellipsis
                    .is_none(),
                "nothing was elided, so there is no ellipsis to point at"
            );

            let narrow =
                egui::Rect::from_min_size(egui::pos2(8.0, 8.0), egui::vec2(120.0, TOP_HEIGHT));
            let geom = top_geometry(ui.painter(), narrow, &path, "", &bare, false);
            let rect = geom.ellipsis.expect("a narrow row elides");
            assert!(narrow.contains(rect.center()));
            let first = geom
                .crumbs
                .iter()
                .find(|r| **r != egui::Rect::NOTHING)
                .copied()
                .expect("one crumb always survives");
            assert!(
                rect.right() <= first.left() + 1e-3,
                "the ellipsis sits before the first crumb that fitted"
            );
        });
    }

    /// The chips tile the strip left to right with one gap between, and stop
    /// growing past the cap rather than spreading over the whole window.
    #[test]
    fn the_tab_chips_are_laid_out_left_to_right() {
        let rects = tab_rects(strip(), &[120.0; 3]);
        assert_eq!(rects.len(), 3);
        assert!((rects[0].left() - strip().left()).abs() < 1e-3);
        assert!((rects[1].left() - rects[0].right() - TAB_GAP).abs() < 1e-3);
        assert!(rects[0].width() <= TAB_MAX_WIDTH + 1e-3);
        assert!(rects.iter().all(|r| r.height() == CHROME_HEIGHT));
        assert!(tab_rects(strip(), &[]).is_empty());
    }

    /// A click lands on the chip it looks like it landed on, and on nothing in
    /// the empty space past the last one.
    #[test]
    fn hit_testing_finds_the_chip_under_the_pointer() {
        let strip = strip();
        let rects = tab_rects(strip, &[120.0; 4]);
        for (index, rect) in rects.iter().enumerate() {
            assert_eq!(
                tab_at(strip, &[120.0; 4], rect.center()),
                Some(Control::Tab(index))
            );
        }
        assert_eq!(
            tab_at(
                strip,
                &[120.0; 4],
                egui::pos2(strip.right() - 1.0, strip.center().y)
            ),
            None
        );
        assert_eq!(tab_at(strip, &[120.0; 4], egui::pos2(-10.0, -10.0)), None);
    }

    /// A chip's numeral slot is its `×`, top to bottom, and nowhere else on
    /// the chip is; the `+` is a strip-high square one gap after the last chip,
    /// and the chips never reach it, however many are squeezed in.
    #[test]
    fn the_strip_has_a_close_on_each_chip_and_a_new_tab_after_them() {
        let strip = strip();
        let widths = [120.0; 3];
        let rects = tab_rects(strip, &widths);
        for (index, chip) in rects.iter().enumerate() {
            let close = tab_close_rect(*chip);
            assert!((close.left() - chip.left() - PAD_X).abs() < 1e-3);
            assert!(
                (close.width() - FONT).abs() < 1e-3,
                "not the numeral's width"
            );
            assert_eq!((close.top(), close.bottom()), (chip.top(), chip.bottom()));
            for y in [chip.top() + 1.0, chip.center().y, chip.bottom() - 1.0] {
                let at = egui::pos2(close.center().x, y);
                assert_eq!(tab_at(strip, &widths, at), Some(Control::TabClose(index)));
            }
            // Either side of the slot is the chip: the pad before it, and the
            // title after it.
            for x in [chip.left() + 1.0, close.right() + 1.0, chip.right() - 1.0] {
                let at = egui::pos2(x, chip.center().y);
                assert_eq!(tab_at(strip, &widths, at), Some(Control::Tab(index)));
            }
        }

        let plus = tab_new_rect(strip, &widths);
        assert!((plus.left() - rects[2].right() - TAB_GAP).abs() < 1e-3);
        assert_eq!(plus.size(), egui::vec2(CHROME_HEIGHT, CHROME_HEIGHT));
        assert_eq!(plus.top(), strip.top());
        assert_eq!(tab_at(strip, &widths, plus.center()), Some(Control::TabNew));
        // The gap between the last chip and the `+` is neither, and past the
        // `+` is empty strip.
        let gap = egui::pos2(rects[2].right() + TAB_GAP / 2.0, strip.center().y);
        assert_eq!(tab_at(strip, &widths, gap), None);
        let past = egui::pos2(plus.right() + 1.0, strip.center().y);
        assert_eq!(tab_at(strip, &widths, past), None);

        // Nine chips at the cap in a strip too narrow for them: squeezed up
        // to the `+`, which is still whole at the end of the strip.
        let narrow =
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(600.0, CHROME_HEIGHT));
        let full = [TAB_MAX_WIDTH; 9];
        let plus = tab_new_rect(narrow, &full);
        assert!((plus.right() - narrow.right()).abs() < 1e-3);
        assert!(tab_rects(narrow, &full)
            .iter()
            .all(|chip| chip.right() + TAB_GAP <= plus.left() + 1e-3));
        assert_eq!(tab_at(narrow, &full, plus.center()), Some(Control::TabNew));
        // …and a chip carried over it is still slotted among the chips.
        assert_eq!(tab_slot(narrow, &full, plus.center().x), 8);
    }

    /// Chips sized to their titles: a short name gets the floor, a long one
    /// the cap, and a strip too narrow for all of them squeezes every chip by
    /// the same factor rather than losing the last one off the end.
    #[test]
    fn the_tab_chips_are_sized_to_their_titles() {
        let widths = [TAB_MIN_WIDTH, TAB_MAX_WIDTH, 100.0];
        let rects = tab_rects(strip(), &widths);
        assert!((rects[0].width() - TAB_MIN_WIDTH).abs() < 1e-3);
        assert!((rects[1].width() - TAB_MAX_WIDTH).abs() < 1e-3);
        assert!((rects[2].width() - 100.0).abs() < 1e-3);
        assert!((rects[1].left() - rects[0].right() - TAB_GAP).abs() < 1e-3);

        let narrow =
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, CHROME_HEIGHT));
        let squeezed = tab_rects(narrow, &[100.0, 100.0, 100.0]);
        // Squeezed up to the `+`, which keeps its square at the end.
        assert!((squeezed[2].right() + TAB_GAP + CHROME_HEIGHT - narrow.right()).abs() < 1e-3);
        let plus = tab_new_rect(narrow, &[100.0, 100.0, 100.0]);
        assert!((plus.right() - narrow.right()).abs() < 1e-3);
        assert!((plus.left() - squeezed[2].right() - TAB_GAP).abs() < 1e-3);
        let ratio = squeezed[0].width() / squeezed[1].width();
        assert!((ratio - 1.0).abs() < 1e-3);
        assert!(squeezed.iter().all(|r| r.width() < 100.0));

        // A reorder lays the slots out again in the new order, so a wide tab
        // carried past a narrow one moves where the slots after them start.
        let shifted = tab_shifted(strip(), &widths, 1, 0);
        assert!((shifted[1].left() - strip().left()).abs() < 1e-3);
        assert!((shifted[0].left() - (strip().left() + TAB_MAX_WIDTH + TAB_GAP)).abs() < 1e-3);
    }

    /// The pigtail is a *visible* radius — the tab's own — and the gap is cut
    /// to fit it, so the active tab's flare lands in the space between two
    /// chips and stops at its neighbour's edge rather than carving into it.
    #[test]
    fn the_gap_is_exactly_one_pigtail_wide() {
        assert_eq!(TAB_PIGTAIL, TAB_RADIUS as f32);
        assert_eq!(TAB_GAP, TAB_PIGTAIL);
        let rects = tab_rects(strip(), &[120.0; 3]);
        // The first tab's left edge is the row's own, which is why it has no
        // left pigtail and why the row squares that corner.
        assert!((rects[0].left() - strip().left()).abs() < 1e-3);
        // …and the flare off the second tab's left edge reaches the first
        // tab's right edge and no further.
        assert!((rects[1].left() - TAB_PIGTAIL - rects[0].right()).abs() < 1e-3);
    }

    /// The strip squares the top row's north-west corner and nothing else.
    #[test]
    fn the_strip_squares_the_corner_it_sits_on() {
        let joined = bar_corners(true);
        assert_eq!(joined.nw, 0);
        assert_eq!(
            (joined.ne, joined.sw, joined.se),
            (ROW_RADIUS, ROW_RADIUS, ROW_RADIUS)
        );
        // With no strip it is the plain rounded row it has always been.
        assert_eq!(bar_corners(false), egui::CornerRadius::same(ROW_RADIUS));
    }

    /// The reorder band is wide enough to slide a chip along and well inside
    /// the travel a detach asks for, so the two gestures cannot both be true.
    #[test]
    fn the_reorder_band_stops_short_of_the_detach() {
        let strip = strip();
        assert!(reordering(strip, strip.center()));
        assert!(reordering(
            strip,
            egui::pos2(strip.center().x, strip.bottom() + TAB_REORDER_BAND - 1.0)
        ));
        assert!(!reordering(
            strip,
            egui::pos2(strip.center().x, strip.bottom() + TAB_REORDER_BAND + 1.0)
        ));
        // Sideways it is the strip itself: a hand that has run off the end of
        // the row is still sliding a chip along it, until it drops below.
        assert!(reordering(strip, egui::pos2(strip.left(), strip.top())));
        const { assert!(TAB_REORDER_BAND < crate::window::DETACH_THRESHOLD) };
    }

    /// The carried chip travels with the point the hand took hold of, and
    /// cannot be dragged out of the strip it belongs to.
    #[test]
    fn a_carried_chip_follows_the_grab_and_stays_in_the_strip() {
        let strip = strip();
        let rects = tab_rects(strip, &[120.0; 4]);
        let grab_dx = 20.0;
        // Picked up where it stands: it does not move.
        let still = tab_carry(strip, &[120.0; 4], 1, grab_dx, rects[1].left() + grab_dx);
        assert!((still.left() - rects[1].left()).abs() < 1e-3);
        assert_eq!(still.size(), rects[1].size());
        // Dragged off either end: clamped, never outside.
        let left = tab_carry(strip, &[120.0; 4], 1, grab_dx, strip.left() - 500.0);
        assert!((left.left() - strip.left()).abs() < 1e-3);
        // …short of the `+`'s square at the far end, which is not a slot.
        let right = tab_carry(strip, &[120.0; 4], 1, grab_dx, strip.right() + 500.0);
        assert!((right.right() + TAB_GAP + CHROME_HEIGHT - strip.right()).abs() < 1e-3);
        // A tab that is no longer there is no rectangle at all.
        assert_eq!(
            tab_carry(strip, &[120.0; 4], 9, grab_dx, 0.0),
            egui::Rect::NOTHING
        );
    }

    /// A chip over a slot drops into that slot, and the strip it leaves opens
    /// in the right place.
    #[test]
    fn a_chip_drops_into_the_slot_it_is_over() {
        let strip = strip();
        let rects = tab_rects(strip, &[120.0; 4]);
        for (index, rect) in rects.iter().enumerate() {
            assert_eq!(tab_slot(strip, &[120.0; 4], rect.center().x), index);
        }
        // Off either end it saturates rather than wrapping or panicking.
        assert_eq!(tab_slot(strip, &[120.0; 4], -1000.0), 0);
        assert_eq!(tab_slot(strip, &[120.0; 4], 100_000.0), 3);
        assert_eq!(tab_slot(strip, &[], 0.0), 0);

        // Carrying tab 0 to slot 2 slides 1 and 2 left by one place and leaves
        // 3 where it was — the order a remove-then-insert gives.
        assert_eq!(tab_order(4, 0, 2), vec![1, 2, 0, 3]);
        assert_eq!(tab_order(4, 3, 0), vec![3, 0, 1, 2]);
        assert_eq!(tab_order(4, 2, 2), vec![0, 1, 2, 3]);
        let shifted = tab_shifted(strip, &[120.0; 4], 0, 2);
        assert_eq!(shifted[1], rects[0], "tab 1 has taken the first slot");
        assert_eq!(shifted[2], rects[1]);
        assert_eq!(shifted[0], rects[2], "the carried tab's would-be slot");
        assert_eq!(shifted[3], rects[3], "nothing past the move moved");
        // Dropping where it started moves nothing at all.
        assert_eq!(tab_shifted(strip, &[120.0; 4], 1, 1), rects);
    }

    /// The path, as segments you can click: the root first, the directory you
    /// are in last, and each one addressing where it points.
    #[test]
    fn the_breadcrumb_is_the_path_one_segment_at_a_time() {
        let path = crumbs(std::path::Path::new("/home/brian/Work/delightfile"));
        let labels: Vec<&str> = path.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, ["/", "home", "brian", "Work", "delightfile"]);
        assert_eq!(
            path.last().map(|c| c.path.as_path()),
            Some(std::path::Path::new("/home/brian/Work/delightfile"))
        );
        assert_eq!(
            path[1].path.as_path(),
            std::path::Path::new("/home"),
            "a segment addresses where it points, not where you are"
        );
        // The root on its own is one crumb, not none.
        assert_eq!(crumbs(std::path::Path::new("/")).len(), 1);
    }

    /// The crumbs tile the bar, the hit test finds what was drawn, and a path
    /// too long for the window loses its *leading* segments rather than
    /// overflowing.
    #[test]
    fn the_crumbs_are_laid_out_and_hit_tested_the_same_way() {
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(Default::default(), |ui| {
            let bar =
                egui::Rect::from_min_size(egui::pos2(8.0, 8.0), egui::vec2(700.0, TOP_HEIGHT));
            let path = crumbs(std::path::Path::new("/home/brian/Work/delightfile/crates"));
            let rects = crumb_rects(ui.painter(), bar, &path, 0.0);
            assert_eq!(rects.len(), path.len());
            for (index, rect) in rects.iter().enumerate() {
                assert_ne!(*rect, egui::Rect::NOTHING, "{index} did not fit a wide bar");
                assert!(bar.contains(rect.center()));
            }
            // Left to right, with a separator's worth of space between.
            for pair in rects.windows(2) {
                assert!(pair[1].left() > pair[0].right());
            }

            // A narrow bar elides from the left and keeps the tail.
            let narrow =
                egui::Rect::from_min_size(egui::pos2(8.0, 8.0), egui::vec2(150.0, TOP_HEIGHT));
            let rects = crumb_rects(ui.painter(), narrow, &path, 0.0);
            assert_eq!(rects[0], egui::Rect::NOTHING, "the root should be elided");
            assert_ne!(
                rects[path.len() - 1],
                egui::Rect::NOTHING,
                "the directory you are in is never elided"
            );
            // An elided segment is not hit-testable, which is what keeps a
            // click landing on the crumb it looks like it landed on.
            assert!(!rects[0].contains(narrow.center()));

            // The cluster's room comes out of the crumbs' room.
            let with_cluster = crumb_rects(ui.painter(), bar, &path, 200.0);
            assert_eq!(with_cluster.len(), path.len());
            assert!(with_cluster
                .iter()
                .filter(|r| **r != egui::Rect::NOTHING)
                .all(|r| r.right() <= bar.right() - 200.0 + 1e-3));
        });
    }

    /// The menu button leads the row: a square chip at the row's corner, the
    /// same inset on the two sides it shares with the row and on the one
    /// below, and the path laid out after it — crumbs, or the `…` standing in
    /// for them — never under it.
    #[test]
    fn the_menu_button_leads_the_row() {
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(Default::default(), |ui| {
            let row =
                egui::Rect::from_min_size(egui::pos2(8.0, 40.0), egui::vec2(900.0, TOP_HEIGHT));
            let button = menu_button_rect(row);
            assert!((button.width() - button.height()).abs() < 1e-3, "square");
            assert!((button.left() - row.left() - CHIP_INSET).abs() < 1e-3);
            assert!((button.top() - row.top() - CHIP_INSET).abs() < 1e-3);
            assert!((row.bottom() - button.bottom() - CHIP_INSET).abs() < 1e-3);

            let path = crumbs(std::path::Path::new("/home/brian/Work/delightfile/crates"));
            let bare = Cluster {
                selected: 0,
                visual: None,
                yank: None,
                branch: None,
                dirty: None,
                position: 0,
                rows: 0,
                pick: None,
                types: None,
                trash: None,
            };
            let geom = top_geometry(ui.painter(), row, &path, "", &bare, false);
            assert_eq!(geom.menu, button);
            let first = geom.crumbs[0];
            assert!(
                (first.left() - (button.right() + ICON_GAP)).abs() < 1e-3,
                "{first:?} after {button:?}"
            );

            // Narrow enough to elide: the `…` takes the first crumb's place,
            // still after the button.
            let narrow =
                egui::Rect::from_min_size(egui::pos2(8.0, 40.0), egui::vec2(200.0, TOP_HEIGHT));
            let geom = top_geometry(ui.painter(), narrow, &path, "", &bare, false);
            let ellipsis = geom.ellipsis.expect("a narrow row elides");
            assert!((ellipsis.left() - (geom.menu.right() + ICON_GAP)).abs() < 1e-3);
            for rect in geom.crumbs.iter().filter(|r| **r != egui::Rect::NOTHING) {
                assert!(rect.left() > geom.menu.right(), "{rect:?} under the button");
            }
        });
    }

    /// `delightful-ui` §15: a row inside a card is inset by the padding and its
    /// radius is the card's less that inset, so the gap stays constant round the
    /// corner. The same rule holds for a chip inside the top row.
    #[test]
    fn the_card_radii_are_concentric() {
        assert_eq!(CARD_ROW_RADIUS as f32 + CARD_PAD, CARD_RADIUS as f32);
        assert_eq!(CHIP_RADIUS as f32 + CHIP_INSET, ROW_RADIUS as f32);
    }

    /// The right-hand cluster is laid out right to left, every chip is inside
    /// the row it is on, and what it occupies is what the crumbs are told to
    /// keep clear.
    #[test]
    fn the_cluster_reserves_what_it_occupies() {
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(Default::default(), |ui| {
            let row =
                egui::Rect::from_min_size(egui::pos2(8.0, 8.0), egui::vec2(900.0, TOP_HEIGHT));
            let yanked = [std::path::PathBuf::from("/tmp/a")];
            let full = Cluster {
                selected: 3,
                visual: Some(false),
                yank: Some(Yank {
                    paths: &yanked,
                    cut: true,
                    alpha: 1.0,
                    tray: false,
                }),
                branch: Some("main"),
                dirty: Some(df_core::git::DirtyCounts {
                    unstaged: 3,
                    ..Default::default()
                }),
                position: 12,
                rows: 340,
                pick: None,
                types: None,
                trash: None,
            };
            let geom = cluster_geometry(ui.painter(), row, &full, false);
            let chips = [
                geom.counter,
                geom.git.expect("a branch was given"),
                geom.yank.expect("a clipboard was given"),
                geom.selected.expect("a selection was given"),
                geom.visual.expect("visual mode was given"),
            ];
            // Right to left, in that order, none of them overlapping.
            for pair in chips.windows(2) {
                assert!(pair[1].right() <= pair[0].left() + 1e-3);
            }
            for chip in chips {
                assert!(row.contains(chip.center()));
                assert!(chip.top() >= row.top() - 1e-3 && chip.bottom() <= row.bottom() + 1e-3);
            }
            // What it reserves reaches from the leftmost chip to the row's end.
            assert!((geom.width - (row.right() - chips[4].left() + GAP)).abs() < 1e-3);

            // A directory with nothing to say about it reserves the counter
            // alone, which is always there.
            let bare = Cluster {
                selected: 0,
                visual: None,
                yank: None,
                branch: None,
                dirty: None,
                position: 0,
                rows: 0,
                pick: None,
                types: None,
                trash: None,
            };
            let quiet = cluster_geometry(ui.painter(), row, &bare, false);
            assert!(quiet.git.is_none() && quiet.yank.is_none());
            assert!(quiet.selected.is_none() && quiet.visual.is_none());
            assert!(quiet.width < geom.width);

            // The committed filter is a trailing crumb, after the last one.
            let path = crumbs(std::path::Path::new("/home/brian/Downloads"));
            let top = top_geometry(ui.painter(), row, &path, "invoice", &bare, false);
            let filter = top.filter.expect("a filter was given");
            let last = top.crumbs.last().copied().expect("a crumb was drawn");
            assert!(filter.left() > last.right());
            assert!(filter.right() <= row.right() - quiet.width + 1e-3);
            // …and nothing is drawn for it when nothing is filtered.
            assert!(top_geometry(ui.painter(), row, &path, "", &bare, false)
                .filter
                .is_none());
        });
    }

    /// A picker's buttons end the row: the primary one in the corner, inset
    /// from the right edge by what it is inset from the top and bottom, the
    /// `Cancel` before it, the counter before that — and all of it reserved,
    /// so the crumbs elide against the buttons rather than running under
    /// them.
    #[test]
    fn the_picker_buttons_end_the_row_and_are_reserved() {
        /// `delightful-ui` §1's smallest target a pointer should have to hit.
        const MIN_TARGET: f32 = 24.0;
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(Default::default(), |ui| {
            let row =
                egui::Rect::from_min_size(egui::pos2(8.0, 8.0), egui::vec2(900.0, TOP_HEIGHT));
            let bare = Cluster {
                selected: 0,
                visual: None,
                yank: None,
                branch: None,
                dirty: None,
                position: 1,
                rows: 4,
                pick: None,
                types: None,
                trash: None,
            };
            let picking = Cluster {
                selected: 0,
                visual: None,
                yank: None,
                branch: None,
                dirty: None,
                position: 1,
                rows: 4,
                pick: Some(Pick {
                    label: "Choose folder".to_string(),
                    enabled: true,
                }),
                types: None,
                trash: None,
            };
            let plain = cluster_geometry(ui.painter(), row, &bare, false);
            assert!(plain.pick.is_none() && plain.cancel.is_none());

            let geom = cluster_geometry(ui.painter(), row, &picking, false);
            let pick = geom.pick.expect("a picker has its button");
            let cancel = geom.cancel.expect("…and its cancel");
            // Even insets on every side the button shares with the row, so
            // its radius can be the row's less that inset (§15).
            assert!((row.right() - pick.right() - CHIP_INSET).abs() < 1e-3);
            assert!((pick.top() - row.top() - CHIP_INSET).abs() < 1e-3);
            assert!((row.bottom() - pick.bottom() - CHIP_INSET).abs() < 1e-3);
            assert_eq!(CHIP_RADIUS as f32 + CHIP_INSET, ROW_RADIUS as f32);
            for button in [pick, cancel] {
                assert!(button.height() >= MIN_TARGET, "{button:?} is too short");
                assert!(row.contains(button.center()));
            }
            // Right to left: the pick, then Cancel, then the counter.
            assert!(cancel.right() <= pick.left() + 1e-3);
            assert!(geom.counter.right() <= cancel.left() + 1e-3);
            // Reserved: the crumbs are told to keep clear of all of it.
            assert!(geom.width >= row.right() - geom.counter.left());
            assert!(geom.width > plain.width + pick.width() + cancel.width() - 1e-3);

            // …which is what makes a long path elide sooner with them there.
            let path = crumbs(std::path::Path::new(
                "/home/brian/Work/delightfile/crates/df-app/src/preview/listing",
            ));
            let narrow =
                egui::Rect::from_min_size(egui::pos2(8.0, 8.0), egui::vec2(420.0, TOP_HEIGHT));
            let with = top_geometry(ui.painter(), narrow, &path, "", &picking, false);
            let cluster_left = narrow.right() - with.cluster.width;
            for crumb in with.crumbs.iter().filter(|r| **r != egui::Rect::NOTHING) {
                assert!(
                    crumb.right() <= cluster_left + 1e-3,
                    "a crumb runs under the buttons"
                );
            }
            let shown = |geom: &TopGeom| {
                geom.crumbs
                    .iter()
                    .filter(|r| **r != egui::Rect::NOTHING)
                    .count()
            };
            let without = top_geometry(ui.painter(), narrow, &path, "", &bare, false);
            assert!(shown(&with) < shown(&without), "the buttons took no room");
        });
    }

    /// A dialog's type chip sits beside the counter — it is what the counter
    /// counts — inset like every chip, and reserved, so the crumbs keep clear
    /// of it; the chips that come and go stand further left.
    #[test]
    fn the_type_chip_stands_beside_the_counter() {
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(Default::default(), |ui| {
            let row =
                egui::Rect::from_min_size(egui::pos2(8.0, 8.0), egui::vec2(900.0, TOP_HEIGHT));
            let cluster = |types: Option<Types<'static>>| Cluster {
                selected: 2,
                visual: None,
                yank: None,
                branch: Some("main"),
                dirty: None,
                position: 1,
                rows: 4,
                pick: Some(Pick {
                    label: "Select".to_string(),
                    enabled: true,
                }),
                types,
                trash: None,
            };
            let images = Types {
                label: "Images",
                narrowing: true,
                open: false,
            };
            let without = cluster_geometry(ui.painter(), row, &cluster(None), false);
            assert!(without.types.is_none());
            let geom = cluster_geometry(ui.painter(), row, &cluster(Some(images)), false);
            let chip = geom.types.expect("a dialog with filters has the chip");
            assert!((chip.right() + GAP - geom.counter.left()).abs() < 1e-3);
            let branch = geom.git.expect("a branch was given");
            assert!(branch.right() <= chip.left() + 1e-3);
            assert!((chip.top() - row.top() - CHIP_INSET).abs() < 1e-3);
            assert!((row.bottom() - chip.bottom() - CHIP_INSET).abs() < 1e-3);
            assert!((geom.width - without.width - chip.width() - GAP).abs() < 1e-3);
            // "All files" is a longer word than "Images", and the chip grows
            // to hold it rather than clipping it.
            let all = cluster_geometry(
                ui.painter(),
                row,
                &cluster(Some(Types {
                    label: crate::menu::ALL_FILES,
                    narrowing: false,
                    open: false,
                })),
                false,
            );
            assert!(all.types.expect("the chip").width() > chip.width());
        });
    }

    /// The chip says what it is holding and how it got there.
    #[test]
    fn the_clipboard_chip_names_its_verb() {
        assert_eq!(yank_label(3, false), "3 yanked");
        assert_eq!(yank_label(1, true), "1 cut");
    }

    /// The help card stays inside the window and above the bar, however small
    /// the window gets.
    #[test]
    fn the_help_card_fits_the_window() {
        for size in [egui::vec2(1400.0, 900.0), egui::vec2(320.0, 200.0)] {
            let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), size);
            let rect = help_rect(area, area.top() + TOP_HEIGHT + GAP, area.bottom() - GAP);
            assert!(rect.width() > 0.0 && rect.width() <= HELP_MAX_WIDTH + 1e-3);
            assert!(rect.left() >= area.left() && rect.right() <= area.right() + 1e-3);
            assert!(rect.top() >= area.top());
        }
    }

    /// The sheet's bar sits in the card's right padding beside its lines,
    /// between the heading and the hint strip, so the `×` in the heading's
    /// corner and the hints keep their own rects; a sheet whose lines fit a
    /// page has none.
    #[test]
    fn the_help_sheets_bar_is_beside_its_lines_and_clear_of_the_close() {
        let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, 900.0));
        let rect = help_rect(area, area.top() + TOP_HEIGHT + GAP, area.bottom() - GAP);
        let page = help_page(rect);
        let help = Help::default();
        assert_eq!(
            help_bar(rect, &help, page),
            None,
            "a page of lines has a bar"
        );
        assert_eq!(help_band(rect, page), None);

        let bar = help_bar(rect, &help, page * 3).expect("three pages overflow");
        let body = help_body(rect);
        assert!(
            bar.thumb.left() >= body.right(),
            "the thumb is over the lines"
        );
        assert!(rect.contains_rect(bar.thumb));
        assert_eq!(help_band(rect, page * 3), Some(bar.hit));
        assert_eq!(bar.hit.right(), rect.right(), "flush with the card's edge");
        let close = close_button_rect(rect);
        assert!(bar.hit.top() >= close.bottom(), "the band is over the ×");
        assert!(bar.hit.bottom() <= hint_rect(rect).top(), "over the hints");
    }

    /// The pointer and the paint read one layout: the boundary the paint puts
    /// a caret at is the boundary a click there finds, a click on either half
    /// of a letter is *on* that letter, and the field is only the text's own
    /// strip — after the title and the directory, before the case indicator.
    #[test]
    fn a_click_in_the_prompt_lands_on_the_boundary_it_is_drawn_at() {
        use crate::input::{Prompt, PromptKind};
        use df_core::input::InputBuffer;
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(Default::default(), |ui| {
            let painter = ui.painter();
            let row =
                egui::Rect::from_min_size(egui::pos2(8.0, 40.0), egui::vec2(900.0, TOP_HEIGHT));
            let path = "/home/brian/Downloads";
            let len = path.chars().count();
            let prompt = Prompt::with(PromptKind::Path, 0, InputBuffer::new(path, len));
            let field = prompt_field_geometry(painter, row, &prompt, None);

            let title = text_width(painter, "Go to:", egui::FontId::proportional(FONT));
            assert!(field.rect.left() >= row.left() + PAD_X + title);
            assert!(row.contains_rect(field.rect));
            assert_eq!(field.scroll, 0.0, "a line that fits is not scrolled");
            assert_eq!(field.origin, field.rect.left());

            for i in 0..=len {
                let x = field.x_of(i);
                assert_eq!(field.boundary_at(x), i, "the caret's own x, {i}");
                if i < len {
                    let next = field.x_of(i + 1);
                    assert!(next > x, "{i}");
                    let (near, far) = (x + (next - x) * 0.25, x + (next - x) * 0.75);
                    assert_eq!(field.boundary_at(near), i, "the left half, {i}");
                    assert_eq!(field.boundary_at(far), i + 1, "the right half, {i}");
                    assert_eq!(field.char_under(near), i, "{i}");
                    assert_eq!(field.char_under(far), i, "still the same letter, {i}");
                }
            }
            // Off either end of the text is that end.
            assert_eq!(field.boundary_at(row.left() - 50.0), 0);
            assert_eq!(field.boundary_at(row.right() + 50.0), len);
            assert_eq!(field.char_under(row.right() + 50.0), len);

            // The double click the brief names: the right half of the `r` in
            // `brian` is still the `r`, and the segment it is in is `brian`.
            let r = field.x_of(7) + (field.x_of(8) - field.x_of(7)) * 0.9;
            assert_eq!(prompt.segment_at(field.char_under(r)), 6..11);

            // The directory tail pushes the field right, and a live prompt's
            // case indicator is at the line's end, outside the field.
            let filter = Prompt::with(PromptKind::Filter, 0, InputBuffer::new("invoice", 7));
            let bare = prompt_field_geometry(painter, row, &filter, None);
            let tailed = prompt_field_geometry(painter, row, &filter, Some("delightfile"));
            assert!(tailed.rect.left() > bare.rect.left());
            assert!(bare.rect.right() < row.right() - PAD_X * 2.0);
            assert_eq!(tailed.rect.right(), bare.rect.right());

            // An error too long for the line has a line of its own, and the
            // field keeps the first one, whole.
            let mut refused = Prompt::with(PromptKind::Create, 0, InputBuffer::new("notes", 5));
            let one = prompt_field_geometry(painter, row, &refused, None);
            refused.error = Some("that name is already taken by a directory".to_string());
            let tall = egui::Rect::from_min_size(
                row.min,
                egui::vec2(row.width(), TOP_HEIGHT + crate::ui::PROMPT_ERROR_LINE),
            );
            let two = prompt_field_geometry(painter, tall, &refused, None);
            assert_eq!(two.rect, one.rect);
            // …and one that fits beside the text takes its room from the field.
            let inline = prompt_field_geometry(painter, row, &refused, None);
            assert!(inline.rect.right() < one.rect.right());
        });
    }

    /// The offset the prompt's text is scrolled by: none while the line fits,
    /// the caret against the right edge when it runs off that way, against the
    /// left when it runs off that way — and otherwise wherever it was, so a
    /// caret moving inside the field moves nothing else.
    #[test]
    fn the_prompt_scrolls_only_as_far_as_the_caret_needs() {
        let near = |a: f32, b: f32| (a - b).abs() < 1e-4;
        let width = 200.0;

        // A line that fits never scrolls, wherever the caret or the last
        // scroll was.
        assert_eq!(caret_scroll(width, 150.0, 180.0, 0.0), 0.0);
        assert_eq!(caret_scroll(width, 0.0, 180.0, 40.0), 0.0);

        // `Go to:` opening on a long path, caret at the end: the end of the
        // path shows, caret whole against the right edge.
        let text = 600.0;
        let end = caret_scroll(width, text, text, 0.0);
        assert!(near(end, text + CARET_WIDTH - width));
        assert!(near(text + CARET_WIDTH - end, width), "caret at the edge");

        // The caret walking left inside the field moves nothing…
        assert_eq!(caret_scroll(width, 500.0, text, end), end);
        assert_eq!(caret_scroll(width, end, text, end), end);
        // …until it passes the left edge, where it is held.
        assert!(near(caret_scroll(width, 300.0, text, end), 300.0));
        // `Home`: all the way back.
        assert_eq!(caret_scroll(width, 0.0, text, end), 0.0);
        // Walking right again from the start moves nothing until the edge…
        assert_eq!(caret_scroll(width, 150.0, text, 0.0), 0.0);
        // …and then keeps the caret against it.
        assert!(near(
            caret_scroll(width, 250.0, text, 0.0),
            250.0 + CARET_WIDTH - width
        ));

        // A line that got shorter under a scroll gives the room back rather
        // than leaving empty field after its end.
        assert!(near(
            caret_scroll(width, 250.0, 300.0, 380.0),
            300.0 + CARET_WIDTH - width
        ));
    }

    /// A long line in the field: the caret is in view, the hit test still
    /// lands on the boundary the paint draws at, and a drag past the edge that
    /// scrolls the text asks the scrolled layout what is at the edge now.
    #[test]
    fn a_long_line_scrolls_its_caret_into_view_and_is_hit_where_it_is_drawn() {
        use crate::input::{Prompt, PromptKind};
        use df_core::input::InputBuffer;
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(Default::default(), |ui| {
            let painter = ui.painter();
            let row =
                egui::Rect::from_min_size(egui::pos2(8.0, 40.0), egui::vec2(260.0, TOP_HEIGHT));
            let path = "/home/brian/Work/delightfile/crates/df-app/src/chrome.rs";
            let len = path.chars().count();
            let mut prompt = Prompt::with(PromptKind::Path, 0, InputBuffer::new(path, len));
            let field = prompt_field_geometry(painter, row, &prompt, None);

            assert!(
                field.galley.size().x > field.rect.width(),
                "the test needs a long line"
            );
            assert!(field.scroll > 0.0);
            let caret = field.x_of(len);
            assert!(
                (caret + CARET_WIDTH - field.rect.right()).abs() < 1e-3,
                "at the right edge"
            );
            // Every boundary in view is hit where it is drawn. Not always *that*
            // boundary: the shaper sets `fi` in `delightfile` as one ligature
            // and gives the `i` a zero-width glyph at its far edge, so the
            // boundary between the two letters is drawn where the one after
            // them is — the same caret egui's own text fields draw there — and
            // a click at that x finds the one after. What must hold is that a
            // click lands on a boundary drawn exactly where it clicked.
            for i in 0..=len {
                let x = field.x_of(i);
                if field.rect.contains(egui::pos2(x, field.rect.center().y)) {
                    let hit = field.boundary_at(x);
                    assert!((field.x_of(hit) - x).abs() < 1e-3, "{i} hit {hit}");
                }
            }

            // The prompt remembers the scroll, and a caret moved inside the
            // field leaves it where it was.
            prompt.scroll = field.scroll;
            let inside = field.boundary_at(field.rect.center().x);
            prompt.buffer.move_to(inside, false);
            let again = prompt_field_geometry(painter, row, &prompt, None);
            assert_eq!(again.scroll, field.scroll, "a click does not move the text");

            // Scrolled back by a drag past the left edge: the character at
            // that edge is an earlier one than it was.
            let left = field.boundary_at(field.rect.left());
            let back = field.scrolled(field.scroll - 60.0);
            assert!(back.boundary_at(back.rect.left()) < left);
            assert_eq!(field.scrolled(-5.0).scroll, 0.0);
            assert_eq!(field.scrolled(1e6).scroll, field.most_scroll());
        });
    }

    /// Everything draws, including the states that are easy to forget: an empty
    /// help sheet, a one-column card, a prompt with a caret mid-string.
    #[test]
    fn the_chrome_paints_without_panicking() {
        use crate::input::{Prompt, PromptKind};
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(Default::default(), |ui| {
            let palette = crate::theme::Palette::default();
            let theme = df_core::config::Theme::default();
            let paint = Painting {
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
            let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, 900.0));

            // Every tab in turn is the active one, because each position
            // draws a different shape: the first has no left pigtail (its edge
            // is the row's), the middle has both, and the last has both only
            // because the chips are capped short of the row's right edge.
            let titles: Vec<String> = ["work", "downloads", "src"]
                .iter()
                .map(|t| t.to_string())
                .collect();
            let widths = tab_widths(ui.painter(), &titles);
            assert!(widths
                .iter()
                .all(|w| (TAB_MIN_WIDTH..=TAB_MAX_WIDTH).contains(w)));
            for active in 0..titles.len() {
                tab_strip(
                    &paint,
                    strip(),
                    &titles,
                    active,
                    if active == 1 { 1.0 } else { 0.0 },
                    &widths,
                    None,
                    &Hovers::new(),
                    &Ripples::new(),
                );
            }
            // …and with a chip off the ground: in the hand (settled 0, carried
            // past its neighbour) and half-way down into a slot, which are the
            // two ends of the landing.
            for (tab, settle) in [(0usize, 0.0f32), (2, 0.5)] {
                let slot = tab_slot(strip(), &widths, strip().center().x);
                tab_strip(
                    &paint,
                    strip(),
                    &titles,
                    1,
                    0.0,
                    &widths,
                    Some(&Carry {
                        tab,
                        rect: tab_carry(strip(), &widths, tab, 20.0, strip().center().x),
                        offsets: tab_shifted(strip(), &widths, tab, slot)
                            .iter()
                            .zip(tab_rects(strip(), &widths))
                            .map(|(to, home)| to.left() - home.left())
                            .collect(),
                        settle,
                    }),
                    &Hovers::new(),
                    &Ripples::new(),
                );
            }
            // A carry naming a tab that has gone is no carry at all.
            tab_strip(
                &paint,
                strip(),
                &titles,
                0,
                0.0,
                &widths,
                Some(&Carry {
                    tab: 9,
                    rect: strip(),
                    offsets: Vec::new(),
                    settle: 0.0,
                }),
                &Hovers::new(),
                &Ripples::new(),
            );
            // Under the pointer: a chip's `×` lit and pressed with a ripple on
            // it, the `+` lit and rippling, on the active chip and off it.
            let now = std::time::Instant::now();
            for (hot, pressed) in [
                (Control::TabClose(1), Some(Control::TabClose(1))),
                (Control::TabClose(0), None),
                (Control::Tab(2), None),
                (Control::TabNew, Some(Control::TabNew)),
            ] {
                let mut hovers = Hovers::new();
                hovers.tick(Some(hot), pressed, now);
                let mut ripples = Ripples::new();
                ripples.spawn(hot, strip().center(), strip(), now);
                tab_strip(
                    &paint,
                    strip(),
                    &titles,
                    0,
                    0.0,
                    &widths,
                    None,
                    &hovers,
                    &ripples,
                );
            }
            let path = crumbs(std::path::Path::new("/home/brian/Work/delightfile"));
            let path_rect =
                egui::Rect::from_min_size(egui::pos2(8.0, 40.0), egui::vec2(1384.0, TOP_HEIGHT));
            let yanked = [
                std::path::PathBuf::from("/tmp/one.txt"),
                std::path::PathBuf::from("/tmp/two.txt"),
            ];
            // Every state the row can be in at once, and the bare one.
            for (branch, filter, yank) in [
                (None, "", None),
                (
                    Some("main"),
                    "invoice",
                    Some(Yank {
                        paths: &yanked,
                        cut: false,
                        alpha: 1.0,
                        tray: false,
                    }),
                ),
            ] {
                let cluster = Cluster {
                    selected: 3,
                    visual: Some(true),
                    yank,
                    branch,
                    dirty: None,
                    position: 12,
                    rows: 340,
                    // The picker's buttons, drawn in the fuller of the two
                    // states — enabled, on the same row as every chip.
                    pick: branch.map(|_| Pick {
                        label: "Select 3".to_string(),
                        enabled: true,
                    }),
                    // …and the type chip lit, with its popover out.
                    types: branch.map(|_| Types {
                        label: "Images",
                        narrowing: true,
                        open: true,
                    }),
                    // …and the trash's weight, still counting, with a clock
                    // for its tooltip.
                    trash: branch.map(|_| TrashChip {
                        label: "37 items · ~1.2 GB".to_string(),
                        tip: Some("Items are removed for good after 30 days".to_string()),
                    }),
                };
                let geom = top_geometry(paint.painter, path_rect, &path, filter, &cluster, false);
                path_bar(
                    &paint,
                    area,
                    path_rect,
                    &path,
                    filter,
                    1.0,
                    true,
                    &cluster,
                    &geom,
                    filter.is_empty(),
                    &Hovers::new(),
                    &Ripples::new(),
                );
            }
            // …and the elided case, which draws its own leading ellipsis.
            let narrow =
                egui::Rect::from_min_size(egui::pos2(8.0, 40.0), egui::vec2(90.0, TOP_HEIGHT));
            let cluster = Cluster {
                selected: 0,
                visual: None,
                yank: None,
                branch: None,
                dirty: None,
                position: 0,
                rows: 0,
                // …and a disabled one, which paints by a path of its own.
                pick: Some(Pick {
                    label: "Select".to_string(),
                    enabled: false,
                }),
                // …and the type chip at rest at "All files", which is dim.
                types: Some(Types {
                    label: crate::menu::ALL_FILES,
                    narrowing: false,
                    open: false,
                }),
                // …and a trash that keeps things for ever: a label, no tip.
                trash: Some(TrashChip {
                    label: "1 item".to_string(),
                    tip: None,
                }),
            };
            let geom = top_geometry(paint.painter, narrow, &path, "", &cluster, false);
            path_bar(
                &paint,
                area,
                narrow,
                &path,
                "",
                0.0,
                false,
                &cluster,
                &geom,
                false,
                &Hovers::new(),
                &Ripples::new(),
            );

            let mut prompt = Prompt::with(
                PromptKind::Filter,
                0,
                df_core::input::InputBuffer::new("READ", 2),
            );
            prompt_row(&paint, path_rect, &prompt, Some("delightfile"), true);
            // …and every mode of it, since each one draws a different caret.
            prompt.feed(df_core::keymap::Chord::plain(df_core::keymap::Key::Escape));
            prompt_row(&paint, path_rect, &prompt, None, false);
            prompt.feed(df_core::keymap::Chord::from_char('v').expect("v"));
            prompt_row(&paint, path_rect, &prompt, Some("delightfile"), false);
            // …and with a selection the pointer made, which is drawn from the
            // same layout as the caret.
            prompt.buffer.set_selection(1, 3);
            prompt_row(&paint, path_rect, &prompt, None, false);
            // …and the two-line form, which an error too long for the line
            // asks the layout for.
            prompt.error = Some("that name is already taken by a directory".to_string());
            let tall = egui::Rect::from_min_size(
                path_rect.min,
                egui::vec2(220.0, TOP_HEIGHT + crate::ui::PROMPT_ERROR_LINE),
            );
            assert_eq!(
                prompt_lines(paint.painter, &prompt, 220.0, Some("delightfile")),
                2
            );
            prompt_row(&paint, tall, &prompt, Some("delightfile"), true);
            prompt.error = None;
            // The filter's hint, which is measured and placed as an error is:
            // beside the query when it fits, on a line of its own when not.
            prompt.hint = Some("No matches here · Enter searches everywhere");
            assert_eq!(prompt.message().map(|(_, error)| error), Some(false));
            assert_eq!(prompt_lines(paint.painter, &prompt, 1200.0, None), 1);
            prompt_row(&paint, path_rect, &prompt, Some("delightfile"), false);
            assert_eq!(
                prompt_lines(paint.painter, &prompt, 220.0, Some("delightfile")),
                2
            );
            prompt_row(&paint, tall, &prompt, Some("delightfile"), false);
            // An error outranks it: that one is about what was typed.
            prompt.error = Some("bad".to_string());
            assert_eq!(prompt.message(), Some(("bad", true)));
            prompt.error = None;
            prompt.hint = None;
            // The archive prompt's inked hint, measured as a hint is and
            // painted in its colours, beside the query and on its own line.
            let mut inked = crate::input::InkedHint::default();
            inked.push("zip", crate::input::Ink::Strong);
            inked.push(" · tar · ", crate::input::Ink::Quiet);
            inked.push("tar.zst needs zstd", crate::input::Ink::Warn);
            inked.push(" · ", crate::input::Ink::Quiet);
            inked.push("7z", crate::input::Ink::Absent);
            prompt.inked = Some(inked);
            assert_eq!(prompt_lines(paint.painter, &prompt, 1200.0, None), 1);
            prompt_row(&paint, path_rect, &prompt, Some("delightfile"), false);
            assert_eq!(
                prompt_lines(paint.painter, &prompt, 220.0, Some("delightfile")),
                2
            );
            prompt_row(&paint, tall, &prompt, Some("delightfile"), false);
            prompt.inked = None;
            let mut rename = Prompt::with(
                PromptKind::Rename,
                0,
                df_core::input::InputBuffer::for_rename_stem("photo.jpg"),
            );
            rename.error = Some("photo.jpg already exists".to_string());
            let row = egui::Rect::from_min_size(egui::pos2(300.0, 400.0), egui::vec2(400.0, 22.0));
            prompt_popup(&paint, area, row, &rename);
            let strip = hint_rect(egui::Rect::from_min_size(
                egui::pos2(300.0, 500.0),
                egui::vec2(400.0, 120.0),
            ));
            let shown = [
                Hint::inert("↑↓", "move"),
                Hint::new("Esc", "close", Command::Escape),
            ];
            hints(
                &paint,
                &shown,
                &hint_rects(paint.painter, strip, &shown),
                &Hovers::new(),
                &Ripples::new(),
            );

            let rows: Vec<crate::whichkey::Row> = (0..14)
                .map(|i| crate::whichkey::Row {
                    keys: format!("{i}"),
                    label: format!("do the {i}th thing"),
                    next: df_core::keymap::Chord::from_char('x').expect("x"),
                })
                .collect();
            let geometry = crate::whichkey::geometry(paint.painter, area, area.bottom(), &rows, 0);
            let (hovers, ripples) = (Hovers::new(), Ripples::new());
            which_key(&paint, &geometry, &rows, 1.0, &hovers, &ripples);
            which_key(&paint, &geometry, &rows, 0.4, &hovers, &ripples);
            let empty = crate::whichkey::geometry(paint.painter, area, area.bottom(), &[], 0);
            which_key(&paint, &empty, &[], 1.0, &hovers, &ripples);

            let registry = df_core::keymap::Registry::defaults();
            let stack = df_core::keymap::ContextStack::with(&[df_core::keymap::Context::Help]);
            let all = crate::help::all_rows(&registry, &stack, df_core::keymap::WhenFlags::NONE);
            let lines = crate::help::lines(&all, "");
            let mut help = Help::default();
            help.reset(&lines);
            let rect = help_rect(area, area.top() + TOP_HEIGHT + GAP, area.bottom() - GAP);
            // Three headings: an empty field, a query with a caret in it, and
            // the placeholder that stands in for both.
            let filter = |query, caret| crate::help::Filter { query, caret };
            help_overlay(
                &paint,
                area,
                rect,
                &lines,
                &help,
                all.len(),
                filter("", None),
                &Hovers::new(),
                &Ripples::new(),
            );
            help_overlay(
                &paint,
                area,
                rect,
                &lines,
                &help,
                all.len(),
                filter("so", Some(1)),
                &Hovers::new(),
                &Ripples::new(),
            );
            help_overlay(
                &paint,
                area,
                rect,
                &[],
                &help,
                all.len(),
                filter("", Some(0)),
                &Hovers::new(),
                &Ripples::new(),
            );
        });
    }
}
