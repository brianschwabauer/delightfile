//! Drag and drop: what a drag *is*, where it can land, and how it gets there
//! (PLAN §7.1).
//!
//! Everything in this file is a pure function or a small `Copy` state machine
//! over `(geometry, Instant)`, for the reason [`crate::mouse`] gives: a gesture
//! decided inside the paint loop is a gesture that can only be tested by waving
//! a mouse at it. The routing lives in [`crate::app`], the wire protocol in
//! [`crate::wayland`]; this module answers the questions both of them ask.
//!
//! ## The three drags, and why they are one gesture
//!
//! A press on a row that travels [`crate::mouse::DRAG_THRESHOLD`] is *one*
//! gesture with three possible endings, and the hand must not have to know
//! which one it is doing when it starts:
//!
//! 1. **Internal.** The pointer stays inside the window. Targets highlight, a
//!    directory springs open if you hover it, and the drop runs through the
//!    same `plan_paste` → conflict → task → journal → undo-toast pipeline that
//!    `y`/`x`/`p` do. Nothing new can go wrong that a paste could not.
//! 2. **Out.** The pointer leaves the window bounds and the drag is handed to
//!    the compositor as a `wl_data_device` drag offering `text/uri-list`. The
//!    handoff point is *leaving the window*, not the threshold, and that is a
//!    decision worth defending: starting the Wayland drag at the threshold
//!    would take the pointer grab away from us immediately, and with it every
//!    pointer event the internal drag is drawn from. Half the feature would be
//!    implemented in terms of the other half's failure mode. Leaving the window
//!    is the moment the pointer stops being ours, which is exactly when the
//!    compositor should take it.
//! 3. **Back.** `Esc`, or a release over nothing, springs the ghost home on
//!    [`Easing::BackOut`] over [`SPRING_BACK`] and does nothing at all.
//!
//! ## Why the target is resolved from geometry and not from hover state
//!
//! [`target_at`] takes the frame's rectangles and a point and answers what a
//! drop would hit. It does *not* consult the hover map, because the hover map
//! is an animation and a drop is a commitment: the row a release lands on has
//! to be the row under the pointer at that instant, not the row whose highlight
//! has not finished fading yet.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::motion::{Easing, Tween};
use crate::ui::{Column, ROW_HEIGHT};

/// What a drop will do, and the word the chip beside the ghost says.
///
/// The three verbs and their modifiers are the desktop's, not ours: move is the
/// bare drag everywhere from Finder to Nautilus, `Ctrl` copies, and `Alt` links
/// (GNOME's `Ctrl+Shift` spelling is a two-hand chord for a one-hand gesture,
/// so the shorter one wins). They are the same three operations `x`/`y`/`-`
/// already spell on the keyboard, which is the point — a drag is a mouse
/// spelling of a command that exists, not a fourth way to move a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verb {
    Move,
    Copy,
    Link,
}

impl Verb {
    /// The chip's text. Sentence case, like every other verb in the program
    /// (`delightful-ui` §10).
    pub fn label(self) -> &'static str {
        match self {
            Verb::Move => "Move",
            Verb::Copy => "Copy",
            Verb::Link => "Link",
        }
    }
}

/// Which verb the modifiers currently held mean.
///
/// `Ctrl` wins over `Alt` when both are down. Not arbitrary: copy is the safe
/// answer of the two — it leaves the sources where they are — and a hand
/// holding both keys has not said anything precise enough to justify the
/// riskier reading.
pub fn verb_for(ctrl: bool, alt: bool) -> Verb {
    match (ctrl, alt) {
        (true, _) => Verb::Copy,
        (false, true) => Verb::Link,
        (false, false) => Verb::Move,
    }
}

/// Somewhere a drop can land.
///
/// Four kinds, and all four are things already on screen that already mean a
/// directory (PLAN §7.1): a directory row in either listing pane, a pane's own
/// background (which *is* that pane's cwd), a breadcrumb segment, and a tab
/// chip. Nothing new is drawn to make a drop possible — the affordance is the
/// highlight, not a landing strip that appears when you pick something up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// A directory row. Only ever a directory: [`target_at`] falls through to
    /// the pane for a file row, because dropping "onto" a file means dropping
    /// into the directory the file is in.
    Row(Column, usize),
    /// The pane's background, meaning that pane's own directory.
    Pane(Column),
    /// A breadcrumb segment, meaning the directory that segment names.
    Crumb(usize),
    /// A tab chip, meaning that tab's cwd (PLAN §2's "drop on a tab header
    /// targets that tab's cwd").
    Tab(usize),
}

impl Target {
    /// Is this a row, and therefore something a hold can spring open? The
    /// panes, crumbs and tabs are places you are already looking at — there is
    /// nothing for holding over one to reveal.
    pub fn is_row(self) -> bool {
        matches!(self, Target::Row(..))
    }
}

/// Where this frame drew everything a drop can land on.
///
/// One copy of the frame's own rectangles, the way [`crate::app`]'s `Geom` is a
/// borrow of them: the hit test and the paint must be reading one set, or the
/// ring lands a pixel away from the thing it is ringing.
#[derive(Debug, Clone, PartialEq)]
pub struct Zones {
    pub strip: Option<egui::Rect>,
    pub tabs: usize,
    /// Owned rather than borrowed: an external drop arrives from the wayland
    /// thread *between* frames, and the geometry it must be resolved against is
    /// the one the last frame drew. A handful of rectangles copied once a frame
    /// is the price of the drop landing where the highlight said it would.
    pub crumbs: Vec<egui::Rect>,
    pub list_pane: egui::Rect,
    pub list_content: egui::Rect,
    pub list_scroll: f32,
    /// `Some` when the list pane is drawn as a grid: the geometry a drop ring
    /// has to be measured in (PLAN §2). `None` is the list, which is what every
    /// test in this file builds.
    pub list_grid: Option<crate::grid::Metrics>,
    pub list_rows: usize,
    pub parent_pane: egui::Rect,
    pub parent_content: egui::Rect,
    pub parent_scroll: f32,
    pub parent_rows: usize,
}

impl Zones {
    /// Where a target was drawn — what the ring goes around.
    ///
    /// `None` for a target that is no longer on screen: a tab that closed
    /// mid-drag, a crumb elided by a narrower window, a row scrolled away.
    pub fn rect_of(&self, target: Target) -> Option<egui::Rect> {
        match target {
            Target::Row(Column::List, index) => Some(crate::grid::pane_rect(
                self.list_content,
                self.list_grid.as_ref(),
                self.list_scroll,
                index,
            )),
            Target::Row(Column::Parent, index) => Some(crate::ui::row_rect(
                self.parent_content,
                self.parent_scroll,
                index,
            )),
            Target::Pane(Column::List) => Some(self.list_pane),
            Target::Pane(Column::Parent) => Some(self.parent_pane),
            Target::Crumb(index) => self
                .crumbs
                .get(index)
                .copied()
                .filter(|rect| rect.is_positive()),
            Target::Tab(index) => {
                let strip = self.strip?;
                crate::chrome::tab_rects(strip, self.tabs)
                    .get(index)
                    .copied()
            }
        }
    }
}

/// What a drop at `pos` would land on, or `None` for the inert parts of the
/// window.
///
/// `is_dir` answers "is row `index` of this column a directory" — passed in
/// rather than read from a `DirState` so the whole resolution is testable
/// without a filesystem.
///
/// The order is the order things are drawn in, topmost first: the chrome above
/// the panes, then rows, then the panes they are in. The preview pane is
/// deliberately absent — it is a picture of the file the cursor is on, not a
/// place, and a drop into it would have to guess which directory it meant.
pub fn target_at(
    zones: &Zones,
    pos: egui::Pos2,
    is_dir: impl Fn(Column, usize) -> bool,
) -> Option<Target> {
    if let Some(strip) = zones.strip {
        if let Some(index) = crate::chrome::tab_at(strip, zones.tabs, pos) {
            return Some(Target::Tab(index));
        }
    }
    if let Some(index) = zones.crumbs.iter().position(|rect| rect.contains(pos)) {
        return Some(Target::Crumb(index));
    }
    for (column, pane, content, scroll, rows, metrics) in [
        (
            Column::List,
            zones.list_pane,
            zones.list_content,
            zones.list_scroll,
            zones.list_rows,
            zones.list_grid.as_ref(),
        ),
        (
            Column::Parent,
            zones.parent_pane,
            zones.parent_content,
            zones.parent_scroll,
            zones.parent_rows,
            // The parent column is always a list: it is a sixth of the window
            // wide, and a grid in it would be one tile across.
            None,
        ),
    ] {
        if !pane.contains(pos) {
            continue;
        }
        if let Some(index) = crate::grid::pane_at(content, metrics, scroll, rows, pos) {
            if is_dir(column, index) {
                return Some(Target::Row(column, index));
            }
        }
        // A file row, or the empty space under a short listing: the pane
        // itself, which is the directory that row lives in either way.
        return Some(Target::Pane(column));
    }
    None
}

/// Could a drop of `dragged` land in `dest` at all?
///
/// Two refusals, and both are about the highlight rather than about safety —
/// `plan_paste` has its own rails and they are the ones that matter. This
/// exists so the ring never appears over somewhere the drop would immediately
/// bounce off:
///
/// - **A dragged directory, or anything inside one.** Dropping a folder into
///   itself is the copy that never ends, and df-core refuses it; lighting it up
///   first would be an invitation to an error message.
/// - **The directory the files are already in**, which is a no-op for a move.
///   Copy and link are legitimate there (that is how you duplicate something),
///   so this only refuses [`Verb::Move`].
pub fn valid_dest(dest: &Path, dragged: &[PathBuf], verb: Verb) -> bool {
    for path in dragged {
        if dest == path || dest.starts_with(path) {
            return false;
        }
        if verb == Verb::Move && path.parent() == Some(dest) {
            return false;
        }
    }
    !dragged.is_empty()
}

// ── The ghost (PLAN §7.1's stacked card) ────────────────────────────────────

/// The ghost card's width, in logical points.
///
/// Narrower than a list row on purpose: the card is a *token* for what is in
/// the hand, not a copy of the row it came from, and one wide enough to hold
/// the longest file name would cover the targets it is being carried towards.
pub const GHOST_WIDTH: f32 = 176.0;

/// Its height — one row plus the padding that lifts it off the rows behind it.
pub const GHOST_HEIGHT: f32 = ROW_HEIGHT + 4.0;

/// The card's corner radius, and the badge's.
pub const GHOST_RADIUS: u8 = 7;

/// How many cards are drawn *behind* the top one.
///
/// Three, which is delightstack's stacked-card count and about the number the
/// eye reads as "several" without counting. Past it the [`ghost_badge`] chip
/// says the number outright, because a fourth card would only be another edge.
pub const GHOST_STACK: usize = 3;

/// How far each card behind is offset from the one in front, in points. Small:
/// a stack is a hint of depth, and a big step turns it into a fan.
pub const GHOST_STEP: f32 = 4.5;

/// How far each card behind is tilted, in radians — about 1.7°, alternating
/// sign so the stack reads as *dropped* rather than as sheared.
///
/// `delightful-ui` §6 asks for "slight rotation"; slight is the operative word.
/// Anything past a couple of degrees on a 26 pt card puts a visible staircase
/// on its own edge at this size.
pub const GHOST_TILT: f32 = 0.030;

/// Where the pointer sits on the top card.
///
/// Near its left edge and vertically centred, so the card hangs off to the
/// right of the cursor and never covers the row the cursor is over — the whole
/// difficulty with a ghost is that it obscures the thing you are aiming at.
pub const GHOST_GRAB: egui::Vec2 = egui::vec2(16.0, GHOST_HEIGHT / 2.0);

/// How far the top card is lifted, as a scale. delightstack's mirror scale, to
/// the digit: enough that the card reads as being *above* the window.
pub const GHOST_LIFT: f32 = 1.025;

/// One card of the stack, back to front.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Card {
    pub rect: egui::Rect,
    /// Radians, signed.
    pub tilt: f32,
    pub alpha: f32,
}

/// The stack for a drag of `count` items, with the pointer at `at`.
///
/// Back to front, so a painter can draw the returned slice in order and the top
/// card — the one with the icon and the name on it — is always last.
pub fn ghost_cards(at: egui::Pos2, count: usize) -> Vec<Card> {
    let behind = count.saturating_sub(1).min(GHOST_STACK);
    let top = egui::Rect::from_min_size(at - GHOST_GRAB, egui::vec2(GHOST_WIDTH, GHOST_HEIGHT));
    let mut cards = Vec::with_capacity(behind + 1);
    for depth in (1..=behind).rev() {
        cards.push(Card {
            rect: top.translate(egui::vec2(
                GHOST_STEP * depth as f32 * 0.5,
                GHOST_STEP * depth as f32,
            )),
            // Alternating, so two cards behind do not lean the same way and
            // read as one thick card.
            tilt: if depth % 2 == 0 {
                GHOST_TILT
            } else {
                -GHOST_TILT
            },
            // Each step back loses a fifth of its opacity: far enough to read
            // as depth, near enough that the bottom card is still an edge.
            alpha: 1.0 - 0.2 * depth as f32,
        });
    }
    cards.push(Card {
        rect: top.expand2(egui::vec2(
            GHOST_WIDTH * (GHOST_LIFT - 1.0) / 2.0,
            GHOST_HEIGHT * (GHOST_LIFT - 1.0) / 2.0,
        )),
        tilt: 0.0,
        alpha: 1.0,
    });
    cards
}

/// The count chip's text, or `None` when the stack already says the number by
/// having that many cards in it.
pub fn ghost_badge(count: usize) -> Option<usize> {
    (count > GHOST_STACK).then_some(count)
}

/// A rectangle's corners, rotated about its own centre — how a tilted card is
/// drawn, since egui has no rotated rounded rect.
///
/// Only the cards *behind* the top one are ever tilted, and they are mostly
/// hidden, so losing their rounding to a convex polygon costs a few pixels
/// nobody can see. The top card keeps its radius and its text.
pub fn tilted(rect: egui::Rect, tilt: f32) -> Vec<egui::Pos2> {
    let (sin, cos) = tilt.sin_cos();
    let centre = rect.center();
    [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
    ]
    .into_iter()
    .map(|corner| {
        let v = corner - centre;
        centre + egui::vec2(v.x * cos - v.y * sin, v.x * sin + v.y * cos)
    })
    .collect()
}

// ── Auto-scroll (PLAN §7.1: a drag has to be able to reach row 4000) ────────

/// How wide the band at a pane's top and bottom edges is, in logical points.
///
/// 26 — a shade over one row. Wide enough to fall into without aiming, narrow
/// enough that the last row of a listing is still a place you can drop on
/// rather than a trigger you fall through.
pub const EDGE_BAND: f32 = 26.0;

/// The fastest the listing travels while a drag sits at the very edge, in rows
/// per second.
///
/// Fourteen: about two thirds of a screenful a second on a normal window. Fast
/// enough to cross a long directory without waiting, slow enough that the names
/// going past are still readable — which is the only reason to scroll during a
/// drag at all, since you are looking for one of them.
pub const EDGE_ROWS_PER_SEC: f32 = 14.0;

/// How fast the listing under `pos` should be travelling, in rows per second —
/// negative for up, and zero when the pointer is not in a band.
///
/// The ramp is squared rather than linear: a drag that has just clipped the
/// band moves the list almost imperceptibly and only accelerates as it presses
/// into the edge, so "I am near the bottom" and "take me to the bottom" are two
/// different gestures rather than two ends of one twitchy one.
pub fn autoscroll(content: egui::Rect, pos: egui::Pos2) -> f32 {
    if pos.x < content.left() || pos.x > content.right() || content.height() <= 0.0 {
        return 0.0;
    }
    // The bands never overlap, however short the pane: at most half of it each.
    let band = EDGE_BAND.min(content.height() / 2.0);
    if band <= 0.0 {
        return 0.0;
    }
    let above = content.top() + band - pos.y;
    let below = pos.y - (content.bottom() - band);
    let ramp = |depth: f32| {
        let t = (depth / band).clamp(0.0, 1.0);
        t * t * EDGE_ROWS_PER_SEC
    };
    if above > 0.0 && pos.y >= content.top() - band {
        -ramp(above)
    } else if below > 0.0 && pos.y <= content.bottom() + band {
        ramp(below)
    } else {
        0.0
    }
}

// ── Spring-open (the macOS hold-to-enter) ───────────────────────────────────

/// How long a drag has to hover a directory before it springs open.
///
/// 700 ms is macOS's spring-loaded-folder delay, and it is *borrowed* rather
/// than chosen for the same reason [`crate::mouse::DOUBLE_CLICK_WINDOW`] is:
/// this timing has to match the one hand already knows. Shorter and a drag that
/// crosses a folder on its way somewhere else navigates by accident; longer and
/// nobody discovers the feature exists.
pub const SPRING_OPEN: Duration = Duration::from_millis(700);

/// The hold timer, and the badge that fills while it runs.
#[derive(Debug, Clone, Copy, Default)]
pub struct SpringOpen {
    over: Option<(Target, Instant)>,
}

impl SpringOpen {
    /// Point the timer at whatever the drag is over now. Moving to a different
    /// target restarts it; moving to nothing cancels it.
    pub fn aim(&mut self, target: Option<Target>, now: Instant) {
        match (self.over, target) {
            (Some((held, _)), Some(target)) if held == target => {}
            (_, Some(target)) => self.over = Some((target, now)),
            (_, None) => self.over = None,
        }
    }

    /// How full the commit badge is, 0…1 — delightviewer's `dismiss.rs` arc,
    /// on a timer instead of on a distance.
    pub fn progress(&self, now: Instant) -> f32 {
        let Some((_, since)) = self.over else {
            return 0.0;
        };
        (now.saturating_duration_since(since).as_secs_f32() / SPRING_OPEN.as_secs_f32())
            .clamp(0.0, 1.0)
    }

    /// Has the hold earned its navigation? **Consumes** it, so a drag that
    /// keeps hovering the folder it just opened does not open it again every
    /// frame — the timer restarts against the directory that is now there.
    pub fn fired(&mut self, now: Instant) -> Option<Target> {
        let (target, since) = self.over?;
        if now.saturating_duration_since(since) < SPRING_OPEN {
            return None;
        }
        self.over = Some((target, now));
        Some(target)
    }

    /// When the next frame is owed, so a drag held perfectly still still opens
    /// the folder (PLAN §1: a waiter is a deadline, never a poll).
    pub fn deadline(&self) -> Option<Instant> {
        self.over.map(|(_, since)| since + SPRING_OPEN)
    }
}

// ── Spring-back (the cancel) ────────────────────────────────────────────────

/// How long the ghost takes to fly home, PLAN §8's spring-back duration.
pub const SPRING_BACK: Duration = Duration::from_millis(400);

/// The ghost's flight home after a cancelled drag.
///
/// A [`Tween`] from 0 to 1 on [`Easing::BackOut`], lerped between two points
/// rather than two `Tween`s on x and y: one clock, one curve, and a diagonal
/// that cannot arrive on one axis before the other.
#[derive(Debug, Clone, Copy)]
pub struct SpringBack {
    from: egui::Pos2,
    to: egui::Pos2,
    count: usize,
    tween: Tween,
}

impl SpringBack {
    pub fn new(from: egui::Pos2, to: egui::Pos2, count: usize, now: Instant) -> SpringBack {
        SpringBack {
            from,
            to,
            count,
            tween: Tween::new(0.0, 1.0, SPRING_BACK, Easing::BackOut, now),
        }
    }

    pub fn at(&self, now: Instant) -> egui::Pos2 {
        let t = self.tween.value(now);
        self.from + (self.to - self.from) * t
    }

    pub fn count(&self) -> usize {
        self.count
    }

    /// How solid the ghost still is. It fades as it lands rather than blinking
    /// out at the end: the card is going away, and `delightful-ui` §5's outros
    /// are the half of a motion you are allowed to hurry.
    pub fn alpha(&self, now: Instant) -> f32 {
        1.0 - self.tween.progress(now).powi(2)
    }

    pub fn finished(&self, now: Instant) -> bool {
        self.tween.finished(now)
    }
}

// ── The drag-out payload ────────────────────────────────────────────────────

/// The private mime that marks an offer as delightfile's.
///
/// A drag that leaves the window and comes back is still the drag the user
/// started, and it must not turn into an anonymous external copy on the way. It
/// carries no bytes — a target that asks for it gets an empty stream — because
/// its whole content is its name.
pub const SELF_MIME: &str = "application/x-delightfile-drag";

/// The same name with **this process's** pid on it, which is the one actually
/// offered.
///
/// The bare [`SELF_MIME`] would say "some delightfile started this drag", and
/// with multi-window that is no longer the question (PLAN §2,
/// [`crate::window`]): every window is its own process, so a drag from one
/// window dropped on another is a genuinely *external* drop that happens to
/// come from a program with the same name. Marking it as ours would suppress
/// the window's drop ring and hand the drop to the local-drag path, which for
/// the receiving window is a drag it never started. The pid makes "ours" mean
/// "this window's", which is what every use of the flag actually wants.
pub fn self_mime() -> &'static str {
    static MIME: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    MIME.get_or_init(|| format!("{SELF_MIME};pid={}", std::process::id()))
}

/// Was this offer started by *this* window?
///
/// An exact match, so another delightfile's drag is external — which it is.
pub fn is_ours(offered: &[String]) -> bool {
    let mine = self_mime();
    offered.iter().any(|mime| mime == mine)
}

/// What the drag offers, in the order it offers it.
///
/// `text/uri-list` is the one every file dialog, browser upload and editor
/// reads; the two `text/plain` spellings are the fallback for a target that
/// wants a path as a string (a terminal, a chat box). Both plain forms carry
/// newline-separated *paths*, not URIs, because a program asking for plain text
/// wants something a person could have typed.
pub fn offer(paths: &[PathBuf]) -> Vec<(String, Vec<u8>)> {
    let uris = crate::clipboard::uri_list(paths);
    let plain = paths
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("\n");
    vec![
        ("text/uri-list".to_string(), uris.into_bytes()),
        (
            "text/plain;charset=utf-8".to_string(),
            plain.clone().into_bytes(),
        ),
        ("text/plain".to_string(), plain.into_bytes()),
        (self_mime().to_string(), Vec::new()),
    ]
}

/// Which of an incoming drag's offered mimes to ask for, most useful first.
///
/// The same preference order [`crate::clipboard::choose_offer`] uses for a
/// paste, and for the same reason: a file manager asked, so a list of files
/// beats a picture of one.
pub fn wanted_mime(offered: &[String]) -> Option<String> {
    let find = |wanted: &str| {
        offered
            .iter()
            .find(|mime| mime.eq_ignore_ascii_case(wanted))
            .cloned()
    };
    find("text/uri-list")
        .or_else(|| find("text/x-moz-url"))
        .or_else(|| {
            offered
                .iter()
                .find(|mime| mime.starts_with("text/plain"))
                .cloned()
        })
}

/// Turn what a drop handed over into paths.
///
/// `text/x-moz-url` is Firefox's spelling — UTF-16, alternating URL and title
/// lines — and it is not a file drag, so it comes back empty rather than as a
/// path made of mojibake. Everything else is parsed as a uri-list, which is
/// lenient enough to also read the plain-text fallback: a bare `/tmp/a.txt`
/// line is not a `file://` URI and is dropped, so a plain-text drag of a path
/// is handled by the `path_lines` half.
pub fn paths_from(mime: &str, bytes: &[u8]) -> Vec<PathBuf> {
    let text = String::from_utf8_lossy(bytes);
    if mime.eq_ignore_ascii_case("text/uri-list") {
        return crate::clipboard::parse_uri_list(&text);
    }
    let uris = crate::clipboard::parse_uri_list(&text);
    if !uris.is_empty() {
        return uris;
    }
    // A plain-text drag of one or more absolute paths. Relative ones are
    // refused: this program has no idea what they would be relative *to*, and
    // guessing at the process working directory is how a drop writes somewhere
    // nobody was looking (the same rail `plan_paste` puts on a rename).
    text.lines()
        .map(str::trim)
        .filter(|line| line.starts_with('/'))
        .map(PathBuf::from)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zones_area() -> (egui::Rect, Vec<egui::Rect>) {
        let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1408.0, 908.0));
        let crumbs = vec![
            egui::Rect::from_min_size(egui::pos2(10.0, 40.0), egui::vec2(30.0, 20.0)),
            egui::Rect::from_min_size(egui::pos2(45.0, 40.0), egui::vec2(60.0, 20.0)),
        ];
        (area, crumbs)
    }

    fn zones(crumbs: &[egui::Rect], layout: &crate::ui::Layout) -> Zones {
        Zones {
            list_grid: None,
            strip: layout.strip,
            tabs: 3,
            crumbs: crumbs.to_vec(),
            list_pane: layout.list,
            list_content: crate::ui::content_rect(layout.list),
            list_scroll: 0.0,
            list_rows: 20,
            parent_pane: layout.parent,
            parent_content: crate::ui::content_rect(layout.parent),
            parent_scroll: 0.0,
            parent_rows: 20,
        }
    }

    /// The modifier table, including the both-held case.
    #[test]
    fn the_modifiers_spell_the_three_verbs() {
        assert_eq!(verb_for(false, false), Verb::Move);
        assert_eq!(verb_for(true, false), Verb::Copy);
        assert_eq!(verb_for(false, true), Verb::Link);
        // Ctrl wins: copy is the answer that cannot lose a file.
        assert_eq!(verb_for(true, true), Verb::Copy);
        assert_eq!(Verb::Move.label(), "Move");
    }

    /// Every kind of target, resolved from a point, in drawing order.
    #[test]
    fn a_point_resolves_to_the_thing_drawn_under_it() {
        let (area, crumbs) = zones_area();
        let layout = crate::ui::layout(area, [1, 4, 3], true, 1);
        let z = zones(&crumbs, &layout);
        let dirs = |_: Column, index: usize| index.is_multiple_of(2);

        // A tab chip.
        let strip = layout.strip.expect("a strip was asked for");
        let tab = crate::chrome::tab_rects(strip, 3)[1];
        assert_eq!(target_at(&z, tab.center(), dirs), Some(Target::Tab(1)));
        // A crumb.
        assert_eq!(
            target_at(&z, crumbs[1].center(), dirs),
            Some(Target::Crumb(1))
        );
        // A directory row in the list, and a *file* row falling through to the
        // pane it is in.
        let row = |index: usize| crate::ui::row_rect(z.list_content, 0.0, index).center();
        assert_eq!(
            target_at(&z, row(2), dirs),
            Some(Target::Row(Column::List, 2))
        );
        assert_eq!(
            target_at(&z, row(3), dirs),
            Some(Target::Pane(Column::List))
        );
        // The parent column takes drops too.
        let parent_row = crate::ui::row_rect(z.parent_content, 0.0, 0).center();
        assert_eq!(
            target_at(&z, parent_row, dirs),
            Some(Target::Row(Column::Parent, 0))
        );
        // The preview pane is not a place.
        assert_eq!(target_at(&z, layout.preview.center(), dirs), None);
        // …and neither is the gap under the panes.
        assert_eq!(
            target_at(&z, egui::pos2(area.center().x, area.bottom() - 1.0), dirs),
            None
        );
    }

    /// The empty space under a short listing is still the pane.
    #[test]
    fn the_space_below_the_rows_is_the_directory_itself() {
        let (area, crumbs) = zones_area();
        let layout = crate::ui::layout(area, [1, 4, 3], false, 1);
        let mut z = zones(&crumbs, &layout);
        z.list_rows = 2;
        let below = crate::ui::row_rect(z.list_content, 0.0, 9).center();
        assert_eq!(
            target_at(&z, below, |_, _| true),
            Some(Target::Pane(Column::List))
        );
    }

    /// Every target the hit test can return has a rectangle to ring.
    #[test]
    fn every_target_can_be_drawn_around() {
        let (area, crumbs) = zones_area();
        let layout = crate::ui::layout(area, [1, 4, 3], true, 1);
        let z = zones(&crumbs, &layout);
        for target in [
            Target::Row(Column::List, 3),
            Target::Row(Column::Parent, 0),
            Target::Pane(Column::List),
            Target::Pane(Column::Parent),
            Target::Crumb(0),
            Target::Tab(2),
        ] {
            assert!(z.rect_of(target).is_some(), "{target:?}");
        }
        // …and one that is no longer on screen has none.
        assert_eq!(z.rect_of(Target::Tab(9)), None);
        let z = Zones {
            crumbs: vec![egui::Rect::NOTHING],
            ..zones(&crumbs, &layout)
        };
        assert_eq!(z.rect_of(Target::Crumb(0)), None);
    }

    /// The two refusals, and the case each of them must *not* refuse.
    #[test]
    fn a_destination_inside_the_drag_is_refused() {
        let dragged = vec![
            PathBuf::from("/home/b/work"),
            PathBuf::from("/home/b/a.txt"),
        ];
        assert!(!valid_dest(Path::new("/home/b/work"), &dragged, Verb::Copy));
        assert!(!valid_dest(
            Path::new("/home/b/work/deep"),
            &dragged,
            Verb::Copy
        ));
        // The directory the files are already in: a move is a no-op there…
        assert!(!valid_dest(Path::new("/home/b"), &dragged, Verb::Move));
        // …but a copy or a link is how you duplicate something in place.
        assert!(valid_dest(Path::new("/home/b"), &dragged, Verb::Copy));
        assert!(valid_dest(Path::new("/home/b"), &dragged, Verb::Link));
        assert!(valid_dest(Path::new("/tmp"), &dragged, Verb::Move));
        // Nothing in hand is nothing to drop.
        assert!(!valid_dest(Path::new("/tmp"), &[], Verb::Move));
    }

    /// The stack: back to front, capped, and hung off the pointer.
    #[test]
    fn the_ghost_stacks_up_to_three_cards_behind_the_top_one() {
        let at = egui::pos2(400.0, 300.0);
        assert_eq!(ghost_cards(at, 1).len(), 1);
        assert_eq!(ghost_cards(at, 2).len(), 2);
        assert_eq!(ghost_cards(at, 4).len(), 1 + GHOST_STACK);
        // A hundred files is still four cards and a number.
        assert_eq!(ghost_cards(at, 100).len(), 1 + GHOST_STACK);

        let cards = ghost_cards(at, 4);
        let top = cards.last().expect("a stack has a top card");
        // The top card is last, upright, opaque and lifted.
        assert_eq!(top.tilt, 0.0);
        assert_eq!(top.alpha, 1.0);
        assert!(top.rect.width() > GHOST_WIDTH);
        // It hangs off to the right of the pointer, and the pointer is on it.
        assert!(top.rect.contains(at));
        assert!(top.rect.left() < at.x && top.rect.right() > at.x + 100.0);
        // The cards behind are further down, fainter, and lean alternately.
        for pair in cards.windows(2) {
            assert!(pair[0].alpha <= pair[1].alpha);
        }
        assert!(cards[0].rect.top() > cards[1].rect.top());
        assert!(
            cards[0].tilt * cards[1].tilt < 0.0,
            "the stack must alternate"
        );
    }

    /// The badge only appears once the cards stop counting.
    #[test]
    fn the_count_chip_appears_past_the_stack() {
        assert_eq!(ghost_badge(1), None);
        assert_eq!(ghost_badge(GHOST_STACK), None);
        assert_eq!(ghost_badge(GHOST_STACK + 1), Some(4));
        assert_eq!(ghost_badge(240), Some(240));
    }

    /// A tilt turns the card about its own centre and keeps its area.
    #[test]
    fn a_tilted_card_turns_about_its_own_centre() {
        let rect = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(100.0, 20.0));
        let flat = tilted(rect, 0.0);
        assert_eq!(flat[0], rect.left_top());
        assert_eq!(flat[2], rect.right_bottom());
        let turned = tilted(rect, GHOST_TILT);
        assert_eq!(turned.len(), 4);
        // The centre is fixed: opposite corners still average to it.
        let mid = (turned[0].to_vec2() + turned[2].to_vec2()) / 2.0;
        assert!((mid - rect.center().to_vec2()).length() < 1e-3);
        // …and every corner is the same distance from it as before.
        for (before, after) in flat.iter().zip(&turned) {
            let a = (*before - rect.center()).length();
            let b = (*after - rect.center()).length();
            assert!((a - b).abs() < 1e-3);
        }
        assert!(turned[0].y < flat[0].y || turned[0].x != flat[0].x);
    }

    /// The bands, their sign, their ramp and the places they must not fire.
    #[test]
    fn the_edge_bands_ramp_up_towards_the_pane_edges() {
        let content = egui::Rect::from_min_size(egui::pos2(0.0, 100.0), egui::vec2(400.0, 300.0));
        let x = 200.0;
        // The middle of the pane is still.
        assert_eq!(autoscroll(content, egui::pos2(x, 250.0)), 0.0);
        // The top band goes up, the bottom band down.
        assert!(autoscroll(content, egui::pos2(x, content.top() + 2.0)) < 0.0);
        assert!(autoscroll(content, egui::pos2(x, content.bottom() - 2.0)) > 0.0);
        // …and it accelerates towards the edge rather than switching on.
        let shallow = autoscroll(content, egui::pos2(x, content.bottom() - EDGE_BAND + 2.0));
        let deep = autoscroll(content, egui::pos2(x, content.bottom() - 1.0));
        assert!(shallow > 0.0 && shallow < deep, "{shallow} !< {deep}");
        assert!(deep <= EDGE_ROWS_PER_SEC);
        // Squared, not linear: halfway into the band is a quarter of the speed.
        let half = autoscroll(content, egui::pos2(x, content.bottom() - EDGE_BAND / 2.0));
        assert!((half - EDGE_ROWS_PER_SEC * 0.25).abs() < 0.5, "{half}");
        // Beside the pane is not in the pane.
        assert_eq!(
            autoscroll(content, egui::pos2(-40.0, content.top() + 2.0)),
            0.0
        );
        // A drag dragged well past the pane keeps scrolling, but only as far as
        // one band's worth beyond it — past that the pointer is somewhere else
        // entirely and the list must stop.
        assert!(autoscroll(content, egui::pos2(x, content.bottom() + 5.0)) > 0.0);
        assert_eq!(
            autoscroll(content, egui::pos2(x, content.bottom() + EDGE_BAND * 3.0)),
            0.0
        );
        // A pane too short for two bands does not get overlapping ones.
        let tiny = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 10.0));
        assert!(autoscroll(tiny, egui::pos2(x, 2.0)) < 0.0);
        assert!(autoscroll(tiny, egui::pos2(x, 8.0)) > 0.0);
    }

    /// The hold timer: restarted by moving, fired once, and re-armed after.
    #[test]
    fn hovering_a_folder_springs_it_open_once() {
        let t0 = Instant::now();
        let mut spring = SpringOpen::default();
        let folder = Target::Row(Column::List, 4);
        spring.aim(Some(folder), t0);
        assert_eq!(spring.fired(t0), None);
        assert!(spring.progress(t0) < 0.01);
        let half = t0 + SPRING_OPEN / 2;
        assert!((spring.progress(half) - 0.5).abs() < 0.01);
        // Still the same target: the timer keeps running.
        spring.aim(Some(folder), half);
        let due = t0 + SPRING_OPEN;
        assert_eq!(spring.fired(due), Some(folder));
        // …and it does not fire again on the very next frame.
        assert_eq!(spring.fired(due + Duration::from_millis(16)), None);

        // Moving to another target restarts the clock.
        let mut spring = SpringOpen::default();
        spring.aim(Some(folder), t0);
        spring.aim(Some(Target::Row(Column::List, 5)), half);
        assert_eq!(spring.fired(due), None);
        assert_eq!(
            spring.fired(half + SPRING_OPEN),
            Some(Target::Row(Column::List, 5))
        );

        // Moving to nothing cancels it, deadline and all.
        let mut spring = SpringOpen::default();
        spring.aim(Some(folder), t0);
        assert_eq!(spring.deadline(), Some(t0 + SPRING_OPEN));
        spring.aim(None, half);
        assert_eq!(spring.deadline(), None);
        assert_eq!(spring.fired(due), None);
        assert_eq!(spring.progress(due), 0.0);
    }

    /// The cancel: from where it was let go to where it came from, overshooting
    /// on the way and gone at the end.
    #[test]
    fn a_cancelled_drag_springs_home_and_stops() {
        let t0 = Instant::now();
        let from = egui::pos2(500.0, 400.0);
        let to = egui::pos2(100.0, 200.0);
        let spring = SpringBack::new(from, to, 3, t0);
        assert_eq!(spring.count(), 3);
        assert_eq!(spring.at(t0), from);
        assert_eq!(spring.alpha(t0), 1.0);
        assert!(!spring.finished(t0));
        let mid = spring.at(t0 + Duration::from_millis(150));
        assert!(mid.x < from.x && mid.x > to.x - 40.0);
        // BackOut overshoots: somewhere in the flight it is *past* home.
        let overshot = (0..=40)
            .map(|i| spring.at(t0 + SPRING_BACK.mul_f32(i as f32 / 40.0)))
            .any(|p| p.x < to.x);
        assert!(overshot, "the spring must overshoot before it settles");
        let landed = spring.at(t0 + SPRING_BACK);
        assert!((landed - to).length() < 1e-3);
        assert!(spring.finished(t0 + SPRING_BACK));
        assert!(spring.alpha(t0 + SPRING_BACK) < 1e-3);
    }

    /// What goes out on the wire, from the encoders `y` already uses.
    #[test]
    fn the_drag_offers_uris_first_and_paths_as_a_fallback() {
        let paths = vec![
            PathBuf::from("/tmp/one.txt"),
            PathBuf::from("/tmp/two files.txt"),
        ];
        let offered = offer(&paths);
        let names: Vec<&str> = offered.iter().map(|(mime, _)| mime.as_str()).collect();
        assert_eq!(
            names,
            [
                "text/uri-list",
                "text/plain;charset=utf-8",
                "text/plain",
                self_mime()
            ]
        );
        let uris = String::from_utf8(offered[0].1.clone()).expect("utf-8");
        assert_eq!(uris, crate::clipboard::uri_list(&paths));
        assert!(uris.contains("%20"), "a space must be escaped in a URI");
        // The plain fallback is what a person would have typed.
        let plain = String::from_utf8(offered[2].1.clone()).expect("utf-8");
        assert_eq!(plain, "/tmp/one.txt\n/tmp/two files.txt");
        // The self-marker carries nothing but its name.
        assert!(offered[3].1.is_empty());
    }

    /// **The multi-window rule** (PLAN §2, [`crate::window`]): "ours" means
    /// *this* window's drag, not any delightfile's. A drag from another
    /// window is another program's drag as far as this one is concerned —
    /// which is what makes window→window drops light the drop ring and paste
    /// like any other external drop.
    #[test]
    fn another_windows_drag_is_not_this_windows_drag() {
        let mine = offer(&[PathBuf::from("/tmp/a")])
            .into_iter()
            .map(|(mime, _)| mime)
            .collect::<Vec<_>>();
        assert!(is_ours(&mine));
        // The same program, a different process: the pid is the only thing
        // that differs, and it is enough.
        let sibling: Vec<String> = mine
            .iter()
            .map(|mime| {
                if mime.starts_with(SELF_MIME) {
                    format!("{SELF_MIME};pid={}", std::process::id() + 1)
                } else {
                    mime.clone()
                }
            })
            .collect();
        assert!(!is_ours(&sibling));
        // An older delightfile's unqualified marker is not ours either, and
        // neither is a drag from anything else.
        assert!(!is_ours(&[SELF_MIME.to_string()]));
        assert!(!is_ours(&["text/uri-list".to_string()]));
        assert!(!is_ours(&[]));
        // …and it is still a *file* drag, so the receiving window knows what
        // to ask for.
        assert_eq!(
            wanted_mime(&sibling),
            Some("text/uri-list".to_string()),
            "a sibling window's drag must still be readable as files"
        );
    }

    /// The incoming half: which mime to ask for, and what comes back.
    #[test]
    fn an_incoming_drag_is_read_as_files() {
        let types = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            wanted_mime(&types(&["text/plain", "text/uri-list"])),
            Some("text/uri-list".to_string())
        );
        assert_eq!(
            wanted_mime(&types(&["text/plain;charset=utf-8"])),
            Some("text/plain;charset=utf-8".to_string())
        );
        assert_eq!(wanted_mime(&types(&["image/png"])), None);
        assert_eq!(wanted_mime(&[]), None);

        let list = b"file:///tmp/a.txt\r\nfile:///tmp/b%20c.txt\r\n";
        assert_eq!(
            paths_from("text/uri-list", list),
            vec![PathBuf::from("/tmp/a.txt"), PathBuf::from("/tmp/b c.txt")]
        );
        // A plain-text drop of URIs is still a file drag…
        assert_eq!(
            paths_from("text/plain", b"file:///tmp/a.txt"),
            vec![PathBuf::from("/tmp/a.txt")]
        );
        // …and one of bare absolute paths is too.
        assert_eq!(
            paths_from("text/plain", b"/tmp/a.txt\n/tmp/b.txt\n"),
            vec![PathBuf::from("/tmp/a.txt"), PathBuf::from("/tmp/b.txt")]
        );
        // A relative path, or a URL, is not something to paste.
        assert!(paths_from("text/plain", b"notes.txt").is_empty());
        assert!(paths_from("text/plain", b"https://example.com/x").is_empty());
        assert!(paths_from("text/uri-list", b"https://example.com/x").is_empty());
    }
}
