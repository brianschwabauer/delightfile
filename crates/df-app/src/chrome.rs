//! The chrome around the panes: the tab strip, the bottom bar, the which-key
//! card and the help browser.
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

use df_core::fs::is_case_sensitive;

use crate::help::{Help, HelpLine};
use crate::hover::{pressed_rect, Hovers};
use crate::input::Prompt;
use crate::ripple::Ripples;
use crate::theme::mix;
use crate::ui::{Control, Painting, CHROME_HEIGHT, GAP, ROW_RADIUS};

/// The padding inside a card, and the inset its rows get on every adjacent
/// side (`delightful-ui` §15's even insets).
pub const CARD_PAD: f32 = 10.0;

/// A row inside a card rounds like a row inside a pane: they are the same kind
/// of thing at the same size, and two radii for one shape would be two.
const CARD_ROW_RADIUS: u8 = ROW_RADIUS;

/// The floating card's corner radius: **its row's plus the padding**, so the
/// gap around a highlighted row stays a constant width as it turns the card's
/// corner. Derived, never picked.
pub const CARD_RADIUS: u8 = CARD_ROW_RADIUS + CARD_PAD as u8;

/// How far a floating card keeps off the window's edge, and off the chrome it
/// sits above.
const CARD_MARGIN: f32 = 16.0;

/// How opaque a card's plate is, 0–255.
///
/// Not quite opaque: a hint of the pane beneath is what says the card is a
/// temporary thing lying on top rather than a region of the window that has
/// changed. Below about 220 the file names underneath start showing through the
/// text on the card, which is where "layered" turns into "unreadable".
const CARD_ALPHA: u8 = 242;

/// Body text on the chrome, in logical points. A shade under the pane's row
/// text: this is the frame, not the content.
const FONT: f32 = 12.5;

/// A line of a card's list.
const CARD_ROW: f32 = 20.0;

/// Horizontal padding inside a bar or a chip.
const PAD_X: f32 = 8.0;

// ── Tab strip (PLAN §2) ─────────────────────────────────────────────────────

/// The gap between two tab chips. Half the window's [`GAP`]: the chips are one
/// group and should read as one, so they sit closer to each other than the
/// strip does to the panes below it.
const TAB_GAP: f32 = 4.0;

/// The widest a tab chip gets, in logical points.
///
/// Directory names are short and a strip of nine equal chips across a 1400 pt
/// window would give each one 150 pt of mostly empty plate. Capping the width
/// keeps two tabs looking like two tabs rather than like a segmented control
/// that has taken over the top of the window.
const TAB_MAX_WIDTH: f32 = 190.0;

/// How far an inactive chip's plate is lifted off the window ground. Small:
/// the strip's job is to show *which* tab is active, so the inactive ones are
/// nearly the ground itself.
const TAB_INACTIVE_LIFT: f32 = 0.5;

/// Where each tab's chip goes.
///
/// Shared by the paint and the hit test, so a click lands on the chip it looks
/// like it landed on — two functions computing this separately is how a strip
/// grows a one-pixel lie at its edges.
pub fn tab_rects(strip: egui::Rect, count: usize) -> Vec<egui::Rect> {
    if count == 0 {
        return Vec::new();
    }
    let total_gap = TAB_GAP * (count - 1) as f32;
    let width = ((strip.width() - total_gap) / count as f32).min(TAB_MAX_WIDTH);
    (0..count)
        .map(|i| {
            let left = strip.left() + i as f32 * (width + TAB_GAP);
            egui::Rect::from_min_size(
                egui::pos2(left, strip.top()),
                egui::vec2(width.max(0.0), strip.height()),
            )
        })
        .collect()
}

/// Which tab chip a point is over, if any.
pub fn tab_at(strip: egui::Rect, count: usize, pos: egui::Pos2) -> Option<usize> {
    tab_rects(strip, count)
        .into_iter()
        .position(|rect| rect.contains(pos))
}

/// Draw the strip. Only called with two or more tabs (PLAN §2).
pub fn tab_strip(
    paint: &Painting<'_>,
    strip: egui::Rect,
    titles: &[String],
    active: usize,
    hovers: &Hovers<Control>,
    ripples: &Ripples<Control>,
) {
    let palette = paint.palette;
    for (index, (rect, title)) in tab_rects(strip, titles.len())
        .into_iter()
        .zip(titles)
        .enumerate()
    {
        let key = Control::Tab(index);
        let is_active = index == active;
        let hover = hovers.hover(key);
        let ground = mix(palette.crust, palette.surface0, TAB_INACTIVE_LIFT);
        let fill = if is_active {
            // The active chip is the *pane's* ground, so the tab and the column
            // it belongs to are visibly one surface.
            mix(palette.base, palette.blue, 0.10)
        } else {
            mix(ground, palette.surface1, hover)
        };
        let rect = pressed_rect(rect, hovers.press(key));
        paint.painter.rect_filled(rect, ROW_RADIUS, fill);
        if is_active {
            // The same 2 pt accent rule the focused pane wears (PLAN §2.1), on
            // the same edge, because it is saying the same thing.
            let rule = egui::Rect::from_min_max(
                egui::pos2(rect.left() + ROW_RADIUS as f32, rect.top()),
                egui::pos2(rect.right() - ROW_RADIUS as f32, rect.top() + 2.0),
            );
            paint.painter.rect_filled(rule, 1, palette.blue);
        }

        let inside = paint.painter.with_clip_rect(rect);
        for splash in ripples.splashes(key, paint.now) {
            inside.circle_filled(
                splash.center,
                splash.radius,
                egui::Color32::from_white_alpha((splash.alpha * 255.0).round() as u8),
            );
        }
        let color = if is_active {
            palette.text
        } else {
            palette.overlay1
        };
        // The number is what `1`–`9` press, so it is on the chip rather than in
        // the help sheet: the strip teaches its own shortcut.
        inside.text(
            egui::pos2(rect.left() + PAD_X, rect.center().y),
            egui::Align2::LEFT_CENTER,
            format!("{}", index + 1),
            key_font(FONT - 1.0),
            palette.overlay0,
        );
        let text_left = rect.left() + PAD_X + FONT;
        truncated(
            &inside,
            egui::pos2(text_left, rect.center().y),
            title,
            color,
            (rect.right() - PAD_X - text_left).max(0.0),
        );
    }
}

// ── The bottom bar ──────────────────────────────────────────────────────────

/// What the status line has to say when nothing is being typed.
pub struct Status<'a> {
    /// How many files are selected (PLAN §4.1's `Space`/`Ctrl+a`/`v`).
    pub selected: usize,
    /// Where the cursor is, 1-based, and how many rows there are.
    pub position: usize,
    pub rows: usize,
    /// The live `f` query, if one is committed.
    pub filter: &'a str,
    /// Visual mode, and which kind — the one piece of modal state in the
    /// browser, so it has to be visible somewhere that is not the row colours.
    pub visual: Option<bool>,
}

/// The bar's ground: the window's own, so the bar reads as part of the frame
/// rather than as a fourth pane.
fn bar_ground(paint: &Painting<'_>, rect: egui::Rect) -> egui::Rect {
    paint
        .painter
        .rect_filled(rect, ROW_RADIUS, mix(paint.palette.crust, paint.palette.base, 0.5));
    rect.shrink2(egui::vec2(PAD_X, 0.0))
}

/// The resting bottom line: the selection count, the mode, the filter, and
/// where the cursor is.
pub fn status_bar(paint: &Painting<'_>, rect: egui::Rect, status: Status<'_>) {
    let palette = paint.palette;
    let inner = bar_ground(paint, rect);

    let mut chips: Vec<(String, egui::Color32)> = Vec::new();
    if status.selected > 0 {
        // The count is in the selection's own colour, on a plate of it: the
        // badge and the yellow bars down the column are visibly the same fact,
        // said twice, in the two places the eye looks.
        chips.push((format!("{} selected", status.selected), palette.yellow));
    }
    if let Some(selecting) = status.visual {
        // Visual mode is the browser's one piece of modal state, and a mode you
        // cannot see is a mode you get caught in.
        let text = if selecting { "visual" } else { "visual unset" };
        chips.push((text.to_string(), palette.sky));
    }
    if !status.filter.is_empty() {
        chips.push((format!("filter: {}", status.filter), palette.blue));
    }
    let mut left = inner.left();
    for (text, accent) in &chips {
        left = chip(paint, inner, left, text, *accent);
    }

    // `12 / 340`, right-aligned: the one number that is always true, in the one
    // place it can be found without reading.
    let counter = if status.rows == 0 {
        "0 / 0".to_string()
    } else {
        format!("{} / {}", status.position, status.rows)
    };
    paint.painter.text(
        egui::pos2(inner.right(), inner.center().y),
        egui::Align2::RIGHT_CENTER,
        counter,
        egui::FontId::proportional(FONT),
        palette.overlay1,
    );
}

/// One labelled pill on the bar. Returns where the next one starts.
fn chip(
    paint: &Painting<'_>,
    inner: egui::Rect,
    left: f32,
    text: &str,
    accent: egui::Color32,
) -> f32 {
    let galley = paint.painter.layout_no_wrap(
        text.to_string(),
        egui::FontId::proportional(FONT),
        accent,
    );
    let width = galley.size().x + PAD_X * 2.0;
    let rect = egui::Rect::from_min_size(
        egui::pos2(left, inner.top() + 3.0),
        egui::vec2(width, inner.height() - 6.0),
    );
    paint
        .painter
        .rect_filled(rect, ROW_RADIUS, mix(paint.palette.crust, accent, 0.16));
    paint.painter.galley(
        egui::pos2(rect.left() + PAD_X, rect.center().y - galley.size().y / 2.0),
        galley,
        accent,
    );
    rect.right() + GAP
}

/// The bar while something is being typed into it: `f`, `/`, `?`, or the help
/// browser's own filter.
pub fn input_bar(paint: &Painting<'_>, rect: egui::Rect, prompt: &Prompt) {
    let palette = paint.palette;
    let inner = bar_ground(paint, rect);
    // The accent rule says the keyboard is *here* and not in the list — the
    // same 2 pt mark a focused pane wears, for the same reason (PLAN §2.1).
    paint.painter.rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(rect.left() + ROW_RADIUS as f32, rect.top()),
            egui::pos2(rect.right() - ROW_RADIUS as f32, rect.top() + 2.0),
        ),
        1,
        palette.blue,
    );

    let title = prompt.kind.title();
    let title_galley =
        paint
            .painter
            .layout_no_wrap(title.to_string(), egui::FontId::proportional(FONT), palette.blue);
    paint.painter.galley(
        egui::pos2(inner.left(), inner.center().y - title_galley.size().y / 2.0),
        title_galley.clone(),
        palette.blue,
    );

    // The smart-case indicator: lit when the query has a capital in it and is
    // therefore case-*sensitive* (df-core's rule, PLAN §7.2). Dim the rest of
    // the time — it reports a mode nobody chose, so it must not shout.
    let (case_color, case_text) = if is_case_sensitive(prompt.query()) {
        (palette.yellow, "Aa")
    } else {
        (palette.overlay0, "aa")
    };
    let case_galley = paint.painter.layout_no_wrap(
        case_text.to_string(),
        key_font(FONT - 0.5),
        case_color,
    );
    let case_width = case_galley.size().x;
    paint.painter.galley(
        egui::pos2(
            inner.right() - case_width,
            inner.center().y - case_galley.size().y / 2.0,
        ),
        case_galley,
        case_color,
    );

    let text_left = inner.left() + title_galley.size().x + PAD_X;
    let room = (inner.right() - case_width - PAD_X - text_left).max(0.0);
    let painter = paint.painter.with_clip_rect(egui::Rect::from_min_max(
        egui::pos2(text_left, inner.top()),
        egui::pos2(text_left + room, inner.bottom()),
    ));
    let query = prompt.query();
    painter.text(
        egui::pos2(text_left, inner.center().y),
        egui::Align2::LEFT_CENTER,
        query,
        egui::FontId::proportional(FONT),
        palette.text,
    );

    // The caret. A solid bar and **not a blinking one** — PLAN §4.2 says no
    // cursor blink, and a blink is an animation that never stops asking for
    // frames (PLAN §1).
    let before = painter
        .layout_no_wrap(
            query[..prompt.line.caret().min(query.len())].to_string(),
            egui::FontId::proportional(FONT),
            palette.text,
        )
        .size()
        .x;
    let caret_x = text_left + before;
    painter.rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(caret_x, inner.top() + 5.0),
            egui::pos2(caret_x + 1.5, inner.bottom() - 5.0),
        ),
        0,
        palette.blue,
    );
}

/// The bar while an overlay owns the keyboard: what the keys do now.
pub fn hint_bar(paint: &Painting<'_>, rect: egui::Rect, hints: &[(&str, &str)]) {
    let inner = bar_ground(paint, rect);
    let mut x = inner.left();
    for (keys, what) in hints {
        let key_galley =
            paint
                .painter
                .layout_no_wrap(keys.to_string(), key_font(FONT - 0.5), paint.palette.text);
        paint.painter.galley(
            egui::pos2(x, inner.center().y - key_galley.size().y / 2.0),
            key_galley.clone(),
            paint.palette.text,
        );
        x += key_galley.size().x + 6.0;
        let what_galley = paint.painter.layout_no_wrap(
            what.to_string(),
            egui::FontId::proportional(FONT),
            paint.palette.overlay1,
        );
        paint.painter.galley(
            egui::pos2(x, inner.center().y - what_galley.size().y / 2.0),
            what_galley.clone(),
            paint.palette.overlay1,
        );
        x += what_galley.size().x + GAP * 2.0;
    }
}

// ── The which-key card (PLAN §4, §8) ────────────────────────────────────────

/// The most continuations one column shows before the card grows a second one.
///
/// The `g` chord has a dozen bookmarks and the `,` chord thirteen sorts; a
/// single column of those is a tower up the middle of the window that the eye
/// has to scan end to end. Nine is about the length a list is still taken in at
/// a glance rather than read.
const WHICH_KEY_COLUMN: usize = 9;

/// Between a key and what it does.
const WHICH_KEY_GAP: f32 = 14.0;

/// Between two columns of the card. Wider than the key/label gap by enough that
/// the columns are unambiguously separate groups.
const WHICH_KEY_COL_SEP: f32 = 26.0;

/// Draw the card listing what could finish the pending chord.
///
/// `alpha` is [`crate::whichkey::WhichKey::alpha`] — 1 while the card is up, and
/// its fade on the way out. `bottom` is what the card sits above: the bar, so
/// the card never covers the thing it is a hint about.
pub fn which_key(
    paint: &Painting<'_>,
    area: egui::Rect,
    bottom: f32,
    rows: &[(String, String)],
    alpha: f32,
) {
    if rows.is_empty() || alpha <= 0.0 {
        return;
    }
    let painter = paint.painter;
    let columns: Vec<&[(String, String)]> = rows.chunks(WHICH_KEY_COLUMN).collect();
    let measure = |group: &[(String, String)]| {
        let key_w = group.iter().fold(0.0f32, |m, (key, _)| {
            m.max(text_width(painter, key, key_font(FONT)))
        });
        let label_w = group.iter().fold(0.0f32, |m, (_, label)| {
            m.max(text_width(painter, label, egui::FontId::proportional(FONT)))
        });
        (key_w, key_w + WHICH_KEY_GAP + label_w)
    };
    let widths: Vec<(f32, f32)> = columns.iter().map(|g| measure(g)).collect();
    let tall = columns.iter().map(|g| g.len()).max().unwrap_or(0);
    let size = egui::vec2(
        widths.iter().map(|(_, w)| w).sum::<f32>()
            + WHICH_KEY_COL_SEP * (columns.len().saturating_sub(1)) as f32
            + CARD_PAD * 2.0,
        tall as f32 * CARD_ROW + CARD_PAD * 2.0,
    );
    // Bottom-anchored and horizontally centred: the card is an answer to
    // something the hand is doing right now, so it belongs where the eyes are —
    // and near the bar, which is where every other transient thing appears.
    let rect = egui::Rect::from_min_size(
        egui::pos2(
            (area.center().x - size.x / 2.0).max(area.left() + CARD_MARGIN),
            (bottom - CARD_MARGIN - size.y).max(area.top() + CARD_MARGIN),
        ),
        size,
    );
    card(paint, rect, alpha);

    let mut left = rect.left() + CARD_PAD;
    for (group, (key_w, col_w)) in columns.iter().zip(&widths) {
        for (i, (key, label)) in group.iter().enumerate() {
            let y = rect.top() + CARD_PAD + i as f32 * CARD_ROW + CARD_ROW / 2.0;
            painter.text(
                egui::pos2(left, y),
                egui::Align2::LEFT_CENTER,
                key,
                key_font(FONT),
                fade(paint.palette.yellow, alpha),
            );
            painter.text(
                egui::pos2(left + key_w + WHICH_KEY_GAP, y),
                egui::Align2::LEFT_CENTER,
                label,
                egui::FontId::proportional(FONT),
                fade(paint.palette.subtext0, alpha),
            );
        }
        left += col_w + WHICH_KEY_COL_SEP;
    }
}

// ── The help browser (PLAN §4.1's `~` / `F1`) ───────────────────────────────

/// One line of the help sheet, and the height its scrolling is measured in.
pub const HELP_ROW: f32 = 20.0;

/// The keys column's width, in logical points. Fixed rather than measured: the
/// descriptions have to start at the same x on every line or the sheet is a
/// ragged mess, and the widest chord in the shipped table (`Ctrl+Shift+z`) fits
/// inside this.
const HELP_KEYS_COLUMN: f32 = 104.0;

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
const HELP_SCRIM: u8 = 150;

/// Where the help card goes: most of the window, above the bar.
pub fn help_rect(area: egui::Rect, bar: egui::Rect) -> egui::Rect {
    let width = (area.width() - CARD_MARGIN * 2.0).min(HELP_MAX_WIDTH);
    egui::Rect::from_min_max(
        egui::pos2(
            area.center().x - width / 2.0,
            area.top() + CARD_MARGIN,
        ),
        egui::pos2(
            area.center().x + width / 2.0,
            (bar.top() - GAP).max(area.top() + CARD_MARGIN + CHROME_HEIGHT),
        ),
    )
}

/// How many help lines fit in the card.
pub fn help_page(rect: egui::Rect) -> usize {
    crate::viewport::visible_rows(rect.height() - CARD_PAD * 2.0 - CARD_ROW, HELP_ROW)
}

/// Draw the help overlay: every live binding, grouped by context.
///
/// Deliberately plain (`ui-anti-slop`: no gratuitous chrome). This is a
/// reference sheet — it is read, not admired — so it is a card, a heading per
/// group, and three columns: what to press, what it does, and the id to write in
/// `keymap.toml` if you want to change it.
pub fn help_overlay(
    paint: &Painting<'_>,
    area: egui::Rect,
    rect: egui::Rect,
    lines: &[HelpLine],
    help: &Help,
    total: usize,
) {
    let painter = paint.painter;
    let palette = paint.palette;
    painter.rect_filled(area, 0, egui::Color32::from_black_alpha(HELP_SCRIM));
    card(paint, rect, 1.0);

    let shown = lines.iter().filter(|l| l.selectable()).count();
    let heading = egui::pos2(rect.left() + CARD_PAD, rect.top() + CARD_PAD + CARD_ROW / 2.0);
    painter.text(
        heading,
        egui::Align2::LEFT_CENTER,
        "Keys",
        egui::FontId::proportional(FONT + 3.0),
        palette.text,
    );
    painter.text(
        egui::pos2(rect.right() - CARD_PAD, heading.y),
        egui::Align2::RIGHT_CENTER,
        if shown == total {
            format!("{total} bindings")
        } else {
            format!("{shown} of {total}")
        },
        egui::FontId::proportional(FONT),
        palette.overlay0,
    );

    let content = egui::Rect::from_min_max(
        egui::pos2(rect.left() + CARD_PAD, rect.top() + CARD_PAD + CARD_ROW),
        egui::pos2(rect.right() - CARD_PAD, rect.bottom() - CARD_PAD),
    );
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
                    palette.yellow,
                );
                let description_left = row.left() + PAD_X + HELP_KEYS_COLUMN;
                let id_galley = painter.layout_no_wrap(
                    binding.id.clone(),
                    egui::FontId::proportional(FONT - 1.0),
                    palette.overlay0,
                );
                painter.galley(
                    egui::pos2(
                        row.right() - PAD_X - id_galley.size().x,
                        row.center().y - id_galley.size().y / 2.0,
                    ),
                    id_galley.clone(),
                    palette.overlay0,
                );
                truncated(
                    &painter,
                    egui::pos2(description_left, row.center().y),
                    &binding.description,
                    palette.subtext0,
                    (row.right() - PAD_X - id_galley.size().x - GAP - description_left).max(0.0),
                );
            }
        }
    }

    if lines.is_empty() {
        // An empty state that says what to do about it, not "no results"
        // (`delightful-ui` §11).
        painter.text(
            egui::pos2(content.center().x, content.top() + content.height() * 0.42),
            egui::Align2::CENTER_CENTER,
            "No binding matches. Backspace to widen the filter.",
            egui::FontId::proportional(FONT),
            palette.overlay0,
        );
    }
}

// ── Shared bits ─────────────────────────────────────────────────────────────

/// A card plate and its hairline edge, at `alpha`.
fn card(paint: &Painting<'_>, rect: egui::Rect, alpha: f32) {
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
        egui::Stroke::new(1.0, fade(paint.palette.surface1, alpha)),
        egui::StrokeKind::Inside,
    );
}

/// The same colour, at `alpha`.
fn fade(color: egui::Color32, alpha: f32) -> egui::Color32 {
    let a = (alpha.clamp(0.0, 1.0) * color.a() as f32).round() as u8;
    egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), a)
}

/// The font every keyboard shortcut on the chrome is drawn in — monospace, so
/// a column of chords lines up and `l` and `1` are not the same shape.
fn key_font(size: f32) -> egui::FontId {
    egui::FontId::monospace(size)
}

fn text_width(painter: &egui::Painter, text: &str, font: egui::FontId) -> f32 {
    painter
        .layout_no_wrap(text.to_string(), font, egui::Color32::WHITE)
        .size()
        .x
}

/// Left-aligned, vertically centred, ellipsised at `max_width`.
fn truncated(
    painter: &egui::Painter,
    pos: egui::Pos2,
    text: &str,
    color: egui::Color32,
    max_width: f32,
) {
    use egui::text::{LayoutJob, TextFormat, TextWrapping};
    let mut job = LayoutJob::single_section(
        text.to_string(),
        TextFormat {
            font_id: egui::FontId::proportional(FONT),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn strip() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(8.0, 8.0), egui::vec2(1384.0, CHROME_HEIGHT))
    }

    /// The chips tile the strip left to right with one gap between, and stop
    /// growing past the cap rather than spreading over the whole window.
    #[test]
    fn the_tab_chips_are_laid_out_left_to_right() {
        let rects = tab_rects(strip(), 3);
        assert_eq!(rects.len(), 3);
        assert!((rects[0].left() - strip().left()).abs() < 1e-3);
        assert!((rects[1].left() - rects[0].right() - TAB_GAP).abs() < 1e-3);
        assert!(rects[0].width() <= TAB_MAX_WIDTH + 1e-3);
        assert!(rects.iter().all(|r| r.height() == CHROME_HEIGHT));
        assert!(tab_rects(strip(), 0).is_empty());
    }

    /// A click lands on the chip it looks like it landed on, and on nothing in
    /// the empty space past the last one.
    #[test]
    fn hit_testing_finds_the_chip_under_the_pointer() {
        let strip = strip();
        let rects = tab_rects(strip, 4);
        for (index, rect) in rects.iter().enumerate() {
            assert_eq!(tab_at(strip, 4, rect.center()), Some(index));
        }
        assert_eq!(tab_at(strip, 4, egui::pos2(strip.right() - 1.0, strip.center().y)), None);
        assert_eq!(tab_at(strip, 4, egui::pos2(-10.0, -10.0)), None);
    }

    /// `delightful-ui` §15: a row inside a card is inset by the padding and its
    /// radius is the card's less that inset, so the gap stays constant round the
    /// corner.
    #[test]
    fn the_card_radii_are_concentric() {
        assert_eq!(CARD_ROW_RADIUS as f32 + CARD_PAD, CARD_RADIUS as f32);
    }

    /// The help card stays inside the window and above the bar, however small
    /// the window gets.
    #[test]
    fn the_help_card_fits_the_window() {
        for size in [egui::vec2(1400.0, 900.0), egui::vec2(320.0, 200.0)] {
            let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), size);
            let bar = egui::Rect::from_min_max(
                egui::pos2(GAP, area.bottom() - GAP - CHROME_HEIGHT),
                egui::pos2(area.right() - GAP, area.bottom() - GAP),
            );
            let rect = help_rect(area, bar);
            assert!(rect.width() > 0.0 && rect.width() <= HELP_MAX_WIDTH + 1e-3);
            assert!(rect.left() >= area.left() && rect.right() <= area.right() + 1e-3);
            assert!(rect.top() >= area.top());
        }
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
                painter: ui.painter(),
                palette: &palette,
                theme: &theme,
                nerd: false,
                show_symlink: true,
                now: std::time::Instant::now(),
            };
            let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, 900.0));
            let bar = egui::Rect::from_min_size(egui::pos2(8.0, 866.0), egui::vec2(1384.0, CHROME_HEIGHT));

            tab_strip(
                &paint,
                strip(),
                &["work".to_string(), "downloads".to_string()],
                1,
                &Hovers::new(),
                &Ripples::new(),
            );
            status_bar(
                &paint,
                bar,
                Status {
                    selected: 3,
                    position: 12,
                    rows: 340,
                    filter: "rs",
                    visual: Some(true),
                },
            );
            let mut prompt = Prompt::new(PromptKind::Filter, 0);
            prompt.line.insert("READ");
            prompt.line.move_left();
            input_bar(&paint, bar, &prompt);
            hint_bar(&paint, bar, &[("Esc", "close"), ("f", "filter")]);

            let rows: Vec<(String, String)> = (0..14)
                .map(|i| (format!("{i}"), format!("do the {i}th thing")))
                .collect();
            which_key(&paint, area, bar.top(), &rows, 1.0);
            which_key(&paint, area, bar.top(), &rows, 0.4);
            which_key(&paint, area, bar.top(), &[], 1.0);

            let registry = df_core::keymap::Registry::defaults();
            let stack = df_core::keymap::ContextStack::with(&[df_core::keymap::Context::Help]);
            let all = crate::help::all_rows(&registry, &stack, df_core::keymap::WhenFlags::LIST);
            let lines = crate::help::lines(&all, "");
            let mut help = Help::default();
            help.reset(&lines);
            let rect = help_rect(area, bar);
            help_overlay(&paint, area, rect, &lines, &help, all.len());
            help_overlay(&paint, area, rect, &[], &help, all.len());
        });
    }
}
