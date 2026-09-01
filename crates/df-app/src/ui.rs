//! Painting the three panes.
//!
//! Everything here is drawn through [`egui::Painter`] rather than with egui
//! widgets, for the reason `app.rs` states: the whole visual language of PLAN §8
//! is hand-drawn, and mixing in a widget theme would only be a second set of
//! rules to fight.
//!
//! ## The geometry, and why the numbers are what they are
//!
//! The three panes are miller columns at PLAN §2's `[1, 4, 3]`, laid on the
//! window's ground with one gap value between and around them. Inside a pane,
//! rows are inset by that same value on **every** adjacent side — `delightful-ui`
//! §15's even insets — and the two radii are concentric: the pane's is the row's
//! plus the inset, so the space between a row and the pane's corner stays a
//! constant width as it turns.
//!
//! ## What is *not* drawn here
//!
//! No preview (Phase 3). The chrome that frames the panes — the tab strip, the
//! bottom bar, the which-key card, the help overlay — is [`crate::chrome`]'s;
//! this file lays out the space it goes in and draws what is inside the panes.
//! The preview pane is drawn as an honest empty state rather than a mock of
//! what will fill it.

use std::path::PathBuf;
use std::time::Instant;

use df_core::config::{LineMode, Theme};
use df_core::fs::{DirState, LoadState, Span};

use crate::format::linemode_text;
use crate::hover::{pressed_rect, Hovers};
use crate::icons::{icon_for, name_color, ICON_FAMILY};
use crate::ripple::Ripples;
use crate::theme::{mix, Palette};

/// The space around and between the panes, in logical points.
///
/// One value for both, because they are the same relationship: a pane is inset
/// from the window by exactly what separates it from its neighbour, so the
/// column of gaps down the middle reads as rhythm rather than as three
/// unrelated margins.
pub const GAP: f32 = 8.0;

/// A row's inset inside its pane, in logical points. Equal to [`GAP`] for the
/// same reason — one spacing value at one level of nesting — and it is the
/// number the two radii are derived from.
pub const ROW_INSET: f32 = 8.0;

/// A row's corner radius. Small: a row is a band, not a card, and anything
/// rounder starts reading as a pill.
pub const ROW_RADIUS: u8 = 6;

/// The pane's corner radius: **the row's plus the inset**, so the gap around a
/// row stays a constant width as it turns the pane's corner (`delightful-ui`
/// §15). Derived, never picked — changing either number above moves this.
pub const PANE_RADIUS: u8 = ROW_RADIUS + ROW_INSET as u8;

/// One row's height, in logical points.
///
/// 22 at a 13.5 pt face is a line box of about 1.5× — the density yazi has at
/// its default row height, which is what the muscle memory this program is a
/// port of expects. Tighter and the icon column starts touching its neighbours;
/// looser and a screenful stops being a screenful.
pub const ROW_HEIGHT: f32 = 22.0;

/// Row text size, in logical points. Above `ui-anti-slop`'s 12 pt floor with
/// room to spare, and the size a file name is legible at across a room.
const FONT_SIZE: f32 = 13.5;

/// Icon size. A hair under the text: the Nerd Font glyphs are drawn to fill
/// their em box while a lowercase letter does not, so matching the numbers
/// would make every icon read as larger than the name beside it.
const ICON_SIZE: f32 = 12.5;

/// The icon column's width, from the row's left padding. Wide enough for the
/// widest glyph in the ported set plus the space that separates it from the
/// name — the gap is part of this number so the names line up whatever the
/// icons are.
const ICON_COLUMN: f32 = 19.0;

/// Horizontal padding inside a row.
const ROW_PAD_X: f32 = 7.0;

/// The gap between the longest name and the linemode column. Names truncate
/// into it rather than colliding with it, so the right-hand column is always
/// readable however long the file names get.
const LINEMODE_GAP: f32 = 12.0;

/// The focused pane's accent rule, in logical points (PLAN §2.1).
const FOCUS_RULE: f32 = 2.0;

/// How far the focused pane's background is tinted towards the accent.
/// PLAN §2.1 says ~4%: enough that the eye can find the focused column without
/// looking for the rule, faint enough that the pane is still the palette's
/// `base` and not a blue panel.
const FOCUS_TINT: f32 = 0.04;

/// How bright the cursor row is in a pane that does **not** have the keyboard
/// (PLAN §2.1's ghost bar, ported from DelightMail's vim-split treatment).
///
/// 35%: enough that "where am I" still has an answer in every pane at once,
/// faint enough that "where do my keys go" has exactly one. The two questions
/// are different questions, and a file manager that answers only the first is
/// the reason people lose their place in a three-column layout.
pub const GHOST_CURSOR: f32 = 0.35;

/// How far a *hovered* row is lifted from the pane towards `surface0`.
/// The full step, because hover is the palette's own next surface — moving up
/// the ramp rather than inventing a colour (see [`crate::theme`]).
const HOVER_LIFT: f32 = 1.0;

/// How far a hovered row *under the cursor bar* is lifted further. Small: the
/// cursor row is already the brightest thing in the column, and a hover that
/// doubled it would make the pointer look like it had selected something.
const CURSOR_HOVER_LIFT: f32 = 0.35;

/// The parent pane's rows, mixed towards its own background. Two thirds of the
/// way: the column is there to say where you are, not to be read, and at full
/// strength it competes with the list for attention.
const PARENT_DIM: f32 = 0.45;

/// How far a *selected* row's ground is tinted towards the selection accent.
///
/// Twice the focus tint, because it means something twice as consequential: the
/// focus tint says where the keyboard is, and this says which files `d` is about
/// to trash. Still a tint and not a fill — at much above this the file names
/// start fighting the ground they are on, and a selection of forty rows would
/// turn the column into a yellow block.
const SELECT_TINT: f32 = 0.10;

/// The selected row's accent bar, in logical points.
///
/// A tint alone is not enough (`delightful-ui`: selection has to be
/// unmistakable at a glance) — on a dark palette a 10% wash is exactly the sort
/// of difference that vanishes on a dim panel or under a colour-blind eye. The
/// bar is the second, redundant channel: a hard edge at a fixed x, which reads
/// as a *list* of marks down the column even at a glance from across the room.
const SELECT_BAR_WIDTH: f32 = 2.5;

/// How far the accent bar is inset from the row's top and bottom, so it reads
/// as a mark on the row rather than as a continuous rule down the pane — two
/// adjacent selected rows must still look like two rows.
const SELECT_BAR_INSET: f32 = 3.5;

/// The clipboard mark's width, on the row's trailing edge. A shade narrower
/// than the selection bar: a yank is a thing you did to a row a moment ago,
/// not a thing you are about to act on, so it says its piece more quietly.
const CLIP_BAR_WIDTH: f32 = 2.0;

/// Height of the tab strip and of the bottom bar, in logical points.
///
/// One number for both: they are the same kind of thing — a single line of
/// chrome bracketing the panes — and giving them different heights would put a
/// wobble in the window's vertical rhythm for no reason. 26 is [`ROW_HEIGHT`]
/// plus the four points that keep a chip's text off its own edge.
pub const CHROME_HEIGHT: f32 = 26.0;

/// Which pane a row belongs to. The hover map's key has to distinguish them —
/// row 3 of the parent column is not row 3 of the list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Column {
    Parent,
    List,
}

/// Everything the pointer can be over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    Row(Column, usize),
    /// A chip in the tab strip, by tab index.
    Tab(usize),
    /// A button on a floating surface — a dialog's answers, the conflict
    /// resolver's apply-to-all — by its index in that surface's own list.
    Action(usize),
    /// A row inside a floating surface: the task panel's tasks, the opener
    /// picker's choices, the conflict resolver's names.
    PanelRow(usize),
}

/// Where the panes and the chrome go.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layout {
    /// The tab strip, when there is more than one tab (PLAN §2).
    pub strip: Option<egui::Rect>,
    pub parent: egui::Rect,
    pub list: egui::Rect,
    pub preview: egui::Rect,
    /// The bottom line: the status, or the input prompt, or the help hint.
    pub bar: egui::Rect,
}

/// Split the window into miller columns at `ratio` (PLAN §2), with the tab
/// strip above and the bar below.
///
/// The gaps come out of the total *before* the ratio is applied, so `[1, 4, 3]`
/// describes the panes themselves rather than the panes plus the spaces between
/// them — otherwise the middle column would quietly shrink as the gap grew.
///
/// The bar is **always** reserved, whether it is showing a status line or a
/// prompt. `f` must not resize the pane it is filtering: rows reflowing under
/// the pointer as a prompt opens is `delightful-ui` §8's spatial stability, and
/// the one place a file manager can least afford to break it is the moment the
/// user is about to act on a row.
pub fn layout(area: egui::Rect, ratio: [u16; 3], tab_strip: bool) -> Layout {
    let outer = area.shrink(GAP);
    // A window narrower or shorter than two gaps shrinks to an *inverted* rect,
    // and every rect derived from it inherits the inversion. Collapsing it to
    // zero instead keeps the geometry degenerate-but-sane while a compositor is
    // mid-resize.
    let outer = egui::Rect::from_min_max(
        outer.min,
        egui::pos2(outer.max.x.max(outer.min.x), outer.max.y.max(outer.min.y)),
    );
    let strip = tab_strip.then(|| {
        egui::Rect::from_min_size(
            outer.min,
            egui::vec2(outer.width(), CHROME_HEIGHT.min(outer.height())),
        )
    });
    let bar = egui::Rect::from_min_max(
        egui::pos2(outer.left(), (outer.bottom() - CHROME_HEIGHT).max(outer.top())),
        outer.max,
    );
    let top = match strip {
        Some(strip) => strip.bottom() + GAP,
        None => outer.top(),
    };
    let inner = egui::Rect::from_min_max(
        egui::pos2(outer.left(), top),
        egui::pos2(outer.right(), (bar.top() - GAP).max(top)),
    );
    let total: f32 = ratio.iter().map(|r| *r as f32).sum();
    // `read_ratio` in df-core rejects an all-zero ratio, so this cannot divide
    // by zero; the guard is here because this function is also reachable from a
    // test with a hand-made ratio.
    let total = if total <= 0.0 { 1.0 } else { total };
    let usable = (inner.width() - GAP * 2.0).max(0.0);
    let width = |r: u16| usable * r as f32 / total;

    let mut x = inner.left();
    let mut next = |w: f32| {
        let rect = egui::Rect::from_min_size(egui::pos2(x, inner.top()), egui::vec2(w, inner.height()));
        x += w + GAP;
        rect
    };
    Layout {
        strip,
        parent: next(width(ratio[0])),
        list: next(width(ratio[1])),
        preview: next(width(ratio[2])),
        bar,
    }
}

/// The area inside a pane that rows are drawn in.
pub fn content_rect(pane: egui::Rect) -> egui::Rect {
    pane.shrink(ROW_INSET)
}

/// One row's rectangle, given how far the view has scrolled (in rows).
pub fn row_rect(content: egui::Rect, scroll_rows: f32, index: usize) -> egui::Rect {
    let top = content.top() + (index as f32 - scroll_rows) * ROW_HEIGHT;
    egui::Rect::from_min_size(
        egui::pos2(content.left(), top),
        egui::vec2(content.width(), ROW_HEIGHT),
    )
}

/// Which row a point is over, if any. `rows` bounds it so the empty space below
/// a short listing is not row 400.
pub fn row_at(
    content: egui::Rect,
    scroll_rows: f32,
    rows: usize,
    pos: egui::Pos2,
) -> Option<usize> {
    if !content.contains(pos) || rows == 0 {
        return None;
    }
    let offset = (pos.y - content.top()) / ROW_HEIGHT + scroll_rows;
    if offset < 0.0 {
        return None;
    }
    let index = offset.floor() as usize;
    (index < rows).then_some(index)
}

/// How brightly each row is lit as "the cursor".
#[derive(Clone, Copy)]
pub enum CursorGlow<'a> {
    /// The cursor row, and only it, at full strength. The parent column, whose
    /// marker is a fact about the path rather than something that just moved.
    Steady,
    /// Per-row amounts from the hover system, so a cursor moved by the keyboard
    /// snaps onto its new row and leaves the old one fading behind it — the
    /// same "instant in, animated out" rule the pointer gets (`delightful-ui`
    /// §3).
    Fading(&'a Hovers<usize>),
}

impl CursorGlow<'_> {
    fn at(self, index: usize, is_cursor: bool) -> f32 {
        match self {
            CursorGlow::Steady => f32::from(is_cursor),
            CursorGlow::Fading(hovers) => hovers.hover(index),
        }
    }
}

/// Which rows are on the clipboard, and how they got there (PLAN §4.1's `y`
/// and `x`).
///
/// Two channels, because they say different things. A **yank** is a copy that
/// has not happened yet, so its rows are marked and otherwise untouched. A
/// **cut** is a row that is on its way out of this directory, so it is dimmed
/// as well — the same treatment the parent column gets for "this is not what
/// you are acting on now".
#[derive(Clone, Copy)]
pub struct ClipMark<'a> {
    pub paths: &'a std::collections::HashSet<PathBuf>,
    /// `x` rather than `y`.
    pub cut: bool,
}

/// One call's worth of "draw this listing here".
///
/// A struct rather than ten positional arguments: half of them are colours and
/// half are booleans, and at a call site that is a row of values whose meaning
/// depends on counting commas.
pub struct ListView<'a> {
    pub pane: egui::Rect,
    /// The pane's own background, already tinted — every row colour is derived
    /// from it, so a row that is not lit draws nothing at all.
    pub ground: egui::Color32,
    pub dir: &'a DirState,
    pub scroll_rows: f32,
    pub column: Column,
    pub hovers: &'a Hovers<Control>,
    pub ripples: &'a Ripples<Control>,
    /// What the cursor row is lifted *towards*. The list uses `surface1`; the
    /// parent's marker is a step quieter, because it reports where you are
    /// rather than what you are about to act on.
    pub cursor_fill: egui::Color32,
    pub cursor_glow: CursorGlow<'a>,
    /// How strongly the cursor row is lit: 1 in the focused pane, and
    /// [`GHOST_CURSOR`] everywhere else.
    pub cursor_alpha: f32,
    /// This pane's right-hand column. The parent passes [`LineMode::None`]: it
    /// is a sixth of the window wide and its job is to say where you are, so
    /// spending a third of it on sizes you did not ask about would crowd out
    /// the names, which are the only thing that column is read for.
    pub linemode: LineMode,
    /// Draw the whole pane muted — the parent column.
    pub dim: bool,
    /// The scan has been running long enough to say so out loud.
    pub slow_load: bool,
    /// How far the rows are displaced horizontally, in points: the tab switch's
    /// slide (see [`crate::tabs`]). The *clip* stays on the pane, so the
    /// content slides inside its column rather than the column moving.
    pub offset_x: f32,
    /// Whether selection marks are drawn in this pane. Off for the parent
    /// column: a selection belongs to the directory it was made in, and marking
    /// the parent's rows would claim you had selected directories you have not
    /// been inside.
    pub show_selection: bool,
    /// The clipboard's marks, when this pane's directory has any in it.
    pub clip: Option<ClipMark<'a>>,
}

/// The shared state a paint pass needs. Bundled because every function below
/// wants all of it and threading nine arguments through each one is how a
/// painter grows a different palette in one corner.
pub struct Painting<'a> {
    pub painter: &'a egui::Painter,
    pub palette: &'a Palette,
    pub theme: &'a Theme,
    /// Whether the real icon glyphs can be drawn (see [`crate::icons`]).
    pub nerd: bool,
    pub show_symlink: bool,
    pub now: Instant,
}

impl Painting<'_> {
    /// A pane's background colour, focus tint included.
    ///
    /// Public because the rows are drawn *from* it — every row colour is a step
    /// away from its pane's ground, so the two must be the same number and not
    /// two constants that happen to agree.
    pub fn pane_fill(&self, fill: egui::Color32, focused: bool) -> egui::Color32 {
        if focused {
            mix(fill, self.palette.blue, FOCUS_TINT)
        } else {
            fill
        }
    }

    /// A pane's background, and the focus treatment if it has focus
    /// (PLAN §2.1).
    pub fn pane(&self, rect: egui::Rect, fill: egui::Color32, focused: bool) {
        let fill = self.pane_fill(fill, focused);
        self.painter.rect_filled(rect, PANE_RADIUS, fill);
        if !focused {
            return;
        }
        // Inset to the pane's corner radius so the rule stops where the corner
        // starts turning, rather than being clipped square against it.
        let rule = egui::Rect::from_min_max(
            egui::pos2(rect.left() + PANE_RADIUS as f32, rect.top()),
            egui::pos2(rect.right() - PANE_RADIUS as f32, rect.top() + FOCUS_RULE),
        );
        self.painter.rect_filled(rule, 1, self.palette.blue);
    }

    /// A directory listing's rows.
    ///
    /// `hovers` and `cursor_glow` are the two "instant in, animated out" tracks
    /// (`delightful-ui` §3): one for the pointer, one for the cursor, so a
    /// cursor moved by the keyboard leaves the same fading trail behind it that
    /// a pointer sweep does.
    pub fn listing(&self, view: ListView<'_>) {
        let ListView {
            pane,
            ground,
            dir,
            scroll_rows,
            column,
            hovers,
            ripples,
            cursor_fill: cursor_color,
            cursor_glow,
            cursor_alpha,
            linemode,
            dim,
            slow_load,
            offset_x,
            show_selection,
            clip,
        } = view;
        let content = content_rect(pane);
        if let Some(message) = self.pane_state_message(dir, slow_load) {
            self.quiet_label(content, &message);
            return;
        }
        // Rows are clipped to the pane's content box so a row half-scrolled off
        // the top does not paint over the pane above it mid-slide.
        let painter = self.painter.with_clip_rect(content);
        let visible = crate::viewport::visible_rows(content.height(), ROW_HEIGHT);
        let first = scroll_rows.floor().max(0.0) as usize;
        // One extra row at each end: mid-slide, the rows entering and leaving
        // are both partly on screen.
        let last = (first + visible + 1).min(dir.len().saturating_sub(1));

        for index in first..=last {
            let Some(entry) = dir.row(index) else { continue };
            let rect = row_rect(content, scroll_rows, index).translate(egui::vec2(offset_x, 0.0));
            if !rect.intersects(content) {
                continue;
            }
            let key = Control::Row(column, index);
            let hover = hovers.hover(key);
            let press = hovers.press(key);
            let on_cursor = index == dir.cursor();
            let selected = show_selection && dir.is_selected(&entry.name);
            // On the clipboard: marked either way, and dimmed when it is a cut.
            let marked = clip.is_some_and(|c| c.paths.contains(&entry.path));
            let cut = marked && clip.is_some_and(|c| c.cut);

            // The row's ground, in one expression: the pane, tinted for a
            // selection, lifted to `surface1` for the cursor row and towards
            // `surface0` for a hover — the last two steps up the palette's own
            // ramp. The selection tint goes on *first* so the cursor still
            // reads as the brightest thing in the column when it is standing on
            // a selected row.
            let ground_here = if selected {
                mix(ground, self.palette.yellow, SELECT_TINT)
            } else {
                ground
            };
            let glow = cursor_glow.at(index, on_cursor) * cursor_alpha;
            let base = mix(ground_here, cursor_color, glow);
            let lift = if glow > 0.5 {
                CURSOR_HOVER_LIFT
            } else {
                HOVER_LIFT
            };
            let fill = mix(base, self.palette.surface0, hover * lift);
            let rect = pressed_rect(rect, press);
            // Nothing is drawn for a row that is the same colour as the pane
            // it sits on — the common case, and one fewer quad per row.
            if fill != ground {
                painter.rect_filled(rect, ROW_RADIUS, fill);
            }
            if selected {
                // The redundant channel: a hard mark at a fixed x, so a
                // selection is legible as a shape and not only as a colour.
                let bar = egui::Rect::from_min_max(
                    egui::pos2(rect.left(), rect.top() + SELECT_BAR_INSET),
                    egui::pos2(
                        rect.left() + SELECT_BAR_WIDTH,
                        rect.bottom() - SELECT_BAR_INSET,
                    ),
                );
                painter.rect_filled(bar, 1, self.palette.yellow);
            }
            if marked {
                // The mirror of the selection bar, on the other edge and in
                // another colour: a row can be both selected and yanked, and
                // the two facts must not fight over one strip of pixels.
                let chip = egui::Rect::from_min_max(
                    egui::pos2(
                        rect.right() - CLIP_BAR_WIDTH,
                        rect.top() + SELECT_BAR_INSET,
                    ),
                    egui::pos2(rect.right(), rect.bottom() - SELECT_BAR_INSET),
                );
                painter.rect_filled(
                    chip,
                    1,
                    if cut {
                        self.palette.peach
                    } else {
                        self.palette.teal
                    },
                );
            }

            // Ripples live inside the row they acknowledge. The clip is
            // rectangular — egui has no rounded clip — which costs a few pixels
            // at the corners, exactly where the ripple is faintest.
            let inside = painter.with_clip_rect(rect);
            for splash in ripples.splashes(key, self.now) {
                inside.circle_filled(
                    splash.center,
                    splash.radius,
                    egui::Color32::from_white_alpha((splash.alpha * 255.0).round() as u8),
                );
            }

            self.row(
                &painter,
                rect,
                entry,
                dir.row_spans(index),
                ground,
                linemode,
                dim || cut,
            );
        }
    }

    /// One row's contents: icon, name, and the linemode column.
    ///
    /// Visible to the crate because the preview pane's directory body draws
    /// with it (PLAN §6, "replaces piper/eza"): a folder previewed and a folder
    /// entered must be the same rows, from one icon table and one set of name
    /// colours, or the two panes quietly disagree about what a file is.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn row(
        &self,
        painter: &egui::Painter,
        rect: egui::Rect,
        entry: &df_core::fs::Entry,
        spans: &[Span],
        ground: egui::Color32,
        linemode: LineMode,
        dim: bool,
    ) {
        let fade = |c: egui::Color32| {
            if dim {
                mix(c, ground, PARENT_DIM)
            } else {
                c
            }
        };

        let icon = icon_for(entry, self.theme, self.palette, self.nerd);
        // The icon family only exists when a patched font was found: egui
        // panics on a `FontFamily::Name` nothing is bound to, so the fallback
        // glyphs (`/`, `@`) are drawn in the monospace face, which is where an
        // `ls -F` classifier belongs anyway.
        let icon_family = if self.nerd {
            egui::FontFamily::Name(ICON_FAMILY.into())
        } else {
            egui::FontFamily::Monospace
        };
        painter.text(
            egui::pos2(rect.left() + ROW_PAD_X, rect.center().y),
            egui::Align2::LEFT_CENTER,
            icon.glyph,
            egui::FontId::new(ICON_SIZE, icon_family),
            fade(icon.color),
        );

        // The linemode is measured first: the name gets whatever is left, so a
        // long name truncates rather than running under the size column.
        let mode_text = linemode_text(entry, linemode);
        let mode_width = if mode_text.is_empty() {
            0.0
        } else {
            let galley = painter.layout_no_wrap(
                mode_text.clone(),
                egui::FontId::proportional(FONT_SIZE),
                fade(self.palette.overlay1),
            );
            let width = galley.size().x;
            painter.galley(
                egui::pos2(
                    rect.right() - ROW_PAD_X - width,
                    rect.center().y - galley.size().y / 2.0,
                ),
                galley,
                fade(self.palette.overlay1),
            );
            width + LINEMODE_GAP
        };

        let name_left = rect.left() + ROW_PAD_X + ICON_COLUMN;
        let name_room = (rect.right() - ROW_PAD_X - mode_width - name_left).max(0.0);
        let name_end = self.text_spans(
            painter,
            egui::pos2(name_left, rect.center().y),
            &entry.name,
            fade(name_color(entry, self.palette)),
            // The part `f` or `/` matched, in the one colour on the palette
            // that is neither a file type nor the selection: the highlight has
            // to be readable as "this is why the row is here" and nothing else.
            fade(self.palette.sky),
            spans,
            name_room,
        );

        // `→ target` after the name, in the dim colour, and only if it fits —
        // a symlink row must never lose its *name* to its target. Not in the
        // parent column, for the reason its linemode is off: that column is
        // one name wide.
        if self.show_symlink && !dim && entry.is_symlink() {
            if let Some(target) = entry.link_target() {
                let room = rect.right() - ROW_PAD_X - mode_width - name_end;
                if room > FONT_SIZE * 3.0 {
                    self.text_truncated(
                        painter,
                        egui::pos2(name_end, rect.center().y),
                        &format!(" → {}", target.to_string_lossy()),
                        fade(self.palette.overlay0),
                        room,
                    );
                }
            }
        }
    }

    /// Draw text left-aligned and vertically centred, truncated with an ellipsis
    /// at `max_width`. Returns where it ended, so the next run can start there.
    fn text_truncated(
        &self,
        painter: &egui::Painter,
        pos: egui::Pos2,
        text: &str,
        color: egui::Color32,
        max_width: f32,
    ) -> f32 {
        self.text_spans(painter, pos, text, color, color, &[], max_width)
    }

    /// The same, with the filter's matched runs in `highlight`.
    ///
    /// The spans are byte ranges into `text` produced by df-core's matcher
    /// (PLAN §7.2), so they are already non-overlapping and in order; they are
    /// still bounds-checked against character boundaries, because they were
    /// computed against the name as it was when the query was applied and this
    /// row could in principle have been rescanned since.
    #[allow(clippy::too_many_arguments)]
    fn text_spans(
        &self,
        painter: &egui::Painter,
        pos: egui::Pos2,
        text: &str,
        color: egui::Color32,
        highlight: egui::Color32,
        spans: &[Span],
        max_width: f32,
    ) -> f32 {
        use egui::text::{LayoutJob, TextFormat, TextWrapping};
        let format = |color: egui::Color32| TextFormat {
            font_id: egui::FontId::proportional(FONT_SIZE),
            color,
            ..Default::default()
        };
        let mut job = LayoutJob::default();
        let push = |job: &mut LayoutJob, run: &str, color| {
            // An empty section is a section egui still lays out; skipping them
            // keeps a name with no matches to exactly one.
            if !run.is_empty() {
                job.append(run, 0.0, format(color));
            }
        };
        let mut at = 0usize;
        for &(start, end) in spans {
            if start < at || end > text.len() || start >= end {
                continue;
            }
            if !text.is_char_boundary(start) || !text.is_char_boundary(end) {
                continue;
            }
            push(&mut job, &text[at..start], color);
            push(&mut job, &text[start..end], highlight);
            at = end;
        }
        push(&mut job, &text[at..], color);
        job.wrap = TextWrapping {
            max_width,
            max_rows: 1,
            // Break mid-word: a file name is not prose, and wrapping it at a
            // word boundary would drop the extension, which is the half a
            // person is reading for.
            break_anywhere: true,
            overflow_character: Some('…'),
        };
        let galley = painter.layout_job(job);
        let width = galley.size().x;
        painter.galley(
            egui::pos2(pos.x, pos.y - galley.size().y / 2.0),
            galley,
            color,
        );
        pos.x + width
    }

    /// What a pane says when it has no rows to show — or `None` when it has.
    ///
    /// Three states, and they must not look alike (`delightful-ui` §11): a
    /// directory that is empty, one that could not be read, and one whose first
    /// batch has not landed yet.
    fn pane_state_message(&self, dir: &DirState, slow_load: bool) -> Option<String> {
        match dir.state() {
            LoadState::Failed => Some(
                dir.error()
                    .map(readable_error)
                    .unwrap_or_else(|| "could not read this directory".to_string()),
            ),
            LoadState::Loading | LoadState::Idle if dir.is_empty() => {
                // Blank until the caller says the read has been slow enough to
                // admit to: the first batch arrives in a few milliseconds, and
                // a label that flashed in and straight back out again would be
                // worse than nothing at all.
                slow_load.then(|| "loading…".to_string())
            }
            _ if dir.is_empty() && !dir.filter().is_empty() => Some("no matches".to_string()),
            _ if dir.is_empty() && dir.total() > 0 => {
                // Everything here is hidden — a `.git`-only directory with `.`
                // off. Saying "empty" would be a lie you cannot act on.
                Some(format!("{} hidden", dir.total()))
            }
            _ if dir.is_empty() => Some("empty".to_string()),
            _ => None,
        }
    }

    /// A quiet centred line: the empty state, the error, the "loading…".
    ///
    /// Biased above true centre (`delightful-ui` §16): text at the mathematical
    /// middle of a tall pane reads as sitting low, and 42% is the usual 60/40
    /// answer.
    pub fn quiet_label(&self, content: egui::Rect, text: &str) {
        self.painter.text(
            egui::pos2(content.center().x, content.top() + content.height() * 0.42),
            egui::Align2::CENTER_CENTER,
            text,
            egui::FontId::proportional(FONT_SIZE),
            self.palette.overlay0,
        );
    }

}

/// Turn a `DfError` string into something a person can act on.
///
/// The error arrives as `path: os error text`, which repeats a path the pane is
/// already showing. What is left is the part that says what to do.
fn readable_error(error: &str) -> String {
    let tail = error.rsplit_once(": ").map(|(_, t)| t).unwrap_or(error);
    let mut text = tail.to_string();
    if let Some(first) = text.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1408.0, 908.0))
    }

    /// The ratio describes the panes, not the panes plus the gaps.
    #[test]
    fn the_panes_split_at_the_configured_ratio() {
        let l = layout(area(), [1, 4, 3], false);
        let usable = 1408.0 - GAP * 2.0 - GAP * 2.0;
        assert!((l.parent.width() - usable / 8.0).abs() < 1e-3);
        assert!((l.list.width() - usable * 4.0 / 8.0).abs() < 1e-3);
        assert!((l.preview.width() - usable * 3.0 / 8.0).abs() < 1e-3);
        // Exactly one gap between neighbours, and the same gap at the edges.
        assert!((l.list.left() - l.parent.right() - GAP).abs() < 1e-3);
        assert!((l.preview.left() - l.list.right() - GAP).abs() < 1e-3);
        assert!((l.parent.left() - GAP).abs() < 1e-3);
        assert!((area().right() - l.preview.right() - GAP).abs() < 1e-3);
    }

    /// `delightful-ui` §15: the radii are concentric, so the gap around a row
    /// is a constant width as it turns the pane's corner.
    #[test]
    fn the_radii_are_concentric() {
        assert_eq!(PANE_RADIUS as f32, ROW_RADIUS as f32 + ROW_INSET);
    }

    /// A window too narrow for three panes must not produce negative widths —
    /// nor a negative *height* once the strip and the bar have taken theirs.
    #[test]
    fn a_tiny_window_does_not_produce_negative_panes() {
        for strip in [false, true] {
            let l = layout(
                egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(10.0, 10.0)),
                [1, 4, 3],
                strip,
            );
            for rect in [l.parent, l.list, l.preview, l.bar] {
                assert!(rect.width() >= 0.0 && rect.height() >= 0.0, "{rect:?}");
            }
        }
    }

    /// The bar is always reserved, and the strip only takes space when it is
    /// asked for (PLAN §2: the strip appears at two tabs).
    #[test]
    fn the_chrome_brackets_the_panes() {
        let bare = layout(area(), [1, 4, 3], false);
        assert_eq!(bare.strip, None);
        assert!((bare.bar.height() - CHROME_HEIGHT).abs() < 1e-3);
        assert!((bare.list.bottom() - (bare.bar.top() - GAP)).abs() < 1e-3);
        assert!((bare.list.top() - GAP).abs() < 1e-3);

        let with_strip = layout(area(), [1, 4, 3], true);
        let strip = with_strip.strip.expect("a strip was asked for");
        assert!((strip.height() - CHROME_HEIGHT).abs() < 1e-3);
        assert!((with_strip.list.top() - (strip.bottom() + GAP)).abs() < 1e-3);
        // The strip costs the panes exactly its own height plus one gap, and
        // nothing else moves.
        assert!(
            (bare.list.height() - with_strip.list.height() - CHROME_HEIGHT - GAP).abs() < 1e-3
        );
        assert_eq!(bare.bar, with_strip.bar);
        assert!((bare.list.width() - with_strip.list.width()).abs() < 1e-3);
    }

    #[test]
    fn rows_stack_downwards_from_the_scroll_position() {
        let content = content_rect(layout(area(), [1, 4, 3], false).list);
        let top = row_rect(content, 0.0, 0);
        assert!((top.top() - content.top()).abs() < 1e-3);
        assert!((top.height() - ROW_HEIGHT).abs() < 1e-3);
        // Scrolled by three rows, row 3 is at the top.
        let scrolled = row_rect(content, 3.0, 3);
        assert!((scrolled.top() - content.top()).abs() < 1e-3);
        // …and a fractional scroll moves it by a fraction of a row.
        let half = row_rect(content, 3.5, 3);
        assert!((half.top() - (content.top() - ROW_HEIGHT / 2.0)).abs() < 1e-3);
    }

    #[test]
    fn hit_testing_finds_the_row_under_the_pointer() {
        let content = content_rect(layout(area(), [1, 4, 3], false).list);
        let inside = |index: usize| row_rect(content, 0.0, index).center();
        assert_eq!(row_at(content, 0.0, 40, inside(0)), Some(0));
        assert_eq!(row_at(content, 0.0, 40, inside(7)), Some(7));
        // Scrolling moves which row is under a fixed point.
        assert_eq!(row_at(content, 5.0, 40, inside(0)), Some(5));
        // Past the end of a short listing there is no row, only pane.
        assert_eq!(row_at(content, 0.0, 3, inside(10)), None);
        // …and neither is there outside the pane.
        assert_eq!(row_at(content, 0.0, 40, egui::pos2(-5.0, -5.0)), None);
    }

    #[test]
    fn errors_lose_the_path_and_keep_the_reason() {
        assert_eq!(
            readable_error("/root/secret: permission denied (os error 13)"),
            "Permission denied (os error 13)"
        );
        assert_eq!(readable_error("gone"), "Gone");
    }
}
