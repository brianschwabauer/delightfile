//! The thumbnail grid: PLAN §2's per-directory alternative to the list.
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
/// - **Nothing wraps.** Off the top is the top and off the bottom is the
///   bottom, exactly as [`df_core::fs::DirState::move_cursor`] clamps in the
///   list. A directory has a beginning and an end and the cursor may not
///   teleport between them.
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
            if cursor < columns {
                cursor
            } else {
                cursor - columns
            }
        }
        Step::Down => {
            let below = cursor + columns;
            if below <= last {
                below
            } else if cursor / columns == last / columns {
                // Already in the last row: there is nothing below.
                cursor
            } else {
                // The row below exists but is short of this column.
                last
            }
        }
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
) -> egui::Rect {
    match metrics {
        Some(metrics) => tile_rect(content, metrics, scroll_rows, index),
        None => crate::ui::row_rect(content, scroll_rows, index),
    }
}

/// Which item the pointer is over, whichever geometry the pane is in.
pub fn pane_at(
    content: egui::Rect,
    metrics: Option<&Metrics>,
    scroll_rows: f32,
    count: usize,
    pos: egui::Pos2,
) -> Option<usize> {
    match metrics {
        Some(metrics) => tile_at(content, metrics, scroll_rows, count, pos),
        None => crate::ui::row_at(content, scroll_rows, count, pos),
    }
}

/// How tall one item of the list pane is — what the wheel converts points into
/// and what the scrolloff rule counts.
pub fn pane_step(metrics: Option<&Metrics>) -> f32 {
    match metrics {
        Some(metrics) => metrics.step.y,
        None => crate::ui::ROW_HEIGHT,
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
pub fn band_items(
    content: egui::Rect,
    metrics: &Metrics,
    scroll_rows: f32,
    count: usize,
    band: egui::Rect,
) -> Option<(usize, usize)> {
    let mut first = None;
    let mut last = None;
    for index in 0..count {
        let rect = tile_rect(content, metrics, scroll_rows, index);
        if !rect.intersects(band) || !rect.intersects(content) {
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
    pub cursor_glow: &'a crate::hover::Hovers<usize>,
    /// How strongly the cursor tile is lit: 1 in the focused pane, and
    /// [`crate::ui::GHOST_CURSOR`] everywhere else — the same "where am I"
    /// answer the list gives (PLAN §2.1).
    pub cursor_alpha: f32,
    pub thumbs: &'a Thumbs,
    pub clip: Option<crate::ui::ClipMark<'a>>,
    pub dragged: &'a std::collections::HashSet<PathBuf>,
    pub flip: Option<&'a crate::flip::Flip>,
    pub slow_load: bool,
}

/// Draw a directory as a wall of tiles.
///
/// Every state a row can be in, a tile is in too, and wearing the same colour:
/// the cursor is a lift towards `surface1`, a selection is the yellow tint plus
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
        cursor_glow,
        cursor_alpha,
        thumbs,
        clip,
        dragged,
        flip,
        slow_load,
    } = view;
    let content = crate::ui::content_rect(pane);
    // The same sentence the list would show — "empty", "still reading", the
    // reason it could not be read. A grid of nothing and a list of nothing are
    // the same fact and must not be two different messages.
    if let Some(message) = paint.pane_state_message(dir, slow_load) {
        paint.quiet_label(content, &message);
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
        let selected = dir.is_selected(&entry.name);
        let marked = clip.is_some_and(|c| c.paths.contains(&entry.path));
        let cut = marked && clip.is_some_and(|c| c.cut);
        let lifted = !dragged.is_empty() && dragged.contains(&entry.path);

        let ground_here = if selected {
            mix(ground, palette.yellow, crate::ui::SELECT_TINT)
        } else {
            ground
        };
        let glow = cursor_glow.hover(index) * cursor_alpha;
        let base = mix(ground_here, palette.surface1, glow);
        let fill = mix(base, palette.surface0, hover * crate::ui::HOVER_LIFT);
        let rect = crate::hover::pressed_rect(rect, hovers.press(key));
        if fill != ground {
            painter.rect_filled(rect, TILE_RADIUS, fill);
        }

        // The picture, or the icon standing in for one.
        let thumb = metrics.thumb_rect(rect);
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
                        crate::chrome::fade(egui::Color32::WHITE, alpha),
                    )
                    .with_texture(
                        texture.id(),
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                    ),
                );
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
                    crate::chrome::fade(
                        if lifted || cut {
                            mix(icon.color, ground, crate::ui::PARENT_DIM)
                        } else {
                            icon.color
                        },
                        alpha,
                    ),
                );
            }
        }

        if selected {
            // The redundant channel, exactly as on a row: a hard mark at a
            // fixed edge, so a selection is legible as a shape and not only as
            // a colour. Down the tile's left side rather than across it,
            // because that is where a row's is and the two views must not
            // teach two different marks.
            let bar = egui::Rect::from_min_max(
                egui::pos2(rect.left(), rect.top() + TILE_PAD),
                egui::pos2(
                    rect.left() + crate::ui::SELECT_BAR_WIDTH,
                    rect.bottom() - TILE_PAD,
                ),
            );
            painter.rect_filled(bar, 1, crate::chrome::fade(palette.yellow, alpha));
        }
        if marked {
            let chip = egui::Rect::from_min_max(
                egui::pos2(
                    rect.right() - crate::ui::CLIP_BAR_WIDTH,
                    rect.top() + TILE_PAD,
                ),
                egui::pos2(rect.right(), rect.bottom() - TILE_PAD),
            );
            let colour = if cut { palette.peach } else { palette.teal };
            painter.rect_filled(chip, 1, crate::chrome::fade(colour, alpha));
        }

        let inside = painter.with_clip_rect(rect);
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
        let label = metrics.label_rect(rect);
        let name_colour = if lifted || cut {
            mix(
                crate::icons::name_color(entry, palette),
                ground,
                crate::ui::PARENT_DIM,
            )
        } else {
            crate::icons::name_color(entry, palette)
        };
        name(
            &inside,
            label,
            &entry.name,
            dir.row_spans(index),
            crate::chrome::fade(name_colour, alpha),
            crate::chrome::fade(palette.sky, alpha),
        );
    }
    paint.flip_ghosts(&painter, flip, content, TILE_RADIUS);
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

/// A tile's name: up to two lines, centred, with the filter's matched runs
/// highlighted the same way a row's are.
fn name(
    painter: &egui::Painter,
    rect: egui::Rect,
    text: &str,
    runs: &[df_core::fs::Span],
    colour: egui::Color32,
    highlight: egui::Color32,
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
    job.halign = egui::Align::Center;
    job.wrap = TextWrapping {
        max_width: rect.width(),
        max_rows: 2,
        // Break mid-word: a file name is not prose, and a screenshot called
        // `Screenshot_2024-08-17_at_18.34.22.png` has no word boundaries to
        // break at anyway.
        break_anywhere: true,
        overflow_character: Some('…'),
    };
    let galley = painter.layout_job(job);
    painter.galley(egui::pos2(rect.center().x, rect.top()), galley, colour);
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
    /// every edge, and the ragged last row.
    #[test]
    fn the_arrows_move_in_two_dimensions_and_clamp_at_the_edges() {
        let go = |from: usize, s: Step| step(from, 7, 3, s);
        // Down a full row.
        assert_eq!(go(0, Step::Down), 3);
        // Down into the ragged last row: there is no tile under the fifth, and
        // "nothing happens" is the wrong answer when there is obviously
        // somewhere below.
        assert_eq!(go(4, Step::Down), 6);
        assert_eq!(go(5, Step::Down), 6);
        // Already in the last row: nothing below, and no wrap to the top.
        assert_eq!(go(6, Step::Down), 6);
        // Up out of the first row stays put.
        assert_eq!(go(1, Step::Up), 1);
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
            // one: `↑`/`←` are the previous file and `↓`/`→` the next.
            let expected = match s {
                Step::Left | Step::Up => 1,
                Step::Right | Step::Down => 2,
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
        // List: the seam is `ui::row_rect` / `ui::row_at`, unchanged.
        let row = pane_rect(content, None, 0.0, 3);
        assert_eq!(row, crate::ui::row_rect(content, 0.0, 3));
        assert_eq!(pane_at(content, None, 0.0, 7, row.center()), Some(3));
        assert_eq!(pane_step(None), crate::ui::ROW_HEIGHT);
        // Grid: the tiles, and the step is a whole row of them.
        let tile = pane_rect(content, Some(&m), 0.0, 4);
        assert_eq!(tile, tile_rect(content, &m, 0.0, 4));
        assert_eq!(pane_at(content, Some(&m), 0.0, 7, tile.center()), Some(4));
        assert_eq!(pane_step(Some(&m)), m.step.y);
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
        let band = one.union(four);
        assert_eq!(band_items(content, &m, 0.0, 7, band), Some((1, 4)));
        // A band over nothing is nothing, not a panic and not `(0, 0)`.
        let empty = egui::Rect::from_min_size(egui::pos2(-500.0, -500.0), egui::vec2(1.0, 1.0));
        assert_eq!(band_items(content, &m, 0.0, 7, empty), None);
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
                painter: ui.painter(),
                palette: &palette,
                theme: &theme,
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
                            cursor_glow: &crate::hover::Hovers::new(),
                            cursor_alpha: 1.0,
                            thumbs: &thumbs,
                            clip: Some(crate::ui::ClipMark {
                                paths: &clip_paths,
                                cut: true,
                            }),
                            dragged: &dragged,
                            flip: flip.as_ref(),
                            slow_load: false,
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
    }
}
