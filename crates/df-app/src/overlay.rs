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
    /// One rectangle per visible row, in the order they are drawn.
    pub rows: Vec<Rect>,
}

impl FinderGeom {
    pub fn row_at(&self, pos: egui::Pos2) -> Option<usize> {
        self.rows.iter().position(|rect| rect.contains(pos))
    }
}

/// Lay the fuzzy card out for a list that will show `shown` rows.
///
/// The card is exactly as tall as it has to be — an empty result list is a
/// field and one line of guidance, not a field over eleven rows of nothing
/// (`delightful-ui` §11).
pub fn finder_geometry(area: Rect, shown: usize) -> FinderGeom {
    let width = (area.width() - chrome::CARD_MARGIN * 2.0).min(CARD_WIDTH);
    // One row's worth of height for the empty state's sentence, so the card
    // does not collapse to a bare field when a query matches nothing.
    let body = (shown.max(1) as f32) * CARD_ROW;
    // …plus the strip its own hints go in (PLAN §4).
    let height = CARD_PAD * 2.0 + FIELD_ROW + GAP + body + chrome::HINT_ROW;
    let height = height.min(area.height() - chrome::CARD_MARGIN * 2.0);
    let top = area.top() + (area.height() - height) * crate::chrome::OPTICAL_CENTRE;
    let card = Rect::from_min_size(
        egui::pos2(area.center().x - width / 2.0, top),
        egui::vec2(width, height),
    );
    let field = Rect::from_min_size(
        card.min + egui::vec2(CARD_PAD, CARD_PAD),
        egui::vec2(width - CARD_PAD * 2.0, FIELD_ROW),
    );
    let rows = (0..shown)
        .map(|n| {
            Rect::from_min_size(
                egui::pos2(
                    card.left() + CARD_PAD,
                    field.bottom() + GAP + n as f32 * CARD_ROW,
                ),
                egui::vec2(width - CARD_PAD * 2.0, CARD_ROW),
            )
        })
        .collect();
    FinderGeom { card, field, rows }
}

/// Draw the command palette / jump card.
pub fn paint_finder(
    paint: &Painting<'_>,
    area: Rect,
    geometry: &FinderGeom,
    finder: &Finder,
    hovers: &Hovers<Control>,
    ripples: &Ripples<Control>,
) {
    let (painter, palette) = (paint.painter, paint.palette);
    painter.rect_filled(area, 0, Color32::from_black_alpha(chrome::HELP_SCRIM));
    chrome::card(paint, geometry.card, 1.0);

    // ── The field ───────────────────────────────────────────────────────────
    let field = geometry.field;
    painter.rect_filled(field, field_radius(), palette.mantle);
    let text_left = field.left() + PAD_X;
    let baseline = field.center().y;
    painter.text(
        egui::pos2(text_left, baseline),
        Align2::LEFT_CENTER,
        finder.source.title(),
        FontId::proportional(FONT),
        palette.overlay0,
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
            palette.overlay0,
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
            palette.overlay0,
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
                palette.overlay0,
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
                Color32::from_white_alpha((splash.alpha * 255.0).round() as u8),
            );
        }

        // A glyph saying what kind of thing this is, in the colour that kind
        // already wears everywhere else in the program.
        let (glyph, tint) = match row.kind {
            Kind::Command => ("›", palette.overlay1),
            Kind::Place => ("/", palette.blue),
            Kind::Tab => ("▣", palette.teal),
            Kind::View => ("▦", palette.mauve),
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
            palette.overlay0,
            palette.sky,
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
            palette.sky,
            &hit.label,
            FontId::proportional(FONT),
            (rect.right() - PAD_X - detail_width - GAP - label_left).max(0.0),
        );
    }
}

/// The corner radius of the field inside the card.
///
/// Concentric with the card (`delightful-ui` §15): the field hugs the card's
/// top corners with [`CARD_PAD`] of air, so its radius is the card's minus that
/// padding — and then the gap stays a constant width as it turns the corner.
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
    pub rows: Vec<Rect>,
}

impl SearchGeom {
    pub fn row_at(&self, pos: egui::Pos2) -> Option<usize> {
        self.rows.iter().position(|rect| rect.contains(pos))
    }

    /// How many rows fit — what the scrolloff rule is given.
    pub fn page(&self) -> usize {
        self.rows.len()
    }
}

/// Lay the search panel out over the parent and list columns, leaving the
/// preview pane alone.
///
/// The `left`/`right` are the two columns' outer edges rather than the whole
/// window: the live preview is half of what this overlay is *for* (PLAN §7.2),
/// and covering or dimming it would be covering the answer.
pub fn search_geometry(left: f32, right: f32, top: f32, bottom: f32, mode: Mode) -> SearchGeom {
    let card = Rect::from_min_max(egui::pos2(left, top), egui::pos2(right, bottom));
    let field = Rect::from_min_size(
        card.min + egui::vec2(CARD_PAD, CARD_PAD),
        egui::vec2(card.width() - CARD_PAD * 2.0, FIELD_ROW),
    );
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
    let rows = (0..count)
        .map(|n| {
            Rect::from_min_size(
                egui::pos2(card.left() + CARD_PAD, body_top + n as f32 * row_height),
                egui::vec2(card.width() - CARD_PAD * 2.0, row_height),
            )
        })
        .collect();
    SearchGeom { card, field, rows }
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
    let baseline = field.center().y;
    let text_left = field.left() + PAD_X;
    painter.text(
        egui::pos2(text_left, baseline),
        Align2::LEFT_CENTER,
        search.mode.title(),
        FontId::proportional(FONT),
        palette.overlay0,
    );
    let title_width = chrome::text_width(painter, search.mode.title(), FontId::proportional(FONT));
    let query_left = text_left + title_width + GAP;
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
            palette.overlay0
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
    let status = if let Some(error) = &search.error {
        error.clone()
    } else if search.searching() {
        format!("{} so far…", search.hits.len())
    } else if search.capped {
        format!("first {}", search.hits.len())
    } else {
        format!("{}", search.hits.len())
    };
    painter.text(
        egui::pos2(field.right() - PAD_X, baseline),
        Align2::RIGHT_CENTER,
        status,
        FontId::proportional(FONT - 1.0),
        if search.error.is_some() {
            palette.red
        } else {
            palette.overlay0
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
                palette.overlay0,
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
                Color32::from_white_alpha((splash.alpha * 255.0).round() as u8),
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
            None => (" ".to_string(), palette.overlay0),
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
                    palette.overlay1,
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
                    palette.overlay1,
                    HIT_PATH_COLUMN,
                );
                inside.text(
                    egui::pos2(text_left + used + GAP, top),
                    Align2::LEFT_CENTER,
                    format!(":{number}"),
                    key_font(FONT - 1.0),
                    palette.peach,
                );
                spans(
                    &inside,
                    egui::pos2(text_left, bottom),
                    hit.text.trim_end(),
                    palette.subtext0,
                    palette.yellow,
                    hit.span.as_slice(),
                    key_font(FONT - 0.5),
                    rect.right() - PAD_X - text_left,
                );
            }
        }
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
    let galley = painter.layout_job(job);
    painter.galley(
        egui::pos2(pos.x, pos.y - galley.size().y / 2.0),
        galley,
        color,
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Rect {
        Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, 900.0))
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
        let geometry = search_geometry(0.0, 800.0, 30.0, 870.0, Mode::Content);
        assert_eq!(geometry.card.right(), 800.0);
        assert!(geometry.page() > 0);
        for (n, rect) in geometry.rows.iter().enumerate() {
            assert!(geometry.card.contains_rect(*rect), "row {n} escaped");
            assert_eq!(geometry.row_at(rect.center()), Some(n));
        }
        // A content row is taller than a name row, because it carries two
        // lines rather than one.
        let names = search_geometry(0.0, 800.0, 30.0, 870.0, Mode::Names);
        assert!(names.rows[0].height() < geometry.rows[0].height());
        assert!(names.page() > geometry.page());
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
                painter: ui.painter(),
                palette: &palette,
                theme: &theme,
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
                        let shown = finder.hits.len().min(crate::finder::ROWS);
                        let geometry = finder_geometry(area, shown);
                        paint_finder(&paint, area, &geometry, &finder, &hovers, &ripples);
                    }
                }

                for mode in [Mode::Names, Mode::Content] {
                    let notify: df_core::fs::Notifier = std::sync::Arc::new(|| {});
                    let mut search = crate::search::Search::new(mode, "/tmp", false, notify);
                    let geometry =
                        search_geometry(area.left(), area.right(), area.top(), area.bottom(), mode);
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
        let geometry = search_geometry(0.0, 400.0, 0.0, 30.0, Mode::Names);
        assert_eq!(geometry.page(), 0);
        assert!(geometry.row_at(egui::pos2(10.0, 10.0)).is_none());
    }
}
