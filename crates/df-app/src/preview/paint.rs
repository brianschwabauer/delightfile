//! Drawing the preview pane.
//!
//! Five bodies — text, markdown, a listing, a hexdump, a picture — and one
//! rule they all obey: whatever is drawn is drawn at the pane's alpha, which is
//! 1 for a body that has just arrived (a new file appears instantly) and the
//! [`super::CROSSFADE`] curve for the two swaps that happen *within* one file,
//! thumbnail → photograph and page → page. The alpha is applied with
//! `gamma_multiply` on every colour rather than by drawing into a layer,
//! because a layer would cost a render pass per frame for eighty milliseconds
//! of fade.
//!
//! Everything is painted through [`egui::Painter`], like the rest of the
//! program (see [`crate::ui`]'s header). The directory body calls straight into
//! the list pane's own row painter, so a folder previewed and a folder entered
//! draw the *same* rows — one icon table, one colour rule, one truncation
//! behaviour.

use std::time::Instant;

use df_core::config::LineMode;
use df_core::fs::Entry;
use df_core::preview::PreviewKind;

use crate::theme::mix;
use crate::ui::{content_rect, Painting, ROW_HEIGHT};

use super::{fade, highlight, markdown, Anim, Body, Media, Pane, Texture};

/// Monospace size for code, hexdumps and inline code, in logical points.
///
/// A shade under the list's 13.5 pt proportional face: a monospace glyph is
/// visually wider and heavier at the same nominal size, so matching the numbers
/// would make the preview shout over the column being read.
const MONO: f32 = 12.0;

/// Proportional size for markdown body text. Equal to the row font, because
/// prose in the preview and a file name in the list are both "the content".
const BODY: f32 = 13.0;

/// One line's height in the text, hex and code bodies.
///
/// 1.33× the monospace size, which is where consecutive lines of code stop
/// touching at descenders and before a screenful stops being a screenful.
const LINE: f32 = 16.0;

/// How many spaces a tab is drawn as.
///
/// Two — the width this project's own sources are written at, and the width
/// that keeps deeply nested code inside a pane three eighths of the window
/// wide. A preview is not an editor: it does not have to agree with the file's
/// `.editorconfig`, it has to fit.
const TAB_SIZE: usize = 2;

/// Whether the text body draws a line-number gutter.
///
/// **Off**, deliberately: yazi has none, the muscle memory this program ports
/// expects none, and a gutter costs five columns of a pane that is already the
/// narrowest of the three. It is a constant rather than a deleted feature
/// because it belongs in the `[preview]` config table that df-core has yet to
/// grow.
const LINE_NUMBERS: bool = false;

/// Bytes per row of the hexdump. Sixteen, as every hexdump since `od`: the
/// offsets are round in hex and the ASCII column lines up with the address.
const HEX_COLS: usize = 16;

/// The scrollbar's width, in logical points. Thin: it reports a position, it is
/// not a control (there is no pointer scrolling in the preview yet).
const SCROLLBAR_WIDTH: f32 = 3.0;

/// The shortest the scrollbar's thumb gets, so a 20,000-line file still has
/// something visible to point at.
const SCROLLBAR_MIN: f32 = 24.0;

/// Padding inside the kind badge and the code plate.
const CHIP_PAD: f32 = 6.0;

/// Draw the preview pane.
///
/// `ppp` is the window's pixels-per-point, needed because a decoded texture is
/// sized in *physical* pixels and the pane is measured in points — fitting one
/// to the other without it is the classic HiDPI bug where every image is drawn
/// at twice its size.
pub fn preview(
    paint: &Painting<'_>,
    pane_rect: egui::Rect,
    pane: &mut Pane,
    ppp: f32,
    now: Instant,
) {
    let content = content_rect(pane_rect);
    if content.width() <= 0.0 || content.height() <= 0.0 {
        return;
    }

    let Some(shown) = &pane.shown else {
        // Nothing has come back yet. Blank until the read has been slow enough
        // to admit to — the same rule the list pane's "loading…" follows, and
        // the reason is the same: a label that flashed in and straight back out
        // is worse than no label.
        if pane.wanted.is_some() && now.duration_since(pane.requested_at) >= super::LOADING_DELAY {
            paint.quiet_label(content, "reading…");
        }
        return;
    };

    // **The body does not fade in.** An item-to-item switch is instant: see
    // `super::Shown`. The parameter stays because the two crossfades that are
    // still real — thumbnail → full image, page → page — multiply into it, and
    // because every drawing helper below takes the pane's alpha rather than
    // deciding one for itself.
    let alpha: f32 = 1.0;
    let painter = paint.painter.with_clip_rect(content);
    // How far a rendered page overflows the pane, in points — reported back to
    // the pane below, because only the paint knows how tall the page came out.
    let mut doc_pan: Option<f32> = None;
    let max_scroll = match &shown.body {
        Body::Empty => {
            quiet(paint, content, "empty", alpha);
            0
        }
        Body::Failed(message) => {
            quiet(paint, content, message, alpha);
            0
        }
        Body::Unsupported { kind } => {
            quiet(paint, content, unsupported_text(kind), alpha);
            0
        }
        Body::Text {
            lines,
            syntax,
            truncated,
            states,
        } => text_body(
            paint,
            &painter,
            content,
            lines,
            *syntax,
            states,
            *truncated,
            pane.scroll,
            alpha,
        ),
        Body::Markdown { blocks, truncated } => markdown_body(
            paint,
            &painter,
            content,
            blocks,
            *truncated,
            pane.scroll,
            alpha,
        ),
        Body::Directory { entries, truncated } => directory_body(
            paint,
            &painter,
            content,
            entries,
            *truncated,
            pane.scroll,
            alpha,
        ),
        Body::Hex { bytes, truncated } => hex_body(
            paint,
            &painter,
            content,
            bytes,
            *truncated,
            pane.scroll,
            alpha,
        ),
        Body::Media(media) => {
            doc_pan = Some(media_body(
                paint,
                &painter,
                content,
                media,
                ppp,
                alpha,
                now,
                pane.media_mounted,
                pane.media_frame,
            ));
            0
        }
    };

    if let Some(pan) = doc_pan {
        pane.set_max_pan(pan);
    }
    // The paint is the only thing that knows how tall the content came out, so
    // it is the thing that tells `seek` how far it may go.
    pane.max_scroll = max_scroll;
    if pane.scroll > max_scroll {
        pane.scroll = max_scroll;
    }
    scrollbar(
        paint,
        content,
        pane.scroll,
        max_scroll,
        pane.scrollbar_alpha(now) * alpha,
    );
}

fn quiet(paint: &Painting<'_>, content: egui::Rect, text: &str, alpha: f32) {
    let color = paint.palette.overlay0.gamma_multiply(alpha);
    paint.painter.text(
        egui::pos2(
            content.center().x,
            content.top() + content.height() * crate::chrome::OPTICAL_BASELINE,
        ),
        egui::Align2::CENTER_CENTER,
        text,
        egui::FontId::proportional(BODY),
        color,
    );
}

/// What a file with no previewer says. Never "error": these are all files that
/// are perfectly fine and simply have their answer somewhere else — the opener
/// rules (PLAN §6).
fn unsupported_text(kind: &PreviewKind) -> &'static str {
    match kind {
        PreviewKind::Denied => "permission denied",
        _ => "no preview — press o to open",
    }
}

// ── Text ────────────────────────────────────────────────────────────────────

fn expand_tabs(line: &str) -> String {
    if !line.contains('\t') {
        return line.to_string();
    }
    line.replace('\t', &" ".repeat(TAB_SIZE))
}

/// Which palette colour a token kind takes.
///
/// catppuccin's own published mapping, so the code in the preview looks like
/// the code in the user's editor rather than like a second theme somebody
/// invented for this pane.
fn tok_color(tok: highlight::Tok, palette: &crate::theme::Palette) -> egui::Color32 {
    use highlight::Tok as T;
    match tok {
        T::Text => palette.text,
        // A step above the quiet-label grey: a comment must be skimmable past
        // and still readable when it is the thing you came for.
        T::Comment => palette.overlay1,
        T::Str => palette.green,
        T::Number => palette.peach,
        T::Keyword => palette.mauve,
        T::Type => palette.yellow,
        T::Func => palette.blue,
        T::Meta => palette.pink,
        T::Punct => palette.sky,
        T::Added => palette.green,
        T::Removed => palette.red,
    }
}

#[allow(clippy::too_many_arguments)]
fn text_body(
    paint: &Painting<'_>,
    painter: &egui::Painter,
    content: egui::Rect,
    lines: &[String],
    syntax: Option<&'static str>,
    states: &[highlight::Block],
    truncated: bool,
    scroll: usize,
    alpha: f32,
) -> usize {
    let profile = highlight::profile_for(syntax);
    let rows = (content.height() / LINE).floor().max(1.0) as usize;
    // One row of the pane is spent on the truncation marker when there is one,
    // so it never covers the last line of the file.
    let extra = usize::from(truncated);
    let first = scroll.min(lines.len());
    let last = (first + rows).min(lines.len());

    let gutter = if LINE_NUMBERS {
        // Wide enough for the largest line number in the file, plus a space.
        let digits = lines.len().to_string().len();
        painter
            .layout_no_wrap(
                "0".repeat(digits + 1),
                egui::FontId::monospace(MONO),
                paint.palette.overlay0,
            )
            .size()
            .x
    } else {
        0.0
    };

    let mut state = states.get(first).copied().unwrap_or_default();
    let mut spans: Vec<highlight::Span> = Vec::new();
    for (row, index) in (first..last).enumerate() {
        let Some(line) = lines.get(index) else { break };
        let y = content.top() + row as f32 * LINE;
        let expanded = expand_tabs(line);
        spans.clear();
        state = highlight::scan_line(&expanded, profile, state, Some(&mut spans));

        if LINE_NUMBERS {
            painter.text(
                egui::pos2(content.left() + gutter, y),
                egui::Align2::RIGHT_TOP,
                format!("{} ", index + 1),
                egui::FontId::monospace(MONO),
                paint.palette.overlay0.gamma_multiply(alpha),
            );
        }

        let job = code_job(
            &expanded,
            &spans,
            paint.palette,
            alpha,
            content.width() - gutter,
        );
        let galley = painter.layout_job(job);
        painter.galley(
            egui::pos2(content.left() + gutter, y),
            galley,
            paint.palette.text,
        );
    }

    if truncated {
        let y = content.top() + (last - first).min(rows) as f32 * LINE;
        if y < content.bottom() {
            painter.text(
                egui::pos2(content.left(), y),
                egui::Align2::LEFT_TOP,
                "… the rest was not read",
                egui::FontId::monospace(MONO),
                paint.palette.overlay0.gamma_multiply(alpha),
            );
        }
    }

    (lines.len() + extra).saturating_sub(rows)
}

/// One line of highlighted code as a [`egui::text::LayoutJob`].
///
/// The gaps between spans are plain text: the scanner emits spans only for what
/// it recognises, so this is where "everything else" gets its colour.
fn code_job(
    line: &str,
    spans: &[highlight::Span],
    palette: &crate::theme::Palette,
    alpha: f32,
    max_width: f32,
) -> egui::text::LayoutJob {
    use egui::text::{LayoutJob, TextFormat, TextWrapping};
    let mut job = LayoutJob::default();
    let format = |color: egui::Color32| TextFormat {
        font_id: egui::FontId::monospace(MONO),
        color: color.gamma_multiply(alpha),
        ..Default::default()
    };
    let mut at = 0usize;
    for span in spans {
        if span.start < at || span.end > line.len() || span.start >= span.end {
            continue;
        }
        if !line.is_char_boundary(span.start) || !line.is_char_boundary(span.end) {
            continue;
        }
        if at < span.start {
            job.append(&line[at..span.start], 0.0, format(palette.text));
        }
        job.append(
            &line[span.start..span.end],
            0.0,
            format(tok_color(span.tok, palette)),
        );
        at = span.end;
    }
    if at < line.len() {
        job.append(&line[at..], 0.0, format(palette.text));
    }
    job.wrap = TextWrapping {
        max_width,
        max_rows: 1,
        // A source line is not prose: breaking it at a word boundary would
        // hide the operator that says what the line does.
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    job
}

// ── Markdown ────────────────────────────────────────────────────────────────

/// Heading sizes, biggest first. Four steps and then a floor: a README with an
/// `#####` in it does not want five distinguishable sizes, it wants the first
/// three to be obvious.
const HEADINGS: [f32; 4] = [18.0, 15.5, 14.0, 13.0];

/// The gap a blank line leaves between two blocks.
const MD_GAP: f32 = 7.0;

/// How far one list level indents.
const MD_INDENT: f32 = 14.0;

/// The gutter a blockquote's bar and a list's marker sit in.
const MD_GUTTER: f32 = 14.0;

#[allow(clippy::too_many_arguments)]
fn markdown_body(
    paint: &Painting<'_>,
    painter: &egui::Painter,
    content: egui::Rect,
    blocks: &[markdown::Block],
    truncated: bool,
    scroll: usize,
    alpha: f32,
) -> usize {
    // Markdown blocks are variable height, so the scroll is converted from
    // lines (what `K`/`J` speak) to points here, at the text body's line
    // height. Five presses of `J` moves the same distance in both bodies.
    let offset = scroll as f32 * LINE;
    let mut y = content.top() - offset;
    for block in blocks {
        let height = md_block(paint, painter, content, y, block, alpha);
        y += height;
        // Everything past the bottom still has to be *measured* — the scroll
        // extent is the sum of the heights — but nothing past it is painted,
        // which `md_block`'s own clip against `content` takes care of.
        if y > content.bottom() + content.height() {
            // …except that measuring a 20,000-line document every frame is not
            // free, so once the remaining content is a screenful past the
            // bottom the extent is reported as "more", and `K`/`J` can walk
            // into it a screen at a time.
            return scroll + rows_in(content) + 1;
        }
    }
    if truncated {
        painter.text(
            egui::pos2(content.left(), y),
            egui::Align2::LEFT_TOP,
            "… the rest was not read",
            egui::FontId::proportional(BODY),
            paint.palette.overlay0.gamma_multiply(alpha),
        );
        y += LINE;
    }
    let total = y + offset - content.top();
    (((total - content.height()) / LINE).ceil().max(0.0)) as usize
}

fn rows_in(content: egui::Rect) -> usize {
    (content.height() / LINE).floor().max(1.0) as usize
}

/// Draw one markdown block at `y` and return how tall it was.
fn md_block(
    paint: &Painting<'_>,
    painter: &egui::Painter,
    content: egui::Rect,
    y: f32,
    block: &markdown::Block,
    alpha: f32,
) -> f32 {
    let palette = paint.palette;
    match block {
        markdown::Block::Gap => MD_GAP,
        markdown::Block::Rule => {
            // A rule, drawn at the same weight as the pane's own separators and
            // inset so it reads as punctuation in the document rather than as
            // an edge of the pane.
            let mid = y + MD_GAP;
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(content.left(), mid),
                    egui::pos2(content.right(), mid + 1.0),
                ),
                0,
                palette.surface1.gamma_multiply(alpha),
            );
            MD_GAP * 2.0 + 1.0
        }
        markdown::Block::Heading { level, spans } => {
            let size = HEADINGS
                .get((*level as usize).saturating_sub(1))
                .copied()
                .unwrap_or(BODY);
            let galley = painter.layout_job(md_job(
                spans,
                palette,
                alpha,
                content.width(),
                size,
                palette.text,
            ));
            let height = galley.size().y;
            painter.galley(egui::pos2(content.left(), y), galley, palette.text);
            // An `#` and `##` get a hairline under them — the one piece of
            // structure a long README is actually navigated by.
            if *level <= 2 {
                let under = y + height + 3.0;
                painter.rect_filled(
                    egui::Rect::from_min_max(
                        egui::pos2(content.left(), under),
                        egui::pos2(content.right(), under + 1.0),
                    ),
                    0,
                    palette.surface0.gamma_multiply(alpha),
                );
                return height + 8.0;
            }
            height + 3.0
        }
        markdown::Block::Paragraph(spans) => {
            let galley = painter.layout_job(md_job(
                spans,
                palette,
                alpha,
                content.width(),
                BODY,
                palette.subtext1,
            ));
            let height = galley.size().y;
            painter.galley(egui::pos2(content.left(), y), galley, palette.subtext1);
            height
        }
        markdown::Block::Item {
            depth,
            marker,
            spans,
        } => {
            let left = content.left() + *depth as f32 * MD_INDENT;
            painter.text(
                egui::pos2(left, y),
                egui::Align2::LEFT_TOP,
                marker,
                egui::FontId::proportional(BODY),
                palette.overlay2.gamma_multiply(alpha),
            );
            let text_left = left + MD_GUTTER;
            let galley = painter.layout_job(md_job(
                spans,
                palette,
                alpha,
                content.right() - text_left,
                BODY,
                palette.subtext1,
            ));
            let height = galley.size().y;
            painter.galley(egui::pos2(text_left, y), galley, palette.subtext1);
            height
        }
        markdown::Block::Quote(spans) => {
            let text_left = content.left() + MD_GUTTER;
            let galley = painter.layout_job(md_job(
                spans,
                palette,
                alpha,
                content.right() - text_left,
                BODY,
                palette.overlay2,
            ));
            let height = galley.size().y;
            // The bar, which is what says "quoted" at a glance — the colour
            // alone would be one more grey among several.
            painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(content.left(), y),
                    egui::pos2(content.left() + 2.0, y + height),
                ),
                1,
                palette.surface2.gamma_multiply(alpha),
            );
            painter.galley(egui::pos2(text_left, y), galley, palette.overlay2);
            height
        }
        markdown::Block::Code { syntax, lines } => {
            let height = lines.len() as f32 * LINE + CHIP_PAD * 2.0;
            let plate = egui::Rect::from_min_max(
                egui::pos2(content.left(), y),
                egui::pos2(content.right(), y + height),
            );
            // A plate one step up the palette's ramp, rounded like a row —
            // the same surface a hovered row uses, because it is the same idea:
            // a region lifted off the pane.
            painter.rect_filled(
                plate,
                crate::ui::ROW_RADIUS,
                mix(palette.mantle, palette.surface0, 0.7).gamma_multiply(alpha),
            );
            let profile = highlight::profile_for(*syntax);
            let mut state = highlight::Block::None;
            let mut spans = Vec::new();
            for (row, line) in lines.iter().enumerate() {
                let expanded = expand_tabs(line);
                spans.clear();
                state = highlight::scan_line(&expanded, profile, state, Some(&mut spans));
                let job = code_job(
                    &expanded,
                    &spans,
                    palette,
                    alpha,
                    plate.width() - CHIP_PAD * 2.0,
                );
                let galley = painter.layout_job(job);
                painter.galley(
                    egui::pos2(plate.left() + CHIP_PAD, y + CHIP_PAD + row as f32 * LINE),
                    galley,
                    palette.text,
                );
            }
            height
        }
    }
}

/// One block's inline spans as a wrapping layout job.
fn md_job(
    spans: &[markdown::Span],
    palette: &crate::theme::Palette,
    alpha: f32,
    max_width: f32,
    size: f32,
    base: egui::Color32,
) -> egui::text::LayoutJob {
    use egui::text::{LayoutJob, TextFormat, TextWrapping};
    let mut job = LayoutJob::default();
    for span in spans {
        let style = span.style;
        // egui's default faces have no bold, so weight is carried by
        // *brightness*: body text sits at `subtext1` and bold steps up to
        // `text`. On a dark palette that reads as weight, which is what bold
        // is for.
        let mut color = if style.bold { palette.text } else { base };
        let mut font = egui::FontId::proportional(size);
        let mut background = egui::Color32::TRANSPARENT;
        if style.code {
            font = egui::FontId::monospace(size - 1.0);
            color = palette.peach;
            background = mix(palette.mantle, palette.surface0, 0.8).gamma_multiply(alpha);
        }
        if style.link {
            color = palette.blue;
        }
        job.append(
            &span.text,
            0.0,
            TextFormat {
                font_id: font,
                color: color.gamma_multiply(alpha),
                background,
                // Real italics: egui shears the glyphs, which is the one text
                // style available without a second font file.
                italics: style.italic,
                underline: if style.link {
                    egui::Stroke::new(1.0, palette.blue.gamma_multiply(alpha * 0.6))
                } else {
                    egui::Stroke::NONE
                },
                ..Default::default()
            },
        );
    }
    job.wrap = TextWrapping {
        max_width,
        ..Default::default()
    };
    job
}

// ── Directory ───────────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
fn directory_body(
    paint: &Painting<'_>,
    painter: &egui::Painter,
    content: egui::Rect,
    entries: &[Entry],
    truncated: bool,
    scroll: usize,
    alpha: f32,
) -> usize {
    // The summary keeps the bottom row for itself, so a listing never runs
    // under it.
    let summary_height = LINE + 4.0;
    let rows_area = egui::Rect::from_min_max(
        content.min,
        egui::pos2(
            content.right(),
            (content.bottom() - summary_height).max(content.top()),
        ),
    );
    let rows = (rows_area.height() / ROW_HEIGHT).floor().max(1.0) as usize;
    let first = scroll.min(entries.len());
    let ground = paint.palette.mantle;
    // The compact step, whatever the list pane is at: this is a *picture* of a
    // folder in the narrowest of the three panes, not the folder you are
    // steering, and scaling it up would spend the preview's width on rows
    // nobody is reading (PLAN §4.1's ladder is about the list).
    let columns = paint.row_columns(painter, crate::ui::Scale::default());

    for (row, index) in (first..(first + rows).min(entries.len())).enumerate() {
        let Some(entry) = entries.get(index) else {
            break;
        };
        let rect = egui::Rect::from_min_size(
            egui::pos2(rows_area.left(), rows_area.top() + row as f32 * ROW_HEIGHT),
            egui::vec2(rows_area.width(), ROW_HEIGHT),
        );
        // The list pane's own row painter: one icon table, one set of name
        // colours, one truncation rule for the whole program. `alpha` rides in
        // through the ground it fades towards, so a row fading in fades from
        // the pane rather than from black.
        paint.row(
            painter,
            rect,
            entry,
            &[],
            ground,
            LineMode::Size,
            // Muted by the parent column's amount: that treatment means "not
            // the thing you are acting on", which is also true of a previewed
            // listing.
            crate::ui::PARENT_DIM,
            // No dots in a previewed listing: the preview says what is in a
            // directory, and a git column there would reserve width in the
            // narrowest pane for a fact about a directory nobody is in.
            crate::ui::GitMark::default(),
            None,
            // A previewed listing is a real directory, so its column is the
            // linemode; the note override belongs to the virtual listings.
            None,
            // …and nobody is measuring a directory nobody is in: the walk is
            // for the pane you are standing in (PLAN §1).
            None,
            columns,
        );
        if alpha < 1.0 {
            // The crossfade, done by veiling rather than by re-tinting every
            // colour the row painter chose: one rect over the row, at the
            // pane's own colour, thinning as the fade completes.
            painter.rect_filled(rect, 0, ground.gamma_multiply(1.0 - alpha));
        }
    }

    let summary = summarise(entries, truncated);
    painter.text(
        egui::pos2(content.left(), content.bottom() - summary_height + 2.0),
        egui::Align2::LEFT_TOP,
        summary,
        egui::FontId::proportional(BODY - 1.0),
        paint.palette.overlay0.gamma_multiply(alpha),
    );
    entries.len().saturating_sub(rows)
}

/// "3 folders, 12 files" — the count a glance into a directory is for.
///
/// Public to the module for its test: the pluralisation and the truncation
/// wording are the sort of thing that is wrong for a year before anybody
/// notices.
fn summarise(entries: &[Entry], truncated: bool) -> String {
    let dirs = entries.iter().filter(|e| e.is_dir()).count();
    let files = entries.len() - dirs;
    let plural =
        |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
    let mut text = match (dirs, files) {
        (0, 0) => "empty".to_string(),
        (d, 0) => plural(d, "folder", "folders"),
        (0, f) => plural(f, "file", "files"),
        (d, f) => format!(
            "{}, {}",
            plural(d, "folder", "folders"),
            plural(f, "file", "files")
        ),
    };
    if truncated {
        text.push_str(" — and more");
    }
    text
}

// ── Hexdump ─────────────────────────────────────────────────────────────────

/// One row of a hexdump: `(offset, hex, ascii)`.
///
/// Split out and pure so the column widths, the padding of a short last row and
/// the non-printable substitution are testable — three things that are visibly
/// wrong the moment they are off by one.
pub fn hex_row(bytes: &[u8], offset: usize) -> (String, String, String) {
    let mut hex = String::with_capacity(HEX_COLS * 3);
    let mut ascii = String::with_capacity(HEX_COLS);
    for column in 0..HEX_COLS {
        if column > 0 {
            hex.push(' ');
            // A wider gap at the halfway mark, which is what makes a byte's
            // column countable without counting from the start.
            if column == HEX_COLS / 2 {
                hex.push(' ');
            }
        }
        match bytes.get(column) {
            Some(b) => {
                hex.push_str(&format!("{b:02x}"));
                ascii.push(if b.is_ascii_graphic() || *b == b' ' {
                    *b as char
                } else {
                    '.'
                });
            }
            // A short final row keeps its columns, so the ASCII pane does not
            // slide left under the last line.
            None => hex.push_str("  "),
        }
    }
    (format!("{offset:08x}"), hex, ascii)
}

#[allow(clippy::too_many_arguments)]
fn hex_body(
    paint: &Painting<'_>,
    painter: &egui::Painter,
    content: egui::Rect,
    bytes: &[u8],
    truncated: bool,
    scroll: usize,
    alpha: f32,
) -> usize {
    let rows = (content.height() / LINE).floor().max(1.0) as usize;
    let total = bytes.len().div_ceil(HEX_COLS);
    let first = scroll.min(total);
    let font = egui::FontId::monospace(MONO);
    // The three columns are measured from the font rather than guessed, so a
    // different monospace face does not shear the layout.
    let offset_width = painter
        .layout_no_wrap(
            "00000000  ".to_string(),
            font.clone(),
            paint.palette.overlay0,
        )
        .size()
        .x;
    let hex_width = painter
        .layout_no_wrap(
            " ".repeat(HEX_COLS * 3 + 2),
            font.clone(),
            paint.palette.text,
        )
        .size()
        .x;

    for (row, index) in (first..(first + rows).min(total)).enumerate() {
        let start = index * HEX_COLS;
        let end = (start + HEX_COLS).min(bytes.len());
        let (offset, hex, ascii) = hex_row(&bytes[start..end], start);
        let y = content.top() + row as f32 * LINE;
        // Offsets dim: they are a ruler, not content.
        painter.text(
            egui::pos2(content.left(), y),
            egui::Align2::LEFT_TOP,
            offset,
            font.clone(),
            paint.palette.overlay0.gamma_multiply(alpha),
        );
        painter.text(
            egui::pos2(content.left() + offset_width, y),
            egui::Align2::LEFT_TOP,
            hex,
            font.clone(),
            paint.palette.subtext0.gamma_multiply(alpha),
        );
        painter.text(
            egui::pos2(content.left() + offset_width + hex_width, y),
            egui::Align2::LEFT_TOP,
            ascii,
            font.clone(),
            paint.palette.teal.gamma_multiply(alpha),
        );
    }

    if truncated {
        let y = content.top() + (total - first).min(rows) as f32 * LINE;
        if y < content.bottom() {
            painter.text(
                egui::pos2(content.left(), y),
                egui::Align2::LEFT_TOP,
                "… the rest was not read",
                font,
                paint.palette.overlay0.gamma_multiply(alpha),
            );
        }
    }
    total.saturating_sub(rows)
}

// ── Pictures ────────────────────────────────────────────────────────────────

/// Where a texture of `size` physical pixels goes inside `area`.
///
/// Centred, aspect preserved, and **fitted to the pane in both directions** —
/// a small picture is enlarged to fill the space the same way a large one is
/// shrunk to fit it. The pane exists to show you the file; a 64-pixel icon
/// stranded in the middle of a 900-pixel pane shows you less of it than it
/// could for no reason a person asked for. What keeps the enlargement honest
/// is the *sampling*: anything small enough to be pixel art is drawn with
/// nearest-neighbour (see [`nearest_for`]), so it comes out crisp rather than
/// as the blurry lie a bilinear upscale would be.
///
/// The enlargement is **capped at [`MAX_MAGNIFICATION`]**, though. Filling a
/// 900-point pane with a 2×2 favicon is not showing you the file, it is showing
/// you four enormous squares; past a point the honest thing to draw is a small
/// picture, centred, that you can see all of at once. Shrinking is never
/// capped — a picture too big for the pane has to come down to it.
///
/// The decode is still capped at the pane (`decode::fit` never enlarges
/// either): what grows is the rectangle, never the memory.
///
/// Pure, so the HiDPI arithmetic is a test rather than a thing noticed on a
/// second monitor.
pub fn fit_rect(area: egui::Rect, size: (u32, u32), ppp: f32) -> egui::Rect {
    let ppp = if ppp > 0.0 { ppp } else { 1.0 };
    let natural = egui::vec2(size.0 as f32 / ppp, size.1 as f32 / ppp);
    if natural.x <= 0.0 || natural.y <= 0.0 {
        return egui::Rect::from_center_size(area.center(), egui::Vec2::ZERO);
    }
    let k = (area.width() / natural.x)
        .min(area.height() / natural.y)
        .min(MAX_MAGNIFICATION);
    egui::Rect::from_center_size(area.center(), natural * k)
}

/// The most [`fit_rect`] will enlarge a picture by.
///
/// Eight: a 16-point favicon comes out at 128, which is a readable icon rather
/// than a wall of pixels, and a 128-point sprite sheet still fills a normal
/// pane. Chosen as a power of two so a nearest-neighbour upscale lands on whole
/// source pixels and the result is crisp instead of unevenly blocky.
pub const MAX_MAGNIFICATION: f32 = 8.0;

/// The long edge, in source pixels, at or below which a picture is drawn with
/// nearest-neighbour sampling.
///
/// Icons, favicons and pixel art. 128 is the largest of the standard icon
/// sizes (`.ico` stops at 256 but a 256 is a small picture, not a glyph), and
/// a photograph that small is a thumbnail nobody is inspecting. Below the
/// line, "crisp" is what the file *means*; above it, "smooth" is.
pub const NEAREST_MAX_SIDE: u32 = 128;

/// Should a source of `size` physical pixels be sampled nearest-neighbour?
pub fn nearest_for(size: (u32, u32)) -> bool {
    size.0.max(size.1) <= NEAREST_MAX_SIDE
}

/// The size a frame *occupies* once the container's display matrix has been
/// applied: a quarter turn swaps the axes, a half turn does not.
///
/// `rotation` is degrees **clockwise**, exactly as `dv_media::ProbeInfo`
/// reports it. Anything that is not a quarter turn is treated as none, which
/// is what every container in practice writes anyway.
pub fn oriented_size(size: (u32, u32), rotation: u32) -> (u32, u32) {
    if matches!(rotation % 360, 90 | 270) {
        (size.1, size.0)
    } else {
        size
    }
}

/// The source UV each corner of the destination rect samples, in the order
/// `[top-left, top-right, bottom-right, bottom-left]` of the **destination**.
///
/// This is the inverse of the display matrix: `rotation` degrees clockwise is
/// the turn the decoded frame needs to sit upright, and `mirrored` is a
/// left-to-right flip applied to the source *before* that turn (dv-media's
/// convention, and ffmpeg's autorotate's). Inverting it here rather than
/// rotating pixels means a portrait clip costs a different set of four UVs and
/// not one byte of extra work per frame.
pub fn oriented_uvs(rotation: u32, mirrored: bool) -> [egui::Pos2; 4] {
    // Read each row as "the destination's TL, TR, BR, BL sample *this* source
    // corner". Derived once, tested exhaustively below.
    let base: [(f32, f32); 4] = match rotation % 360 {
        90 => [(0.0, 1.0), (0.0, 0.0), (1.0, 0.0), (1.0, 1.0)],
        180 => [(1.0, 1.0), (0.0, 1.0), (0.0, 0.0), (1.0, 0.0)],
        270 => [(1.0, 0.0), (1.0, 1.0), (0.0, 1.0), (0.0, 0.0)],
        _ => [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)],
    };
    base.map(|(u, v)| egui::pos2(if mirrored { 1.0 - u } else { u }, v))
}

/// A textured quad at `rect` with the display matrix already inverted into its
/// UVs — the one place the still pictures and the video frames agree on what
/// "upright" means.
pub fn oriented_mesh(
    texture: egui::TextureId,
    rect: egui::Rect,
    rotation: u32,
    mirrored: bool,
    tint: egui::Color32,
) -> egui::Mesh {
    let mut mesh = egui::Mesh::with_texture(texture);
    let corners = [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
    ];
    for (pos, uv) in corners.into_iter().zip(oriented_uvs(rotation, mirrored)) {
        mesh.vertices.push(egui::epaint::Vertex {
            pos,
            uv,
            color: tint,
        });
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    mesh
}

/// Where a rendered document page goes inside the pane.
///
/// **Fit, then zoom, then pan.** The page is fitted to the pane by aspect (a
/// PDF page smaller than the pane *is* enlarged — that is what fit-to-pane
/// means and what every document viewer does, unlike a photograph, which would
/// only get blurrier), the fit is multiplied by the zoom, and the result is
/// centred horizontally and scrolled vertically. Deriving the rect from the
/// pane rather than from the texture's own pixel count means a page that hit
/// the rasteriser's size cap is drawn at the right *size* and merely softer,
/// instead of shrinking.
///
/// Returns the rect and how far it overflows the pane, which is how far `↑`/`↓`
/// may scroll it.
pub fn doc_rect(content: egui::Rect, size: (u32, u32), zoom: f32, pan: f32) -> (egui::Rect, f32) {
    let (w, h) = (size.0 as f32, size.1 as f32);
    if w <= 0.0 || h <= 0.0 || content.width() <= 0.0 || content.height() <= 0.0 {
        return (
            egui::Rect::from_center_size(content.center(), egui::Vec2::ZERO),
            0.0,
        );
    }
    let zoom = if zoom.is_finite() {
        zoom.max(0.01)
    } else {
        1.0
    };
    let k = (content.width() / w).min(content.height() / h) * zoom;
    let drawn = egui::vec2(w * k, h * k);
    let overflow = (drawn.y - content.height()).max(0.0);
    let pan = pan.clamp(0.0, overflow);
    let top = if overflow > 0.0 {
        content.top() - pan
    } else {
        content.center().y - drawn.y / 2.0
    };
    let rect =
        egui::Rect::from_min_size(egui::pos2(content.center().x - drawn.x / 2.0, top), drawn);
    (rect, overflow)
}

/// Draw a picture-shaped body, and return how far a rendered page overflows the
/// pane (zero for everything that is not a document).
#[allow(clippy::too_many_arguments)]
fn media_body(
    paint: &Painting<'_>,
    painter: &egui::Painter,
    content: egui::Rect,
    media: &Media,
    ppp: f32,
    alpha: f32,
    now: Instant,
    // `mounted`: a transport is mounted on this file, so the frame and the
    // position strip are drawn over this pane by `crate::playback` — the kind
    // badge and the "no decoder" line both stand down.
    mounted: bool,
    // `frame`: the player has a *decoded* frame over this pane, so the poster
    // it stood in for is done — see `Pane::set_media_frame`.
    frame: bool,
) -> f32 {
    // The placeholder is at full strength the moment it decodes; the real
    // pixels fade in over it. That is delightviewer's trick and the reason a
    // photograph appears to be there instantly (PLAN §6).
    let swap = media
        .swapped_at
        .map(|at| {
            if media.thumb.is_some() {
                fade(at, now)
            } else {
                1.0
            }
        })
        .unwrap_or(0.0);

    // …unless the player has landed a real frame, in which case the poster has
    // done its job. Two pictures of the same moment stacked on each other read
    // as one picture right up until they disagree about their footprint — a
    // portrait clip whose frame is turned and whose poster is not — and then
    // they read as a rendering bug.
    if !frame {
        if let Some(thumb) = &media.thumb {
            draw_texture(painter, content, thumb, ppp, alpha);
        }
        if let Some(full) = &media.full {
            draw_texture(painter, content, full, ppp, alpha * swap);
        }
        // The loop goes over the still it started life as a copy of, at the
        // same fit and the same upscale limit every other picture in this pane
        // gets — a GIF is a picture that moves, not a different kind of thing.
        // Over rather than instead, so the frames arriving one at a time never
        // leave a hole where the picture was.
        if let Some(texture) = media.anim.as_ref().and_then(Anim::texture) {
            draw_texture(painter, content, texture, ppp, alpha);
        }
    }

    // A document's own pixels go over the cached thumbnail that stood in for
    // them, on the same crossfade every other arrival gets.
    if let Some(view) = &media.doc {
        let overflow = doc_body(paint, painter, content, view, media, alpha, now);
        return overflow;
    }

    if media.full.is_none() && media.thumb.is_none() {
        if mounted {
            // The poster frame is on its way from the decoder and the strip is
            // already under it; a "video" label in the middle of the pane would
            // be a caption on a picture about to appear.
        } else if let Some(message) = &media.error {
            quiet(paint, content, message, alpha);
        } else if media.decoding {
            // Blank: the decode is in flight and a spinner over an image about
            // to appear is a flash, not feedback.
        } else if let Some(badge) = media.badge() {
            quiet(paint, content, badge, alpha);
        } else {
            quiet(paint, content, "no decoder for this format", alpha);
        }
        return 0.0;
    }

    // A kind badge in the corner for everything that is not a still image —
    // the seam PLAN §10's playback checkbox replaces. It says what the frame
    // on screen belongs to, which is exactly the thing a video's first frame
    // does not.
    if let Some(badge) = media.badge().filter(|_| !mounted) {
        chip(
            paint,
            painter,
            content,
            badge,
            egui::Align2::LEFT_BOTTOM,
            alpha,
        );
    }
    0.0
}

/// A rendered document page, its summary and its page indicator.
///
/// Returns how far the page overflows the pane, which is how far `↑`/`↓` may
/// scroll it.
fn doc_body(
    paint: &Painting<'_>,
    painter: &egui::Painter,
    content: egui::Rect,
    view: &super::DocView,
    media: &Media,
    alpha: f32,
    now: Instant,
) -> f32 {
    let swap = view
        .swapped_at
        .map(|at| {
            if view.previous.is_some() {
                fade(at, now)
            } else {
                1.0
            }
        })
        .unwrap_or(0.0);
    let mut overflow = 0.0;

    // The outgoing page underneath, at full strength, with the incoming one
    // fading in over it: the same trick the thumbnail-to-photograph swap uses,
    // so a page turn reads as a page turn and not as a blink.
    if let Some(previous) = &view.previous {
        let (rect, _) = doc_rect(content, previous.size, view.zoom, view.pan);
        draw_at(painter, rect, previous, alpha);
    }
    if let Some(current) = &view.current {
        let (rect, over) = doc_rect(content, current.size, view.zoom, view.pan);
        overflow = over;
        draw_at(
            painter,
            rect,
            current,
            alpha * swap.max(f32::from(view.previous.is_none())),
        );
    }

    let nothing_yet = view.current.is_none() && media.thumb.is_none() && media.full.is_none();
    if let Some(message) = &view.error {
        if nothing_yet {
            quiet(paint, content, message, alpha);
        }
    } else if view.unavailable {
        // The library is missing, which is a missing *feature*: the cached
        // thumbnail and the kind badge are the answer, exactly as before
        // pdfium was here at all (PLAN §6).
        if let Some(badge) = media.badge() {
            chip(
                paint,
                painter,
                content,
                badge,
                egui::Align2::LEFT_BOTTOM,
                alpha,
            );
        } else if nothing_yet {
            quiet(paint, content, "no reader for this format", alpha);
        }
    } else if nothing_yet {
        // Blank: the render is in flight, and a spinner over a page about to
        // appear is a flash rather than feedback.
    }

    // The two chips, on the same linger-then-leave the playback strip uses:
    // what the document *is* on the left, where you are in it on the right.
    let chip_alpha = view.chip_alpha(now) * alpha;
    if chip_alpha > 0.0 {
        if let Some(meta) = &view.meta {
            if !meta.summary.is_empty() {
                chip(
                    paint,
                    painter,
                    content,
                    &meta.summary,
                    egui::Align2::LEFT_BOTTOM,
                    chip_alpha,
                );
            }
        }
        if let Some(counter) = view.counter() {
            chip(
                paint,
                painter,
                content,
                &counter,
                egui::Align2::RIGHT_BOTTOM,
                chip_alpha,
            );
        }
    }
    overflow
}

fn draw_texture(
    painter: &egui::Painter,
    content: egui::Rect,
    texture: &Texture,
    ppp: f32,
    alpha: f32,
) {
    draw_at(
        painter,
        fit_rect(content, texture.size, ppp),
        texture,
        alpha,
    );
}

/// The same, at a rect somebody else worked out.
fn draw_at(painter: &egui::Painter, rect: egui::Rect, texture: &Texture, alpha: f32) {
    if alpha <= 0.0 {
        return;
    }
    let mut mesh = egui::Mesh::with_texture(texture.handle.id());
    mesh.add_rect_with_uv(
        rect,
        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
        egui::Color32::from_white_alpha((alpha.clamp(0.0, 1.0) * 255.0).round() as u8),
    );
    painter.add(egui::Shape::mesh(mesh));
}

/// A small plate in one of the pane's bottom corners: the kind badge, the
/// document's summary, the page indicator.
///
/// Inset from both adjacent edges by the same amount and rounded concentrically
/// with the pane (`delightful-ui` §15), so the gap around it stays a constant
/// width as it turns the corner.
fn chip(
    paint: &Painting<'_>,
    painter: &egui::Painter,
    content: egui::Rect,
    text: &str,
    align: egui::Align2,
    alpha: f32,
) {
    if alpha <= 0.0 {
        return;
    }
    let galley = painter.layout_no_wrap(
        text.to_string(),
        egui::FontId::proportional(BODY - 2.0),
        paint.palette.subtext0,
    );
    let size = galley.size() + egui::vec2(CHIP_PAD * 2.0, CHIP_PAD);
    let left = if align.x() == egui::Align::Max {
        content.right() - size.x
    } else {
        content.left()
    };
    let rect = egui::Rect::from_min_size(egui::pos2(left, content.bottom() - size.y), size);
    painter.rect_filled(
        rect,
        crate::ui::ROW_RADIUS,
        paint.palette.crust.gamma_multiply(alpha * 0.85),
    );
    painter.galley(
        rect.min + egui::vec2(CHIP_PAD, CHIP_PAD / 2.0),
        galley,
        paint.palette.subtext0.gamma_multiply(alpha),
    );
}

// ── The scrollbar ───────────────────────────────────────────────────────────

/// A thin bar down the pane's right edge, only while it has something to say.
///
/// It reports a position rather than offering a control — there is no pointer
/// scrolling in the preview yet — so it is three points wide, has no track, and
/// leaves on its own (`delightful-ui`: transient chrome lingers, then fades).
fn scrollbar(
    paint: &Painting<'_>,
    content: egui::Rect,
    scroll: usize,
    max_scroll: usize,
    alpha: f32,
) {
    if alpha <= 0.0 || max_scroll == 0 {
        return;
    }
    let total = (max_scroll + rows_in(content)) as f32;
    let visible = rows_in(content) as f32;
    let height = (content.height() * (visible / total).clamp(0.0, 1.0)).max(SCROLLBAR_MIN);
    let travel = (content.height() - height).max(0.0);
    let at = scroll as f32 / max_scroll as f32;
    let top = content.top() + travel * at.clamp(0.0, 1.0);
    let rect = egui::Rect::from_min_size(
        egui::pos2(content.right() - SCROLLBAR_WIDTH, top),
        egui::vec2(SCROLLBAR_WIDTH, height),
    );
    paint.painter.rect_filled(
        rect,
        (SCROLLBAR_WIDTH / 2.0) as u8,
        paint.palette.overlay0.gamma_multiply(alpha),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hex_row_has_three_aligned_columns() {
        let (offset, hex, ascii) = hex_row(b"Hello, world!\x00\x01\x02", 0);
        assert_eq!(offset, "00000000");
        assert!(hex.starts_with("48 65 6c 6c 6f 2c 20 77  6f"), "{hex}");
        assert_eq!(ascii, "Hello, world!...");
        // Sixteen bytes, fifteen separators, one extra at the halfway mark.
        assert_eq!(hex.len(), 16 * 2 + 15 + 1);
    }

    #[test]
    fn a_short_last_row_keeps_its_columns() {
        let (offset, hex, ascii) = hex_row(b"ab", 0x1234_5678);
        assert_eq!(offset, "12345678");
        assert_eq!(hex.len(), 16 * 2 + 15 + 1, "the columns collapsed");
        assert!(hex.starts_with("61 62   "), "{hex}");
        assert_eq!(ascii, "ab");
    }

    /// Every non-printable is one dot, so the ASCII column stays as wide as
    /// the bytes it describes.
    #[test]
    fn non_printables_become_one_dot_each() {
        // A space is printable and stays a space; everything else outside the
        // graphic range is one dot, so the column stays as wide as the bytes.
        let (_, _, ascii) = hex_row(&[0x00, 0x7f, 0x80, 0xff, b' ', b'~'], 0);
        assert_eq!(ascii, ".... ~", "got {ascii:?}");
    }

    fn area() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 600.0))
    }

    #[test]
    fn a_picture_fits_its_pane_and_stays_centred() {
        // Wider than the pane: the width binds and it is centred vertically.
        let rect = fit_rect(area(), (800, 400), 1.0);
        assert!((rect.width() - 400.0).abs() < 1e-3);
        assert!((rect.height() - 200.0).abs() < 1e-3);
        assert!((rect.center() - area().center()).length() < 1e-3);

        // Taller than the pane: the height binds.
        let rect = fit_rect(area(), (400, 1200), 1.0);
        assert!((rect.height() - 600.0).abs() < 1e-3);
        assert!((rect.width() - 200.0).abs() < 1e-3);
    }

    /// A texture is physical pixels and the pane is points; at 2× a
    /// 1600-pixel image is 800 points wide, so it is the *height* of this
    /// pane that binds and not the width it would bind at if the division
    /// were missed.
    #[test]
    fn a_picture_respects_the_scale_factor() {
        let rect = fit_rect(area(), (1600, 1600), 2.0);
        assert!((rect.width() - 400.0).abs() < 1e-3, "{rect:?}");
        assert!((rect.height() - 400.0).abs() < 1e-3);
    }

    /// The rule that changed: a picture smaller than the pane is *enlarged*
    /// towards it, because a pane exists to show the file (PLAN §6). What keeps
    /// that honest is [`nearest_for`] and [`MAX_MAGNIFICATION`].
    #[test]
    fn a_small_picture_fills_the_pane() {
        // 64 points into a 400×600 pane is 6¼×, under the cap, so this is the
        // plain fit-to-pane rule.
        let rect = fit_rect(area(), (64, 64), 1.0);
        assert!((rect.width() - 400.0).abs() < 1e-3, "{rect:?}");
        assert!((rect.height() - 400.0).abs() < 1e-3);
        assert!((rect.center() - area().center()).length() < 1e-3);
    }

    /// …and past 8× it stops, so a favicon is a favicon and not four hundred
    /// points of one pixel.
    #[test]
    fn a_tiny_picture_stops_at_the_magnification_cap() {
        let rect = fit_rect(area(), (2, 2), 1.0);
        assert!((rect.width() - 16.0).abs() < 1e-3, "{rect:?}");
        assert!((rect.height() - 16.0).abs() < 1e-3);
        assert!((rect.center() - area().center()).length() < 1e-3);

        // The cap is on magnification, not on the rectangle: a 32-point icon
        // still reaches 256, and one big enough to fill the pane still does.
        let rect = fit_rect(area(), (32, 32), 1.0);
        assert!((rect.width() - 256.0).abs() < 1e-3, "{rect:?}");
    }

    #[test]
    fn only_small_sources_are_sampled_nearest() {
        assert!(nearest_for((16, 16)));
        assert!(nearest_for((128, 40)));
        assert!(!nearest_for((129, 40)));
        assert!(!nearest_for((4000, 3000)));
    }

    /// A quarter turn swaps the footprint; a half turn does not. This is what
    /// makes a rotated clip's frame cover the same rectangle its poster did.
    #[test]
    fn a_quarter_turn_swaps_the_footprint() {
        assert_eq!(oriented_size((1920, 1080), 0), (1920, 1080));
        assert_eq!(oriented_size((1920, 1080), 90), (1080, 1920));
        assert_eq!(oriented_size((1920, 1080), 180), (1920, 1080));
        assert_eq!(oriented_size((1920, 1080), 270), (1080, 1920));
        // Not a quarter turn, and a full turn: neither swaps.
        assert_eq!(oriented_size((1920, 1080), 45), (1920, 1080));
        assert_eq!(oriented_size((1920, 1080), 360), (1920, 1080));
    }

    /// The UVs are the *inverse* of the display matrix, so the check is: take
    /// the source corner each destination corner samples, push it forward
    /// through the matrix, and it must land back on that destination corner.
    #[test]
    fn the_uvs_invert_the_display_matrix() {
        // The forward transform dv-media describes: mirror left-to-right
        // first, then turn `rotation` degrees clockwise.
        fn forward(u: f32, v: f32, rotation: u32, mirrored: bool) -> (f32, f32) {
            let u = if mirrored { 1.0 - u } else { u };
            match rotation % 360 {
                90 => (1.0 - v, u),
                180 => (1.0 - u, 1.0 - v),
                270 => (v, 1.0 - u),
                _ => (u, v),
            }
        }
        // Destination corners in the order `oriented_uvs` returns them.
        let dest = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)];
        for rotation in [0, 90, 180, 270] {
            for mirrored in [false, true] {
                let uvs = oriented_uvs(rotation, mirrored);
                for (i, uv) in uvs.iter().enumerate() {
                    let landed = forward(uv.x, uv.y, rotation, mirrored);
                    assert_eq!(
                        landed, dest[i],
                        "rotation {rotation}, mirrored {mirrored}, corner {i}"
                    );
                }
                // …and the four are still the four distinct corners, which is
                // what rules out a transform that folds the picture onto
                // itself.
                let mut seen: Vec<(u32, u32)> =
                    uvs.iter().map(|p| (p.x as u32, p.y as u32)).collect();
                seen.sort_unstable();
                seen.dedup();
                assert_eq!(seen.len(), 4, "rotation {rotation}, mirrored {mirrored}");
            }
        }
    }

    /// 0° unmirrored has to be exactly what `add_rect_with_uv` would have
    /// written, or every unrotated picture in the program moves.
    #[test]
    fn an_unrotated_picture_is_left_alone() {
        assert_eq!(
            oriented_uvs(0, false),
            [
                egui::pos2(0.0, 0.0),
                egui::pos2(1.0, 0.0),
                egui::pos2(1.0, 1.0),
                egui::pos2(0.0, 1.0),
            ]
        );
    }

    #[test]
    fn a_degenerate_texture_does_not_produce_a_nan() {
        let rect = fit_rect(area(), (0, 0), 1.0);
        assert!(rect.width().is_finite() && rect.height().is_finite());
        let rect = fit_rect(area(), (100, 100), 0.0);
        assert!(rect.width().is_finite() && rect.width() > 0.0);
    }

    #[test]
    fn the_summary_counts_and_pluralises() {
        use df_core::fs::Kind;
        use std::path::PathBuf;
        let entry = |name: &str, kind: Kind| Entry {
            name: name.to_string(),
            path: PathBuf::from("/tmp").join(name),
            kind,
            len: 0,
            mtime: None,
            btime: None,
            mode: 0o644,
            uid: 0,
            gid: 0,
            is_hidden: false,
            mime: "text/plain",
            file_kind: df_core::fs::classify(kind, name, "text/plain", 0o644),
        };
        let entries = vec![
            entry("a", Kind::Dir),
            entry("b", Kind::File),
            entry("c", Kind::File),
        ];
        assert_eq!(summarise(&entries, false), "1 folder, 2 files");
        assert_eq!(summarise(&entries, true), "1 folder, 2 files — and more");
        assert_eq!(summarise(&entries[..1], false), "1 folder");
        assert_eq!(summarise(&entries[1..], false), "2 files");
        assert_eq!(summarise(&[], false), "empty");
    }

    /// Every body draws, including the states that are easy to forget: an
    /// unterminated block comment scrolled past its opening, a hexdump of a
    /// single byte, a listing longer than the pane, a zero-width window.
    ///
    /// A painting test cannot check what it *looks* like, but it can check that
    /// none of the index arithmetic above walks off the end of anything — which
    /// is the failure mode a preview pane has.
    #[test]
    fn every_body_paints_without_panicking() {
        use df_core::fs::Kind;
        use std::path::PathBuf;
        let entry = |name: &str, kind: Kind| Entry {
            name: name.to_string(),
            path: PathBuf::from("/tmp").join(name),
            kind,
            len: 4096,
            mtime: None,
            btime: None,
            mode: 0o644,
            uid: 0,
            gid: 0,
            is_hidden: false,
            mime: "text/plain",
            file_kind: df_core::fs::classify(kind, name, "text/plain", 0o644),
        };
        let source = "/* open\nfn main() {\n\tlet s = \"héllo\";\n}\n";
        let lines: Vec<String> = source.split('\n').map(str::to_string).collect();
        let profile = highlight::profile_for(Some("Rust"));
        let bodies = vec![
            Body::Empty,
            Body::Failed("Permission denied".to_string()),
            Body::Unsupported {
                kind: PreviewKind::Unsupported,
            },
            Body::Unsupported {
                kind: PreviewKind::Denied,
            },
            Body::Text {
                states: highlight::block_states(&lines, profile),
                lines: lines.clone(),
                syntax: Some("Rust"),
                truncated: true,
            },
            Body::Markdown {
                blocks: markdown::parse(
                    "# Title\n\ntext with **bold**, `code` and a [link](x).\n\n- one\n  - two\n\n> quoted\n\n---\n\n```rust\nfn f() {}\n```\n",
                ),
                truncated: true,
            },
            Body::Directory {
                entries: (0..80)
                    .map(|i| entry(&format!("file-{i}"), Kind::File))
                    .chain(std::iter::once(entry("sub", Kind::Dir)))
                    .collect(),
                truncated: true,
            },
            Body::Hex {
                bytes: vec![0x7f],
                truncated: false,
            },
            Body::Hex {
                bytes: (0..=255u8).collect(),
                truncated: true,
            },
            Body::Media(Media {
                kind: PreviewKind::Video,
                thumb: None,
                full: None,
                swapped_at: None,
                anim: None,
                error: None,
                decoding: false,
                doc: None,
            }),
            Body::Media(Media {
                kind: PreviewKind::Image,
                thumb: None,
                full: None,
                swapped_at: None,
                anim: None,
                error: Some("no decoder for this format".to_string()),
                decoding: false,
                doc: None,
            }),
            // A document whose worker has answered but whose page has not
            // arrived, and one that has no reader at all: the two states a PDF
            // is in on a machine with and without pdfium.
            Body::Media(Media {
                kind: PreviewKind::Pdf,
                thumb: None,
                full: None,
                swapped_at: None,
                anim: None,
                error: None,
                decoding: false,
                doc: Some(Box::new(doc_fixture(42, crate::preview::doc::Counter::Page, false))),
            }),
            Body::Media(Media {
                kind: PreviewKind::Pdf,
                thumb: None,
                full: None,
                swapped_at: None,
                anim: None,
                error: None,
                decoding: false,
                doc: Some(Box::new(doc_fixture(1, crate::preview::doc::Counter::Page, true))),
            }),
            Body::Media(Media {
                kind: PreviewKind::Gcode,
                thumb: None,
                full: None,
                swapped_at: None,
                anim: None,
                error: None,
                decoding: false,
                doc: Some(Box::new(doc_fixture(312, crate::preview::doc::Counter::Layer, false))),
            }),
        ];

        let ctx = egui::Context::default();
        // Whether a Nerd Font is installed decides which family the row painter
        // asks for, so the test runs against whatever this machine has.
        let nerd = crate::icons::install(&ctx);
        let _ = ctx.run_ui(Default::default(), |ui| {
            let palette = crate::theme::Palette::default();
            let theme = df_core::config::Theme::default();
            let now = Instant::now();
            let paint = Painting {
                tips: None,
                painter: ui.painter(),
                palette: &palette,
                theme: &theme,
                nerd,
                show_symlink: true,
                now,
            };
            let mut pane = Pane::start(df_core::fs::no_notifier());
            for body in &bodies {
                for scroll in [0usize, 3, 10_000] {
                    for size in [
                        egui::vec2(420.0, 800.0),
                        egui::vec2(0.0, 0.0),
                        egui::vec2(30.0, 24.0),
                    ] {
                        let rect = egui::Rect::from_min_size(egui::pos2(900.0, 8.0), size);
                        pane.shown = Some(crate::preview::Shown {
                            body: clone_body(body),
                        });
                        pane.scroll = scroll;
                        preview(&paint, rect, &mut pane, 2.0, now);
                    }
                }
            }
            // …and the states with nothing shown at all.
            pane.shown = None;
            preview(
                &paint,
                egui::Rect::from_min_size(egui::pos2(900.0, 8.0), egui::vec2(420.0, 800.0)),
                &mut pane,
                1.0,
                now,
            );
        });
    }

    /// A document that has been opened but has no pixels yet — the state every
    /// PDF, specimen and toolpath passes through, and the one where the chips
    /// and the fallbacks are all that is on screen.
    fn doc_fixture(
        pages: usize,
        counter: crate::preview::doc::Counter,
        unavailable: bool,
    ) -> crate::preview::DocView {
        let mut view = crate::preview::DocView::new();
        view.meta = Some(crate::preview::doc::Meta {
            pages,
            summary: "312 layers · 0.20 mm".to_string(),
            counter,
        });
        view.page = pages / 2;
        view.unavailable = unavailable;
        view.chip_at = Some(Instant::now());
        view
    }

    /// `delightful-ui` §8: stepping pages must not move the page under the
    /// pointer, and a page smaller than the pane is enlarged to fit — which is
    /// what fit-to-pane means for a document and is the opposite of what a
    /// photograph gets.
    #[test]
    fn a_page_fits_the_pane_and_zooms_from_there() {
        let content = area();
        // US Letter in a 400×600 pane: the *width* binds, the aspect ratio
        // survives, and what is left over is space above and below.
        let (rect, over) = doc_rect(content, (612, 792), 1.0, 0.0);
        assert!((rect.width() - 400.0).abs() < 1e-3, "{rect:?}");
        assert!(
            (rect.height() - 400.0 * 792.0 / 612.0).abs() < 1e-2,
            "{rect:?}"
        );
        assert_eq!(over, 0.0, "a fitted page has nothing to scroll");
        assert!((rect.center().x - content.center().x).abs() < 1e-3);
        assert!(
            (rect.center().y - content.center().y).abs() < 1e-3,
            "{rect:?}"
        );

        // A tiny page is *enlarged* to fit — a document viewer's rule.
        let (small, _) = doc_rect(content, (60, 80), 1.0, 0.0);
        assert!(small.height() > 100.0, "{small:?}");

        // Zoomed, it overflows and can be scrolled by exactly the overflow.
        let (zoomed, over) = doc_rect(content, (612, 792), 2.0, 0.0);
        assert!((zoomed.width() - 800.0).abs() < 1e-2, "{zoomed:?}");
        assert!(
            (over - (zoomed.height() - 600.0)).abs() < 1e-3,
            "got {over}"
        );
        assert!(
            over > 400.0,
            "a doubled letter page overflows a 600 pt pane"
        );
        assert!(
            (zoomed.top() - content.top()).abs() < 1e-3,
            "a zoomed page starts at its top"
        );
        let (panned, _) = doc_rect(content, (612, 792), 2.0, 1000.0);
        assert!(
            (panned.top() - (content.top() - over)).abs() < 1e-2,
            "the pan was not clamped to the overflow"
        );
    }

    #[test]
    fn a_degenerate_page_does_not_produce_a_nan() {
        for size in [(0u32, 0u32), (100, 0), (0, 100)] {
            let (rect, over) = doc_rect(area(), size, 1.0, 0.0);
            assert!(rect.width().is_finite() && over.is_finite(), "{size:?}");
        }
        let (rect, _) = doc_rect(area(), (100, 100), f32::NAN, f32::NAN);
        assert!(rect.width().is_finite() && rect.width() > 0.0);
        let empty = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::Vec2::ZERO);
        let (rect, over) = doc_rect(empty, (100, 100), 1.0, 0.0);
        assert!(rect.width().is_finite() && over == 0.0);
    }

    /// `Body` holds a texture handle and so cannot derive `Clone`; the test
    /// only needs the variants it builds by hand.
    fn clone_body(body: &Body) -> Body {
        match body {
            Body::Empty => Body::Empty,
            Body::Failed(m) => Body::Failed(m.clone()),
            Body::Unsupported { kind } => Body::Unsupported { kind: kind.clone() },
            Body::Text {
                lines,
                syntax,
                truncated,
                states,
            } => Body::Text {
                lines: lines.clone(),
                syntax: *syntax,
                truncated: *truncated,
                states: states.clone(),
            },
            Body::Markdown { blocks, truncated } => Body::Markdown {
                blocks: blocks.clone(),
                truncated: *truncated,
            },
            Body::Directory { entries, truncated } => Body::Directory {
                entries: entries.clone(),
                truncated: *truncated,
            },
            Body::Hex { bytes, truncated } => Body::Hex {
                bytes: bytes.clone(),
                truncated: *truncated,
            },
            Body::Media(media) => Body::Media(Media {
                kind: media.kind.clone(),
                thumb: None,
                full: None,
                swapped_at: media.swapped_at,
                anim: None,
                error: media.error.clone(),
                decoding: media.decoding,
                doc: media.doc.as_deref().map(|view| {
                    let mut copy = crate::preview::DocView::new();
                    copy.meta = view.meta.clone();
                    copy.page = view.page;
                    copy.zoom = view.zoom;
                    copy.pan = view.pan;
                    copy.unavailable = view.unavailable;
                    copy.error = view.error.clone();
                    copy.chip_at = view.chip_at;
                    Box::new(copy)
                }),
            }),
        }
    }

    #[test]
    fn tabs_expand_to_the_configured_width() {
        assert_eq!(expand_tabs("\tif x:"), "  if x:");
        assert_eq!(expand_tabs("\t\tdeep"), "    deep");
        // A line with no tab is not rewritten at all.
        assert_eq!(expand_tabs("plain"), "plain");
    }
}
