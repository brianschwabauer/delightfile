//! The thumbnail grid: PLAN §2's alternative to the list, the top step of a
//! tab's view-scale ladder.
//!
//! `~/Pictures` and a plex mount are not lists of names, they are walls of
//! pictures, and a file manager that can only draw one row per file is a file
//! manager you leave to look at your photographs. The grid is the same
//! directory, the same cursor, the same selection and the same drag — only the
//! geometry differs. That is the design constraint this module exists to keep:
//! **everything else in the program goes on working**, because the only thing
//! that changed is the function from an index to a rectangle.
//!
//! ## Two halves
//!
//! 1. **Geometry**, which is pure arithmetic over `(content box, index)` and is
//!    the exact grid counterpart of [`crate::ui::row_rect`] / [`crate::ui::row_at`]
//!    — including the invariant that those two are inverses of each other. The
//!    cursor arithmetic lives here too, because "what is below this tile" has a
//!    real answer at the edges and in a ragged last row and it should be pinned
//!    by a test, not discovered.
//! 2. **[`Thumbs`]**, a small worker pool that turns the visible tiles into
//!    textures. It exists separately from [`crate::preview::decode::Decoder`]
//!    because that one is deliberately single-slot — one preview pane, one live
//!    token — and a grid needs forty decodes in flight whose priorities change
//!    every time the view scrolls.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};

use crossbeam_channel::{unbounded, Receiver, Sender};
use df_core::fs::Notifier;

use crate::preview::decode::{decode_file, Rgba};
use crate::ui::{ROW_INSET, ROW_RADIUS};

/// The width a tile wants, in logical points.
///
/// Sixteen tens of a point. A thumbnail smaller than about 120 stops being a
/// picture and becomes a
/// coloured stamp — you can tell a photo from a screenshot and nothing else —
/// and past about 200 a 1600-point-wide pane fits seven of them, which is not
/// enough of a wall to be worth leaving the list for. 160 puts nine or ten
/// across Brian's list pane and keeps faces recognisable, which is the actual
/// job.
///
/// It is a *minimum*, not a fixed size: the last column would otherwise leave a
/// ragged strip of dead pane on the right, so the tiles stretch to divide the
/// width exactly. The stretch is bounded under 2× by construction — at 2× the
/// column count would have gone up by one.
pub const TILE_WIDTH: f32 = 160.0;

/// The gap between tiles, and between a tile and the pane's edge.
///
/// The same number for both, which is `delightful-ui` §15's even-insets rule:
/// a tile hugging the pane's edge gets the same air on every side it touches.
/// It is [`ROW_INSET`] because that is already the distance a *row* sits from
/// the pane's edge, so switching a directory between list and grid does not
/// change where the content starts.
pub const TILE_GAP: f32 = ROW_INSET;

/// How much of a tile's height the name takes.
///
/// Two lines at [`LABEL_LINE`] plus the padding under the thumbnail. Two rather
/// than one because the names this view is for — `IMG_20240817_183422.jpg`,
/// `S03E07 - The One Where…` — do not fit on one line at 160 points, and a grid
/// where every tile says `IMG_2024…` is a grid you cannot read.
pub const TILE_LABEL: f32 = LABEL_LINE * 2.0 + 4.0;

/// One line of a tile's name.
pub const LABEL_LINE: f32 = 14.0;

/// The padding inside a tile, between its edge and the thumbnail.
pub const TILE_PAD: f32 = 4.0;

/// A tile's corner radius. [`ROW_RADIUS`], so a selected tile and a selected
/// row are the same shape — the grid is a different geometry, not a different
/// visual language.
pub const TILE_RADIUS: u8 = ROW_RADIUS;

/// The thumbnail's corner radius inside the tile.
///
/// Derived, never picked: `delightful-ui` §15's concentric rule is
/// `outer = inner + gap`, so the inner radius is the tile's minus the padding
/// around it. Then the white space between picture and tile edge stays the same
/// width as it turns the corner.
pub const THUMB_RADIUS: u8 = TILE_RADIUS - TILE_PAD as u8;

/// The ring a cursor or a selected tile wears, in logical points.
///
/// Two, which is the weight every other "this is the one" statement in the
/// window is drawn at — the drop target's ring, the chrome's accent rules. A
/// tile is a *card*, and the mark that says a card is chosen goes around it;
/// the list's left-hand bar was inherited from the row it is not, and on a wall
/// of pictures it read as a stripe of colour beside a photograph rather than as
/// a selection.
pub const TILE_RING: f32 = 2.0;

/// The air between the tile's own edge and the ring around it.
///
/// One point. The ring has to read as a ring *around* the tile rather than as a
/// border drawn on it, and one point of the pane showing through is the least
/// that does it without eating into the gap between neighbours.
pub const TILE_RING_GAP: f32 = 1.0;

/// The ring's outer corner radius.
///
/// Derived, never picked: `delightful-ui` §15's concentric rule again, from the
/// other side. The card is the inner element and the ring is what encloses it,
/// so the ring's radius is the card's plus everything between the two edges —
/// the ring's own weight and the air inside it — and the space around the
/// corner stays the width it is along the sides.
pub const TILE_RING_RADIUS: u8 = TILE_RADIUS + TILE_RING_GAP as u8 + TILE_RING as u8;

/// The card inside one tile's cell.
///
/// [`tile_rect`] gives the whole **cell**, which is what the hit test, the drag
/// and the drop ring are measured in and what the ring is drawn around. The
/// card is what the cell *contains*: the ground, the picture and the name,
/// inset by the ring and the air inside it.
///
/// Reserved on every tile whether or not one is wearing a ring, because a
/// picture that grew three points the moment the cursor arrived would be a
/// reflow (`delightful-ui` §8) — and because a ring that had to fit outside the
/// cell would be clipped off at the pane's edge, where the first column and the
/// last one live.
pub fn card_rect(cell: egui::Rect) -> egui::Rect {
    cell.shrink(TILE_RING + TILE_RING_GAP)
}

/// The clipboard badge's radius, in logical points.
///
/// The list marks a yank with a bar down the row's trailing edge; a tile has no
/// trailing edge worth the name — it is a square of picture — so the same fact
/// is a pip in its top corner instead. Small: it is a note about something you
/// did a moment ago, not a thing you are about to act on, and it must not
/// become a sticker on every photograph in the directory.
pub const TILE_BADGE: f32 = 4.0;

/// How far the badge's halo of pane-ground extends past it.
///
/// A teal pip on a bright photograph is a teal pip nobody can see. The halo is
/// the same trick a caption bar is: put the mark on the pane's own colour so it
/// is read against something known, whatever the picture underneath is doing.
pub const TILE_BADGE_HALO: f32 = 1.5;

/// How many rows of tiles beyond the visible ones are decoded ahead.
///
/// One. A wheel roll or a `↓` moves the view by less than a row per frame, so
/// one row of lookahead is the difference between a thumbnail that is already
/// there when it scrolls in and one that pops in a frame later. Two rows would
/// double the decode work for a second row nobody reaches on most scrolls.
pub const LOOKAHEAD_ROWS: usize = 1;

/// How many decoded tiles are kept as textures before the oldest are dropped.
///
/// Two hundred and fifty six: comfortably more than three screenfuls at any
/// pane size this program
/// gets, so arrowing up and down inside a directory never re-decodes, and
/// bounded so a walk through ten thousand photographs does not end with ten
/// thousand textures on the GPU. Eviction is by when a texture was last
/// *asked for*, not when it was made.
const CACHE_TILES: usize = 256;

/// How many threads decode tiles.
///
/// Two, which is [`df_core::preview::PREVIEW_WORKERS`]'s number and for the
/// same reason: thumbnail decoding is I/O-bound far more than it is CPU-bound
/// (most tiles are a 600-pixel JPEG already sitting in the shared cache), and a
/// third thread mostly contends for the same disk. It is also two threads that
/// exist only while a grid is open.
const THUMB_WORKERS: usize = 2;

/// How the tiles divide up a pane, for one frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    pub columns: usize,
    /// One tile's size, thumbnail and label together.
    pub tile: egui::Vec2,
    /// Tile size plus the gap: how far the next tile is, in each axis.
    pub step: egui::Vec2,
}

/// How the tiles fit into a content box `width` points wide.
///
/// Always at least one column, because a pane too narrow for a tile still has
/// to show something and a zero-column grid divides by zero on its way to
/// deciding what.
pub fn metrics(width: f32) -> Metrics {
    let columns = (((width + TILE_GAP) / (TILE_WIDTH + TILE_GAP)).floor() as usize).max(1);
    // The tiles stretch to divide the width exactly: `columns` tiles and
    // `columns - 1` gaps between them.
    let tile_width = ((width - TILE_GAP * (columns as f32 - 1.0)) / columns as f32).max(1.0);
    // A square picture plus the name under it. Square because the thumbnails
    // are every aspect ratio there is and a square is the shape that wastes the
    // least on the average of them.
    let tile = egui::vec2(tile_width, tile_width + TILE_LABEL);
    Metrics {
        columns,
        tile,
        step: tile + egui::vec2(TILE_GAP, TILE_GAP),
    }
}

impl Metrics {
    /// How many *rows of tiles* `count` items make.
    pub fn rows(&self, count: usize) -> usize {
        count.div_ceil(self.columns.max(1))
    }

    /// How many rows of tiles fit in `height` points.
    ///
    /// The same "a partly visible row does not count" rule
    /// [`crate::viewport::visible_rows`] uses, so the scrolloff arithmetic
    /// means the same thing in both views.
    pub fn visible_rows(&self, height: f32) -> usize {
        crate::viewport::visible_rows(height, self.step.y)
    }

    /// Where the thumbnail goes inside a tile.
    pub fn thumb_rect(&self, tile: egui::Rect) -> egui::Rect {
        egui::Rect::from_min_max(
            tile.min + egui::vec2(TILE_PAD, TILE_PAD),
            egui::pos2(tile.max.x - TILE_PAD, tile.max.y - TILE_LABEL),
        )
    }

    /// Where the name goes inside a tile.
    pub fn label_rect(&self, tile: egui::Rect) -> egui::Rect {
        egui::Rect::from_min_max(
            egui::pos2(tile.min.x + TILE_PAD, tile.max.y - TILE_LABEL),
            egui::pos2(tile.max.x - TILE_PAD, tile.max.y),
        )
    }
}

/// One tile's rectangle, given how far the view has scrolled **in rows of
/// tiles**.
///
/// The unit is deliberately the same as [`crate::ui::row_rect`]'s: the view's
/// scroll position is one number in one unit whichever way the directory is
/// drawn, so the tween that animates it, the scrolloff that clamps it and the
/// wheel that pushes it are all unchanged by the toggle.
pub fn tile_rect(
    content: egui::Rect,
    metrics: &Metrics,
    scroll_rows: f32,
    index: usize,
) -> egui::Rect {
    let row = (index / metrics.columns) as f32;
    let column = (index % metrics.columns) as f32;
    egui::Rect::from_min_size(
        egui::pos2(
            content.left() + column * metrics.step.x,
            content.top() + (row - scroll_rows) * metrics.step.y,
        ),
        metrics.tile,
    )
}

/// Which tile a point is over, if any.
///
/// The inverse of [`tile_rect`], and it has to stay the inverse — a hit test
/// that computes this its own way is how a grid grows a one-tile lie at its
/// edges. The gap between tiles is **not** part of any tile: a click in the
/// alley between two pictures is a click on the pane, which is what starts a
/// band select.
pub fn tile_at(
    content: egui::Rect,
    metrics: &Metrics,
    scroll_rows: f32,
    count: usize,
    pos: egui::Pos2,
) -> Option<usize> {
    if !content.contains(pos) || count == 0 {
        return None;
    }
    let x = pos.x - content.left();
    let y = pos.y - content.top() + scroll_rows * metrics.step.y;
    if x < 0.0 || y < 0.0 {
        return None;
    }
    let column = (x / metrics.step.x).floor();
    let row = (y / metrics.step.y).floor();
    if column < 0.0 || column >= metrics.columns as f32 {
        return None;
    }
    // Inside the gap rather than on the tile.
    if x - column * metrics.step.x > metrics.tile.x || y - row * metrics.step.y > metrics.tile.y {
        return None;
    }
    let index = row as usize * metrics.columns + column as usize;
    (index < count).then_some(index)
}

/// Which way an arrow key moves in two dimensions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    Left,
    Right,
    Up,
    Down,
}

/// Where the cursor lands after `step`, out of `count` tiles in `columns`
/// columns.
///
/// Three rules, each of them a decision:
///
/// - **`←`/`→` are linear**, not row-bound. Right at the end of a row goes to
///   the start of the next, because the tiles are one sequence read the way you
///   read a page, and a `→` that refuses at the edge makes you press `↓` and
///   then five `←`s to reach the tile that was *next*.
/// - **`↓` into a ragged last row lands on the last tile** rather than
///   refusing. Nine files in a three-wide grid have no tile below the eighth,
///   and "nothing happens" is the wrong answer when there is obviously
///   somewhere below to go.
/// - **`↑`/`↓` wrap, `←`/`→` do not.** A column is a ring
///   ([`df_core::fs::DirState::wrap_cursor`] is the list's half of the same
///   rule): `↓` off the bottom of a column comes back at its top, and `↑` off
///   the top goes to the bottom of *that same column*, so the key that moves
///   vertically never moves you sideways. `←`/`→` still refuse at the two ends,
///   and that refusal is load-bearing — [`crate::app`] reads it as "this key
///   had nowhere to go" and turns it into leave-the-directory and enter-it.
pub fn step(cursor: usize, count: usize, columns: usize, step: Step) -> usize {
    if count == 0 {
        return 0;
    }
    let columns = columns.max(1);
    let last = count - 1;
    let cursor = cursor.min(last);
    match step {
        Step::Left => cursor.saturating_sub(1),
        Step::Right => (cursor + 1).min(last),
        Step::Up => {
            if cursor >= columns {
                cursor - columns
            } else {
                // Off the top: the bottom-most tile in this column, which is
                // the last one whose column matches — the last row may be
                // short of it, in which case the row above it is the bottom.
                bottom_of_column(cursor % columns, last, columns)
            }
        }
        Step::Down => {
            let below = cursor + columns;
            if below <= last {
                below
            } else if cursor / columns == last / columns {
                // Already in the last row: off the bottom, back to the top of
                // this column.
                cursor % columns
            } else {
                // The row below exists but is short of this column.
                last
            }
        }
    }
}

/// The last tile in `column`, out of `last + 1` tiles in `columns` columns.
fn bottom_of_column(column: usize, last: usize, columns: usize) -> usize {
    let rows = last / columns + 1;
    let candidate = (rows - 1) * columns + column;
    if candidate <= last {
        candidate
    } else {
        candidate - columns
    }
}

/// The tiles worth having a thumbnail for: everything on screen, plus
/// [`LOOKAHEAD_ROWS`] rows past the bottom.
///
/// Deliberately *not* symmetric. A view scrolls downwards far more than
/// upwards, and the rows above have almost always been decoded already and are
/// still in the cache — so the lookahead is spent where it buys something.
pub fn wanted(
    count: usize,
    columns: usize,
    first_row: usize,
    rows: usize,
) -> std::ops::Range<usize> {
    if count == 0 {
        return 0..0;
    }
    let columns = columns.max(1);
    let start = first_row * columns;
    // `rows + 1` because a partly visible row at the bottom is a row whose
    // pictures are on screen, and `visible_rows` does not count it.
    let end = (first_row + rows + 1 + LOOKAHEAD_ROWS) * columns;
    start.min(count)..end.min(count)
}

// ── The thumbnail workers ───────────────────────────────────────────────────

/// One tile a thumbnail is wanted for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Want {
    pub path: PathBuf,
    /// Whether the file itself may be decoded when the shared cache has no
    /// thumbnail for it.
    ///
    /// True only for still images. A video, a PDF or a font *has* a cached
    /// thumbnail when yazi or delightviewer has looked at it, and that is the
    /// one the grid uses — but decoding forty videos to fill a screen of tiles
    /// is not a thing a file manager may do while you scroll, so without a
    /// cache entry those get an icon tile and stay cheap.
    pub decode_source: bool,
}

/// A finished tile.
struct Done {
    path: PathBuf,
    image: Option<Rgba>,
}

/// What the pool and the app share.
struct Shared {
    /// The tiles still wanted, nearest first. **Replaced** rather than appended
    /// to on every view change, which is how a tile scrolled past is cancelled:
    /// it simply stops being in the list a worker pops from.
    queue: VecDeque<Want>,
    /// What a worker is decoding right now, so a re-request does not queue a
    /// second copy of it.
    inflight: HashSet<PathBuf>,
    stop: bool,
}

/// The tile decoder pool.
///
/// Dropping it stops the workers and joins them, so nothing outlives the
/// window — the same contract [`crate::preview::decode::Decoder`] has.
pub struct Thumbs {
    shared: Arc<(Mutex<Shared>, Condvar)>,
    results: Receiver<Done>,
    /// Decoded tiles. `None` is "tried and there is nothing to draw", which is
    /// remembered so a directory of undecodable files is not re-attempted on
    /// every scroll.
    cache: HashMap<PathBuf, Option<egui::TextureHandle>>,
    /// Least-recently-wanted first, for the eviction at [`CACHE_TILES`].
    order: VecDeque<PathBuf>,
    workers: Vec<std::thread::JoinHandle<()>>,
}

impl Thumbs {
    /// Start the pool. `notify` is rung once per finished tile.
    pub fn start(notify: Notifier) -> Thumbs {
        let shared = Arc::new((
            Mutex::new(Shared {
                queue: VecDeque::new(),
                inflight: HashSet::new(),
                stop: false,
            }),
            Condvar::new(),
        ));
        let (tx, results) = unbounded::<Done>();
        let mut workers = Vec::with_capacity(THUMB_WORKERS);
        for n in 0..THUMB_WORKERS {
            let shared = Arc::clone(&shared);
            let tx = tx.clone();
            let notify = Arc::clone(&notify);
            match std::thread::Builder::new()
                .name(format!("df-thumb-{n}"))
                .spawn(move || {
                    // Eight of these decoding JPEGs while the grid scrolls; the
                    // scroll is what has to stay smooth (`df_core::thread`).
                    df_core::thread::lower_priority(df_core::thread::NICE_INTERACTIVE);
                    worker(shared, tx, notify)
                }) {
                Ok(handle) => workers.push(handle),
                // A worker that will not spawn costs some thumbnails and
                // nothing else; the tiles fall back to their icons.
                Err(e) => log::warn!("a thumbnail worker did not start: {e}"),
            }
        }
        Thumbs {
            shared,
            results,
            cache: HashMap::new(),
            order: VecDeque::new(),
            workers,
        }
    }

    /// The texture for `path`, if there is one.
    ///
    /// Also the "recently wanted" signal the eviction order is built from, so
    /// this is called for every visible tile every frame and is a hash lookup
    /// on purpose.
    pub fn get(&self, path: &Path) -> Option<&egui::TextureHandle> {
        self.cache.get(path)?.as_ref()
    }

    /// Ask for exactly these tiles, in this order, and stop wanting everything
    /// else.
    ///
    /// The cancellation in PLAN §2's "cancel on scroll past" is this call: the
    /// queue is *replaced*, so a tile that has scrolled out of the lookahead
    /// stops being work the moment the next frame asks for a different set.
    /// Only a decode already running finishes, and it finishes because killing
    /// a half-done JPEG saves nothing.
    pub fn want(&mut self, wants: &[Want]) {
        for want in wants {
            self.touch(&want.path);
        }
        let Ok((mut shared, condvar)) = self.shared.0.lock().map(|shared| (shared, &self.shared.1))
        else {
            return;
        };
        shared.queue = wants
            .iter()
            .filter(|want| {
                !self.cache.contains_key(&want.path) && !shared.inflight.contains(&want.path)
            })
            .cloned()
            .collect();
        if !shared.queue.is_empty() {
            condvar.notify_all();
        }
    }

    /// Take whatever the workers finished, turning it into textures. Returns
    /// whether anything arrived.
    ///
    /// The upload is the one step that has to happen on the thread egui lives
    /// on, which is why it is here and not in the worker.
    pub fn poll(&mut self, ctx: Option<&egui::Context>) -> bool {
        let done: Vec<Done> = self.results.try_iter().collect();
        if done.is_empty() {
            return false;
        }
        for Done { path, image } in done {
            let texture = image.and_then(|image| upload(ctx, &path, &image));
            self.cache.insert(path.clone(), texture);
            self.touch(&path);
        }
        self.evict();
        true
    }

    /// Move `path` to the young end of the eviction order.
    fn touch(&mut self, path: &Path) {
        if let Some(at) = self.order.iter().position(|held| held == path) {
            let held = self.order.remove(at).unwrap_or_else(|| path.to_path_buf());
            self.order.push_back(held);
        } else {
            self.order.push_back(path.to_path_buf());
        }
    }

    fn evict(&mut self) {
        while self.order.len() > CACHE_TILES {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            self.cache.remove(&oldest);
        }
    }
}

impl Drop for Thumbs {
    fn drop(&mut self) {
        if let Ok(mut shared) = self.shared.0.lock() {
            shared.stop = true;
            shared.queue.clear();
        }
        self.shared.1.notify_all();
        for handle in self.workers.drain(..) {
            let _ = handle.join();
        }
    }
}

/// One worker: wait for a tile, decode it, ring the bell.
fn worker(shared: Arc<(Mutex<Shared>, Condvar)>, out: Sender<Done>, notify: Notifier) {
    loop {
        let want = {
            let Ok(mut held) = shared.0.lock() else {
                return;
            };
            loop {
                if held.stop {
                    return;
                }
                if let Some(want) = held.queue.pop_front() {
                    held.inflight.insert(want.path.clone());
                    break want;
                }
                let Ok(next) = shared.1.wait(held) else {
                    return;
                };
                held = next;
            }
        };
        let image = tile_image(&want);
        if let Ok(mut held) = shared.0.lock() {
            held.inflight.remove(&want.path);
        }
        if out
            .send(Done {
                path: want.path,
                image,
            })
            .is_err()
        {
            return;
        }
        notify();
    }
}

/// The size a tile's texture is decoded to, in physical pixels.
///
/// Fixed rather than measured from the pane so the cache is not invalidated by
/// a one-point resize, and generous enough that the picture is still sharp when
/// the tiles stretch to the widest they get (just under 2× [`TILE_WIDTH`]) on a
/// 2× display.
const TILE_TEXTURE: u32 = (TILE_WIDTH * 2.0) as u32;

/// Decode one tile. `None` when there is nothing to draw, which is a perfectly
/// ordinary answer — a text file has no thumbnail and gets its icon.
fn tile_image(want: &Want) -> Option<Rgba> {
    let target = (TILE_TEXTURE, TILE_TEXTURE);
    // The shared yazi cache first, always: it is a 600-pixel JPEG and it is the
    // whole reason delightfile and delightviewer hand each other the same frame
    // (PLAN §6).
    if let Some(thumb) = df_core::preview::cached_thumb(&want.path) {
        if let Ok(image) = decode_file(&thumb, target) {
            return Some(image);
        }
        // A cache entry that will not decode is a miss, not an error.
    }
    if !want.decode_source {
        return None;
    }
    decode_file(&want.path, target).ok()
}

/// Upload one decoded tile as a texture.
fn upload(ctx: Option<&egui::Context>, path: &Path, image: &Rgba) -> Option<egui::TextureHandle> {
    let ctx = ctx?;
    let size = [image.width as usize, image.height as usize];
    let colour = egui::ColorImage::from_rgba_unmultiplied(size, &image.pixels);
    Some(ctx.load_texture(
        format!("tile:{}", path.display()),
        colour,
        // Linear, because a tile is a downscale of something much bigger and
        // nearest sampling on a photograph is a shimmer.
        egui::TextureOptions::LINEAR,
    ))
}

// ── The seam ────────────────────────────────────────────────────────────────
//
// Everything outside this module that asks "where is row `n`" or "what is the
// pointer over" goes through these two, and they take `Option<&Metrics>` where
// `None` means "drawn as a list". That is the whole generalisation the grid
// needed: one place decides which geometry the list pane is in, every caller
// asks the same question, and the click, the drag, the drop ring, the band and
// the rename popup all follow the toggle without knowing it happened.

/// Where item `index` of the list pane is, whichever geometry it is drawn in.
pub fn pane_rect(
    content: egui::Rect,
    metrics: Option<&Metrics>,
    scroll_rows: f32,
    index: usize,
    scale: crate::ui::Scale,
) -> egui::Rect {
    match metrics {
        Some(metrics) => tile_rect(content, metrics, scroll_rows, index),
        None => crate::ui::row_rect(content, scroll_rows, index, scale.row_height),
    }
}

/// Which item the pointer is over, whichever geometry the pane is in.
pub fn pane_at(
    content: egui::Rect,
    metrics: Option<&Metrics>,
    scroll_rows: f32,
    count: usize,
    pos: egui::Pos2,
    scale: crate::ui::Scale,
) -> Option<usize> {
    match metrics {
        Some(metrics) => tile_at(content, metrics, scroll_rows, count, pos),
        None => crate::ui::row_at(content, scroll_rows, count, pos, scale.row_height),
    }
}

/// How tall one item of the list pane is — what the wheel converts points into
/// and what the scrolloff rule counts.
pub fn pane_step(metrics: Option<&Metrics>, scale: crate::ui::Scale) -> f32 {
    match metrics {
        Some(metrics) => metrics.step.y,
        None => scale.row_height,
    }
}

/// The items a band-select rectangle covers, as an inclusive index range.
///
/// A rectangle over a grid genuinely covers a *set* of tiles that is not a
/// contiguous run of indices — three columns wide over four rows misses the
/// tiles either side of it. This returns the run from the first covered tile to
/// the last, which is what
/// [`select_range`](df_core::fs::DirState::select_range) can express and what
/// dragging over a grid visibly *means*: everything from here to there.
///
/// What the band covers is [`crate::mouse::band_span`]'s answer, in rows of
/// tiles: clipped to the content box so a band started in the parent column or
/// the preview pane takes no tile until it reaches the grid, with the pointer's
/// corner held to the pane's edge, and with the origin carried along by the
/// scroll so the tiles a band has scrolled past stay in it. Its bottom edge is
/// exclusive, for the reason [`crate::mouse::band_rows`] gives: the row of
/// tiles whose top only *touches* the bottom of the pane is not on screen, and
/// a band clipped to that edge has not reached it.
pub fn band_items(
    content: egui::Rect,
    metrics: &Metrics,
    scroll_rows: f32,
    count: usize,
    from: crate::mouse::Corner,
    to: crate::mouse::Corner,
) -> Option<(usize, usize)> {
    let span = crate::mouse::band_span(content, metrics.step.y, scroll_rows, from, to)?;
    let covers = |tile: egui::Rect| {
        let across = tile.left() <= span.right() && tile.right() >= span.left();
        // A band dragged straight across has no height, and covers the tiles
        // it passes through rather than none of them.
        let down = if span.height() > 0.0 {
            tile.top() < span.bottom() && tile.bottom() > span.top()
        } else {
            tile.top() <= span.top() && span.top() < tile.bottom()
        };
        across && down
    };
    let mut first = None;
    let mut last = None;
    for index in 0..count {
        if !covers(tile_rect(content, metrics, scroll_rows, index)) {
            continue;
        }
        first.get_or_insert(index);
        last = Some(index);
    }
    Some((first?, last?))
}

// ── Painting ────────────────────────────────────────────────────────────────

/// One call's worth of "draw this directory as tiles".
///
/// The same shape [`crate::ui::ListView`] is, and for the same reason: half of
/// these are colours and half are booleans, and at a call site that is a row of
/// values whose meaning depends on counting commas.
pub struct GridView<'a> {
    pub pane: egui::Rect,
    pub ground: egui::Color32,
    pub dir: &'a df_core::fs::DirState,
    pub scroll_rows: f32,
    pub metrics: Metrics,
    pub hovers: &'a crate::hover::Hovers<crate::ui::Control>,
    pub ripples: &'a crate::ripple::Ripples<crate::ui::Control>,
    /// How strongly the cursor tile is lit. Always 1 in practice — the grid is
    /// the list pane, and the keys always go there (PLAN §2.1) — and still a
    /// number rather than a constant so it stays the list's own value.
    pub cursor_alpha: f32,
    pub thumbs: &'a Thumbs,
    pub clip: Option<crate::ui::ClipMark<'a>>,
    pub dragged: &'a std::collections::HashSet<PathBuf>,
    pub flip: Option<&'a crate::flip::Flip>,
    pub slow_load: bool,
    /// What git thinks of these tiles (PLAN §7.3), or `None` outside a
    /// repository.
    ///
    /// The grid draws no dots — a tile has no right-hand column to put them in
    /// — but it *must* dim what the list dims. A `target/` that is grey in the
    /// list and bright in the grid is the same directory telling you two
    /// different things depending on which key you last pressed.
    pub git: Option<&'a df_core::git::RepoStatus>,
    /// The line under "empty", as the list has it
    /// ([`crate::ui::ListView::empty_note`]).
    pub empty_note: Option<&'a str>,
}

/// Draw a directory as a wall of tiles.
///
/// Every state a row can be in, a tile is in too, and wearing the same colour:
/// the cursor is a lift towards [`crate::theme::cursor_fill`], a selection is
/// the yellow tint plus
/// its hard bar, a yank is the teal chip and a cut is the peach one. That is
/// not decoration — it is the promise this view makes, that toggling it changes
/// the geometry and nothing else.
pub fn paint(paint: &crate::ui::Painting<'_>, view: GridView<'_>) {
    use crate::theme::mix;

    let GridView {
        pane,
        ground,
        dir,
        scroll_rows,
        metrics,
        hovers,
        ripples,
        cursor_alpha,
        thumbs,
        clip,
        dragged,
        flip,
        slow_load,
        git,
        empty_note,
    } = view;
    let content = crate::ui::content_rect(pane);
    // The same sentence the list would show — "empty", "still reading", the
    // reason it could not be read. A grid of nothing and a list of nothing are
    // the same fact and must not be two different messages.
    if let Some(message) = paint.pane_state_message(dir, slow_load) {
        paint.quiet_label(content, &message);
        paint.empty_note(content, dir, empty_note);
        return;
    }
    let painter = paint.painter.with_clip_rect(content);
    let palette = paint.palette;
    let first_row = scroll_rows.floor().max(0.0) as usize;
    let window = wanted(
        dir.len(),
        metrics.columns,
        first_row,
        metrics.visible_rows(content.height()),
    );

    for index in window {
        let Some(entry) = dir.row(index) else {
            continue;
        };
        let rect = tile_rect(content, &metrics, scroll_rows, index);
        let (rect, alpha) = match flip {
            Some(flip) => (
                rect.translate(flip.offset(&entry.path, paint.now)),
                flip.alpha(&entry.path, paint.now),
            ),
            None => (rect, 1.0),
        };
        if !rect.intersects(content) {
            continue;
        }
        let key = crate::ui::Control::Row(crate::ui::Column::List, index);
        let hover = hovers.hover(key);
        // Asked once per tile, exactly as the list asks it per row.
        let ignored = git
            .and_then(|g| g.status_for(&entry.path))
            .is_some_and(|s| s == df_core::git::FileStatus::Ignored);
        let selected = dir.is_selected(&entry.name);
        let marked = clip.is_some_and(|c| c.paths.contains(&entry.path));
        let cut = marked && clip.is_some_and(|c| c.cut);
        let lifted = !dragged.is_empty() && dragged.contains(&entry.path);

        let ground_here = if selected {
            mix(ground, palette.yellow, crate::ui::SELECT_TINT)
        } else {
            ground
        };
        // Instant, both ways: the keyboard cursor is not a pointer and does
        // not leave a trail (see the note above `crate::ui::ListView`).
        let glow = f32::from(index == dir.cursor()) * cursor_alpha;
        // The cursor's inner glow is lit only when the ring is busy saying
        // something else — on a *selected* tile, where the ring is yellow and
        // the cursor still has to be findable inside the selection. On a tile
        // that is only the cursor, the ring is already the cursor's own colour,
        // and lifting the ground to that same colour underneath it would rub
        // the ring out against its own fill.
        let inner_glow = if selected { glow } else { 0.0 };
        let base = mix(ground_here, crate::theme::cursor_fill(palette), inner_glow);
        let fill = mix(
            base,
            crate::theme::hover_fill(palette),
            hover * crate::ui::HOVER_LIFT,
        );
        let rect = crate::hover::pressed_rect(rect, hovers.press(key));
        // The cell is what the pointer and the ring are measured against; the
        // card is what is drawn (see [`card_rect`]).
        let card = card_rect(rect);
        if fill != ground {
            painter.rect_filled(card, TILE_RADIUS, fill);
        }

        // The picture, or the icon standing in for one.
        let thumb = metrics.thumb_rect(card);
        match thumbs.get(&entry.path) {
            Some(texture) => {
                let fitted = fit_into(texture.size_vec2(), thumb);
                // A `RectShape` with the texture as its brush rather than
                // `Painter::image`, because that one draws a hard-cornered
                // quad and this one takes a corner radius — which is the
                // concentric [`THUMB_RADIUS`] the tile's own rounding is
                // derived against (`delightful-ui` §15). egui has no rounded
                // clip, so the rounding has to come from the shape.
                painter.add(
                    egui::epaint::RectShape::filled(
                        fitted,
                        THUMB_RADIUS,
                        // The tint a texture is drawn through, carrying the
                        // FLIP fade and nothing else: white is "the picture as
                        // it is".
                        crate::chrome::fade(egui::Color32::WHITE, alpha),
                    )
                    .with_texture(
                        texture.id(),
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    ),
                );
                // …and the mute as a veil over it rather than as a multiply
                // through it — see [`dim_amount`].
                let veil = dim_amount(lifted || cut, ignored) * alpha;
                if veil > 0.0 {
                    painter.add(egui::epaint::RectShape::filled(
                        fitted,
                        THUMB_RADIUS,
                        ground.gamma_multiply(veil),
                    ));
                }
            }
            None => {
                let icon = crate::icons::icon_for(entry, paint.theme, palette, paint.nerd);
                let family = if paint.nerd {
                    egui::FontFamily::Name(crate::icons::ICON_FAMILY.into())
                } else {
                    egui::FontFamily::Monospace
                };
                painter.text(
                    thumb.center(),
                    egui::Align2::CENTER_CENTER,
                    icon.glyph,
                    egui::FontId::new(ICON_TILE, family),
                    crate::chrome::fade(mute(icon.color, ground, lifted || cut, ignored), alpha),
                );
            }
        }

        // The redundant channel (`delightful-ui`: a selection has to be
        // unmistakable at a glance, and a 10% wash is not). On a row it is a
        // bar at a fixed x; on a tile it is a **ring around the whole card**,
        // because a tile is a card and the thing a card is chosen by is its
        // edge. Yellow for a selection, the cursor's own colour for the cursor,
        // and when a tile is both the ring stays yellow — the cursor is already
        // saying its piece with the glow in the fill underneath, and two rings
        // is one more ring than there is room for.
        let ring = if selected {
            Some(palette.yellow)
        } else if glow > 0.0 {
            Some(crate::theme::cursor_fill(palette))
        } else {
            None
        };
        if let Some(colour) = ring {
            painter.rect_stroke(
                rect,
                TILE_RING_RADIUS,
                egui::Stroke::new(TILE_RING, crate::chrome::fade(colour, alpha)),
                egui::StrokeKind::Inside,
            );
        }
        if marked {
            // A pip in the top corner rather than a bar down the side: the
            // side is where the ring is now, and a mark competing with it for
            // the same two points of edge would read as a broken ring.
            let centre = egui::pos2(
                card.right() - TILE_PAD - TILE_BADGE,
                card.top() + TILE_PAD + TILE_BADGE,
            );
            let colour = if cut { palette.peach } else { palette.teal };
            painter.circle_filled(
                centre,
                TILE_BADGE + TILE_BADGE_HALO,
                crate::chrome::fade(ground, alpha),
            );
            painter.circle_filled(centre, TILE_BADGE, crate::chrome::fade(colour, alpha));
        }

        let inside = painter.with_clip_rect(card);
        for splash in ripples.splashes(key, paint.now) {
            inside.circle_filled(
                splash.center,
                splash.radius,
                egui::Color32::from_white_alpha((splash.alpha * 255.0).round() as u8),
            );
        }

        // The name, over two lines, ellipsised. Centred under the picture
        // rather than left-aligned: the tile is a card about one file and its
        // name is that card's caption.
        let label = metrics.label_rect(card);
        let name_colour = mute(
            crate::icons::name_color(entry, palette),
            ground,
            lifted || cut,
            ignored,
        );
        // The tag dots, muted as the name is, at the end of its last line.
        let dots: Vec<egui::Color32> = paint
            .tags
            .dots(&entry.tags, palette)
            .into_iter()
            .map(|colour| crate::chrome::fade(mute(colour, ground, lifted || cut, ignored), alpha))
            .collect();
        name(
            &inside,
            label,
            &entry.name,
            dir.row_spans(index),
            crate::chrome::fade(name_colour, alpha),
            crate::chrome::fade(palette.sky, alpha),
            Dots {
                colours: &dots,
                behind: fill,
            },
        );
    }
    // The ghosts are drawn at whole cells, so they take the cell's radius.
    paint.flip_ghosts(&painter, flip, content, TILE_RING_RADIUS);
}

/// How far one tile's ink is mixed back into the pane behind it.
///
/// The list's two answers, in the list's order of precedence: a tile on its way
/// out of here ([`crate::ui::PARENT_DIM`], for a cut or a lift) outranks one
/// that is merely gitignored ([`crate::ui::IGNORED_DIM`]), because "this is
/// leaving" is the more urgent of the two facts and they cannot both be said in
/// one channel. Both numbers are the list's own — a grid that dimmed by its own
/// amount would be the same directory looking different depending on which key
/// you last pressed.
fn mute(
    colour: egui::Color32,
    ground: egui::Color32,
    leaving: bool,
    ignored: bool,
) -> egui::Color32 {
    crate::theme::mix(colour, ground, dim_amount(leaving, ignored))
}

/// The mute as a *number*, for the one thing that cannot be muted by mixing:
/// a photograph.
///
/// A texture is drawn through a tint, and a tint multiplies. Handing the
/// thumbnail `mix(WHITE, ground, dim)` therefore does not move the picture
/// towards the pane the way it moves a glyph — it multiplies every pixel by a
/// dark colour, so a dark photograph in a dimmed tile goes black while a bright
/// one merely dulls, and the same amount of dim reads as two different amounts
/// of dim. The tile draws the picture at full strength and lays a veil of the
/// pane's own colour over it at this amount instead, which is the gesture
/// [`crate::preview::paint`]'s crossfade makes for the same reason.
fn dim_amount(leaving: bool, ignored: bool) -> f32 {
    if leaving {
        crate::ui::PARENT_DIM
    } else if ignored {
        crate::ui::IGNORED_DIM
    } else {
        0.0
    }
}

/// How big the stand-in glyph is on a tile with no picture.
///
/// Two thirds of a tile's width would be a glyph so large it reads as a logo;
/// 44 points is about a quarter of one and sits where the eye expects a
/// thumbnail's subject to be. A directory of text files is still a legible
/// grid rather than a wall of enormous icons.
const ICON_TILE: f32 = 44.0;

/// Fit an image of `size` inside `into`, centred, never enlarged past the box.
fn fit_into(size: egui::Vec2, into: egui::Rect) -> egui::Rect {
    if size.x <= 0.0 || size.y <= 0.0 {
        return into;
    }
    let scale = (into.width() / size.x).min(into.height() / size.y);
    let fitted = size * scale;
    egui::Rect::from_center_size(into.center(), fitted)
}

/// The tag dots a caption ends with ([`crate::tags`]), and the colour the
/// tile is lit with, which they wear as a ring.
struct Dots<'a> {
    colours: &'a [egui::Color32],
    behind: egui::Color32,
}

/// A tile's name: up to two lines, centred, with the filter's matched runs
/// highlighted the same way a row's are — and its tag dots at the right of
/// the last line, the name and the dots centred as one.
fn name(
    painter: &egui::Painter,
    rect: egui::Rect,
    text: &str,
    runs: &[df_core::fs::Span],
    colour: egui::Color32,
    highlight: egui::Color32,
    dots: Dots<'_>,
) {
    use egui::text::{LayoutJob, TextFormat, TextWrapping};
    let format = |colour: egui::Color32| TextFormat {
        font_id: egui::FontId::proportional(LABEL_FONT),
        color: colour,
        ..Default::default()
    };
    let mut job = LayoutJob::default();
    let mut at = 0usize;
    for &(start, end) in runs {
        if start < at
            || end > text.len()
            || start >= end
            || !text.is_char_boundary(start)
            || !text.is_char_boundary(end)
        {
            continue;
        }
        if at < start {
            job.append(&text[at..start], 0.0, format(colour));
        }
        job.append(&text[start..end], 0.0, format(highlight));
        at = end;
    }
    if at < text.len() {
        job.append(&text[at..], 0.0, format(colour));
    }
    // The dots' room comes out of the caption's lines, so a name that fills
    // both is cut short before them rather than under them.
    let room = if dots.colours.is_empty() {
        0.0
    } else {
        crate::tags::DOT_GAP + crate::tags::dots_width(dots.colours.len())
    };
    job.halign = egui::Align::Center;
    job.wrap = TextWrapping {
        max_width: (rect.width() - room).max(0.0),
        max_rows: 2,
        // Break mid-word: a file name is not prose, and a screenshot called
        // `Screenshot_2024-08-17_at_18.34.22.png` has no word boundaries to
        // break at anyway.
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    let galley = painter.layout_job(job);
    let origin = egui::pos2(rect.center().x - room / 2.0, rect.top());
    let last = galley.rows.last().map(|row| row.rect());
    painter.galley(origin, galley, colour);
    if let Some(last) = last.filter(|_| room > 0.0) {
        crate::tags::paint_dots(
            painter,
            origin.x + last.right() + crate::tags::DOT_GAP,
            origin.y + last.center().y,
            dots.colours,
            dots.behind,
        );
    }
}

/// The tile name's font.
///
/// A point smaller than the list's, because a tile shows two lines of it in a
/// column a fifth as wide and the extra characters per line are worth more here
/// than the extra legibility is.
const LABEL_FONT: f32 = 12.5;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mouse::Corner;

    /// The columns divide the width exactly, never leaving a ragged strip, and
    /// there is always at least one of them.
    #[test]
    fn the_tiles_divide_the_pane_exactly() {
        for width in [1.0, 40.0, 160.0, 337.0, 800.0, 1440.0] {
            let m = metrics(width);
            assert!(m.columns >= 1, "at {width}");
            let used = m.tile.x * m.columns as f32 + TILE_GAP * (m.columns as f32 - 1.0);
            assert!(
                (used - width).abs() < 0.01 || width < TILE_WIDTH,
                "at {width}: used {used}"
            );
        }
        // A tile never shrinks below its minimum, and never reaches twice it —
        // at twice, another column would have fitted.
        for width in [160.0, 200.0, 500.0, 1000.0, 2000.0] {
            let m = metrics(width);
            assert!(m.tile.x >= TILE_WIDTH - 0.01, "at {width}: {}", m.tile.x);
            assert!(m.tile.x < TILE_WIDTH * 2.0, "at {width}: {}", m.tile.x);
        }
    }

    fn content() -> egui::Rect {
        egui::Rect::from_min_size(
            egui::pos2(10.0, 20.0),
            egui::vec2(3.0 * TILE_WIDTH + 2.0 * TILE_GAP, 400.0),
        )
    }

    /// A three-wide grid, so the arithmetic can be read off by hand.
    fn three_wide() -> Metrics {
        let m = metrics(content().width());
        assert_eq!(m.columns, 3, "the fixture is meant to be three wide");
        m
    }

    /// The hit test is the inverse of the layout — the invariant the list's
    /// `row_at` / `row_rect` pair has, transplanted.
    #[test]
    fn the_hit_test_is_the_inverse_of_the_layout() {
        let (content, m) = (content(), three_wide());
        for scroll in [0.0, 0.5, 2.0] {
            for index in 0..7 {
                let rect = tile_rect(content, &m, scroll, index);
                if !content.contains(rect.center()) {
                    continue;
                }
                assert_eq!(
                    tile_at(content, &m, scroll, 7, rect.center()),
                    Some(index),
                    "index {index} at scroll {scroll}"
                );
            }
        }
    }

    /// The alley between two tiles belongs to the pane, not to either of them —
    /// which is what lets a drag started there be a band select.
    #[test]
    fn the_gap_between_tiles_is_not_a_tile() {
        let (content, m) = (content(), three_wide());
        let first = tile_rect(content, &m, 0.0, 0);
        let between = egui::pos2(first.right() + TILE_GAP / 2.0, first.center().y);
        assert_eq!(tile_at(content, &m, 0.0, 7, between), None);
        // And so does the strip under the last row.
        let below = egui::pos2(first.center().x, first.bottom() + TILE_GAP / 2.0);
        assert_eq!(tile_at(content, &m, 0.0, 7, below), None);
    }

    /// Past the end of the listing is nothing, not tile four hundred.
    #[test]
    fn past_the_last_tile_is_nothing() {
        let (content, m) = (content(), three_wide());
        let seventh = tile_rect(content, &m, 0.0, 7);
        assert_eq!(tile_at(content, &m, 0.0, 7, seventh.center()), None);
        assert_eq!(tile_at(content, &m, 0.0, 0, content.center()), None);
        assert_eq!(
            tile_at(content, &m, 0.0, 7, egui::pos2(-100.0, -100.0)),
            None
        );
    }

    /// Seven files in a three-wide grid: `[0 1 2] [3 4 5] [6]`. Every arrow at
    /// every edge, the ragged last row, and the vertical wrap.
    #[test]
    fn the_arrows_move_in_two_dimensions_and_wrap_vertically() {
        let go = |from: usize, s: Step| step(from, 7, 3, s);
        // Down a full row.
        assert_eq!(go(0, Step::Down), 3);
        // Down into the ragged last row: there is no tile under the fifth, and
        // "nothing happens" is the wrong answer when there is obviously
        // somewhere below.
        assert_eq!(go(4, Step::Down), 6);
        assert_eq!(go(5, Step::Down), 6);
        // Already in the last row: off the bottom and back to the top of the
        // same column, never sideways.
        assert_eq!(go(6, Step::Down), 0);
        assert_eq!(go(4, Step::Up), 1);
        // Up out of the first row is the bottom of that column — tile 6 for
        // column 0, and the row above it for the two columns the ragged last
        // row is short of.
        assert_eq!(go(0, Step::Up), 6);
        assert_eq!(go(1, Step::Up), 4);
        assert_eq!(go(2, Step::Up), 5);
        assert_eq!(go(6, Step::Up), 3);
        // Left and right are linear: they walk off the end of a row into the
        // next one, because the tiles are one sequence read like a page.
        assert_eq!(go(3, Step::Left), 2);
        assert_eq!(go(2, Step::Right), 3);
        // …and clamp at the two real ends.
        assert_eq!(go(0, Step::Left), 0);
        assert_eq!(go(6, Step::Right), 6);
    }

    /// An empty directory and a one-column pane are both real and neither may
    /// divide by zero.
    #[test]
    fn the_degenerate_grids_do_not_panic() {
        for s in [Step::Left, Step::Right, Step::Up, Step::Down] {
            assert_eq!(step(0, 0, 3, s), 0);
            assert_eq!(step(0, 1, 0, s), 0);
            // A one-column grid is a list, and the arrows have to behave like
            // one: `↑`/`←` are the previous file and `↓`/`→` the next — except
            // that `↓` on the last row wraps to the first and `→` refuses, so
            // that `→` can still mean "enter" there.
            let expected = match s {
                Step::Left | Step::Up => 1,
                Step::Right => 2,
                Step::Down => 0,
            };
            assert_eq!(step(5, 3, 1, s), expected, "{s:?}");
        }
    }

    /// The decode window is the visible rows plus the partial one at the
    /// bottom plus the lookahead — and it never runs off the end of the
    /// listing.
    #[test]
    fn the_lookahead_reaches_one_row_past_the_bottom() {
        // 30 files, 3 columns, 10 rows. Two rows visible from row 0.
        let window = wanted(30, 3, 0, 2);
        // Rows 0 and 1 visible, row 2 partly, row 3 the lookahead → 12 tiles.
        assert_eq!(window, 0..12);
        // Scrolled down, the window travels with the view.
        assert_eq!(wanted(30, 3, 4, 2), 12..24);
        // Near the end it is clamped to what exists rather than asking for
        // tiles that are not there.
        assert_eq!(wanted(30, 3, 8, 2), 24..30);
        assert_eq!(wanted(0, 3, 0, 2), 0..0);
        // A degenerate column count must not divide by zero.
        assert_eq!(wanted(5, 0, 0, 1), 0..3);
    }

    /// The seam every caller outside this module goes through: `None` is a
    /// list and `Some(metrics)` is a grid, and both answer the same two
    /// questions the same way round.
    #[test]
    fn the_seam_answers_for_both_geometries_and_stays_invertible() {
        let (content, m) = (content(), three_wide());
        let scale = crate::ui::Scale::default();
        // List: the seam is `ui::row_rect` / `ui::row_at`, unchanged.
        let row = pane_rect(content, None, 0.0, 3, scale);
        assert_eq!(row, crate::ui::row_rect(content, 0.0, 3, scale.row_height));
        assert_eq!(pane_at(content, None, 0.0, 7, row.center(), scale), Some(3));
        assert_eq!(pane_step(None, scale), crate::ui::ROW_HEIGHT);
        // Grid: the tiles, and the step is a whole row of them.
        let tile = pane_rect(content, Some(&m), 0.0, 4, scale);
        assert_eq!(tile, tile_rect(content, &m, 0.0, 4));
        assert_eq!(
            pane_at(content, Some(&m), 0.0, 7, tile.center(), scale),
            Some(4)
        );
        assert_eq!(pane_step(Some(&m), scale), m.step.y);
        // The list half of the seam follows the ladder; the grid half is the
        // top of that ladder and has a geometry of its own, so the scale it is
        // handed changes nothing about it.
        let roomy = crate::ui::Scale::new(df_core::config::ViewScale::Roomy);
        assert_eq!(pane_step(None, roomy), roomy.row_height);
        assert!(roomy.row_height > scale.row_height);
        assert_eq!(pane_step(Some(&m), roomy), m.step.y);
        assert_eq!(pane_rect(content, Some(&m), 0.0, 4, roomy), tile);
    }

    /// A band over a grid selects everything from the first tile it touches to
    /// the last — the run `select_range` can express, and what dragging over a
    /// wall of pictures visibly means.
    #[test]
    fn a_band_over_the_grid_is_the_run_it_covers() {
        let (content, m) = (content(), three_wide());
        // A band over tiles 1 and 4 (one above the other) covers 1..=4.
        let one = tile_rect(content, &m, 0.0, 1);
        let four = tile_rect(content, &m, 0.0, 4);
        let band = still(content, &m, 0.0, 7, one.min, four.max);
        assert_eq!(band, Some((1, 4)));
        // A band over nothing is nothing, not a panic and not `(0, 0)`.
        let (far, farther) = (egui::pos2(-500.0, -500.0), egui::pos2(-499.0, -499.0));
        assert_eq!(still(content, &m, 0.0, 7, far, farther), None);
        // Dragged straight across the first row, with no height at all: the
        // tiles it passes through.
        let zero = tile_rect(content, &m, 0.0, 0);
        let across = egui::pos2(one.center().x, zero.center().y);
        assert_eq!(
            still(content, &m, 0.0, 7, zero.center(), across),
            Some((0, 1))
        );
    }

    /// A band from the parent column, left of the grid: nothing until it
    /// reaches the tiles, then the run it covers.
    #[test]
    fn a_band_from_left_of_the_grid_takes_tiles_once_it_reaches_them() {
        let (content, m) = (content(), three_wide());
        let zero = tile_rect(content, &m, 0.0, 0);
        let four = tile_rect(content, &m, 0.0, 4);
        let from = egui::pos2(content.left() - 100.0, zero.center().y);
        let short = egui::pos2(content.left() - 10.0, four.center().y);
        assert_eq!(still(content, &m, 0.0, 9, from, short), None);
        // Over the first two columns of two rows: 0, 1, 3, 4.
        let band = still(content, &m, 0.0, 9, from, four.center());
        assert_eq!(band, Some((0, 4)));
    }

    /// A band from below the grid is clipped to the pane's bottom edge — which,
    /// in this fixture, is exactly the top of the third row of tiles. That row
    /// is off screen, and the band only touching it must not take it.
    #[test]
    fn a_band_from_below_the_grid_stops_at_the_tiles_on_screen() {
        let (content, m) = (content(), three_wide());
        let hidden = tile_rect(content, &m, 0.0, 7);
        assert_eq!(hidden.top(), content.bottom(), "the fixture's premise");
        let one = tile_rect(content, &m, 0.0, 1);
        let from = egui::pos2(one.center().x, content.bottom() + 40.0);
        // Tiles 1 and 4 — not 7, under the pane.
        let band = still(content, &m, 0.0, 9, from, one.center());
        assert_eq!(band, Some((1, 4)));
        // Half a row scrolled, the third row is partly on screen and is taken.
        let one = tile_rect(content, &m, 0.5, 1);
        let band = still(content, &m, 0.5, 9, from, one.center());
        assert_eq!(band, Some((1, 7)));
    }

    /// A band from the preview pane, right of the grid: the mirror image.
    #[test]
    fn a_band_from_the_preview_pane_takes_tiles_once_it_reaches_them() {
        let (content, m) = (content(), three_wide());
        let two = tile_rect(content, &m, 0.0, 2);
        let five = tile_rect(content, &m, 0.0, 5);
        let from = egui::pos2(content.right() + 150.0, two.center().y);
        let short = egui::pos2(content.right() + 20.0, five.center().y);
        assert_eq!(still(content, &m, 0.0, 9, from, short), None);
        // Into the last column only: 2 and 5.
        let band = still(content, &m, 0.0, 9, from, five.center());
        assert_eq!(band, Some((2, 5)));
    }

    /// A band that scrolled the grid keeps the tiles it scrolled past: the
    /// origin rides with the rows of tiles (see `mouse::band_span`).
    #[test]
    fn a_grid_band_that_scrolled_keeps_the_tiles_it_scrolled_past() {
        let (content, m) = (content(), three_wide());
        let one = tile_rect(content, &m, 0.0, 1);
        let origin = Corner {
            at: one.center(),
            scroll: 0.0,
        };
        // Pulled off the bottom and scrolled three rows of tiles: rows 3 and
        // 4 are on screen, and the band runs from tile 1 to the end of row 4
        // — the last column is under the pointer, which is right of it.
        let pointer = Corner {
            at: egui::pos2(content.right() - 5.0, content.bottom() + 30.0),
            scroll: 3.0,
        };
        assert_eq!(
            band_items(content, &m, 3.0, 30, origin, pointer),
            Some((1, 14))
        );
    }

    /// A band drawn with the grid held still: both corners placed at `scroll`.
    fn still(
        content: egui::Rect,
        m: &Metrics,
        scroll: f32,
        count: usize,
        from: egui::Pos2,
        to: egui::Pos2,
    ) -> Option<(usize, usize)> {
        let corner = |at| Corner { at, scroll };
        band_items(content, m, scroll, count, corner(from), corner(to))
    }

    /// The whole grid lays out and paints — with a cursor, a selection, a
    /// clipboard mark, a drag in progress, a re-sort in flight and a pane too
    /// small to hold a single tile. And with **no thumbnails at all**, which is
    /// PLAN §2's "directories with no thumbnailable content still work": every
    /// tile falls back to its icon and nothing about that is a special case.
    #[test]
    fn the_grid_paints_in_every_state_without_panicking() {
        use df_core::config::MgrConfig;
        use std::collections::HashSet;

        let now = std::time::Instant::now();
        let mgr = MgrConfig::default();
        // A real directory, because `DirState` reads one — and this crate's own
        // source tree is one that is certainly there.
        let mut dir = df_core::fs::DirState::new(env!("CARGO_MANIFEST_DIR"), &mgr);
        let _ = dir.load_blocking();
        dir.set_cursor(1);
        if let Some(name) = dir.row(0).map(|e| e.name.clone()) {
            dir.toggle_selected(0);
            assert!(dir.is_selected(&name));
        }
        let clip_paths: HashSet<PathBuf> = dir.row(1).map(|e| e.path.clone()).into_iter().collect();
        let dragged: HashSet<PathBuf> = dir.row(2).map(|e| e.path.clone()).into_iter().collect();

        let notify: df_core::fs::Notifier = Arc::new(|| {});
        let thumbs = Thumbs::start(notify);
        let mut before = crate::flip::Snapshot::new();
        if let Some(entry) = dir.row(0) {
            before.insert(
                entry.path.clone(),
                egui::Rect::from_min_size(egui::pos2(0.0, 900.0), egui::vec2(10.0, 10.0)),
            );
        }
        let after = crate::flip::Snapshot::new();
        let flip = crate::flip::Flip::begin(&before, &after, now);

        let ctx = egui::Context::default();
        let _ = ctx.run_ui(Default::default(), |ui| {
            let palette = crate::theme::Palette::default();
            let theme = df_core::config::Theme::default();
            let painting = crate::ui::Painting {
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
            for pane in [
                egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(900.0, 700.0)),
                // Narrower than one tile, and shorter than one row of them.
                egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(60.0, 40.0)),
            ] {
                let content = crate::ui::content_rect(pane);
                let metrics = metrics(content.width());
                for scroll in [0.0, 0.5, 3.0] {
                    paint(
                        &painting,
                        GridView {
                            pane,
                            ground: palette.base,
                            dir: &dir,
                            scroll_rows: scroll,
                            metrics,
                            hovers: &crate::hover::Hovers::new(),
                            ripples: &crate::ripple::Ripples::new(),
                            cursor_alpha: 1.0,
                            thumbs: &thumbs,
                            clip: Some(crate::ui::ClipMark {
                                paths: &clip_paths,
                                cut: true,
                            }),
                            dragged: &dragged,
                            flip: flip.as_ref(),
                            slow_load: false,
                            git: None,
                            empty_note: None,
                        },
                    );
                }
            }
        });
    }

    /// The thumbnail's radius is derived from the tile's and the padding, not
    /// picked — `delightful-ui` §15's concentric rule, pinned so a later tweak
    /// to one of the three cannot silently break it.
    #[test]
    fn the_nested_radii_are_concentric() {
        assert_eq!(
            u32::from(TILE_RADIUS),
            u32::from(THUMB_RADIUS) + TILE_PAD as u32
        );
        // And a tile sits the same distance from the pane's edge as a row does,
        // so toggling the view does not move where the content starts.
        assert_eq!(TILE_GAP, ROW_INSET);
        // The ring is the next shell out, by the same rule: its radius is the
        // card's plus everything between the two edges.
        assert_eq!(
            u32::from(TILE_RING_RADIUS),
            u32::from(TILE_RADIUS) + TILE_RING as u32 + TILE_RING_GAP as u32
        );
    }

    /// The ring goes *around* the card, inside the cell — so it is never
    /// clipped off against the pane's edge, which is exactly where the first
    /// and last columns of every grid live.
    #[test]
    fn the_ring_fits_inside_the_cell_it_rings() {
        let (content, m) = (content(), three_wide());
        for index in [0usize, 2, 3, 5] {
            let cell = tile_rect(content, &m, 0.0, index);
            let card = card_rect(cell);
            // The card is inset by the ring and its air, on every side.
            let inset = TILE_RING + TILE_RING_GAP;
            assert!(
                (card.left() - (cell.left() + inset)).abs() < 1e-3,
                "{index}"
            );
            assert!(
                (card.bottom() - (cell.bottom() - inset)).abs() < 1e-3,
                "{index}"
            );
            // …and the cell — which is what the ring is stroked inside — is
            // within the pane's content box, first column and last alike.
            assert!(cell.left() >= content.left() - 1e-3, "{index}");
            assert!(cell.right() <= content.right() + 1e-3, "{index}");
        }
        // Neighbouring cells still do not touch: the ring took its room out of
        // the card, not out of the gap between tiles.
        let first = tile_rect(content, &m, 0.0, 0);
        let second = tile_rect(content, &m, 0.0, 1);
        assert!((second.left() - first.right() - TILE_GAP).abs() < 1e-3);
    }
}
