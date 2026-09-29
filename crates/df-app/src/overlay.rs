//! Where the two new overlays are drawn: the fuzzy card and the search panel
//! (PLAN §4.4, §7.2).
//!
//! Geometry and paint live together here, and every hit test the app does
//! against these surfaces goes through the same `*_geometry` function the paint
//! uses — the rule the tab strip and the breadcrumb already follow, because two
//! functions computing a layout separately is how a surface grows a one-pixel
//! lie at its edges.
//!
//! ## Why they are two different shapes
//!
//! The **fuzzy card** ([`finder_geometry`]) is a centred plate over a dimmed
//! window, biased above true centre (`delightful-ui` §16). It is modal in the
//! strongest sense: while it is up, the window behind it is scenery, and
//! dimming it says so.
//!
//! The **search panel** ([`search_geometry`]) is not. Its whole point is the
//! live preview of the highlighted hit (PLAN §7.2), so the preview pane has to
//! stay bright and legible — a scrim over the window would dim the one thing
//! you opened the overlay to look at. So it takes the parent and list columns,
//! leaves the preview pane alone, and the two halves read as "results here,
//! what they are there".
//!
//! Its title is a switch rather than a sentence — **Names | Contents**, the
//! lit half being the mode it is in — because the panel is one search asked
//! two ways ([`crate::search`]), and a title that only named the current way
//! would hide that the other one is a click (or `Tab`) away.

use egui::{Align2, Color32, FontId, Rect};

use crate::chrome::{self, key_font, CARD_PAD, CARD_ROW, CARD_ROW_RADIUS, FONT, PAD_X};
use crate::finder::{Finder, Kind};
use crate::hover::{pressed_rect, Hovers};
use crate::icons::icon_for;
use crate::ripple::Ripples;
use crate::search::{Mode, Search};
use crate::theme::mix;
use crate::ui::{Control, Painting, GAP};

/// The widest the fuzzy card gets.
///
/// Six hundred and forty points. A palette row is a short label and a shorter
/// chord; past this the two
/// are separated by a desert and the eye loses the row on the way across
/// (`ui-anti-slop`'s line-length rule, the same one that caps the help sheet —
/// and this card's rows are much shorter than the help sheet's three columns,
/// so it is narrower).
const CARD_WIDTH: f32 = 640.0;

/// The query field's height inside the card.
///
/// A little taller than a row, because it is the one thing on the card being
/// *typed into* and it has to look like a field rather than the first result.
const FIELD_ROW: f32 = 28.0;

/// A search result row's height, per mode.
///
/// A name is one line and a content hit is two — the file and line on top, the
/// matched text under it — so they are two heights rather than one compromise
/// that wastes a third of the panel on the common case.
const NAME_ROW: f32 = 24.0;
const CONTENT_ROW: f32 = 36.0;

/// The width the `path:line` column gets in a content result.
///
/// Fixed rather than measured, for the reason the help sheet's key column is:
/// the matched text has to start at the same x on every row or the panel is a
/// ragged mess and the thing you are scanning for moves as you scan.
const HIT_PATH_COLUMN: f32 = 220.0;

// ── The fuzzy card ──────────────────────────────────────────────────────────

/// Where the fuzzy card's pieces are, for one frame.
pub struct FinderGeom {
    pub card: Rect,
    /// The query field.
    pub field: Rect,
    /// Where the rows go: the part of the card under the field, as tall as
    /// the rows the card has room for.
    pub body: Rect,
    /// One rectangle per visible row, in the order they are drawn.
    pub rows: Vec<Rect>,
    /// The `×` at the field's right, in the card's corner.
    pub close: Option<Rect>,
    /// The band the rows' bar is pointed at by, while there are more hits
    /// than rows ([`crate::scrollbar::band`]).
    pub band: Option<Rect>,
}

impl FinderGeom {
    pub fn row_at(&self, pos: egui::Pos2) -> Option<usize> {
        self.rows.iter().position(|rect| rect.contains(pos))
    }
}

/// Lay the fuzzy card out for a list of `hits`, showing up to
/// [`crate::finder::ROWS`] of them, or as many as the window has room for.
///
/// The card is exactly as tall as it has to be — an empty result list is a
/// field and one line of guidance, not a field over eleven rows of nothing
/// (`delightful-ui` §11) — and never taller than the window: a short window
/// gets fewer rows, which the cursor scrolls, rather than rows drawn past the
/// card's foot ([`crate::dialog::fit_rows`]).
pub fn finder_geometry(area: Rect, hits: usize) -> FinderGeom {
    let shown = hits.min(crate::finder::ROWS);
    let width = (area.width() - chrome::CARD_MARGIN * 2.0).min(CARD_WIDTH);
    // The field, and the strip its own hints go in (PLAN §4).
    let fixed = CARD_PAD * 2.0 + FIELD_ROW + GAP + chrome::HINT_ROW;
    // One row's worth of height for the empty state's sentence, so the card
    // does not collapse to a bare field when a query matches nothing.
    let (lines, card) = crate::dialog::fit_rows(area, width, fixed, CARD_ROW, shown.max(1));
    let field = Rect::from_min_size(
        card.min + egui::vec2(CARD_PAD, CARD_PAD),
        egui::vec2(field_width(card), FIELD_ROW),
    );
    let body = Rect::from_min_size(
        egui::pos2(card.left() + CARD_PAD, field.bottom() + GAP),
        egui::vec2(
            (card.width() - CARD_PAD * 2.0).max(0.0),
            lines as f32 * CARD_ROW,
        ),
    );
    let rows: Vec<Rect> = (0..shown.min(lines))
        .map(|n| {
            Rect::from_min_size(
                egui::pos2(body.left(), body.top() + n as f32 * CARD_ROW),
                egui::vec2(body.width(), CARD_ROW),
            )
        })
        .collect();
    FinderGeom {
        card,
        field,
        body,
        band: crate::scrollbar::band(card, body, rows.len() as f32, hits as f32),
        rows,
        close: Some(close_beside(card, field)),
    }
}

/// The card's bar, beside its rows, while it shows fewer than it has.
pub fn finder_bar(geometry: &FinderGeom, finder: &Finder) -> Option<crate::scrollbar::Geometry> {
    crate::scrollbar::card(
        geometry.card,
        geometry.body,
        finder.first as f32,
        geometry.rows.len() as f32,
        finder.hits.len() as f32,
    )
}

/// How wide a card's query field is: the card's inner width less the `×`
/// beside it and the gap between them.
fn field_width(card: Rect) -> f32 {
    (card.width() - CARD_PAD * 2.0 - CARD_ROW - GAP).max(0.0)
}

/// The `×` right of the query field: a [`CARD_ROW`] square centred on the
/// field's row and [`CARD_PAD`] in from the card's right edge. Centred rather
/// than tucked into the corner, because the field is taller than the button
/// and the two read as one row.
fn close_beside(card: Rect, field: Rect) -> Rect {
    Rect::from_center_size(
        egui::pos2(card.right() - CARD_PAD - CARD_ROW / 2.0, field.center().y),
        egui::vec2(CARD_ROW, CARD_ROW),
    )
}

/// Draw the command palette / jump card, over the scrim the app lays for it.
pub fn paint_finder(
    paint: &Painting<'_>,
    geometry: &FinderGeom,
    finder: &Finder,
    hovers: &Hovers<Control>,
    ripples: &Ripples<Control>,
) {
    let (painter, palette) = (paint.painter, paint.palette);
    chrome::card(paint, geometry.card, 1.0);

    // ── The field ───────────────────────────────────────────────────────────
    let field = geometry.field;
    painter.rect_filled(field, field_radius(), palette.mantle);
    if let Some(close) = geometry.close {
        chrome::close_button(paint, close, hovers, ripples);
    }
    let text_left = field.left() + PAD_X;
    let baseline = field.center().y;
    painter.text(
        egui::pos2(text_left, baseline),
        Align2::LEFT_CENTER,
        finder.source.title(),
        FontId::proportional(FONT),
        palette.faint,
    );
    let title_width =
        chrome::text_width(painter, finder.source.title(), FontId::proportional(FONT));
    let query_left = text_left + title_width + GAP;
    let query = finder.query();
    if query.is_empty() {
        painter.text(
            egui::pos2(query_left, baseline),
            Align2::LEFT_CENTER,
            finder.source.placeholder(),
            FontId::proportional(FONT),
            palette.faint,
        );
    } else {
        painter.text(
            egui::pos2(query_left, baseline),
            Align2::LEFT_CENTER,
            query,
            FontId::proportional(FONT),
            palette.text,
        );
    }
    // The caret. No blink — PLAN §4.2's rule, and a blinking caret is a window
    // that never stops asking for frames.
    let caret_x = query_left
        + chrome::text_width(
            painter,
            &query[..finder.buffer.cursor_byte().min(query.len())],
            FontId::proportional(FONT),
        );
    painter.rect_filled(
        Rect::from_min_size(
            egui::pos2(caret_x, field.top() + 6.0),
            egui::vec2(crate::chrome::CARET_WIDTH, field.height() - 12.0),
        ),
        0,
        palette.text,
    );
    // How many of how many, on the right, so a long list says so before you
    // scroll it.
    if finder.hits.len() > geometry.rows.len() {
        painter.text(
            egui::pos2(field.right() - PAD_X, baseline),
            Align2::RIGHT_CENTER,
            format!("{} of {}", finder.cursor + 1, finder.hits.len()),
            FontId::proportional(FONT - 1.0),
            palette.faint,
        );
    }

    // ── The rows ────────────────────────────────────────────────────────────
    if finder.hits.is_empty() {
        if let Some(rect) = geometry.rows.first() {
            painter.text(
                rect.center(),
                Align2::CENTER_CENTER,
                finder.source.empty_message(),
                FontId::proportional(FONT),
                palette.faint,
            );
        }
        return;
    }
    for (offset, rect) in geometry.rows.iter().enumerate() {
        let index = finder.first + offset;
        let Some(hit) = finder.hits.get(index) else {
            break;
        };
        let Some(row) = finder.pool.get(hit.index) else {
            continue;
        };
        let key = Control::PanelRow(offset);
        let on_cursor = index == finder.cursor;
        let hover = hovers.hover(key);
        let rect = pressed_rect(*rect, hovers.press(key));
        if on_cursor || hover > 0.0 {
            let ground = if on_cursor {
                palette.surface1
            } else {
                palette.crust
            };
            painter.rect_filled(rect, CARD_ROW_RADIUS, mix(ground, palette.surface0, hover));
        }
        let inside = painter.with_clip_rect(rect);
        for splash in ripples.splashes(key, paint.now) {
            inside.circle_filled(
                splash.center,
                splash.radius,
                crate::theme::splash(palette, splash.alpha),
            );
        }

        // A glyph saying what kind of thing this is, in the colour that kind
        // already wears everywhere else in the program.
        let (glyph, tint) = match row.kind {
            Kind::Command => ("›", palette.quiet),
            Kind::Place => ("/", palette.blue),
            Kind::Tab => ("▣", palette.teal),
            // `▦` is not in the stock faces; `⊞` is the same idea and is.
            Kind::View => ("⊞", palette.mauve),
        };
        inside.text(
            egui::pos2(rect.left() + PAD_X, rect.center().y),
            Align2::LEFT_CENTER,
            glyph,
            key_font(FONT),
            tint,
        );

        // The detail is measured first so the label gets whatever is left and
        // truncates rather than running under the chord.
        let detail_font = if row.kind == Kind::Command {
            key_font(FONT - 0.5)
        } else {
            FontId::proportional(FONT - 0.5)
        };
        let detail_width = chrome::text_width(&inside, &row.detail, detail_font.clone());
        spans(
            &inside,
            egui::pos2(rect.right() - PAD_X - detail_width, rect.center().y),
            &row.detail,
            palette.faint,
            crate::theme::ink(palette, palette.sky),
            &hit.detail,
            detail_font,
            detail_width,
        );
        let label_left = rect.left() + PAD_X + ICON_COLUMN;
        spans(
            &inside,
            egui::pos2(label_left, rect.center().y),
            &row.label,
            palette.text,
            crate::theme::ink(palette, palette.sky),
            &hit.label,
            FontId::proportional(FONT),
            (rect.right() - PAD_X - detail_width - GAP - label_left).max(0.0),
        );
    }
    if let Some(bar) = finder_bar(geometry, finder) {
        crate::scrollbar::paint_card(
            paint,
            &bar,
            crate::scrollbar::Surface::Palette,
            hovers,
            finder.scrolled_at(),
            1.0,
        );
    }
}

/// The corner radius of the field inside the card.
///
/// Concentric with the card (`delightful-ui` §15): the field hugs the card's
/// top-left corner with [`CARD_PAD`] of air, so its radius is the card's minus
/// that padding — and then the gap stays a constant width as it turns the
/// corner.
fn field_radius() -> u8 {
    chrome::CARD_RADIUS.saturating_sub(CARD_PAD as u8).max(2)
}

/// The width the kind glyph takes at the left of a row.
const ICON_COLUMN: f32 = 16.0;

// ── The search panel ────────────────────────────────────────────────────────

/// Where the search panel's pieces are, for one frame.
pub struct SearchGeom {
    pub card: Rect,
    pub field: Rect,
    /// The `×` at the field's right, in the card's corner.
    pub close: Option<Rect>,
    /// The **Names | Contents** switch at the head of the field: one plate per
    /// [`Mode::ALL`], in that order, side by side.
    pub switch: [Rect; 2],
    /// Where the hits go: the part of the panel under the field, as tall as
    /// the rows it has room for.
    pub body: Rect,
    pub rows: Vec<Rect>,
    /// The band the hits' bar is pointed at and taken by, while there are
    /// more hits than rows ([`crate::scrollbar::band`]).
    pub band: Option<Rect>,
}

impl SearchGeom {
    pub fn row_at(&self, pos: egui::Pos2) -> Option<usize> {
        self.rows.iter().position(|rect| rect.contains(pos))
    }

    /// Which half of the switch `pos` is on.
    ///
    /// Asked of each plate grown by [`SWITCH_INSET`] above and below, to the
    /// field's full height: the plates are inset inside the field, and a press
    /// in the sliver of field over one is aimed at it — the target is allowed
    /// to be bigger than what is drawn (`delightful-ui` §1). Not sideways,
    /// where the two halves touch and a grown one would take the other's edge.
    pub fn switch_at(&self, pos: egui::Pos2) -> Option<Mode> {
        Mode::ALL
            .into_iter()
            .zip(self.switch)
            .find(|(_, rect)| rect.expand2(egui::vec2(0.0, SWITCH_INSET)).contains(pos))
            .map(|(mode, _)| mode)
    }

    /// Where `mode`'s half of the switch is drawn, for the ripple to start in.
    pub fn switch_rect(&self, mode: Mode) -> Rect {
        match mode {
            Mode::Names => self.switch[0],
            Mode::Content => self.switch[1],
        }
    }

    /// How many rows fit — what the scrolloff rule is given.
    pub fn page(&self) -> usize {
        self.rows.len()
    }

    /// How tall one hit is in the mode laid out, which is what the wheel's
    /// travel is counted in ([`crate::mouse::wheel_rows`]).
    pub fn row_height(&self) -> f32 {
        self.rows.first().map_or(NAME_ROW, Rect::height)
    }
}

/// The pointer's name for one half of the switch — what its hover, its press
/// and its ripple are keyed on.
pub fn switch_control(mode: Mode) -> Control {
    match mode {
        Mode::Names => Control::SearchNames,
        Mode::Content => Control::SearchContents,
    }
}

/// How far the switch's plates sit inside the field, on every side they share
/// with it (`delightful-ui` §15's even insets): the top, the bottom, and the
/// field's left end.
///
/// The top row's chip inset, because a switch plate is the same kind of thing
/// — a pill lying in a bar — at the same scale.
const SWITCH_INSET: f32 = chrome::CHIP_INSET;

/// The plates' corner radius: **the field's less the inset**, so the gap
/// between the first plate and the field's own corner stays a constant width
/// as it turns (`delightful-ui` §15). Derived, never picked.
fn switch_radius() -> u8 {
    field_radius().saturating_sub(SWITCH_INSET as u8).max(2)
}

/// Lay the search panel out over the parent and list columns, leaving the
/// preview pane alone.
///
/// The `left`/`right` are the two columns' outer edges rather than the whole
/// window: the live preview is half of what this overlay is *for* (PLAN §7.2),
/// and covering or dimming it would be covering the answer.
///
/// `painter` measures the switch's two words, which is what its plates are
/// sized to: each half is as wide as its word plus a chip's padding, so
/// **Names** and **Contents** are not stretched to one width that would leave
/// the short word floating in the middle of a wide plate.
///
/// `hits` is how many the search has found, for the band its bar is pointed
/// at by, beside the rows and under the field — clear of the switch, which
/// heads the field.
pub fn search_geometry(
    painter: &egui::Painter,
    left: f32,
    right: f32,
    top: f32,
    bottom: f32,
    mode: Mode,
    hits: usize,
) -> SearchGeom {
    let card = Rect::from_min_max(egui::pos2(left, top), egui::pos2(right, bottom));
    let field = Rect::from_min_size(
        card.min + egui::vec2(CARD_PAD, CARD_PAD),
        egui::vec2(field_width(card), FIELD_ROW),
    );
    let mut x = field.left() + SWITCH_INSET;
    let switch = Mode::ALL.map(|half| {
        let width =
            chrome::text_width(painter, half.label(), FontId::proportional(FONT)) + PAD_X * 2.0;
        let rect = Rect::from_min_max(
            egui::pos2(x, field.top() + SWITCH_INSET),
            egui::pos2(x + width, field.bottom() - SWITCH_INSET),
        );
        x += width;
        rect
    });
    let row_height = match mode {
        Mode::Names => NAME_ROW,
        Mode::Content => CONTENT_ROW,
    };
    let body_top = field.bottom() + GAP;
    // The panel is as tall as the two panes it covers, so its hint strip comes
    // out of the rows rather than adding to the card.
    let count = crate::viewport::visible_rows(
        card.bottom() - CARD_PAD - chrome::HINT_ROW - body_top,
        row_height,
    );
    let body = Rect::from_min_size(
        egui::pos2(card.left() + CARD_PAD, body_top),
        egui::vec2(card.width() - CARD_PAD * 2.0, count as f32 * row_height),
    );
    let rows = (0..count)
        .map(|n| {
            Rect::from_min_size(
                egui::pos2(body.left(), body.top() + n as f32 * row_height),
                egui::vec2(body.width(), row_height),
            )
        })
        .collect();
    SearchGeom {
        card,
        field,
        close: Some(close_beside(card, field)),
        switch,
        body,
        rows,
        band: crate::scrollbar::band(card, body, count as f32, hits as f32),
    }
}

/// The panel's bar, beside its hits, while it shows fewer than it has.
pub fn search_bar(geometry: &SearchGeom, search: &Search) -> Option<crate::scrollbar::Geometry> {
    crate::scrollbar::card(
        geometry.card,
        geometry.body,
        search.first as f32,
        geometry.page() as f32,
        search.hits.len() as f32,
    )
}

/// Draw the `s` / `S` panel.
pub fn paint_search(
    paint: &Painting<'_>,
    geometry: &SearchGeom,
    search: &Search,
    hovers: &Hovers<Control>,
    ripples: &Ripples<Control>,
) {
    let (painter, palette) = (paint.painter, paint.palette);
    chrome::card(paint, geometry.card, 1.0);

    // ── The field ───────────────────────────────────────────────────────────
    let field = geometry.field;
    painter.rect_filled(field, field_radius(), palette.mantle);
    if let Some(close) = geometry.close {
        chrome::close_button(paint, close, hovers, ripples);
    }
    let baseline = field.center().y;
    paint_switch(paint, geometry, search.mode, hovers, ripples);
    let query_left = geometry.switch[1].right() + GAP;
    let query = search.query();
    painter.text(
        egui::pos2(query_left, baseline),
        Align2::LEFT_CENTER,
        if query.is_empty() {
            search.mode.placeholder()
        } else {
            query
        },
        FontId::proportional(FONT),
        if query.is_empty() {
            palette.faint
        } else {
            palette.text
        },
    );
    if !query.is_empty() {
        let caret_x = query_left
            + chrome::text_width(
                painter,
                &query[..search.buffer.cursor_byte().min(query.len())],
                FontId::proportional(FONT),
            );
        painter.rect_filled(
            Rect::from_min_size(
                egui::pos2(caret_x, field.top() + 6.0),
                egui::vec2(crate::chrome::CARET_WIDTH, field.height() - 12.0),
            ),
            0,
            palette.text,
        );
    }
    // The count, and whether it is still growing. "Searching…" rather than a
    // spinner: the number climbing *is* the progress indicator, and a spinner
    // beside a climbing number would be two of them.
    let hits = df_core::text::grouped(search.hits.len() as u64);
    let status = if let Some(error) = &search.error {
        error.clone()
    } else if search.searching() {
        format!("{hits} so far…")
    } else if search.capped {
        format!("first {hits}")
    } else {
        hits
    };
    painter.text(
        egui::pos2(field.right() - PAD_X, baseline),
        Align2::RIGHT_CENTER,
        status,
        FontId::proportional(FONT - 1.0),
        if search.error.is_some() {
            crate::theme::ink(palette, palette.red)
        } else {
            palette.faint
        },
    );

    // ── The results ─────────────────────────────────────────────────────────
    if search.hits.is_empty() {
        if let Some(rect) = geometry.rows.first() {
            let message = if search.error.is_some() {
                // The error is already in the field's status; down here goes
                // what to do about it.
                "Nothing to show. Esc to close."
            } else if search.query().is_empty() {
                "Type to search. Esc to close, Ctrl+s to stop."
            } else if search.searching() {
                "Searching…"
            } else {
                "Nothing matched. Backspace to widen the search."
            };
            painter.text(
                egui::pos2(rect.center().x, rect.center().y),
                Align2::CENTER_CENTER,
                message,
                FontId::proportional(FONT),
                palette.faint,
            );
        }
        return;
    }

    let painter = painter.with_clip_rect(geometry.card);
    for (offset, rect) in geometry.rows.iter().enumerate() {
        let index = search.first + offset;
        let Some(hit) = search.hits.get(index) else {
            break;
        };
        let key = Control::PanelRow(offset);
        let on_cursor = index == search.cursor;
        let hover = hovers.hover(key);
        let rect = pressed_rect(*rect, hovers.press(key));
        if on_cursor || hover > 0.0 {
            let ground = if on_cursor {
                palette.surface1
            } else {
                palette.crust
            };
            painter.rect_filled(rect, CARD_ROW_RADIUS, mix(ground, palette.surface0, hover));
        }
        let inside = painter.with_clip_rect(rect);
        for splash in ripples.splashes(key, paint.now) {
            inside.circle_filled(
                splash.center,
                splash.radius,
                crate::theme::splash(palette, splash.alpha),
            );
        }

        // The icon, from the same table the listing uses — a `.rs` in the
        // search results and a `.rs` in the column have to be the same glyph in
        // the same colour or the two panes disagree about what a file is.
        let (glyph, tint) = match &hit.entry {
            Some(entry) => {
                let icon = icon_for(entry, paint.theme, palette, paint.nerd);
                (icon.glyph.to_string(), icon.color)
            }
            None => (" ".to_string(), palette.faint),
        };
        let icon_family = if paint.nerd {
            egui::FontFamily::Name(crate::icons::ICON_FAMILY.into())
        } else {
            egui::FontFamily::Monospace
        };
        let text_left = rect.left() + PAD_X + ICON_COLUMN;
        match hit.line {
            None => {
                inside.text(
                    egui::pos2(rect.left() + PAD_X, rect.center().y),
                    Align2::LEFT_CENTER,
                    glyph,
                    FontId::new(FONT, icon_family),
                    tint,
                );
                path_text(
                    &inside,
                    egui::pos2(text_left, rect.center().y),
                    &hit.relative,
                    palette.text,
                    palette.quiet,
                    rect.right() - PAD_X - text_left,
                );
            }
            Some(number) => {
                // Two lines: where it is, then what it says.
                let top = rect.top() + rect.height() * 0.3;
                let bottom = rect.top() + rect.height() * 0.72;
                inside.text(
                    egui::pos2(rect.left() + PAD_X, top),
                    Align2::LEFT_CENTER,
                    glyph,
                    FontId::new(FONT, icon_family),
                    tint,
                );
                let used = path_text(
                    &inside,
                    egui::pos2(text_left, top),
                    &hit.relative,
                    palette.text,
                    palette.quiet,
                    HIT_PATH_COLUMN,
                );
                inside.text(
                    egui::pos2(text_left + used + GAP, top),
                    Align2::LEFT_CENTER,
                    format!(":{number}"),
                    key_font(FONT - 1.0),
                    crate::theme::ink(palette, palette.peach),
                );
                spans(
                    &inside,
                    egui::pos2(text_left, bottom),
                    hit.text.trim_end(),
                    palette.subtext0,
                    crate::theme::ink(palette, palette.yellow),
                    hit.span.as_slice(),
                    key_font(FONT - 0.5),
                    rect.right() - PAD_X - text_left,
                );
            }
        }
    }
    if let Some(bar) = search_bar(geometry, search) {
        crate::scrollbar::paint_card(
            paint,
            &bar,
            crate::scrollbar::Surface::Search,
            hovers,
            search.scrolled_at(),
            1.0,
        );
    }
}

/// The **Names | Contents** switch at the head of the search field.
///
/// The lit half is a plate in the colour the cursor's row wears on this card —
/// the card's one word for "this one" — with its word in full text colour. It
/// does not lift or press: it is where you already are, so the pointer treats
/// it as inert (see the app's hit handling), and a plate that answered a hover
/// would be promising a click that changes nothing.
///
/// The other half is a word and no plate until the pointer is on it. At rest
/// it has to read as the title's second word rather than as a second button
/// competing with the field; under the pointer it lifts instantly and fades
/// back out (`delightful-ui` §3), and presses like every other control.
///
/// The ripple is drawn on **both** halves. The press that switches the mode
/// lights the half it landed on in the same frame, so the splash it started
/// plays out on the plate it just lit — which is the acknowledgement landing
/// where the hand is — and a lit half never starts one of its own.
fn paint_switch(
    paint: &Painting<'_>,
    geometry: &SearchGeom,
    mode: Mode,
    hovers: &Hovers<Control>,
    ripples: &Ripples<Control>,
) {
    let (painter, palette) = (paint.painter, paint.palette);
    for half in Mode::ALL {
        let key = switch_control(half);
        let lit = half == mode;
        let (rect, ink) = if lit {
            let rect = geometry.switch_rect(half);
            painter.rect_filled(rect, switch_radius(), palette.surface1);
            (rect, palette.text)
        } else {
            let hover = hovers.hover(key);
            let rect = pressed_rect(geometry.switch_rect(half), hovers.press(key));
            if hover > 0.0 {
                painter.rect_filled(rect, switch_radius(), chrome::fade(palette.surface0, hover));
            }
            (rect, mix(palette.quiet, palette.text, hover))
        };
        let inside = painter.with_clip_rect(rect);
        for splash in ripples.splashes(key, paint.now) {
            inside.circle_filled(
                splash.center,
                splash.radius,
                crate::theme::splash(palette, splash.alpha),
            );
        }
        inside.text(
            rect.center(),
            Align2::CENTER_CENTER,
            half.label(),
            FontId::proportional(FONT),
            ink,
        );
    }
}

/// A path with its directory part dimmed and its last component bright.
///
/// The directory is context and the file name is the answer, and drawing them
/// the same weight makes every row a wall of grey text you have to read to the
/// end of. Returns how wide it came out, so a caller can put something after
/// it.
fn path_text(
    painter: &egui::Painter,
    pos: egui::Pos2,
    path: &str,
    bright: Color32,
    dim: Color32,
    max_width: f32,
) -> f32 {
    use egui::text::{LayoutJob, TextFormat, TextWrapping};
    let split = path.rfind('/').map(|at| at + 1).unwrap_or(0);
    let format = |color: Color32| TextFormat {
        font_id: FontId::proportional(FONT),
        color,
        ..Default::default()
    };
    let mut job = LayoutJob::default();
    if split > 0 {
        job.append(&path[..split], 0.0, format(dim));
    }
    job.append(&path[split..], 0.0, format(bright));
    job.wrap = TextWrapping {
        max_width,
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    let galley = painter.layout_job(job);
    let width = galley.size().x;
    painter.galley(
        egui::pos2(pos.x, pos.y - galley.size().y / 2.0),
        galley,
        bright,
    );
    width
}

/// Text with its matched runs in `highlight`.
///
/// The listing has one of these too ([`crate::ui::Painting`]'s `text_spans`),
/// pinned to the row font. This one takes the font, because an overlay draws
/// the same idea at three sizes — and a shared version parameterised over font,
/// colour, wrap and width would be a worse thing to read than two short
/// functions (`Duplication is fine`).
#[allow(clippy::too_many_arguments)]
fn spans(
    painter: &egui::Painter,
    pos: egui::Pos2,
    text: &str,
    color: Color32,
    highlight: Color32,
    runs: &[df_core::fs::Span],
    font: FontId,
    max_width: f32,
) {
    use egui::text::{LayoutJob, TextFormat, TextWrapping};
    let format = |color: Color32| TextFormat {
        font_id: font.clone(),
        color,
        ..Default::default()
    };
    let mut job = LayoutJob::default();
    let mut at = 0usize;
    for &(start, end) in runs {
        // Bounds-checked: the spans were computed against the text as it stood
        // and a streaming result could in principle have replaced it since.
        if start < at
            || end > text.len()
            || start >= end
            || !text.is_char_boundary(start)
            || !text.is_char_boundary(end)
        {
            continue;
        }
        if at < start {
            job.append(&text[at..start], 0.0, format(color));
        }
        job.append(&text[start..end], 0.0, format(highlight));
        at = end;
    }
    if at < text.len() {
        job.append(&text[at..], 0.0, format(color));
    }
    job.wrap = TextWrapping {
        max_width,
        max_rows: 1,
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    let line = crate::glyphs::layout(painter, job);
    let height = line.size().y;
    line.paint(painter, egui::pos2(pos.x, pos.y - height / 2.0), color);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Rect {
        Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, 900.0))
    }

    /// A frame's painter, for the geometry that measures words.
    fn with_painter(mut f: impl FnMut(&egui::Painter)) {
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(Default::default(), |ui| f(ui.painter()));
    }

    /// The card is centred horizontally and biased *above* true centre — the
    /// optical-centring rule, pinned so a later tidy-up cannot quietly drop it.
    #[test]
    fn the_card_sits_above_true_centre() {
        let geometry = finder_geometry(area(), 12);
        assert!((geometry.card.center().x - area().center().x).abs() < 0.01);
        assert!(
            geometry.card.center().y < area().center().y,
            "the card should be biased upwards"
        );
        let above = geometry.card.top() - area().top();
        let below = area().bottom() - geometry.card.bottom();
        assert!(
            below > above,
            "more space below than above: {above} / {below}"
        );
    }

    /// Every row is inside the card, none of them overlap, and they are in
    /// order — the invariant the hit test depends on.
    #[test]
    fn the_rows_tile_the_card_in_order() {
        let geometry = finder_geometry(area(), 12);
        assert_eq!(geometry.rows.len(), 12);
        for (n, rect) in geometry.rows.iter().enumerate() {
            assert!(
                geometry.card.contains_rect(*rect),
                "row {n} escaped the card"
            );
            assert_eq!(geometry.row_at(rect.center()), Some(n));
            if n > 0 {
                assert!(rect.top() >= geometry.rows[n - 1].bottom() - 0.01);
            }
        }
        assert!(geometry.row_at(egui::pos2(-10.0, -10.0)).is_none());
        // The field never overlaps the first row.
        assert!(geometry.field.bottom() <= geometry.rows[0].top() + 0.01);
    }

    /// An empty result list is a field and one line of guidance, not a field
    /// over eleven rows of nothing (`delightful-ui` §11).
    #[test]
    fn an_empty_list_makes_a_short_card() {
        let empty = finder_geometry(area(), 0);
        let full = finder_geometry(area(), 12);
        assert!(empty.card.height() < full.card.height());
        assert_eq!(empty.rows.len(), 0);
    }

    /// A short window gets the rows it has room for rather than rows drawn
    /// past the card's foot: the card inside the window, never no rows, and a
    /// bar and its band only while the list has more than the card shows. A
    /// tall window gets the twelve it always did.
    #[test]
    fn a_short_window_fits_the_palettes_rows() {
        use crate::finder::{Choice, Kind, Row, Source, ROWS};
        let row = |n: usize| Row {
            label: format!("Row {n}"),
            detail: String::new(),
            kind: Kind::Command,
            choice: Choice::Cd(std::path::PathBuf::from("/tmp")),
        };
        let many = Finder::new(Source::Commands, (0..40).map(row).collect());
        let few = Finder::new(Source::Commands, (0..3).map(row).collect());
        let short = Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, 300.0));

        let tall = finder_geometry(area(), 40);
        assert_eq!(tall.rows.len(), ROWS, "the old count");
        assert!(finder_bar(&tall, &many).is_some(), "forty hits in twelve");
        assert!(tall.band.is_some());
        let tall = finder_geometry(area(), 3);
        assert_eq!(finder_bar(&tall, &few), None, "three hits fit");
        assert_eq!(tall.band, None, "nothing to point at");

        let geometry = finder_geometry(short, 40);
        assert!(short.contains_rect(geometry.card), "{:?}", geometry.card);
        assert!(!geometry.rows.is_empty() && geometry.rows.len() < ROWS);
        assert!(geometry
            .rows
            .iter()
            .all(|rect| geometry.card.contains_rect(*rect)));
        assert!(finder_bar(&geometry, &many).is_some());
        let band = geometry.band.expect("the rows overflow");
        assert!(geometry.card.contains_rect(band));
        assert_eq!(band.right(), geometry.card.right(), "flush with the edge");
        assert_eq!(band.width(), crate::scrollbar::HIT_WIDTH);
        assert_eq!(finder_bar(&finder_geometry(short, 3), &few), None);

        // The cursor scrolls by the rows drawn, so it is never below the last.
        let mut finder = many;
        finder.move_cursor(20);
        finder.scroll_into_view(geometry.rows.len(), 0, std::time::Instant::now());
        assert!(finder.cursor < finder.first + geometry.rows.len());

        // …and a window too short for any row still gets one.
        let tiny = Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, 110.0));
        let geometry = finder_geometry(tiny, 40);
        assert_eq!(geometry.rows.len(), 1);
        assert!(tiny.contains_rect(geometry.card));
    }

    /// The bar comes and goes by the panes' rule and by nothing else: a card
    /// that overflows but has not scrolled, with nothing on its band, shows
    /// no bar however long the pointer rests elsewhere on the card; a scroll
    /// or the band's hover brings it up.
    #[test]
    fn the_palettes_bar_shows_by_the_panes_rule() {
        use crate::finder::{Choice, Kind, Row, Source};
        use crate::scrollbar::{visibility, Surface};
        let rows = (0..40)
            .map(|n| Row {
                label: format!("Row {n}"),
                detail: String::new(),
                kind: Kind::Command,
                choice: Choice::Cd(std::path::PathBuf::from("/tmp")),
            })
            .collect();
        let mut finder = Finder::new(Source::Commands, rows);
        let geometry = finder_geometry(area(), finder.hits.len());
        assert!(finder_bar(&geometry, &finder).is_some());
        let now = std::time::Instant::now();
        finder.scroll_into_view(geometry.rows.len(), 0, now);
        let mut hovers: Hovers<Control> = Hovers::new();
        hovers.tick(Some(Control::PanelRow(3)), None, now);
        let lit = hovers.hover(Control::Bar(crate::scrollbar::Bar::Card(Surface::Palette)));
        assert_eq!(lit, 0.0, "the pointer is on a row, not the band");
        assert_eq!(visibility(finder.scrolled_at(), lit, false, now), 0.0);

        hovers.tick(
            Some(Control::Bar(crate::scrollbar::Bar::Card(Surface::Palette))),
            None,
            now,
        );
        let lit = hovers.hover(Control::Bar(crate::scrollbar::Bar::Card(Surface::Palette)));
        assert_eq!(visibility(finder.scrolled_at(), lit, false, now), 1.0);

        finder.move_cursor(30);
        finder.scroll_into_view(geometry.rows.len(), 0, now);
        assert_eq!(visibility(finder.scrolled_at(), 0.0, false, now), 1.0);
    }

    /// The field's radius is derived from the card's, not picked — the
    /// concentric rule.
    #[test]
    fn the_field_is_concentric_with_the_card() {
        assert_eq!(
            u32::from(chrome::CARD_RADIUS),
            u32::from(field_radius()) + CARD_PAD as u32
        );
    }

    /// The search panel covers the two left columns and stops at the preview's
    /// edge — the whole reason it is a panel and not a card.
    #[test]
    fn the_search_panel_leaves_the_preview_pane_alone() {
        with_painter(|painter| {
            let geometry = search_geometry(painter, 0.0, 800.0, 30.0, 870.0, Mode::Content, 0);
            assert_eq!(geometry.card.right(), 800.0);
            assert!(geometry.page() > 0);
            for (n, rect) in geometry.rows.iter().enumerate() {
                assert!(geometry.card.contains_rect(*rect), "row {n} escaped");
                assert_eq!(geometry.row_at(rect.center()), Some(n));
            }
            // A content row is taller than a name row, because it carries two
            // lines rather than one.
            let names = search_geometry(painter, 0.0, 800.0, 30.0, 870.0, Mode::Names, 0);
            assert!(names.rows[0].height() < geometry.rows[0].height());
            assert!(names.page() > geometry.page());
        });
    }

    /// The panel's bar sits in its right padding beside the hits, below the
    /// field — clear of the switch, the query and the `×` — and only while
    /// there are more hits than rows.
    #[test]
    fn the_search_panels_bar_is_beside_its_hits_and_clear_of_the_switch() {
        with_painter(|painter| {
            let fits = search_geometry(painter, 0.0, 800.0, 30.0, 870.0, Mode::Names, 5);
            assert!(fits.page() > 5);
            assert_eq!(fits.band, None, "five hits have a band");

            let hits = 200;
            let geometry = search_geometry(painter, 0.0, 800.0, 30.0, 870.0, Mode::Names, hits);
            let band = geometry.band.expect("two hundred hits overflow");
            let close = geometry.close.expect("a ×");
            for rect in geometry.switch.into_iter().chain([geometry.field, close]) {
                assert!(!band.intersects(rect), "the band is over {rect:?}");
            }
            assert_eq!(band.right(), geometry.card.right(), "flush with the edge");
            assert!(band.top() >= geometry.body.top() && band.bottom() <= geometry.body.bottom());
            let notify: df_core::fs::Notifier = std::sync::Arc::new(|| {});
            let mut search = Search::new(Mode::Names, "/tmp", false, notify);
            search.hits = (0..hits)
                .map(|n| crate::search::Hit {
                    path: format!("/tmp/{n}").into(),
                    relative: n.to_string(),
                    entry: None,
                    line: None,
                    text: String::new(),
                    span: None,
                })
                .collect();
            let bar = search_bar(&geometry, &search).expect("the hits overflow");
            assert_eq!(bar.hit, band, "the band the frame hit-tests is the bar's");
            assert!(bar.thumb.left() >= geometry.body.right(), "over the hits");
            assert!(geometry.card.contains_rect(bar.thumb));
        });
    }

    /// The **Names | Contents** switch: two plates inside the field at its
    /// left end, evenly inset, rounded concentric with it, each sized to its
    /// word — and each half answers the pointer as itself, including in the
    /// sliver of field above and below its plate.
    #[test]
    fn the_switch_heads_the_field_and_each_half_is_its_own_target() {
        with_painter(|painter| {
            let geometry = search_geometry(painter, 0.0, 800.0, 30.0, 870.0, Mode::Names, 0);
            let [names, contents] = geometry.switch;
            let field = geometry.field;
            for rect in [names, contents] {
                assert!(field.contains_rect(rect), "{rect:?} is outside the field");
            }
            // Even insets on the three sides the switch shares with the field.
            assert!((names.left() - field.left() - SWITCH_INSET).abs() < 0.01);
            assert!((names.top() - field.top() - SWITCH_INSET).abs() < 0.01);
            assert!((field.bottom() - names.bottom() - SWITCH_INSET).abs() < 0.01);
            // Concentric with the field's corner.
            assert_eq!(
                u32::from(field_radius()),
                u32::from(switch_radius()) + SWITCH_INSET as u32
            );
            // Side by side, in the order `Mode::ALL` gives, sized to the words.
            assert!((names.right() - contents.left()).abs() < 0.01);
            assert!(
                contents.width() > names.width(),
                "Contents is the longer word"
            );

            assert_eq!(geometry.switch_at(names.center()), Some(Mode::Names));
            assert_eq!(geometry.switch_at(contents.center()), Some(Mode::Content));
            // The target is the field's full height, not just the plate.
            let above = egui::pos2(contents.center().x, field.top() + 1.0);
            assert_eq!(geometry.switch_at(above), Some(Mode::Content));
            // The query's side of the field is not the switch.
            let query = egui::pos2(contents.right() + GAP * 2.0, field.center().y);
            assert_eq!(geometry.switch_at(query), None);
            assert_eq!(geometry.switch_rect(Mode::Content), contents);
            assert_eq!(switch_control(Mode::Names), Control::SearchNames);
            assert_eq!(switch_control(Mode::Content), Control::SearchContents);
        });
    }

    /// Both surfaces lay out and paint, at a comfortable window size and at one
    /// too small for either of them, in every state they have — including the
    /// empty ones, which is where an overlay usually panics because nobody drew
    /// it that way while building it.
    #[test]
    fn both_overlays_paint_in_every_state_without_panicking() {
        use crate::finder::{Choice, Kind, Row};
        use crate::search::Hit;
        use std::path::PathBuf;

        let now = std::time::Instant::now();
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
                now,
            };
            let (hovers, ripples) = (Hovers::new(), Ripples::new());
            let rows: Vec<Row> = (0..40)
                .map(|n| Row {
                    label: format!("Do the thing number {n}"),
                    detail: format!("Ctrl+{n}"),
                    kind: [Kind::Command, Kind::Place, Kind::Tab, Kind::View][n % 4],
                    choice: Choice::Cd(PathBuf::from("/tmp")),
                })
                .collect();
            for area in [
                Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, 900.0)),
                // A window too small for the card at all.
                Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(120.0, 90.0)),
            ] {
                for pool in [rows.clone(), Vec::new()] {
                    let mut finder = Finder::new(crate::finder::Source::Commands, pool);
                    for query in ["", "thing", "no-such-row-anywhere"] {
                        let _ = finder.buffer.insert_text(query);
                        finder.requery();
                        let geometry = finder_geometry(area, finder.hits.len());
                        paint_finder(&paint, &geometry, &finder, &hovers, &ripples);
                    }
                }

                for mode in [Mode::Names, Mode::Content] {
                    let notify: df_core::fs::Notifier = std::sync::Arc::new(|| {});
                    let mut search = crate::search::Search::new(mode, "/tmp", false, notify);
                    let geometry = search_geometry(
                        ui.painter(),
                        area.left(),
                        area.right(),
                        area.top(),
                        area.bottom(),
                        mode,
                        30,
                    );
                    // Empty, then failed, then full — every state the panel has.
                    paint_search(&paint, &geometry, &search, &hovers, &ripples);
                    search.error = Some("rg is not installed".to_string());
                    paint_search(&paint, &geometry, &search, &hovers, &ripples);
                    search.error = None;
                    search.capped = true;
                    search.hits = (0..30)
                        .map(|n| Hit {
                            path: PathBuf::from(format!("/tmp/deep/nested/file-{n}.rs")),
                            relative: format!("deep/nested/file-{n}.rs"),
                            entry: None,
                            line: (mode == Mode::Content).then_some(n + 1),
                            text: "    let needle = 3; // and a much longer tail".to_string(),
                            span: (mode == Mode::Content).then_some((8, 14)),
                        })
                        .collect();
                    search.cursor = 5;
                    paint_search(&paint, &geometry, &search, &hovers, &ripples);
                }
            }
        });
    }

    /// A panel too short for a single row must not produce a negative count on
    /// its way to saying so.
    #[test]
    fn a_panel_with_no_room_has_no_rows() {
        with_painter(|painter| {
            let geometry = search_geometry(painter, 0.0, 400.0, 0.0, 30.0, Mode::Names, 0);
            assert_eq!(geometry.page(), 0);
            assert!(geometry.row_at(egui::pos2(10.0, 10.0)).is_none());
        });
    }
}
