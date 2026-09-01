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
//! top row, the which-key card, the help overlay — is [`crate::chrome`]'s;
//! this file lays out the space it goes in and draws what is inside the panes.
//! The preview pane is drawn as an honest empty state rather than a mock of
//! what will fill it.

use std::path::PathBuf;
use std::time::Instant;

use df_core::config::{LineMode, Theme};
use df_core::fs::{DirState, LoadState, Span};

use crate::chrome::fade;
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

/// The usage bar's track width, in logical points (PLAN §7.3's du mode).
///
/// 46 — a third of the linemode column, which is as much as can be given to a
/// proportion without the *names* losing width to it. The bar is a comparison
/// between rows, and a comparison only needs enough length to be ordered by eye;
/// the exact number is the text beside it.
pub(crate) const USAGE_BAR: f32 = 46.0;

/// How thick that bar is. Two points: a hairline, because there is one of these
/// on every row and anything heavier turns the column into a chart nobody asked
/// for (`delightful-ui`: restraint over decoration).
pub(crate) const USAGE_BAR_HEIGHT: f32 = 2.5;

/// The gap between the bar and the number beside it.
pub(crate) const USAGE_BAR_GAP: f32 = 7.0;

/// The git dot's radius, in logical points (PLAN §7.3).
///
/// 2.5 — a 5 px disc. Big enough to read as a deliberate mark at a glance down
/// a column, small enough that a directory of modified files does not turn into
/// a row of bullets competing with the names. It is a *decoration*: the row is
/// read for its name, and the dot is what the eye finds when it goes looking.
pub(crate) const GIT_DOT_RADIUS: f32 = 2.5;

/// The width the dot reserves between the name and the linemode column, gaps
/// included. The name truncates into this rather than running under the dot.
pub(crate) const GIT_DOT_COLUMN: f32 = GIT_DOT_RADIUS * 2.0 + 10.0;

/// How far a gitignored row's ink is mixed back into the pane behind it.
///
/// It is there, it is legible, and it is not what you are looking at.
/// Deliberately *not* hidden — `target/` has to stay enterable.
///
/// **A third, not [`PARENT_DIM`]'s 45%.** They were one number, on the argument
/// that the window should have one answer to "how far away is quieter". They
/// are two now because they are two different sentences: the parent column is
/// dim because *the whole column* is context, and every row in it agrees, while
/// an ignored row is dim in the middle of bright ones and has to stay readable
/// against them. At 45% a `build/` sitting between two source folders was a
/// smudge you could not read the name of; a third is unmistakably quieter and
/// still a name.
///
/// The dim is also no longer the *only* signal — see [`IGNORED_TAG`]. A row
/// that is greyer than its neighbours for a reason the user cannot see is a
/// rendering bug as far as they are concerned.
pub(crate) const IGNORED_DIM: f32 = 0.3;

/// What an ignored row says for itself, next to the size column.
///
/// The word rather than a glyph: this is the answer to "why is that one grey",
/// and the pane has no tooltips to put it in — every row here is painted, not a
/// widget. Four letters of `overlay0` at [`TAG_SIZE`] is quieter than the size
/// beside it and still a word you can read.
pub(crate) const IGNORED_TAG: &str = "ignored";

/// The tag's text size, in logical points. Two under the row's, which is the
/// smallest step that reads as a different *register* rather than as a
/// rendering accident, and still above `ui-anti-slop`'s 12 pt floor.
const TAG_SIZE: f32 = FONT_SIZE - 2.0;

/// The gap between the tag and whatever is to its right.
const TAG_GAP: f32 = 8.0;

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

/// The cursor-row brightness for a pane that is `focused` far through the
/// focus fade: [`GHOST_CURSOR`] when the keyboard is elsewhere, 1 when it is
/// here, and eased between the two while it is moving (PLAN §2.1).
///
/// The ghost bar is part of the same statement as the tint and the rule, so it
/// travels at the same speed. Left as a hard `if`, it was the one piece of the
/// treatment that still popped.
pub fn ghost_cursor(focused: f32) -> f32 {
    GHOST_CURSOR + (1.0 - GHOST_CURSOR) * focused.clamp(0.0, 1.0)
}

/// How far a *hovered* row is lifted from the pane towards `surface0`.
/// The full step, because hover is the palette's own next surface — moving up
/// the ramp rather than inventing a colour (see [`crate::theme`]).
pub(crate) const HOVER_LIFT: f32 = 1.0;

/// How far a hovered row *under the cursor bar* is lifted further. Small: the
/// cursor row is already the brightest thing in the column, and a hover that
/// doubled it would make the pointer look like it had selected something.
const CURSOR_HOVER_LIFT: f32 = 0.35;

/// The parent pane's rows, mixed towards its own background. Two thirds of the
/// way: the column is there to say where you are, not to be read, and at full
/// strength it competes with the list for attention.
pub(crate) const PARENT_DIM: f32 = 0.45;

/// How solid a departing row's plate is at the start of its fade.
///
/// A third. It has to say "something was here" without competing with the rows
/// that are actually still in the list and travelling past it — a full-strength
/// plate under a name that is leaving reads as a new selection.
const GHOST_PLATE: f32 = 0.33;

/// How far a *selected* row's ground is tinted towards the selection accent.
///
/// Twice the focus tint, because it means something twice as consequential: the
/// focus tint says where the keyboard is, and this says which files `d` is about
/// to trash. Still a tint and not a fill — at much above this the file names
/// start fighting the ground they are on, and a selection of forty rows would
/// turn the column into a yellow block.
pub(crate) const SELECT_TINT: f32 = 0.10;

/// The selected row's accent bar, in logical points.
///
/// A tint alone is not enough (`delightful-ui`: selection has to be
/// unmistakable at a glance) — on a dark palette a 10% wash is exactly the sort
/// of difference that vanishes on a dim panel or under a colour-blind eye. The
/// bar is the second, redundant channel: a hard edge at a fixed x, which reads
/// as a *list* of marks down the column even at a glance from across the room.
pub(crate) const SELECT_BAR_WIDTH: f32 = 2.5;

/// How far the accent bar is inset from the row's top and bottom, so it reads
/// as a mark on the row rather than as a continuous rule down the pane — two
/// adjacent selected rows must still look like two rows.
const SELECT_BAR_INSET: f32 = 3.5;

/// The clipboard mark's width, on the row's trailing edge. A shade narrower
/// than the selection bar: a yank is a thing you did to a row a moment ago,
/// not a thing you are about to act on, so it says its piece more quietly.
pub(crate) const CLIP_BAR_WIDTH: f32 = 2.0;

/// Height of the tab strip and of the top row, in logical points.
///
/// One number for both: they are the same kind of thing — a single line of
/// chrome above the panes — and giving them different heights would put a
/// wobble in the window's vertical rhythm for no reason. 26 is [`ROW_HEIGHT`]
/// plus the four points that keep a chip's text off its own edge.
pub const CHROME_HEIGHT: f32 = 26.0;

/// What a second line costs the top row, in logical points (see [`layout`]).
///
/// A line of the chrome's own face plus its leading — the row is not gaining
/// another chip's worth of padding, only somewhere to put a sentence.
pub const PROMPT_ERROR_LINE: f32 = 17.0;

/// A drop target's ring, in logical points. Two: the same weight as the
/// focused pane's accent rule ([`FOCUS_RULE`]), because it is the same kind of
/// statement — "this is the one" — and a second thickness would be a second
/// vocabulary for one idea.
const DROP_RING: f32 = 2.0;

/// How far a live drop target's ground is washed towards the accent. Between
/// the focus tint and the selection tint: louder than "the keyboard is here",
/// quieter than "these are marked", which is exactly where "let go and it lands
/// here" belongs.
const DROP_TINT: f32 = 0.08;

/// The spring-open badge's radius, in logical points. Small enough to sit
/// inside a 22 pt row without touching its edges.
const DROP_BADGE: f32 = 7.0;

/// The ghost's chips — the count and the verb. One height and one radius for
/// both, because they are the same object twice.
const GHOST_CHIP_HEIGHT: f32 = 15.0;
const GHOST_CHIP_RADIUS: u8 = 5;

/// The gap between the bottom of the card stack and the verb chip under it.
const GHOST_CHIP_GAP: f32 = 5.0;

/// What is written on the ghost's top card.
pub struct GhostFace<'a> {
    pub icon: crate::icons::Icon,
    pub name: &'a str,
    /// The count chip, when the stack has stopped counting for itself.
    pub count: Option<usize>,
    pub verb: &'a str,
}

/// A progress arc, clockwise from twelve o'clock.
///
/// Tessellated at a fixed angular step rather than at a fixed number of
/// segments, so a badge a tenth full is not drawn with the same forty points as
/// a full one — delightviewer's `dismiss.rs` arc, same step.
/// The colour of one row's git dot, or `None` for a row that does not get one.
///
/// A pure function of the status and the palette, so the mapping is a test
/// rather than a thing you have to run a repository to check (PLAN §9).
///
/// The assignments are PLAN §7.3's, and each one is the palette's own word for
/// what the state means elsewhere in the window: `peach` is the colour a cut row
/// already wears (something is in flight), `green` is new, `red` is the only
/// thing in the palette that means *stop*, `blue` is the colour of a directory —
/// a rename is a path that came from somewhere else. Untracked is green mixed
/// most of the way back into the pane's own grey: git does not know about it
/// yet, so it should read as "nearly nothing" beside a real modification.
///
/// [`df_core::git::FileStatus::Ignored`] gets no dot at all. It is said with
/// [`IGNORED_DIM`] and [`IGNORED_TAG`] instead — a dot would be a mark drawing
/// the eye to the one row in the pane that is asking for less of it.
/// The git decoration for one row.
///
/// Two fields rather than one `Option`, because "this pane is in a repository"
/// and "this row has a status" are different questions and the *column* answers
/// the first: it is reserved for every row of a repository listing, so every
/// name in the pane truncates at the same x and a status arriving a second after
/// the scan does not reflow the row under the cursor (`delightful-ui` §8).
#[derive(Clone, Copy, Default)]
pub(crate) struct GitMark {
    pub column: bool,
    pub status: Option<df_core::git::FileStatus>,
    /// Whether this pane spells out *why* an ignored row is dim (see
    /// [`IGNORED_TAG`]).
    ///
    /// The list pane does. The parent column does not, and neither does a
    /// previewed listing: both of those are already uniformly dim for a reason
    /// of their own, so a tag there would be explaining the wrong dimming — and
    /// the parent column is one name wide, with no room for a second word
    /// anyway.
    pub explain: bool,
}

pub(crate) fn git_dot(
    status: df_core::git::FileStatus,
    palette: &Palette,
) -> Option<egui::Color32> {
    use df_core::git::FileStatus as S;
    Some(match status {
        S::Ignored => return None,
        S::Untracked => mix(palette.green, palette.overlay0, 0.55),
        S::Added => palette.green,
        S::Deleted => palette.maroon,
        S::Renamed => palette.blue,
        S::Typechange => palette.yellow,
        S::Modified => palette.peach,
        S::Conflict => palette.red,
    })
}

fn arc(centre: egui::Pos2, radius: f32, progress: f32) -> Vec<egui::Pos2> {
    let sweep = progress.clamp(0.0, 1.0) * std::f32::consts::TAU;
    let steps = ((sweep / 0.15).ceil() as usize).max(2);
    (0..=steps)
        .map(|i| {
            let angle = -std::f32::consts::FRAC_PI_2 + sweep * i as f32 / steps as f32;
            centre + egui::vec2(angle.cos(), angle.sin()) * radius
        })
        .collect()
}

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
    /// A segment of the breadcrumb path bar, from the root rightwards
    /// (PLAN §2).
    Crumb(usize),
    /// The committed filter's trailing chip on the top row, which re-opens the
    /// prompt that set it (PLAN §7.2).
    FilterChip,
    /// The clipboard chip on the top row, which clears it — the pointer's `X`
    /// (PLAN §4.1).
    YankChip,
    /// A row of the right-click menu, and a row of its opener submenu
    /// (PLAN §7.5).
    MenuItem(usize),
    SubmenuItem(usize),
    /// The selection basket's chip, and the rows of the tray it opens
    /// (PLAN §7.1).
    BasketChip,
    BasketRow(usize),
    BasketRemove(usize),
}

/// Where the panes and the chrome go.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layout {
    /// The tab strip, when there is more than one tab (PLAN §2).
    pub strip: Option<egui::Rect>,
    /// The top row (PLAN §2): the breadcrumbs and the status cluster, or the
    /// prompt that has taken their place.
    ///
    /// Under the strip because a tab *contains* a path: the strip says which
    /// session you are in and the row says where that session is, and the
    /// outer fact goes above the inner one. Always reserved — a pane that
    /// changed height when a second tab opened would move rows under the
    /// pointer.
    pub path: egui::Rect,
    pub parent: egui::Rect,
    pub list: egui::Rect,
    pub preview: egui::Rect,
}

/// Split the window into miller columns at `ratio` (PLAN §2), with the tab
/// strip and the top row above them.
///
/// The gaps come out of the total *before* the ratio is applied, so `[1, 4, 3]`
/// describes the panes themselves rather than the panes plus the spaces between
/// them — otherwise the middle column would quietly shrink as the gap grew.
///
/// `path_lines` is how many lines the top row needs: one in browse mode and
/// while most prompts are open, two only while a prompt is showing an error
/// that will not fit beside its query (see [`crate::chrome::prompt_lines`]).
/// The panes reflow under it, which is the one place they may: the alternative
/// is an error message clipped to three characters, and a prompt that cannot
/// say what is wrong with what you typed is worse than a list that moved a row
/// while you were typing.
pub fn layout(area: egui::Rect, ratio: [u16; 3], tab_strip: bool, path_lines: usize) -> Layout {
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
    let top = match strip {
        Some(strip) => strip.bottom() + GAP,
        None => outer.top(),
    };
    // The second line is a *line*, not a second row: it is the same plate
    // carrying one more baseline, so it grows by the height of a line of text
    // rather than by another CHROME_HEIGHT of padding.
    let path_height = CHROME_HEIGHT + (path_lines.max(1) - 1) as f32 * PROMPT_ERROR_LINE;
    let path = egui::Rect::from_min_max(
        egui::pos2(outer.left(), top),
        // Clamped against the window's bottom *and* against its own top: a
        // window too short for the chrome collapses the top row to nothing
        // rather than to an inverted rectangle every rect derived from it
        // would inherit.
        egui::pos2(
            outer.right(),
            (top + path_height).min(outer.bottom()).max(top),
        ),
    );
    let top = (path.bottom() + GAP).min(outer.bottom());
    let inner = egui::Rect::from_min_max(
        egui::pos2(outer.left(), top),
        egui::pos2(outer.right(), outer.bottom().max(top)),
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
        let rect =
            egui::Rect::from_min_size(egui::pos2(x, inner.top()), egui::vec2(w, inner.height()));
        x += w + GAP;
        rect
    };
    Layout {
        strip,
        path,
        parent: next(width(ratio[0])),
        list: next(width(ratio[1])),
        preview: next(width(ratio[2])),
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

// **The keyboard cursor does not fade, in or out.** It used to be a
// [`Hovers`] track of its own, so a cursor moved by the keyboard left the row
// it came from glowing for 240 ms — the pointer's "instant in, animated out"
// rule (`delightful-ui` §3) applied to something that is not a pointer. It is
// wrong twice over: a trail behind an arrow key held down is a smear of four
// half-lit rows and no answer to "where am I", and a fade nobody asked for
// costs fourteen forced 60 fps frames per keypress (PLAN §1's idle rule). The
// pointer keeps its trail — it is a real pointer, and its outro is what makes a
// sweep down the column read as responsive.

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
    /// The rows currently in the hand (PLAN §7.1). Dimmed while they are, the
    /// same way a cut row is: both mean "this is on its way out of here".
    pub dragged: &'a std::collections::HashSet<PathBuf>,
    /// A re-sort in flight (PLAN §2's FLIP). Every row is drawn at the place
    /// the layout says, displaced by however far this animation still has to
    /// carry it — so the *state* is committed and correct and only the pixels
    /// are catching up (`delightful-ui` §5).
    pub flip: Option<&'a crate::flip::Flip>,
    /// What git thinks of this pane's rows (PLAN §7.3), or `None` outside a
    /// repository and on a machine without git — where the feature is silently
    /// absent rather than an empty column reserved for it.
    ///
    /// One `RepoStatus` for the whole pane rather than a lookup per row: the
    /// walk up for a repository root is the expensive half, and it is the same
    /// answer a thousand times over for one listing.
    pub git: Option<&'a df_core::git::RepoStatus>,
    /// PLAN §7.3's "what's big" mode, while it is on for this directory. The
    /// list pane only: the parent column is one name wide, and the preview's
    /// directory body is about a folder nobody is measuring.
    pub usage: Option<&'a crate::usage::Usage>,
    /// A per-row replacement for the linemode column, keyed by row name.
    ///
    /// One virtual listing wants it (PLAN §7.4's trash, where the column is the
    /// directory each row came out of), and it is a *replacement* rather than a
    /// second column for the same reason the usage bar is: they are the same
    /// strip of pixels, and a row showing both would have no room left for its
    /// name. `None` — every listing but that one — costs a `None` check per row
    /// and nothing else.
    pub notes: Option<&'a std::collections::HashMap<String, String>>,
    /// What the background walk has said about this pane's directories
    /// (PLAN §7.3), or `None` when folder sizes are off, when the listing is
    /// virtual, or in a pane whose linemode is not the size.
    ///
    /// It fills in the size column for directory rows only, and only when
    /// neither [`ListView::usage`] nor [`ListView::notes`] has claimed that
    /// column first — the three are the same strip of pixels and the one that
    /// was asked for most explicitly wins.
    pub folders: Option<&'a crate::folders::Folders>,
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
    /// `focused` is an *amount*, not a flag: 0 for a pane the keyboard has
    /// left, 1 for the one it is in, and the eased values in between while
    /// [`crate::focus::FocusFade`] carries the treatment across (PLAN §2.1's
    /// 120 ms). Both the tint and the rule ride it, so the two halves of the
    /// treatment always agree about where the keyboard is.
    pub fn pane_fill(&self, fill: egui::Color32, focused: f32) -> egui::Color32 {
        mix(
            fill,
            self.palette.blue,
            FOCUS_TINT * focused.clamp(0.0, 1.0),
        )
    }

    /// A pane's background, and the focus treatment if it has focus
    /// (PLAN §2.1).
    pub fn pane(&self, rect: egui::Rect, fill: egui::Color32, focused: f32) {
        let focused = focused.clamp(0.0, 1.0);
        let fill = self.pane_fill(fill, focused);
        self.painter.rect_filled(rect, PANE_RADIUS, fill);
        if focused <= 0.0 {
            return;
        }
        // Inset to the pane's corner radius so the rule stops where the corner
        // starts turning, rather than being clipped square against it.
        //
        // The rule fades rather than growing out from the middle: it is a
        // statement about the whole pane, and a 2 px bar unrolling across the
        // top would draw the eye to the *motion* instead of to the column.
        let rule = egui::Rect::from_min_max(
            egui::pos2(rect.left() + PANE_RADIUS as f32, rect.top()),
            egui::pos2(rect.right() - PANE_RADIUS as f32, rect.top() + FOCUS_RULE),
        );
        self.painter
            .rect_filled(rule, 1, crate::chrome::fade(self.palette.blue, focused));
    }

    /// A directory listing's rows.
    ///
    /// `hovers` is the pointer's "instant in, animated out" track
    /// (`delightful-ui` §3), so a sweep down the column leaves a trail behind
    /// it. The keyboard cursor is not on it: it snaps on and off (see the note
    /// above [`ListView`]).
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
            cursor_alpha,
            linemode,
            dim,
            slow_load,
            offset_x,
            show_selection,
            clip,
            dragged,
            flip,
            git,
            usage,
            notes,
            folders,
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
            let Some(entry) = dir.row(index) else {
                continue;
            };
            let rect = row_rect(content, scroll_rows, index).translate(egui::vec2(offset_x, 0.0));
            if !rect.intersects(content) {
                continue;
            }
            // The FLIP displacement, and how solid a row that has just
            // arrived is. Both are zero and one for every row on a settled
            // list, which is the common case and costs a hash miss.
            let (rect, alpha) = match flip {
                Some(flip) => (
                    rect.translate(flip.offset(&entry.path, self.now)),
                    flip.alpha(&entry.path, self.now),
                ),
                None => (rect, 1.0),
            };
            let key = Control::Row(column, index);
            let hover = hovers.hover(key);
            let press = hovers.press(key);
            let on_cursor = index == dir.cursor();
            let selected = show_selection && dir.is_selected(&entry.name);
            // On the clipboard: marked either way, and dimmed when it is a cut.
            let marked = clip.is_some_and(|c| c.paths.contains(&entry.path));
            let cut = marked && clip.is_some_and(|c| c.cut);
            let lifted = !dragged.is_empty() && dragged.contains(&entry.path);

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
            let glow = f32::from(on_cursor) * cursor_alpha;
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
                    egui::pos2(rect.right() - CLIP_BAR_WIDTH, rect.top() + SELECT_BAR_INSET),
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

            // git's word for this row, asked once. `status_for` is a hash
            // lookup plus — only for a repository that reported collapsed
            // directories — a walk up the ancestors, so a pane of rows in a
            // clean repository costs one miss each.
            let status = git.and_then(|g| g.status_for(&entry.path));
            let ignored = status == Some(df_core::git::FileStatus::Ignored);

            self.row(
                &painter,
                rect,
                entry,
                dir.row_spans(index),
                ground,
                linemode,
                if dim || cut || lifted {
                    PARENT_DIM
                } else if ignored {
                    IGNORED_DIM
                } else {
                    0.0
                }
                .max(1.0 - alpha),
                GitMark {
                    column: git.is_some(),
                    status,
                    // Not in the parent column: see [`GitMark::explain`].
                    explain: !dim,
                },
                usage.map(|usage| {
                    // A directory's weight comes from the walk; a file's is
                    // simply its own length, which is known and final from the
                    // moment the row was scanned.
                    let weight = usage.weight(&entry.name);
                    let bytes = weight.map(|w| w.bytes).unwrap_or(entry.len);
                    crate::usage::RowUsage {
                        fraction: usage.fraction(bytes),
                        bytes,
                        estimate: weight.is_some_and(|w| !w.settled),
                        growth: usage.growth(index.saturating_sub(first), self.now),
                    }
                }),
                notes
                    .and_then(|notes| notes.get(&entry.name))
                    .map(String::as_str),
                folders,
            );
        }
        self.flip_ghosts(&painter, flip, content, ROW_RADIUS);
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
        // `mute` is how far this row's ink is mixed back into the pane behind
        // it, 0–1. One number rather than the `dim: bool` it replaced, because
        // three different things now want to mute a row by three different
        // amounts: the parent column and a cut row by `PARENT_DIM`, and a row
        // *arriving* in a FLIP re-sort by however far through its fade it is
        // (see `crate::flip`). A boolean could only express the first.
        mute: f32,
        // What git says about this path (PLAN §7.3), already looked up by the
        // caller — the listing asks its repository once and hands the answer
        // down, because the row painter has no idea which repository it is in
        // and finding out would be a walk up the tree per row.
        git: GitMark,
        // The "what's big" column, when the mode is on (PLAN §7.3). `Some`
        // *replaces* the linemode text rather than sitting beside it: the two
        // are the same strip of pixels, and a row showing both a usage bar and
        // a permission string would have no room left for its name.
        usage: Option<crate::usage::RowUsage>,
        // A virtual listing's own text for the linemode column (PLAN §7.4).
        // Outranked by `usage` for the reason above, and outranking the
        // linemode for the reason in [`ListView::notes`].
        note: Option<&str>,
        // Recursive directory sizes, when they are being measured for this
        // pane (PLAN §7.3). Last in precedence: it fills in the one thing the
        // plain linemode cannot say, and gets out of the way of the two modes
        // that own the column outright.
        folders: Option<&crate::folders::Folders>,
    ) {
        let mute = mute.clamp(0.0, 1.0);
        let fade = |c: egui::Color32| {
            if mute > 0.0 {
                mix(c, ground, mute)
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
        let mode_text = match (&usage, note) {
            (Some(usage), _) => usage.label(),
            (None, Some(note)) => note.to_string(),
            // A directory's recursive size, if the walk has one — otherwise the
            // linemode's own answer, which for a directory is the em dash it
            // has always been.
            (None, None) => folders
                .filter(|_| linemode == LineMode::Size && entry.is_dir())
                .and_then(|folders| folders.label(&entry.name))
                .unwrap_or_else(|| linemode_text(entry, linemode)),
        };
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

        // The bar, immediately left of its number, so the column reads as one
        // measurement rather than as a graphic and a caption.
        let mode_width = match &usage {
            None => mode_width,
            Some(usage) => {
                let right = rect.right() - ROW_PAD_X - mode_width;
                let track = egui::Rect::from_min_max(
                    egui::pos2(
                        right - USAGE_BAR_GAP - USAGE_BAR,
                        rect.center().y - USAGE_BAR_HEIGHT / 2.0,
                    ),
                    egui::pos2(
                        right - USAGE_BAR_GAP,
                        rect.center().y + USAGE_BAR_HEIGHT / 2.0,
                    ),
                );
                if track.is_positive() {
                    painter.rect_filled(track, 1, fade(self.palette.surface1));
                    let filled = usage.filled();
                    if filled > 0.0 {
                        let mut bar = track;
                        bar.set_right(track.left() + track.width() * filled);
                        painter.rect_filled(bar, 1, fade(self.palette.blue));
                    }
                }
                mode_width + USAGE_BAR + USAGE_BAR_GAP
            }
        };

        // `ignored`, left of the dot column: the word that says why this row is
        // grey. Drawn at *half* the row's mute rather than at all of it —
        // fading the explanation as hard as the thing it explains is how the
        // reason ends up as unreadable as the problem.
        let mode_width = if git.explain && git.status == Some(df_core::git::FileStatus::Ignored) {
            let galley = painter.layout_no_wrap(
                IGNORED_TAG.to_string(),
                egui::FontId::proportional(TAG_SIZE),
                self.palette.overlay0,
            );
            let width = galley.size().x;
            let colour = mix(self.palette.overlay0, ground, mute * 0.5);
            painter.galley(
                egui::pos2(
                    rect.right() - ROW_PAD_X - mode_width - width,
                    rect.center().y - galley.size().y / 2.0,
                ),
                galley,
                colour,
            );
            mode_width + width + TAG_GAP
        } else {
            mode_width
        };

        // The dot sits between the name and the linemode column, so the two
        // right-hand facts read as one column of marks and one column of
        // numbers rather than as text with a bullet in the middle of it. Its
        // width is added to what the name is measured against *whether or not
        // there is a dot*, so a row does not reflow under the cursor when a
        // status lands (`delightful-ui` §8).
        let dot_width = if git.column { GIT_DOT_COLUMN } else { 0.0 };
        if let Some(color) = git.status.and_then(|s| git_dot(s, self.palette)) {
            painter.circle_filled(
                egui::pos2(
                    rect.right() - ROW_PAD_X - mode_width - GIT_DOT_COLUMN / 2.0,
                    rect.center().y,
                ),
                GIT_DOT_RADIUS,
                fade(color),
            );
        }
        let mode_width = mode_width + dot_width;

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
        if self.show_symlink && mute <= 0.0 && entry.is_symlink() {
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
    pub(crate) fn pane_state_message(&self, dir: &DirState, slow_load: bool) -> Option<String> {
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

    /// A live drop target: an accent ring and a wash inside it (PLAN §7.1's
    /// "valid targets highlight as the drag approaches").
    ///
    /// `amount` is the hover track's, so the ring snaps on the instant the drag
    /// arrives and fades over [`crate::hover::FADE`] when it leaves
    /// (`delightful-ui` §3). `progress` is the spring-open hold, drawn as the
    /// badge filling around a disc on the target's trailing edge —
    /// delightviewer's `dismiss.rs` commit threshold, on a timer instead of on
    /// a distance.
    pub fn drop_target(&self, rect: egui::Rect, radius: u8, amount: f32, progress: f32) {
        if amount <= 0.0 || !rect.is_positive() {
            return;
        }
        let accent = self.palette.blue;
        self.painter
            .rect_filled(rect, radius, fade(accent, DROP_TINT * amount));
        self.painter.rect_stroke(
            rect,
            radius,
            egui::Stroke::new(DROP_RING, fade(accent, amount)),
            egui::StrokeKind::Inside,
        );
        if progress <= 0.0 {
            return;
        }
        // The badge sits inside the target's trailing edge, where a linemode
        // column has already given up its space to the ring.
        let centre = egui::pos2(rect.right() - DROP_BADGE - DROP_RING * 2.0, rect.center().y);
        self.painter
            .circle_filled(centre, DROP_BADGE, fade(self.palette.crust, 0.85 * amount));
        self.painter.add(egui::Shape::line(
            arc(centre, DROP_BADGE - DROP_RING, progress),
            egui::Stroke::new(DROP_RING, fade(accent, amount)),
        ));
    }

    /// A drag from *another* application is over the window (PLAN §7.1's "drop
    /// in").
    ///
    /// A ring around the whole window and no wash: the window-level statement
    /// is "this program will take that", and the *place* it would land is said
    /// by [`Painting::drop_target`] on the row or pane under the pointer. Two
    /// facts, two marks — a tint here as well would make the second one harder
    /// to see, which is the one that matters.
    pub fn drop_window(&self, area: egui::Rect) {
        self.painter.rect_stroke(
            area.shrink(GAP / 2.0),
            PANE_RADIUS + (GAP / 2.0) as u8,
            egui::Stroke::new(DROP_RING, fade(self.palette.blue, 0.9)),
            egui::StrokeKind::Inside,
        );
    }

    /// The ghost: a stack of cards under the pointer, and the verb it is
    /// carrying (PLAN §7.1, `delightful-ui` §6).
    pub fn ghost(&self, cards: &[crate::dnd::Card], top: &GhostFace<'_>, alpha: f32) {
        if alpha <= 0.0 {
            return;
        }
        let card_fill = self.palette.surface1;
        for card in cards {
            let a = card.alpha * alpha;
            if card.tilt != 0.0 {
                // A tilted card has no rounding — egui has no rotated rounded
                // rect — which costs a few pixels on cards that are mostly
                // behind the top one anyway (see `crate::dnd::tilted`).
                self.painter.add(egui::Shape::convex_polygon(
                    crate::dnd::tilted(card.rect, card.tilt),
                    fade(card_fill, a),
                    egui::Stroke::new(1.0, fade(self.palette.crust, a * 0.6)),
                ));
                continue;
            }
            self.painter
                .rect_filled(card.rect, crate::dnd::GHOST_RADIUS, fade(card_fill, a));
            self.painter.rect_stroke(
                card.rect,
                crate::dnd::GHOST_RADIUS,
                egui::Stroke::new(1.0, fade(self.palette.crust, a * 0.6)),
                egui::StrokeKind::Inside,
            );
        }
        let Some(face) = cards.last() else { return };
        let rect = face.rect;
        let icon_family = if self.nerd {
            egui::FontFamily::Name(ICON_FAMILY.into())
        } else {
            egui::FontFamily::Monospace
        };
        self.painter.text(
            egui::pos2(rect.left() + ROW_PAD_X, rect.center().y),
            egui::Align2::LEFT_CENTER,
            top.icon.glyph,
            egui::FontId::new(ICON_SIZE, icon_family),
            fade(top.icon.color, alpha),
        );
        // The badge takes its room out of the name's, so a count never lands on
        // top of a file name however long the name is.
        let badge = top.count.map(|count| {
            let font = egui::FontId::proportional(FONT_SIZE - 1.5);
            let width = self
                .painter
                .layout_no_wrap(count.to_string(), font.clone(), self.palette.crust)
                .size()
                .x;
            (count.to_string(), font, width + DROP_BADGE)
        });
        let reserved = badge
            .as_ref()
            .map(|(_, _, w)| *w + ROW_PAD_X)
            .unwrap_or(0.0);
        let left = rect.left() + ROW_PAD_X + ICON_COLUMN;
        let inside = self.painter.with_clip_rect(rect);
        self.text_truncated(
            &inside,
            egui::pos2(left, rect.center().y),
            top.name,
            fade(self.palette.text, alpha),
            (rect.right() - ROW_PAD_X - reserved - left).max(0.0),
        );
        if let Some((text, font, width)) = badge {
            let chip = egui::Rect::from_center_size(
                egui::pos2(rect.right() - ROW_PAD_X - width / 2.0, rect.center().y),
                egui::vec2(width, GHOST_CHIP_HEIGHT),
            );
            self.painter
                .rect_filled(chip, GHOST_CHIP_RADIUS, fade(self.palette.blue, alpha));
            self.painter.text(
                chip.center(),
                egui::Align2::CENTER_CENTER,
                text,
                font,
                fade(self.palette.crust, alpha),
            );
        }
        // The verb, under the stack rather than on it: what the drop *is* is a
        // different fact from what is being dropped, and putting them on one
        // card would make the card read as a file called "Move". A cancelled
        // drag has no verb left to say, and says nothing.
        if top.verb.is_empty() {
            return;
        }
        let font = egui::FontId::proportional(FONT_SIZE - 1.5);
        let width = self
            .painter
            .layout_no_wrap(top.verb.to_string(), font.clone(), self.palette.crust)
            .size()
            .x;
        let chip = egui::Rect::from_min_size(
            egui::pos2(
                rect.left(),
                cards
                    .iter()
                    .map(|card| card.rect.bottom())
                    .fold(f32::MIN, f32::max)
                    + GHOST_CHIP_GAP,
            ),
            egui::vec2(width + ROW_PAD_X * 2.0, GHOST_CHIP_HEIGHT),
        );
        self.painter
            .rect_filled(chip, GHOST_CHIP_RADIUS, fade(self.palette.blue, alpha));
        self.painter.text(
            chip.center(),
            egui::Align2::CENTER_CENTER,
            top.verb,
            font,
            fade(self.palette.crust, alpha),
        );
    }

    /// A quiet centred line: the empty state, the error, the "loading…".
    ///
    /// Biased above true centre (`delightful-ui` §16): text at the mathematical
    /// middle of a tall pane reads as sitting low, and 42% is the usual 60/40
    /// answer.
    /// The rows a re-sort has removed, fading out of where they were.
    ///
    /// Drawn *after* the surviving rows and from the [`crate::flip::Flip`]
    /// rather than from the listing, because the listing no longer contains
    /// them — that is what "removed" means. There is no entry left to ask for
    /// an icon or a colour, so a ghost is the name it had, in the quietest
    /// colour on the palette, on its way out. `.` toggling a directory of
    /// dotfiles closed should look like the names leaving, not like the list
    /// blinking.
    pub(crate) fn flip_ghosts(
        &self,
        painter: &egui::Painter,
        flip: Option<&crate::flip::Flip>,
        clip: egui::Rect,
        radius: u8,
    ) {
        let Some(flip) = flip else { return };
        for (path, rect, alpha) in flip.ghosts(self.now) {
            if !rect.intersects(clip) {
                continue;
            }
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            painter.rect_filled(
                rect,
                radius,
                crate::chrome::fade(self.palette.surface0, alpha * GHOST_PLATE),
            );
            crate::chrome::truncated(
                painter,
                egui::pos2(rect.left() + ROW_PAD_X + ICON_COLUMN, rect.center().y),
                &name,
                crate::chrome::fade(self.palette.overlay0, alpha),
                (rect.width() - ROW_PAD_X * 2.0 - ICON_COLUMN).max(0.0),
            );
        }
    }

    pub fn quiet_label(&self, content: egui::Rect, text: &str) {
        self.painter.text(
            egui::pos2(
                content.center().x,
                content.top() + content.height() * crate::chrome::OPTICAL_BASELINE,
            ),
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
        let l = layout(area(), [1, 4, 3], false, 1);
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
                1,
            );
            for rect in [l.parent, l.list, l.preview, l.path] {
                assert!(rect.width() >= 0.0 && rect.height() >= 0.0, "{rect:?}");
            }
        }
    }

    /// The top row is always reserved, the strip only takes space when it is
    /// asked for (PLAN §2: the strip appears at two tabs), and the panes run to
    /// the window's own bottom edge now that there is no bar under them.
    #[test]
    fn the_chrome_sits_above_the_panes() {
        let bare = layout(area(), [1, 4, 3], false, 1);
        assert_eq!(bare.strip, None);
        assert!((bare.list.bottom() - (area().bottom() - GAP)).abs() < 1e-3);
        // The top row is always there, and the panes start below it.
        assert!((bare.path.top() - GAP).abs() < 1e-3);
        assert!((bare.path.height() - CHROME_HEIGHT).abs() < 1e-3);
        assert!((bare.list.top() - (bare.path.bottom() + GAP)).abs() < 1e-3);

        let with_strip = layout(area(), [1, 4, 3], true, 1);
        let strip = with_strip.strip.expect("a strip was asked for");
        assert!((strip.height() - CHROME_HEIGHT).abs() < 1e-3);
        assert!((with_strip.path.top() - (strip.bottom() + GAP)).abs() < 1e-3);
        assert!((with_strip.list.top() - (with_strip.path.bottom() + GAP)).abs() < 1e-3);
        // The strip costs the panes exactly its own height plus one gap, and
        // nothing else moves.
        assert!((bare.list.height() - with_strip.list.height() - CHROME_HEIGHT - GAP).abs() < 1e-3);
        assert!((bare.list.width() - with_strip.list.width()).abs() < 1e-3);
    }

    /// A prompt that grew a second line takes it out of the panes, and gives it
    /// back when it closes: the top row is the only chrome that moves them.
    #[test]
    fn a_two_line_prompt_reflows_the_panes() {
        let one = layout(area(), [1, 4, 3], false, 1);
        let two = layout(area(), [1, 4, 3], false, 2);
        assert!((two.path.height() - one.path.height() - PROMPT_ERROR_LINE).abs() < 1e-3);
        assert!((one.list.height() - two.list.height() - PROMPT_ERROR_LINE).abs() < 1e-3);
        assert!((two.list.top() - (two.path.bottom() + GAP)).abs() < 1e-3);
        // Zero lines is one line: the row is never absent.
        assert_eq!(layout(area(), [1, 4, 3], false, 0).path, one.path);
    }

    #[test]
    fn rows_stack_downwards_from_the_scroll_position() {
        let content = content_rect(layout(area(), [1, 4, 3], false, 1).list);
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
        let content = content_rect(layout(area(), [1, 4, 3], false, 1).list);
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

    /// The drag chrome draws, at every stage and over every degenerate
    /// geometry a resize can hand it.
    ///
    /// A smoke test rather than a pixel test, for the reason `chrome.rs`'s is:
    /// what these functions can actually get wrong is an inverted rectangle, a
    /// zero-radius arc or an empty stack, and every one of those is a panic
    /// inside egui's tessellator rather than a wrong colour.
    #[test]
    fn the_drag_chrome_paints_without_panicking() {
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(Default::default(), |ui| {
            let palette = Palette::default();
            let theme = Theme::default();
            let paint = Painting {
                painter: ui.painter(),
                palette: &palette,
                theme: &theme,
                nerd: false,
                show_symlink: true,
                now: Instant::now(),
            };
            let area = area();
            paint.drop_window(area);
            // A window too small for its own chrome must not paint an inverted
            // ring, and neither must a target that has been scrolled to nothing.
            paint.drop_window(egui::Rect::from_min_size(area.min, egui::vec2(4.0, 4.0)));
            let row = egui::Rect::from_min_size(egui::pos2(300.0, 400.0), egui::vec2(400.0, 22.0));
            for progress in [0.0, 0.01, 0.5, 1.0] {
                paint.drop_target(row, ROW_RADIUS, 1.0, progress);
                paint.drop_target(row, PANE_RADIUS, 0.4, progress);
            }
            // Nothing is drawn for a target that has faded out, or one with no
            // rectangle left.
            paint.drop_target(row, ROW_RADIUS, 0.0, 0.5);
            paint.drop_target(egui::Rect::NOTHING, ROW_RADIUS, 1.0, 0.5);

            let icon = crate::icons::Icon {
                glyph: '/',
                color: palette.blue,
            };
            for count in [1usize, 2, 4, 137] {
                let cards = crate::dnd::ghost_cards(egui::pos2(600.0, 380.0), count);
                paint.ghost(
                    &cards,
                    &GhostFace {
                        icon,
                        name: "a name long enough to need truncating in a 176 pt card",
                        count: crate::dnd::ghost_badge(count),
                        verb: "Move",
                    },
                    1.0,
                );
            }
            // The spring-back's ghost: no name, no verb, half faded.
            let cards = crate::dnd::ghost_cards(egui::pos2(60.0, 60.0), 3);
            paint.ghost(
                &cards,
                &GhostFace {
                    icon,
                    name: "",
                    count: None,
                    verb: "",
                },
                0.5,
            );
            // …and one that has finished fading draws nothing at all.
            paint.ghost(
                &cards,
                &GhostFace {
                    icon,
                    name: "",
                    count: None,
                    verb: "",
                },
                0.0,
            );
            paint.ghost(
                &[],
                &GhostFace {
                    icon,
                    name: "x",
                    count: None,
                    verb: "Copy",
                },
                1.0,
            );
        });
    }

    /// PLAN §7.3's mapping, pinned: each state gets the palette's own word for
    /// what it means, and no two states share a colour — a dot the eye cannot
    /// tell from its neighbour is a decoration, not information.
    #[test]
    fn every_git_state_gets_its_own_colour() {
        use df_core::git::FileStatus as S;
        let palette = Palette::from_theme(&Theme::default());
        assert_eq!(git_dot(S::Modified, &palette), Some(palette.peach));
        assert_eq!(git_dot(S::Added, &palette), Some(palette.green));
        assert_eq!(git_dot(S::Conflict, &palette), Some(palette.red));
        assert_eq!(git_dot(S::Renamed, &palette), Some(palette.blue));
        assert_eq!(git_dot(S::Deleted, &palette), Some(palette.maroon));
        assert_eq!(git_dot(S::Typechange, &palette), Some(palette.yellow));

        // Untracked is a green nobody would mistake for `Added`: git has not
        // been told about the file, so the dot says "nearly nothing".
        let untracked = git_dot(S::Untracked, &palette);
        assert!(untracked.is_some());
        assert_ne!(untracked, Some(palette.green));

        // Ignored is said by dimming the whole row, not by a mark on it.
        assert_eq!(git_dot(S::Ignored, &palette), None);

        let all = [
            S::Untracked,
            S::Added,
            S::Deleted,
            S::Renamed,
            S::Typechange,
            S::Modified,
            S::Conflict,
        ];
        for (i, a) in all.iter().enumerate() {
            for b in &all[i + 1..] {
                assert_ne!(
                    git_dot(*a, &palette),
                    git_dot(*b, &palette),
                    "{a:?} vs {b:?}"
                );
            }
        }
    }

    /// The dot column is reserved for every row of a repository listing, so a
    /// status landing after the scan does not reflow a name under the cursor
    /// (`delightful-ui` §8).
    #[test]
    fn the_dot_column_is_reserved_whether_or_not_the_row_has_a_dot() {
        let with = GitMark {
            column: true,
            status: None,
            explain: true,
        };
        let without = GitMark::default();
        assert!(with.column && with.status.is_none());
        assert!(!without.column);
        // The reserved width has to be wider than the disc it holds, or the dot
        // would touch the name beside it.
        const { assert!(GIT_DOT_COLUMN > GIT_DOT_RADIUS * 2.0) };
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
