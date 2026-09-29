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

use df_core::config::{LineMode, Theme, ViewScale};
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

/// The four numbers above, at one step of [`ViewScale`]'s ladder.
///
/// One multiplier over all of them rather than four independently tuned sets,
/// because the row's proportions are the thing that was designed: the icon is a
/// hair under the text, the name starts at a column wide enough for the widest
/// glyph, and the line box is about 1.5× the face. A step that tuned those
/// relationships separately would be three more row designs to keep in
/// agreement, and the first one to drift would do it silently.
///
/// It is a *value*, threaded down from whoever knows which directory is being
/// drawn, rather than a global anybody can read: the parent column and the list
/// pane are drawn at the same step in one frame and the preview pane's listing
/// is not, and a hidden global is exactly how those three quietly disagree.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Scale {
    /// What every row-counting sum in the window multiplies by — the scrolloff
    /// arithmetic, the wheel, the FLIP's travel, the hit test.
    pub row_height: f32,
    /// The name's and the linemode column's face.
    pub font: f32,
    /// The icon glyph's size.
    pub icon: f32,
    /// How far the name is indented past the row's padding.
    pub icon_column: f32,
}

impl Scale {
    /// The metrics one step of the ladder draws at.
    pub fn new(scale: ViewScale) -> Scale {
        let factor = scale.row_factor();
        Scale {
            row_height: ROW_HEIGHT * factor,
            font: FONT_SIZE * factor,
            icon: ICON_SIZE * factor,
            icon_column: ICON_COLUMN * factor,
        }
    }
}

impl Default for Scale {
    /// The compact list — the constants above, untouched. Every surface that
    /// draws rows without being part of the scaled panes takes this.
    fn default() -> Scale {
        Scale::new(ViewScale::Compact)
    }
}

/// The icon column's width, from the row's left padding. Wide enough for the
/// widest glyph in the ported set plus the space that separates it from the
/// name — the gap is part of this number so the names line up whatever the
/// icons are.
const ICON_COLUMN: f32 = 19.0;

/// Horizontal padding inside a row.
pub(crate) const ROW_PAD_X: f32 = 7.0;

/// The gap between the longest name and the linemode column. Names truncate
/// into it rather than colliding with it, so the right-hand column is always
/// readable however long the file names get.
pub(crate) const LINEMODE_GAP: f32 = 12.0;

/// The most of a row the `m t` column may take, as a share of its width.
///
/// Two fifths: enough for three or four short tags on an ordinary row, and
/// always leaving the name the larger half — the name is what a row is read
/// for, and the tags past the cut are one `Tab` away on the spot panel.
const TAGS_COLUMN_SHARE: f32 = 0.4;

/// The most of a row a content hit's matched line may take in the linemode
/// column (PLAN §7.2), as a share of the row's width.
///
/// Two fifths. The name is the row's first fact and the line its second, and
/// a line is as long as whatever file it came from: past this it is cut at its
/// end with `…`, which leaves the name the larger part of every row while the
/// line keeps enough words to be recognised.
const NOTE_SHARE: f32 = 0.4;

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
/// Hidden rows (a leading `.`) take the same step: the listing shows them, and
/// says at the same time that they are the machine's business more than
/// yours. The help sheet's legend names both reasons, so a row that is greyer
/// than its neighbours is never grey for a reason the user cannot find.
pub(crate) const IGNORED_DIM: f32 = 0.3;

/// How bright the parent column's marker is, against the list cursor's 1.
///
/// 35%. The keyboard is always in the list (PLAN §2.1), so the parent's marker
/// is never a cursor somebody is steering — it reports *where you are*, which
/// is a quieter fact than *what you are about to act on* and is drawn as one.
/// Loud enough that "where am I" has an answer in the column at a glance,
/// quiet enough that it never competes with the row the keys are aimed at.
pub const PARENT_MARKER: f32 = 0.35;

/// How far a *hovered* row is lifted from the pane towards `surface0`.
/// The full step, because hover is the palette's own next surface — moving up
/// the ramp rather than inventing a colour (see [`crate::theme`]).
pub(crate) const HOVER_LIFT: f32 = 1.0;

/// How far a hovered row *under the cursor bar* is lifted further. Small: the
/// cursor row is already the brightest thing in the column, and a hover that
/// doubled it would make the pointer look like it had selected something.
pub(crate) const CURSOR_HOVER_LIFT: f32 = 0.35;

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
pub(crate) const SELECT_BAR_INSET: f32 = 3.5;

/// The clipboard mark's width, on the row's trailing edge. A shade narrower
/// than the selection bar: a yank is a thing you did to a row a moment ago,
/// not a thing you are about to act on, so it says its piece more quietly.
pub(crate) const CLIP_BAR_WIDTH: f32 = 2.0;

/// Height of one line of chrome — the tab strip, and the floor a floating
/// card is held to — in logical points.
///
/// 30 is [`ROW_HEIGHT`] plus eight: the four points that keep a chip's text
/// off its own edge, and four more of air so the row reads as a bar rather
/// than as one more row of the listing that happens to be on top.
pub const CHROME_HEIGHT: f32 = 30.0;

/// Height of the top row — crumbs, prompt, status cluster — in logical points.
///
/// [`CHROME_HEIGHT`] **plus [`GAP`]**, and the extra eight points are not a
/// second opinion about how tall a bar should be: they are the gap above the
/// strip, claimed back.
///
/// The strip has no top edge of its own. Its inactive tabs are the window
/// ground (see [`crate::chrome::tab_strip`]), so there is nothing to mark
/// where the strip begins, and the block a reader sees as *the tab bar* runs
/// from the window's edge down to the strip's foot — [`GAP`] plus
/// [`CHROME_HEIGHT`]. The top row's plate, sized to the strip's own 30, was
/// the same height and looked eight points shorter, because it was the only
/// one of the two whose top edge you could see. Matching the *block* is what
/// makes the two read as one rhythm. Derived, never picked.
pub const TOP_HEIGHT: f32 = CHROME_HEIGHT + GAP;

/// What a second line costs the top row, in logical points (see [`layout`]).
///
/// A line of the chrome's own face plus its leading — the row is not gaining
/// another chip's worth of padding, only somewhere to put a sentence.
pub const PROMPT_ERROR_LINE: f32 = 17.0;

/// The narrowest each pane is laid out at while it is open, in logical points:
/// parent, list, preview.
///
/// The parent's is a column of short names — an icon and a word or two —
/// which is what it is for: saying where you came from. The list's is the
/// one a name and its linemode can still share a row at. The preview's is
/// the one a page, a picture or a transport strip is still worth drawing
/// at. Below these a pane is not narrow, it is broken, so a divider stops
/// here (and folds a side pane past it — see [`crate::divider`]), and a
/// window resized under a ratio that would go below one takes the width
/// from the list first, which is the pane with the most to spare.
pub const PARENT_MIN: f32 = 96.0;
pub const LIST_MIN: f32 = 160.0;
pub const PREVIEW_MIN: f32 = 180.0;

/// How far a divider's grab zone reaches onto the plate either side of its
/// gap, in logical points. The gap alone is [`GAP`] wide, which is a target
/// the hand has to aim at; two points either side makes it twelve, and costs
/// no row a click, because rows are inset [`ROW_INSET`] from the plate's edge.
pub const DIVIDER_OVERLAP: f32 = 2.0;

/// A drop target's ring, in logical points. Two: the same weight as the accent
/// rules the chrome wears elsewhere, because it is the same kind of statement —
/// "this is the one" — and a second thickness would be a second vocabulary for
/// one idea.
const DROP_RING: f32 = 2.0;

/// How far a live drop target's ground is washed towards the accent. Under the
/// selection tint: quieter than "these are marked", which is exactly where
/// "let go and it lands here" belongs.
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
/// [`IGNORED_DIM`] instead — a dot would be a mark drawing the eye to the one
/// row in the pane that is asking for less of it.
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
}

/// The widths the right-hand columns keep **whatever an individual row says**.
///
/// Measured once per listing and handed to every row in it, for the reason
/// [`GIT_DOT_COLUMN`] is a constant: a column whose width is whatever this
/// row's text happens to need is a column that reflows the name beside it every
/// time the text changes. The size column changes constantly — a directory goes
/// `—` → `12 items` → `~4.2 MB` → `4.2 MB` while the walk runs — and a name
/// re-ellipsised four times per directory reads as a rendering fault
/// (`delightful-ui` §8).
///
/// Once per listing rather than once per row because measuring text means
/// laying it out, and the answer is the same for all sixty rows on screen.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct RowColumns {
    /// The widest thing the size column can ever hold.
    pub size: f32,
    /// The widest number "what's big" can put beside a bar, `≈` and all.
    ///
    /// The bar is placed just left of the number column, so while that column
    /// was as wide as each row's own text, every bar's right edge sat somewhere
    /// different: `≈4.2 MB` pushed its bar further left than `12.0 KB` did, and
    /// a column whose whole job is to be compared by eye had no common edge to
    /// compare against. Reserved at this width, the numbers right-align inside
    /// one strip and every track starts and ends at the same x.
    pub usage: f32,
    /// How big this listing's rows are drawn ([`Scale`]).
    ///
    /// It rides along here rather than as a tenth argument to [`Painting::row`]
    /// because the two are the same fact measured twice: the widths above were
    /// laid out at this face, and a row drawn at another one would truncate its
    /// name against a column measured for a different listing.
    pub scale: Scale,
}

/// The size-column strings that fight for widest, **produced by the same
/// function that fills the column** rather than written out by hand.
///
/// The hand-written pair used to be `["999 items", "999.9 GB"]`, and it was
/// wrong at both ends. A count that hits [`df_core::du::MAX_COUNTED_ENTRIES`]
/// says `10,000+ items`, which is four characters longer than any `999 items`;
/// and the tilde on a running size is *not* narrower than nothing at all — a
/// proportional font gives `~999.9 GB` more width than `999.9 GB`, and the
/// column that had been measured without it reflowed the name beside it the
/// first time a big folder started counting.
///
/// Derived, so the answer cannot drift from the formats again: change
/// [`crate::format::folder_size_text`] and this follows.
fn size_column_widest() -> [String; 4] {
    use df_core::du::{ChildCount, MAX_COUNTED_ENTRIES};

    let size = |bytes: u64, settled: bool| crate::folders::Size { bytes, settled };
    let count = |entries: u64, capped: bool| ChildCount { entries, capped };
    let text = |size, count| crate::format::folder_size_text(size, count).unwrap_or_default();
    [
        text(None, Some(count(999, false))),
        text(None, Some(count(MAX_COUNTED_ENTRIES, true))),
        text(Some(size(WIDEST_DIRECTORY_BYTES, true)), None),
        text(Some(size(WIDEST_DIRECTORY_BYTES, false)), None),
    ]
}

/// The largest total [`crate::format::human_size`] prints without reaching
/// petabytes: `1023.9 TB`, one carry short of `1.0 PB`.
///
/// The judgement the old constant made and the only part of it worth keeping —
/// the column is measured for a directory somebody is browsing, and reserving
/// room for the exabyte a `u64` can technically hold would push every name in
/// every listing in for a number no filesystem will ever produce.
const WIDEST_DIRECTORY_BYTES: u64 = 1_125_789_955_679_846;

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
    /// A scrollbar's band, thumb and track alike, whichever bar it is — a
    /// pane's, a floating card's, a menu's, the bulk rename card's
    /// ([`crate::scrollbar::Bar`]), and only while its list is longer than
    /// it shows. The thumb is dragged, the track pages, and the pointer on
    /// the band brings the bar up.
    Bar(crate::scrollbar::Bar),
    /// A divider between two panes ([`crate::divider`]): dragged to trade
    /// width between them, double-clicked to fold the side pane away or open
    /// it again.
    Divider(crate::divider::Divider),
    /// A chip in the tab strip, by tab index.
    Tab(usize),
    /// The `×` a chip wears in its numeral's slot while the pointer is on it,
    /// by tab index: the pointer's `Ctrl+c` for *that* tab, which need not be
    /// the one on screen. Its own variant rather than a part of [`Control::Tab`]
    /// so a press on it closes and never picks the chip up to drag.
    TabClose(usize),
    /// The `+` after the last chip: the pointer's `t`.
    TabNew,
    /// A button on a floating surface — a dialog's answers, the conflict
    /// resolver's apply-to-all — by its index in that surface's own list.
    Action(usize),
    /// A row inside a floating surface: the task panel's tasks, the opener
    /// picker's choices, the conflict resolver's names.
    PanelRow(usize),
    /// The `×` at a floating card's top-right corner: the pointer's `Esc`.
    /// One variant serves every card, because only one closable surface is
    /// ever under the pointer.
    Close,
    /// A hint on the open card's strip that runs what its key runs, by its
    /// index in that strip's list ([`crate::chrome::Hint`]). One variant for
    /// every card, for the reason [`Control::Close`] is one.
    Hint(usize),
    /// A row of the which-key card, by its index in the card's rows: a click
    /// presses that row's key into the pending chord
    /// ([`crate::whichkey::Row`]).
    WhichKey(usize),
    /// The two halves of the search panel's **Names | Contents** switch
    /// (PLAN §7.2). Two variants rather than one carrying the mode, so this
    /// enum stays free of what the search module means by a mode — the
    /// panel's own geometry maps between them ([`crate::overlay::switch_control`]).
    SearchNames,
    SearchContents,
    /// A segment of the breadcrumb path bar, from the root rightwards
    /// (PLAN §2).
    Crumb(usize),
    /// The committed filter's trailing chip on the top row, which re-opens the
    /// prompt that set it (PLAN §7.2).
    FilterChip,
    /// A file dialog's type-filter chip on the top row — "Images", or "All
    /// files" — which drops the dialog's list of filters out beneath it.
    TypeChip,
    /// The clipboard chip on the top row, which opens and closes the tray that
    /// lists what is carried — the pointer's `B` (PLAN §4.1, §7.1). Pressed
    /// and pulled, it drags the whole clipboard out as one payload.
    YankChip,
    /// A row of the open menu — the right-click one or the app menu — and a
    /// row of the submenu it has flown out (PLAN §7.5).
    MenuItem(usize),
    SubmenuItem(usize),
    /// The three bars at the top row's leading end, which drop the app menu
    /// out: every command a person might not know the key for, where the
    /// pointer can find it.
    MenuButton,
    /// The yank tray's rows, each row's `×`, and the header's `Clear` — the
    /// pointer's `X` (PLAN §7.1, [`crate::tray`]).
    YankRow(usize),
    YankRemove(usize),
    YankClear,
    /// The leading `…` the breadcrumb wears when the path did not fit. Nothing
    /// happens when it is clicked — there is no one segment it stands for — but
    /// it is the only place the hidden part of the path can be asked for, so it
    /// takes the pointer in order to answer.
    CrumbEllipsis,
    /// The `112 / 197` at the right-hand end of the top row, which opens `/`
    /// (PLAN §7.2): the number says where in the listing you are, and `/` is
    /// how you go somewhere else in it.
    Counter,
    /// The branch chip. Hover-only — see [`crate::app`]'s click routing.
    GitChip,
    /// The trash's `37 items · 1.2 GB`, whose tooltip says how long the trash
    /// keeps things (PLAN §7.4). Hover-only, as the branch chip is.
    TrashChip,
    /// `4 selected`, which clears the selection: the pointer's `Esc`.
    SelectedChip,
    /// `visual` / `visual unset`, which leaves the run.
    VisualChip,
    /// A picker session's primary button at the far right of the top row —
    /// `Select`, `Choose folder` or `Save` — which answers the dialog.
    PickButton,
    /// …and the quiet `Cancel` beside it, which closes the dialog having
    /// picked nothing.
    CancelButton,
    /// The toast itself, which a click dismisses, and the offer chip on an undo
    /// toast, which a click takes (PLAN §5).
    Toast,
    ToastAction,
    /// The text of the prompt that has taken the top row: a click puts the
    /// caret there, a drag selects, two clicks take a segment and three the
    /// line ([`crate::chrome::FieldGeom`]). Not a button — it has no hover or
    /// press of its own to draw, and no ripple — but it is under the pointer
    /// like one, and the click counter keys on it.
    PromptField,
    /// The bulk rename card's template field: text like
    /// [`Control::PromptField`], taking its press at the press site with a
    /// click count, and no hover, press or ripple of its own.
    BulkTemplate,
    /// The new name on one of the rename card's rows, by its place among the
    /// rows drawn (the card's `first` turns it into a row of the card). Text,
    /// as the template is.
    BulkRow(usize),
    /// A candidate in the rename card's `{` list, by its index in the list.
    BulkCandidate(usize),
}

/// How the three panes share the window this frame: what [`layout`] is asked
/// to lay out.
///
/// Built once a frame by the app from the widths the state file keeps, a
/// divider in the hand, and whatever is folding or springing back. Three
/// numbers rather than one ratio because the panes move in three different
/// ways — a share that follows a drag, an opening that folds a pane and its
/// gap together, and a rubber band that is points, not a share, because it is
/// bounded in points — and folding them into one set of widths before the
/// layout had seen the window would lose the minimums.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Split {
    /// Parent, list, preview: each pane's share of the width the panes have
    /// between them — the window less its margins and **both** gaps, so the
    /// shares mean the same thing whichever panes are open. A folded pane is
    /// here at the share it would open to, and [`Split::open`] is what folds
    /// it.
    pub fractions: [f32; 3],
    /// How open the parent and the preview are: 1 open, 0 folded away. A
    /// pane's width *and the gap beside it* are both scaled by this, and what
    /// they give up is the list's — so a folded pane leaves the list at the
    /// window's margin rather than a gap away from it.
    pub open: [f32; 2],
    /// How far past its share each divider is being pulled, in points added
    /// to the parent's and the preview's width: the rubber band past a
    /// minimum and the lag of a snap. The list pays for it; the third pane
    /// never does.
    pub overshoot: [f32; 2],
    /// Whether the parent and the preview are folded, as the rest of the
    /// window means it: a folded pane's divider is grabbed at the window's
    /// margin, and nothing in the pane is pointed at. True from the moment a
    /// pane starts folding, false from the moment it starts to open.
    pub collapsed: [bool; 2],
}

impl Split {
    /// The config's `ratio`, at rest — nothing folding, nothing pulled, and
    /// a folded pane's share already the list's: what the tests lay out.
    #[cfg(test)]
    pub fn at(ratio: [u16; 3]) -> Split {
        let panes = df_core::state::Panes::from_ratio(ratio);
        Split {
            fractions: panes.ratio,
            open: [
                if panes.parent_collapsed { 0.0 } else { 1.0 },
                if panes.preview_collapsed { 0.0 } else { 1.0 },
            ],
            overshoot: [0.0; 2],
            collapsed: [panes.parent_collapsed, panes.preview_collapsed],
        }
    }
}

/// Where the panes and the chrome go.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Layout {
    /// The tab strip, when there is more than one tab (PLAN §2).
    pub strip: Option<egui::Rect>,
    /// The top row (PLAN §2): the breadcrumbs and the status cluster, or the
    /// prompt that has taken their place.
    ///
    /// Directly under the strip, touching it, because a tab *contains* a path:
    /// the strip says which session you are in and the row says where that
    /// session is, and the active tab is drawn joined to this row to say so.
    /// Always reserved — a pane that
    /// changed height when a second tab opened would move rows under the
    /// pointer.
    pub path: egui::Rect,
    /// The three panes. A folded one is a rect of no width at the edge it
    /// folded to — never inverted, so everything measured off it stays sane —
    /// but a zero-width rect still *contains* the points on its edge, so ask
    /// [`Layout::in_parent`] and [`Layout::in_preview`] rather than the rects
    /// whether the pointer is in one.
    pub parent: egui::Rect,
    pub list: egui::Rect,
    pub preview: egui::Rect,
    /// Where each divider is taken hold of: parent|list, then list|preview.
    ///
    /// The gap between the two plates, the panes' full height, and
    /// [`DIVIDER_OVERLAP`] onto each plate. A folded pane's divider is the
    /// window's margin on that side instead, reaching the same two points
    /// onto the list — the only place left to take hold of a pane that is not
    /// there.
    pub dividers: [egui::Rect; 2],
    /// Where the hairline a held divider draws goes: the middle of its gap,
    /// or of the margin once the pane beside it has folded, and between the
    /// two while it folds, so the line never jumps.
    pub hairlines: [f32; 2],
    /// [`Split::collapsed`], carried for the two questions below.
    pub collapsed: [bool; 2],
    /// The width the panes have between them — the window less its margins
    /// and both gaps — which is what a [`Split`]'s shares are shares of.
    pub usable: f32,
}

impl Layout {
    /// Whether `at` is over the parent pane, and there is a parent pane to be
    /// over.
    pub fn in_parent(&self, at: egui::Pos2) -> bool {
        !self.collapsed[0] && self.parent.contains(at)
    }

    /// Whether `at` is over the preview pane, and it is open.
    pub fn in_preview(&self, at: egui::Pos2) -> bool {
        !self.collapsed[1] && self.preview.contains(at)
    }
}

/// Split the window into miller columns at `split` (PLAN §2), with the tab
/// strip and the top row above them.
///
/// The gaps come out of the total *before* the shares are applied, so
/// `[1, 4, 3]` describes the panes themselves rather than the panes plus the
/// spaces between them — otherwise the middle column would quietly shrink as
/// the gap grew. Both gaps, always: a folded pane's gap goes to the list along
/// with its width, so a share means the same number of points whether the
/// pane beside it is open or not.
///
/// `path_lines` is how many lines the top row needs: one in browse mode and
/// while most prompts are open, two only while a prompt is showing an error
/// that will not fit beside its query (see [`crate::chrome::prompt_lines`]).
/// The panes reflow under it, which is the one place they may: the alternative
/// is an error message clipped to three characters, and a prompt that cannot
/// say what is wrong with what you typed is worse than a list that moved a row
/// while you were typing.
pub fn layout(area: egui::Rect, split: &Split, tab_strip: bool, path_lines: usize) -> Layout {
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
    // **Flush**, with no gap: the active tab is drawn as a folder tab joined to
    // this row and filled with its ground (see [`crate::chrome::tab_strip`]),
    // and a gap between them would be a seam through the middle of one shape.
    let top = match strip {
        Some(strip) => strip.bottom(),
        None => outer.top(),
    };
    // The second line is a *line*, not a second row: it is the same plate
    // carrying one more baseline, so it grows by the height of a line of text
    // rather than by another TOP_HEIGHT of padding.
    let path_height = TOP_HEIGHT + (path_lines.max(1) - 1) as f32 * PROMPT_ERROR_LINE;
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
    let usable = (inner.width() - GAP * 2.0).max(0.0);
    let widths = pane_widths(usable, split);
    let open = split.open.map(|o| o.clamp(0.0, 1.0));

    let mut x = inner.left();
    let mut next = |w: f32, gap: f32| {
        let rect =
            egui::Rect::from_min_size(egui::pos2(x, inner.top()), egui::vec2(w, inner.height()));
        x += w + gap;
        rect
    };
    let parent = next(widths[0], GAP * open[0]);
    let list = next(widths[1], GAP * open[1]);
    let preview = next(widths[2], 0.0);

    let band = |left: f32, right: f32| {
        egui::Rect::from_min_max(
            egui::pos2(left, inner.top()),
            egui::pos2(right.max(left), inner.bottom()),
        )
    };
    let dividers = [
        if split.collapsed[0] {
            band(area.left(), inner.left() + DIVIDER_OVERLAP)
        } else {
            band(
                parent.right() - DIVIDER_OVERLAP,
                list.left() + DIVIDER_OVERLAP,
            )
        },
        if split.collapsed[1] {
            band(inner.right() - DIVIDER_OVERLAP, area.right())
        } else {
            band(
                list.right() - DIVIDER_OVERLAP,
                preview.left() + DIVIDER_OVERLAP,
            )
        },
    ];
    // The middle of the gap, blended towards the middle of the margin by how
    // far the pane has folded. At either end it is exactly one of the two.
    let margin = [area.left() + GAP / 2.0, area.right() - GAP / 2.0];
    let gap_mid = [
        (parent.right() + list.left()) / 2.0,
        (list.right() + preview.left()) / 2.0,
    ];
    let hairlines = [0, 1].map(|i| gap_mid[i] * open[i] + margin[i] * (1.0 - open[i]));
    Layout {
        strip,
        path,
        parent,
        list,
        preview,
        dividers,
        hairlines,
        collapsed: split.collapsed,
        usable,
    }
}

/// The three panes' widths across `usable` points, with the list holding
/// whatever a folding pane and its gap have given up — so the returned widths
/// and the two gaps still standing add up to the row.
///
/// In this order, and the order is the point: the shares; the minimums, paid
/// for by the list first; the dividers' pull; the folding. The minimums come
/// before the pull so a rubber band can draw a pane *under* its minimum — that
/// is what a rubber band is — and before the folding so a pane shrinks to
/// nothing from the width it was drawn at rather than from a share nobody saw.
fn pane_widths(usable: f32, split: &Split) -> [f32; 3] {
    let fractions = split
        .fractions
        .map(|f| if f.is_finite() { f.max(0.0) } else { 0.0 });
    let total: f32 = fractions.iter().sum();
    let mut widths = if total > 0.0 {
        fractions.map(|f| usable * f / total)
    } else {
        [usable / 3.0; 3]
    };
    let open = split.open.map(|o| o.clamp(0.0, 1.0));
    // A pane folded all the way has no minimum: holding room for it would be
    // width the list could never have back. And the list's is what it will
    // be *drawn* at, so whatever a folding pane and its gap are about to hand
    // it counts towards it.
    let handed = (widths[0] + GAP) * (1.0 - open[0]) + (widths[2] + GAP) * (1.0 - open[1]);
    let minimums = [
        if open[0] > 0.0 { PARENT_MIN } else { 0.0 },
        (LIST_MIN - handed).max(0.0),
        if open[1] > 0.0 { PREVIEW_MIN } else { 0.0 },
    ];
    with_minimums(&mut widths, minimums, usable);
    // The pull, traded with the list alone, and never past nothing.
    for (i, pull) in [(0, split.overshoot[0]), (2, split.overshoot[1])] {
        let pull = if pull.is_finite() { pull } else { 0.0 };
        let pull = pull.max(-widths[i]).min(widths[1]);
        widths[i] += pull;
        widths[1] -= pull;
    }
    for (i, open) in [(0, open[0]), (2, open[1])] {
        widths[1] += (widths[i] + GAP) * (1.0 - open);
        widths[i] *= open;
    }
    widths
}

/// Raise every pane under its minimum to it, paid for by the list first —
/// the pane with the most to spare, and the one whose width is not a choice
/// anybody made by dragging — and then by the side panes, each by how far it
/// is over its own. A window too narrow for all three minimums at once is
/// left at its shares: there is no fair way to break two promises.
fn with_minimums(widths: &mut [f32; 3], minimums: [f32; 3], usable: f32) {
    if minimums.iter().sum::<f32>() > usable {
        return;
    }
    let mut owed = 0.0;
    for (width, minimum) in widths.iter_mut().zip(minimums) {
        if *width < minimum {
            owed += minimum - *width;
            *width = minimum;
        }
    }
    let list_spare = (widths[1] - minimums[1]).max(0.0);
    let from_list = list_spare.min(owed);
    widths[1] -= from_list;
    owed -= from_list;
    let spare = [0, 2].map(|i| (widths[i] - minimums[i]).max(0.0));
    let total_spare = spare[0] + spare[1];
    if owed > 0.0 && total_spare > 0.0 {
        let owed = owed.min(total_spare);
        widths[0] -= owed * spare[0] / total_spare;
        widths[2] -= owed * spare[1] / total_spare;
    }
}

/// The area inside a pane that rows are drawn in.
pub fn content_rect(pane: egui::Rect) -> egui::Rect {
    pane.shrink(ROW_INSET)
}

/// One row's rectangle, given how far the view has scrolled (in rows).
///
/// `row_height` is [`Scale::row_height`] — the constant is only the *smallest*
/// step's answer now (see [`Scale`]), and a caller that reached past this
/// argument for [`ROW_HEIGHT`] would draw a scaled listing on unscaled
/// geometry: rows overlapping, and a click landing two names away.
pub fn row_rect(
    content: egui::Rect,
    scroll_rows: f32,
    index: usize,
    row_height: f32,
) -> egui::Rect {
    let top = content.top() + (index as f32 - scroll_rows) * row_height;
    egui::Rect::from_min_size(
        egui::pos2(content.left(), top),
        egui::vec2(content.width(), row_height),
    )
}

/// Which row a point is over, if any. `rows` bounds it so the empty space below
/// a short listing is not row 400.
///
/// The exact inverse of [`row_rect`], and it has to stay one — including the
/// `row_height` it is asked at.
pub fn row_at(
    content: egui::Rect,
    scroll_rows: f32,
    rows: usize,
    pos: egui::Pos2,
    row_height: f32,
) -> Option<usize> {
    if !content.contains(pos) || rows == 0 || row_height <= 0.0 {
        return None;
    }
    let offset = (pos.y - content.top()) / row_height + scroll_rows;
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
    /// How big this pane's rows are (PLAN §4.1's view-scale ladder).
    ///
    /// The list pane and the parent column are handed the **same** one, because
    /// they are two columns of one listing and a parent whose rows were a
    /// different height would put the directory you are in out of line with the
    /// one you came from. The preview pane's directory body keeps the default:
    /// it is a picture of a folder, not the folder you are steering.
    pub scale: Scale,
    pub column: Column,
    pub hovers: &'a Hovers<Control>,
    pub ripples: &'a Ripples<Control>,
    /// What the cursor row is lifted *towards*. The list uses `surface1`; the
    /// parent's marker is a step quieter, because it reports where you are
    /// rather than what you are about to act on.
    pub cursor_fill: egui::Color32,
    /// How strongly the cursor row is lit: 1 in the list, whose cursor is the
    /// one the keys move, and [`PARENT_MARKER`] in the parent column, whose
    /// marker only says where you are.
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
    /// Whether the rows' names are paths — a search's hits (PLAN §7.2), each
    /// named by its path from the root the search ran in. The folders are
    /// drawn in the quiet ink and the file's own name in the row's, cut from
    /// the front when the column is narrow ([`crate::hits`]).
    pub paths: bool,
    /// What the background walk has said about this pane's directories
    /// (PLAN §7.3), or `None` when folder sizes are off, when the listing is
    /// virtual, or in a pane whose linemode is not the size.
    ///
    /// It fills in the size column for directory rows only, and only when
    /// neither [`ListView::usage`] nor [`ListView::notes`] has claimed that
    /// column first — the three are the same strip of pixels and the one that
    /// was asked for most explicitly wins.
    pub folders: Option<&'a crate::folders::Folders>,
    /// A quieter line under the pane's "empty", when the place has something
    /// to say about why it is — the trash's clock (PLAN §7.4). Only under a
    /// truly empty listing: "no matches" and "3 hidden" are about the filter
    /// and `.`, not about the place.
    pub empty_note: Option<&'a str>,
}

/// The shared state a paint pass needs. Bundled because every function below
/// wants all of it and threading nine arguments through each one is how a
/// painter grows a different palette in one corner.
pub struct Painting<'a> {
    pub painter: &'a egui::Painter,
    pub palette: &'a Palette,
    pub theme: &'a Theme,
    /// What colour each tag is ([`crate::tags`]), for a row's dots and the
    /// spot panel's `Tags` row.
    pub tags: &'a crate::tags::TagColors,
    /// Whether the real icon glyphs can be drawn (see [`crate::icons`]).
    pub nerd: bool,
    pub show_symlink: bool,
    pub now: Instant,
    /// Where the pointer is, and somewhere to leave what it turned out to be
    /// over inside a row. `None` everywhere the answer is not wanted — the
    /// preview's directory body, the tests.
    pub tips: Option<&'a RowTips>,
    /// The bar a hand is on this frame, which whatever paints that bar draws
    /// held: a card's ([`crate::scrollbar::paint_card`]), a menu's, the bulk
    /// rename card's.
    pub held: Option<crate::scrollbar::Bar>,
}

/// One of the small marks at the right-hand end of a row that is worth a word
/// when the pointer stops on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowTip {
    /// A directory size still being walked — the `~` (PLAN §7.3).
    Counting,
}

impl RowTip {
    /// The whole tooltip: one short line, because a mark this small is being
    /// pointed at for one missing word and not for a paragraph.
    pub fn text(self) -> &'static str {
        match self {
            RowTip::Counting => "still counting",
        }
    }
}

/// The pointer, offered to the row painter so it can say what is under it.
///
/// The marks at the right-hand end of a row are laid out as the painter goes,
/// right to left, off widths it measures on the spot. Working out where they
/// landed a second time in the hit test would be that arithmetic written twice,
/// and the second copy is the one that drifts. So the painter is asked instead:
/// it is handed the pointer and leaves behind whichever mark contained it.
pub struct RowTips {
    at: egui::Pos2,
    found: std::cell::Cell<Option<(RowTip, egui::Rect)>>,
}

impl RowTips {
    pub fn new(at: egui::Pos2) -> RowTips {
        RowTips {
            at,
            found: std::cell::Cell::new(None),
        }
    }

    /// A mark was drawn here. Kept only if the pointer is inside it — and only
    /// the first, since two marks on one row never overlap.
    pub(crate) fn note(&self, tip: RowTip, rect: egui::Rect) {
        if self.found.get().is_none() && rect.contains(self.at) {
            self.found.set(Some((tip, rect)));
        }
    }

    pub fn found(&self) -> Option<(RowTip, egui::Rect)> {
        self.found.get()
    }
}

impl Painting<'_> {
    /// A pane's background.
    ///
    /// Every pane is painted the same: the keyboard is always in the list
    /// (PLAN §2.1), so there is no "focused pane" treatment left to wear — no
    /// accent rule across the top, no 4% tint — and a mark that never moves is
    /// a mark that says nothing.
    pub fn pane(&self, rect: egui::Rect, fill: egui::Color32) {
        self.painter.rect_filled(rect, PANE_RADIUS, fill);
    }

    /// Measure the fixed right-hand columns for one listing. See
    /// [`RowColumns`].
    pub(crate) fn row_columns(&self, painter: &egui::Painter, scale: Scale) -> RowColumns {
        let font = egui::FontId::proportional(scale.font);
        let size = size_column_widest()
            .into_iter()
            .map(|text| {
                painter
                    .layout_no_wrap(text, font.clone(), self.palette.quiet)
                    .size()
                    .x
            })
            .fold(0.0, f32::max);
        // The same derivation as the size column's: the strings come from the
        // function that draws them, so the `≈` is measured as the glyph it is
        // rather than assumed to be free.
        let usage = [true, false]
            .into_iter()
            .map(|estimate| {
                let widest = crate::usage::RowUsage {
                    fraction: 0.0,
                    bytes: WIDEST_DIRECTORY_BYTES,
                    estimate,
                    growth: 0.0,
                };
                painter
                    .layout_no_wrap(widest.label(), font.clone(), self.palette.quiet)
                    .size()
                    .x
            })
            .fold(0.0, f32::max);
        RowColumns { size, usage, scale }
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
            scale,
            column,
            hovers,
            ripples,
            cursor_fill: cursor_color,
            cursor_alpha,
            linemode,
            dim,
            slow_load,
            show_selection,
            clip,
            dragged,
            flip,
            git,
            usage,
            notes,
            paths,
            folders,
            empty_note,
        } = view;
        let content = content_rect(pane);
        if let Some(message) = self.pane_state_message(dir, slow_load) {
            // A search that found nothing did not find an empty folder.
            let message = match message.as_str() {
                "empty" if paths => "nothing found".to_string(),
                _ => message,
            };
            self.quiet_label(content, &message);
            self.empty_note(content, dir, empty_note);
            return;
        }
        // Rows are clipped to the pane's content box so a row half-scrolled off
        // the top does not paint over the pane above it mid-slide.
        let painter = self.painter.with_clip_rect(content);
        let columns = self.row_columns(&painter, scale);
        let visible = crate::viewport::visible_rows(content.height(), scale.row_height);
        let first = scroll_rows.floor().max(0.0) as usize;
        // One extra row at each end: mid-slide, the rows entering and leaving
        // are both partly on screen.
        let last = (first + visible + 1).min(dir.len().saturating_sub(1));

        for index in first..=last {
            let Some(entry) = dir.row(index) else {
                continue;
            };
            let rect = row_rect(content, scroll_rows, index, scale.row_height);
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
            // selection, lifted to the cursor's fill for the cursor row and
            // towards the hover's for a pointer — the last two steps up the
            // palette's own ramp, both derived in [`crate::theme`] so the list
            // and the grid cannot drift apart. The selection tint goes on
            // *first* so the cursor still
            // reads as the brightest thing in the column when it is standing on
            // a selected row.
            let ground_here = if selected {
                crate::theme::select_fill(self.palette, ground)
            } else {
                ground
            };
            let glow = f32::from(on_cursor) * cursor_alpha;
            // On a selected row the cursor stands on the selection's wash:
            // the same fill on a dark palette, a third colour on a light one
            // ([`crate::theme::cursor_on_selection`]).
            let cursor_here = if selected {
                crate::theme::cursor_on_selection(self.palette, ground_here, cursor_color)
            } else {
                cursor_color
            };
            let base = mix(ground_here, cursor_here, glow);
            let lift = if glow > 0.5 {
                CURSOR_HOVER_LIFT
            } else {
                HOVER_LIFT
            };
            let fill = crate::theme::lift(self.palette, base, hover * lift);
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
                    crate::theme::splash(self.palette, splash.alpha),
                );
            }

            // git's word for this row, asked once. `status_for` is a hash
            // lookup plus — only for a repository that reported collapsed
            // directories — a walk up the ancestors, so a pane of rows in a
            // clean repository costs one miss each.
            let status = git.and_then(|g| g.status_for(&entry.path));
            // Hidden rows are muted the same step as gitignored ones: both are
            // things the listing shows you but the machine would rather you
            // looked past.
            let ignored = status == Some(df_core::git::FileStatus::Ignored) || entry.is_hidden;

            self.row(
                &painter,
                rect,
                entry,
                dir.row_spans(index),
                ground,
                fill,
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
                },
                usage.map(|usage| {
                    // A directory's weight comes from the walk; a file's is
                    // simply its own length, which is known and final from the
                    // moment the row was scanned.
                    //
                    // A directory the walk has not reached and nothing
                    // remembered is still counting, whatever its `len` says:
                    // its `0 B` is the stat size, not a measurement, and a
                    // number that is about to be replaced wears the `≈` like
                    // every other one that is. Once the walk has finished it
                    // is a directory the walk never entered, and the zero is
                    // as settled as it is going to get.
                    let weight = usage.weight(&entry.name);
                    let bytes = weight.map(|w| w.bytes).unwrap_or(entry.len);
                    let estimate = match weight {
                        Some(w) => !w.settled,
                        None => entry.is_dir() && !usage.done,
                    };
                    crate::usage::RowUsage {
                        fraction: usage.fraction(bytes),
                        bytes,
                        estimate,
                        growth: usage.growth(index.saturating_sub(first), self.now),
                    }
                }),
                notes
                    .and_then(|notes| notes.get(&entry.name))
                    .map(String::as_str),
                paths,
                folders,
                columns,
            );
        }
        self.flip_ghosts(&painter, flip, content, ROW_RADIUS);
    }

    /// One row's contents: icon, name, its tag dots, and the linemode column.
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
        // The colour the row is lit with — the pane's, the cursor's, a
        // hover's — which the tag dots wear as a ring, so where two overlap
        // the one on top is cut out of the one under it.
        behind: egui::Color32,
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
        // The name is a path from a search's root ([`ListView::paths`]).
        path_name: bool,
        // Recursive directory sizes, when they are being measured for this
        // pane (PLAN §7.3). Last in precedence: it fills in the one thing the
        // plain linemode cannot say, and gets out of the way of the two modes
        // that own the column outright.
        folders: Option<&crate::folders::Folders>,
        // What the right-hand columns keep whatever this row says, measured
        // once for the whole listing (see [`RowColumns`]).
        columns: RowColumns,
    ) {
        // How big this listing draws itself — measured once for the whole
        // pane and handed down with the column widths it produced.
        let scale = columns.scale;
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
            egui::FontId::new(scale.icon, icon_family),
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
        // …but the *reservation* is fixed, in the size linemode and in the
        // "what's big" column: those are the two whose text changes under you
        // while a directory is being measured. A permission string and a
        // timestamp are the same width on every row already, and a virtual
        // listing's `note` does its own arithmetic.
        //
        // The usage column has a second reason. Its bar is placed from this
        // width, so a reservation that varied with the row's number put the
        // bars' right edges at a different x on every row; reserved, the
        // numbers right-align in one strip and the bars line up to be compared.
        let reserved = if usage.is_some() {
            columns.usage
        } else if note.is_none() && linemode == LineMode::Size {
            columns.size
        } else {
            0.0
        };
        let mode_width = if mode_text.is_empty() && reserved <= 0.0 {
            0.0
        } else {
            let font = egui::FontId::proportional(scale.font);
            // Two columns have no natural width, and each stops at a share of
            // the row and says the rest with `…`.
            let share = match (note, path_name) {
                // A content hit's matched line is as long as the line was, and
                // this column is laid out before the name: uncapped, one long
                // line would take the whole row and leave the name nothing. So
                // it gets at most [`NOTE_SHARE`] of the row, cut at its end.
                (Some(_), true) => Some(NOTE_SHARE),
                // A file can carry any number of tags, and a column that took
                // what it liked would leave the name nothing. `m t` is a
                // glance, and the spot panel lists them all.
                (None, _) if usage.is_none() && linemode == LineMode::Tags => {
                    Some(TAGS_COLUMN_SHARE)
                }
                _ => None,
            };
            let galley = match share {
                Some(share) => {
                    use egui::text::{LayoutJob, TextFormat, TextWrapping};
                    let mut job = LayoutJob::single_section(
                        mode_text.clone(),
                        TextFormat {
                            font_id: font,
                            color: fade(self.palette.quiet),
                            ..Default::default()
                        },
                    );
                    job.wrap = TextWrapping {
                        max_width: (rect.width() * share).max(0.0),
                        max_rows: 1,
                        break_anywhere: true,
                        overflow_character: Some('…'),
                    };
                    painter.layout_job(job)
                }
                None => painter.layout_no_wrap(mode_text.clone(), font, fade(self.palette.quiet)),
            };
            let width = galley.size().x;
            // Right-aligned inside the reserved column, which for a column
            // whose right edge is the row's is the same place it was drawn
            // before — what changed is only what the name is measured against.
            painter.galley(
                egui::pos2(
                    rect.right() - ROW_PAD_X - width,
                    rect.center().y - galley.size().y / 2.0,
                ),
                galley,
                fade(self.palette.quiet),
            );
            // A number that is still moving is worth a word: `~` is the mark
            // and "still counting" is what it means, and the mark is the only
            // place in the window the question can be asked.
            let counting = usage.as_ref().is_some_and(|usage| usage.estimate)
                || (usage.is_none()
                    && note.is_none()
                    && linemode == LineMode::Size
                    && entry.is_dir()
                    && folders
                        .and_then(|folders| folders.size(&entry.name))
                        .is_some_and(|size| !size.settled));
            if let (Some(tips), true) = (self.tips, counting) {
                tips.note(
                    RowTip::Counting,
                    egui::Rect::from_min_max(
                        egui::pos2(rect.right() - ROW_PAD_X - width, rect.top()),
                        egui::pos2(rect.right() - ROW_PAD_X, rect.bottom()),
                    ),
                );
            }
            width.max(reserved) + LINEMODE_GAP
        };

        // The bar, immediately left of the number's reserved strip, so the
        // column reads as one measurement rather than as a graphic and a
        // caption. Against the strip and not the number itself, so a short
        // number leaves a little air on its left rather than moving its bar.
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

        // A dot per coloured tag, right after the name (see [`crate::tags`]).
        // Their room comes out of the name's, so a long name is cut short
        // before the dots rather than under them.
        let dots = self.tags.dots(&entry.tags, self.palette);
        let dots_room = if dots.is_empty() {
            0.0
        } else {
            crate::tags::DOT_GAP + crate::tags::dots_width(dots.len())
        };
        let name_left = rect.left() + ROW_PAD_X + scale.icon_column;
        let name_room = (rect.right() - ROW_PAD_X - mode_width - name_left - dots_room).max(0.0);
        // The part `f` or `/` matched, in the one colour on the palette that
        // is neither a file type nor the selection: the highlight has to be
        // readable as "this is why the row is here" and nothing else — so on
        // a light palette it is deepened as type is ([`crate::theme::ink`]).
        let matched = fade(crate::theme::ink(self.palette, self.palette.sky));
        let name_end = if path_name {
            self.path_spans(
                painter,
                egui::pos2(name_left, rect.center().y),
                &entry.name,
                scale.font,
                [
                    fade(self.palette.subtext0),
                    fade(name_color(entry, self.palette)),
                    matched,
                ],
                spans,
                name_room,
            )
        } else {
            self.text_spans(
                painter,
                egui::pos2(name_left, rect.center().y),
                &entry.name,
                scale.font,
                fade(name_color(entry, self.palette)),
                matched,
                spans,
                name_room,
            )
        };
        if !dots.is_empty() {
            // Muted with the name — a hidden file's dots are as quiet as its
            // name, a cut row's as dim — since they are part of what it says.
            let dots: Vec<egui::Color32> = dots.into_iter().map(fade).collect();
            crate::tags::paint_dots(
                painter,
                name_end + crate::tags::DOT_GAP,
                rect.center().y,
                &dots,
                behind,
            );
        }
        let name_end = name_end + dots_room;

        // `→ target` after the name, in the dim colour, and only if it fits —
        // a symlink row must never lose its *name* to its target. Not in the
        // parent column, for the reason its linemode is off: that column is
        // one name wide.
        if self.show_symlink && mute <= 0.0 && entry.is_symlink() {
            if let Some(target) = entry.link_target() {
                let room = rect.right() - ROW_PAD_X - mode_width - name_end;
                if room > scale.font * 3.0 {
                    self.text_truncated(
                        painter,
                        egui::pos2(name_end, rect.center().y),
                        &format!(" → {}", target.to_string_lossy()),
                        scale.font,
                        fade(self.palette.faint),
                        room,
                    );
                }
            }
        }
    }

    /// A search hit's name: its path from the root, the folders in the first
    /// of `inks`, the file's own name in the second and what `f` matched in
    /// the third (PLAN §7.2, [`crate::hits`]). Returns where it ended.
    ///
    /// A column of hits is a column of shared beginnings — `src/preview/` on
    /// every other row — so when it does not fit it is cut from the *front*, a
    /// folder at a time, `…/` standing for what went
    /// ([`crate::chrome::elide_segments`]): the end is what tells two rows
    /// apart. The spans are byte ranges into the whole name, so they are
    /// carried across the cut; a name so narrow its own last part is cut too
    /// is drawn without them.
    #[allow(clippy::too_many_arguments)]
    fn path_spans(
        &self,
        painter: &egui::Painter,
        pos: egui::Pos2,
        text: &str,
        font: f32,
        [folders, name, highlight]: [egui::Color32; 3],
        spans: &[Span],
        max_width: f32,
    ) -> f32 {
        use egui::text::{LayoutJob, TextFormat, TextWrapping};
        let font_id = egui::FontId::proportional(font);
        // `/`-separated only: see plans/other-platforms/03-paths.md for the port.
        let segments: Vec<&str> = text.split('/').collect();
        let shown = crate::chrome::elide_segments(&segments, |candidate| {
            painter
                .layout_no_wrap(candidate.to_string(), font_id.clone(), name)
                .size()
                .x
                <= max_width
        });
        // Where the part of the name still shown starts, and what stands in
        // front of it. `None` when even the last part had to be cut.
        const LEAD: &str = "…/";
        let kept = if shown == text {
            Some(("", 0))
        } else {
            shown
                .strip_prefix(LEAD)
                .filter(|tail| text.ends_with(tail))
                .map(|tail| (LEAD, text.len() - tail.len()))
        };
        let format = |color: egui::Color32| TextFormat {
            font_id: font_id.clone(),
            color,
            ..Default::default()
        };
        let mut job = LayoutJob::default();
        match kept {
            Some((lead, start)) => {
                if !lead.is_empty() {
                    job.append(lead, 0.0, format(folders));
                }
                // Every place the ink changes: where the file's own name
                // starts, and each end of each match.
                let leaf = text.rfind('/').map_or(0, |slash| slash + 1);
                let mut cuts = vec![start, leaf.max(start), text.len()];
                for &(from, to) in spans {
                    for at in [from, to] {
                        if at > start && at < text.len() && text.is_char_boundary(at) {
                            cuts.push(at);
                        }
                    }
                }
                cuts.sort_unstable();
                cuts.dedup();
                for pair in cuts.windows(2) {
                    let (from, to) = (pair[0], pair[1]);
                    if from >= to {
                        continue;
                    }
                    let matched = spans.iter().any(|&(a, b)| a <= from && to <= b);
                    let color = if matched {
                        highlight
                    } else if from < leaf {
                        folders
                    } else {
                        name
                    };
                    job.append(&text[from..to], 0.0, format(color));
                }
            }
            None => job.append(&shown, 0.0, format(name)),
        }
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
            name,
        );
        pos.x + width
    }

    /// Draw text left-aligned and vertically centred, truncated with an ellipsis
    /// at `max_width`. Returns where it ended, so the next run can start there.
    fn text_truncated(
        &self,
        painter: &egui::Painter,
        pos: egui::Pos2,
        text: &str,
        font: f32,
        color: egui::Color32,
        max_width: f32,
    ) -> f32 {
        self.text_spans(painter, pos, text, font, color, color, &[], max_width)
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
        font: f32,
        color: egui::Color32,
        highlight: egui::Color32,
        spans: &[Span],
        max_width: f32,
    ) -> f32 {
        use egui::text::{LayoutJob, TextFormat, TextWrapping};
        let format = |color: egui::Color32| TextFormat {
            font_id: egui::FontId::proportional(font),
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
        let line = crate::glyphs::layout(painter, job);
        let size = line.size();
        line.paint(painter, egui::pos2(pos.x, pos.y - size.y / 2.0), color);
        pos.x + size.x
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
                Some(format!(
                    "{} hidden",
                    df_core::text::grouped(dir.total() as u64)
                ))
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
                    egui::Stroke::new(1.0, crate::theme::shadow(self.palette, a * 0.6)),
                ));
                continue;
            }
            self.painter
                .rect_filled(card.rect, crate::dnd::GHOST_RADIUS, fade(card_fill, a));
            self.painter.rect_stroke(
                card.rect,
                crate::dnd::GHOST_RADIUS,
                egui::Stroke::new(1.0, crate::theme::shadow(self.palette, a * 0.6)),
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
            let text = df_core::text::grouped(count as u64);
            let font = egui::FontId::proportional(FONT_SIZE - 1.5);
            let width = self
                .painter
                .layout_no_wrap(text.clone(), font.clone(), self.palette.crust)
                .size()
                .x;
            (text, font, width + DROP_BADGE)
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
            FONT_SIZE,
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
                crate::chrome::fade(self.palette.faint, alpha),
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
            self.palette.faint,
        );
    }

    /// The line under an empty pane's "empty", when the place has one
    /// ([`ListView::empty_note`]): a step smaller and a row lower, in the same
    /// quiet ink, so it reads as the footnote to the word above it. Nothing
    /// under "loading…", "no matches", "3 hidden" or a failed read, which are
    /// all about something other than the place.
    pub fn empty_note(&self, content: egui::Rect, dir: &DirState, note: Option<&str>) {
        let Some(note) = note else { return };
        if dir.state() != LoadState::Loaded || dir.total() > 0 || !dir.filter().is_empty() {
            return;
        }
        use egui::text::{LayoutJob, TextFormat, TextWrapping};
        let color = self.palette.faint;
        let mut job = LayoutJob::single_section(
            note.to_string(),
            TextFormat {
                font_id: egui::FontId::proportional(FONT_SIZE - 1.0),
                color,
                ..Default::default()
            },
        );
        // One line, cut short with `…` in a pane too narrow for it, rather
        // than running out over the panes beside this one.
        job.wrap = TextWrapping {
            max_width: (content.width() - GAP * 2.0).max(0.0),
            max_rows: 1,
            break_anywhere: true,
            overflow_character: Some('…'),
        };
        let galley = self.painter.layout_job(job);
        let centre = egui::pos2(
            content.center().x,
            content.top() + content.height() * crate::chrome::OPTICAL_BASELINE + FONT_SIZE * 1.6,
        );
        self.painter
            .galley(centre - galley.size() / 2.0, galley, color);
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

    /// The sink keeps the first mark the pointer was actually inside, and
    /// nothing when it was inside none of them.
    #[test]
    fn the_row_tip_sink_keeps_the_mark_under_the_pointer() {
        let tag = egui::Rect::from_min_size(egui::pos2(100.0, 10.0), egui::vec2(40.0, 20.0));
        let size = egui::Rect::from_min_size(egui::pos2(150.0, 10.0), egui::vec2(40.0, 20.0));

        let tips = RowTips::new(size.center());
        tips.note(RowTip::Counting, tag);
        tips.note(RowTip::Counting, size);
        assert_eq!(tips.found(), Some((RowTip::Counting, size)));

        // A second mark under the pointer cannot displace the first — two
        // marks on one row never overlap, so the first is the answer.
        let tips = RowTips::new(tag.center());
        tips.note(RowTip::Counting, tag);
        tips.note(RowTip::Counting, tag);
        assert_eq!(tips.found(), Some((RowTip::Counting, tag)));

        let tips = RowTips::new(egui::pos2(0.0, 0.0));
        tips.note(RowTip::Counting, tag);
        tips.note(RowTip::Counting, size);
        assert_eq!(tips.found(), None);
    }

    fn area() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1408.0, 908.0))
    }

    /// The ratio describes the panes, not the panes plus the gaps.
    #[test]
    fn the_panes_split_at_the_configured_ratio() {
        let l = layout(area(), &Split::at([1, 4, 3]), false, 1);
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
    /// nor a negative *height* once the strip and the bar have taken theirs —
    /// and neither may a folded pane, a divider or a pull.
    #[test]
    fn a_tiny_window_does_not_produce_negative_panes() {
        let mut splits = vec![Split::at([1, 4, 3])];
        for collapsed in [[true, false], [false, true], [true, true]] {
            splits.push(Split {
                open: collapsed.map(|c| if c { 0.0 } else { 1.0 }),
                collapsed,
                ..Split::at([1, 4, 3])
            });
        }
        splits.push(Split {
            overshoot: [-24.0, 24.0],
            open: [0.5, 0.5],
            ..Split::at([1, 4, 3])
        });
        for size in [10.0, 40.0, 300.0] {
            for split in &splits {
                for strip in [false, true] {
                    let l = layout(
                        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(size, 10.0)),
                        split,
                        strip,
                        1,
                    );
                    for rect in [
                        l.parent,
                        l.list,
                        l.preview,
                        l.path,
                        l.dividers[0],
                        l.dividers[1],
                    ] {
                        assert!(
                            rect.width() >= 0.0 && rect.height() >= 0.0,
                            "{size} {split:?}: {rect:?}"
                        );
                    }
                }
            }
        }
    }

    /// The fractions a split is made of, as widths across `usable`.
    fn usable() -> f32 {
        1408.0 - GAP * 2.0 - GAP * 2.0
    }

    fn folded(parent: bool, preview: bool) -> Split {
        Split {
            open: [
                if parent { 0.0 } else { 1.0 },
                if preview { 0.0 } else { 1.0 },
            ],
            collapsed: [parent, preview],
            ..Split::at([1, 4, 3])
        }
    }

    /// A folded pane is a rect of no width, and **its gap goes with it**: the
    /// list runs to the window's margin, not a gap short of it, and the pane
    /// on the other side does not move.
    #[test]
    fn a_folded_pane_gives_its_width_and_its_gap_to_the_list() {
        let open = layout(area(), &Split::at([1, 4, 3]), false, 1);

        let l = layout(area(), &folded(true, false), false, 1);
        assert_eq!(l.parent.width(), 0.0);
        assert!((l.list.left() - GAP).abs() < 1e-3, "{:?}", l.list);
        assert!(
            (l.parent.left() - GAP).abs() < 1e-3,
            "folded at its own edge"
        );
        assert!((l.list.width() - (usable() * 5.0 / 8.0 + GAP)).abs() < 1e-3);
        assert_eq!(l.preview, open.preview, "the other side never moves");

        let l = layout(area(), &folded(false, true), false, 1);
        assert_eq!(l.preview.width(), 0.0);
        assert!((area().right() - l.list.right() - GAP).abs() < 1e-3);
        assert!((l.preview.left() - l.list.right()).abs() < 1e-3);
        assert!((l.list.width() - (usable() * 7.0 / 8.0 + GAP)).abs() < 1e-3);
        assert_eq!(l.parent, open.parent);

        // Both: the list is the whole row.
        let l = layout(area(), &folded(true, true), false, 1);
        assert!((l.list.left() - GAP).abs() < 1e-3);
        assert!((area().right() - l.list.right() - GAP).abs() < 1e-3);

        // And the pointer is never in a folded pane, even on the edge a
        // zero-width rect still contains.
        let edge = egui::pos2(l.parent.left(), l.list.center().y);
        assert!(l.parent.contains(edge), "the rect alone would say yes");
        assert!(!l.in_parent(edge) && !l.in_preview(l.preview.center()));
        assert!(open.in_parent(open.parent.center()));
        assert!(open.in_preview(open.preview.center()));
    }

    /// Half folded is half the pane and half the gap, and the row still adds
    /// up: nothing jumps at either end of a fold.
    #[test]
    fn a_folding_pane_takes_its_gap_down_with_it() {
        let l = layout(
            area(),
            &Split {
                open: [1.0, 0.5],
                ..Split::at([1, 4, 3])
            },
            false,
            1,
        );
        assert!((l.preview.width() - usable() * 3.0 / 16.0).abs() < 1e-3);
        assert!((l.preview.left() - l.list.right() - GAP / 2.0).abs() < 1e-3);
        assert!((area().right() - l.preview.right() - GAP).abs() < 1e-3);
    }

    /// The grab zones: the gap and two points onto each plate while the pane
    /// is open, the window's margin and two points onto the list once it is
    /// folded — and the hairline in the middle of whichever it is.
    #[test]
    fn the_dividers_are_the_gaps_widened_onto_each_plate() {
        let l = layout(area(), &Split::at([1, 4, 3]), false, 1);
        let [left, right] = l.dividers;
        assert!((left.left() - (l.parent.right() - DIVIDER_OVERLAP)).abs() < 1e-3);
        assert!((left.right() - (l.list.left() + DIVIDER_OVERLAP)).abs() < 1e-3);
        assert!((left.width() - (GAP + 2.0 * DIVIDER_OVERLAP)).abs() < 1e-3);
        assert!((right.left() - (l.list.right() - DIVIDER_OVERLAP)).abs() < 1e-3);
        assert!((right.right() - (l.preview.left() + DIVIDER_OVERLAP)).abs() < 1e-3);
        for zone in l.dividers {
            assert_eq!(zone.top(), l.list.top(), "the panes' full height");
            assert_eq!(zone.bottom(), l.list.bottom());
        }
        // The overlap stays clear of the rows, which are inset past it.
        assert!(left.right() < content_rect(l.list).left());
        assert!(right.left() > content_rect(l.list).right());
        assert!((l.hairlines[0] - (l.parent.right() + GAP / 2.0)).abs() < 1e-3);
        assert!((l.hairlines[1] - (l.list.right() + GAP / 2.0)).abs() < 1e-3);

        let l = layout(area(), &folded(true, true), false, 1);
        let [left, right] = l.dividers;
        assert_eq!(left.left(), area().left());
        assert!((left.right() - (GAP + DIVIDER_OVERLAP)).abs() < 1e-3);
        assert!((left.right() - (l.list.left() + DIVIDER_OVERLAP)).abs() < 1e-3);
        assert_eq!(right.right(), area().right());
        assert!((right.left() - (l.list.right() - DIVIDER_OVERLAP)).abs() < 1e-3);
        assert!((l.hairlines[0] - GAP / 2.0).abs() < 1e-3);
        assert!((l.hairlines[1] - (area().right() - GAP / 2.0)).abs() < 1e-3);
    }

    /// No open pane is laid out under its minimum while the window has room
    /// for all three, and the list pays for it first — so a narrow parent
    /// does not cost the preview anything.
    #[test]
    fn a_pane_is_never_laid_out_under_its_minimum() {
        let at = |fractions: [f32; 3]| {
            layout(
                area(),
                &Split {
                    fractions,
                    ..Split::at([1, 4, 3])
                },
                false,
                1,
            )
        };
        let l = at([0.01, 0.6, 0.39]);
        assert!((l.parent.width() - PARENT_MIN).abs() < 1e-3);
        assert!((l.preview.width() - usable() * 0.39).abs() < 1e-2);
        let l = at([0.2, 0.75, 0.05]);
        assert!((l.preview.width() - PREVIEW_MIN).abs() < 1e-3);
        assert!((l.parent.width() - usable() * 0.2).abs() < 1e-2);
        // A list with no share is given its minimum by the side panes, each
        // paying by how far it is over its own.
        let l = at([0.5, 0.0, 0.5]);
        assert!((l.list.width() - LIST_MIN).abs() < 1e-3);
        let spare = [usable() / 2.0 - PARENT_MIN, usable() / 2.0 - PREVIEW_MIN];
        let paid = LIST_MIN * spare[0] / (spare[0] + spare[1]);
        assert!((l.parent.width() - (usable() / 2.0 - paid)).abs() < 1e-2);
        for l in [at([0.01, 0.6, 0.39]), at([0.5, 0.0, 0.5])] {
            let row = l.parent.width() + l.list.width() + l.preview.width();
            assert!((row - usable()).abs() < 1e-2, "the row still adds up");
        }
        // …but a folded pane has none, and gives the list everything.
        let l = layout(
            area(),
            &Split {
                fractions: [0.0, 0.625, 0.375],
                ..folded(true, false)
            },
            false,
            1,
        );
        assert_eq!(l.parent.width(), 0.0);
        assert!((l.preview.width() - usable() * 0.375).abs() < 1e-2);
    }

    /// A divider's pull trades with the list and nothing else.
    #[test]
    fn the_pull_is_traded_with_the_list_alone() {
        let rest = layout(area(), &Split::at([1, 4, 3]), false, 1);
        let pulled = layout(
            area(),
            &Split {
                overshoot: [-10.0, 6.0],
                ..Split::at([1, 4, 3])
            },
            false,
            1,
        );
        assert!((pulled.parent.width() - (rest.parent.width() - 10.0)).abs() < 1e-3);
        assert!((pulled.preview.width() - (rest.preview.width() + 6.0)).abs() < 1e-3);
        assert!((pulled.list.width() - (rest.list.width() + 4.0)).abs() < 1e-3);
        assert_eq!(pulled.preview.right(), rest.preview.right());
    }

    /// The top row is always reserved, the strip only takes space when it is
    /// asked for (PLAN §2: the strip appears at two tabs), and the panes run to
    /// the window's own bottom edge now that there is no bar under them.
    #[test]
    fn the_chrome_sits_above_the_panes() {
        let bare = layout(area(), &Split::at([1, 4, 3]), false, 1);
        assert_eq!(bare.strip, None);
        assert!((bare.list.bottom() - (area().bottom() - GAP)).abs() < 1e-3);
        // The top row is always there, and the panes start below it.
        assert!((bare.path.top() - GAP).abs() < 1e-3);
        assert!((bare.path.height() - TOP_HEIGHT).abs() < 1e-3);
        assert!((bare.list.top() - (bare.path.bottom() + GAP)).abs() < 1e-3);

        let with_strip = layout(area(), &Split::at([1, 4, 3]), true, 1);
        let strip = with_strip.strip.expect("a strip was asked for");
        assert!((strip.height() - CHROME_HEIGHT).abs() < 1e-3);
        // Flush: the active tab is drawn joined to the top row, so there is no
        // gap between the two for a seam to appear in.
        assert!((with_strip.path.top() - strip.bottom()).abs() < 1e-3);
        assert!((with_strip.list.top() - (with_strip.path.bottom() + GAP)).abs() < 1e-3);
        // The strip costs the panes exactly its own height, and nothing else
        // moves.
        assert!((bare.list.height() - with_strip.list.height() - CHROME_HEIGHT).abs() < 1e-3);
        assert!((bare.list.width() - with_strip.list.width()).abs() < 1e-3);
    }

    /// A prompt that grew a second line takes it out of the panes, and gives it
    /// back when it closes: the top row is the only chrome that moves them.
    #[test]
    fn a_two_line_prompt_reflows_the_panes() {
        let one = layout(area(), &Split::at([1, 4, 3]), false, 1);
        let two = layout(area(), &Split::at([1, 4, 3]), false, 2);
        assert!((two.path.height() - one.path.height() - PROMPT_ERROR_LINE).abs() < 1e-3);
        assert!((one.list.height() - two.list.height() - PROMPT_ERROR_LINE).abs() < 1e-3);
        assert!((two.list.top() - (two.path.bottom() + GAP)).abs() < 1e-3);
        // Zero lines is one line: the row is never absent.
        assert_eq!(
            layout(area(), &Split::at([1, 4, 3]), false, 0).path,
            one.path
        );
    }

    #[test]
    fn rows_stack_downwards_from_the_scroll_position() {
        let content = content_rect(layout(area(), &Split::at([1, 4, 3]), false, 1).list);
        let top = row_rect(content, 0.0, 0, ROW_HEIGHT);
        assert!((top.top() - content.top()).abs() < 1e-3);
        assert!((top.height() - ROW_HEIGHT).abs() < 1e-3);
        // Scrolled by three rows, row 3 is at the top.
        let scrolled = row_rect(content, 3.0, 3, ROW_HEIGHT);
        assert!((scrolled.top() - content.top()).abs() < 1e-3);
        // …and a fractional scroll moves it by a fraction of a row.
        let half = row_rect(content, 3.5, 3, ROW_HEIGHT);
        assert!((half.top() - (content.top() - ROW_HEIGHT / 2.0)).abs() < 1e-3);
    }

    /// The same arithmetic at every step of the ladder: a scaled row is taller
    /// by exactly its factor, the rows still stack without a gap or an overlap,
    /// and the hit test still lands on the row that was drawn.
    #[test]
    fn a_scaled_row_stacks_and_hit_tests_at_its_own_height() {
        let content = content_rect(layout(area(), &Split::at([1, 4, 3]), false, 1).list);
        for step in df_core::config::VIEW_SCALES {
            let scale = Scale::new(step);
            assert!(
                (scale.row_height - ROW_HEIGHT * step.row_factor()).abs() < 1e-3,
                "{step:?}"
            );
            let first = row_rect(content, 0.0, 0, scale.row_height);
            let second = row_rect(content, 0.0, 1, scale.row_height);
            assert!((first.height() - scale.row_height).abs() < 1e-3, "{step:?}");
            // No gap and no overlap: one row's bottom is the next one's top.
            assert!((second.top() - first.bottom()).abs() < 1e-3, "{step:?}");
            // …and the hit test is the exact inverse at that height.
            for index in [0usize, 1, 5] {
                let rect = row_rect(content, 0.0, index, scale.row_height);
                assert_eq!(
                    row_at(content, 0.0, 40, rect.center(), scale.row_height),
                    Some(index),
                    "{step:?} {index}"
                );
            }
        }
        // The smallest step is today's list, to the point.
        assert!((Scale::default().row_height - ROW_HEIGHT).abs() < 1e-3);
    }

    #[test]
    fn hit_testing_finds_the_row_under_the_pointer() {
        let content = content_rect(layout(area(), &Split::at([1, 4, 3]), false, 1).list);
        let inside = |index: usize| row_rect(content, 0.0, index, ROW_HEIGHT).center();
        assert_eq!(row_at(content, 0.0, 40, inside(0), ROW_HEIGHT), Some(0));
        assert_eq!(row_at(content, 0.0, 40, inside(7), ROW_HEIGHT), Some(7));
        // Scrolling moves which row is under a fixed point.
        assert_eq!(row_at(content, 5.0, 40, inside(0), ROW_HEIGHT), Some(5));
        // Past the end of a short listing there is no row, only pane.
        assert_eq!(row_at(content, 0.0, 3, inside(10), ROW_HEIGHT), None);
        // …and neither is there outside the pane.
        assert_eq!(
            row_at(content, 0.0, 40, egui::pos2(-5.0, -5.0), ROW_HEIGHT),
            None
        );
        // A pane mid-resize can hand this a zero height; that is no row, not a
        // division by zero.
        assert_eq!(row_at(content, 0.0, 40, inside(0), 0.0), None);
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
                tips: None,
                held: None,
                painter: ui.painter(),
                palette: &palette,
                theme: &theme,
                tags: &crate::tags::BUILT_IN,
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
                let cards = crate::dnd::ghost_cards(
                    egui::pos2(600.0, 380.0),
                    count,
                    Scale::default().row_height,
                );
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
            let cards =
                crate::dnd::ghost_cards(egui::pos2(60.0, 60.0), 3, Scale::default().row_height);
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
        let palette = Palette::default();
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
        };
        let without = GitMark::default();
        assert!(with.column && with.status.is_none());
        assert!(!without.column);
        // The reserved width has to be wider than the disc it holds, or the dot
        // would touch the name beside it.
        const { assert!(GIT_DOT_COLUMN > GIT_DOT_RADIUS * 2.0) };
    }

    /// The size column is measured once per listing against a fixed set of
    /// candidates, so the widest thing the column can ever *say* has to be one
    /// of them. Both extremes the hand-written pair used to miss are checked by
    /// name: the capped child count, and a running size wearing its tilde.
    #[test]
    fn the_widest_size_column_strings_cover_the_extremes() {
        let widest = size_column_widest();
        let has = |text: &str| widest.iter().any(|w| w == text);

        // A directory the counting pass gave up on.
        assert_eq!(
            crate::format::folder_size_text(
                None,
                Some(df_core::du::ChildCount {
                    entries: df_core::du::MAX_COUNTED_ENTRIES,
                    capped: true,
                }),
            )
            .as_deref(),
            Some("10,000+ items")
        );
        assert!(has("10,000+ items"), "{widest:?}");

        // …and the widest a size gets, still counting.
        let running = crate::format::folder_size_text(
            Some(crate::folders::Size {
                bytes: WIDEST_DIRECTORY_BYTES,
                settled: false,
            }),
            None,
        );
        assert_eq!(running.as_deref(), Some("~1023.9 TB"));
        assert!(has("~1023.9 TB"), "{widest:?}");

        let settled = crate::format::folder_size_text(
            Some(crate::folders::Size {
                bytes: WIDEST_DIRECTORY_BYTES,
                settled: true,
            }),
            None,
        );
        assert!(has(settled.as_deref().unwrap_or_default()));
        assert!(
            running.unwrap_or_default().len() > settled.unwrap_or_default().len(),
            "the tilde is width, not a free rider on the digit beside it"
        );
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
