//! The preview pane: what is under the cursor, drawn (PLAN §6).
//!
//! ```text
//!   cursor moves ──▶ Pane::sync ──▶ df_core::preview::Previewer  (debounced,
//!                                        │                        newest wins)
//!                    Pane::poll ◀── PreviewUpdate ────────────────┘
//!                        │
//!                        ├─ Text / Markdown ──▶ prepare::Preparer ──▶ paint
//!                        ├─ Directory / Hex ──▶ paint
//!                        ├─ NeedsDecode ──▶ decode::Decoder ──▶ texture ──▶ paint
//!                        └─ …an archive ──▶ body::Bodies ──▶ listing ──▶ paint
//! ```
//!
//! Three rules hold the whole thing together.
//!
//! **Nothing decodes on the paint thread — and nothing parses on it either.**
//! df-core's workers read the file, [`decode`]'s worker turns bytes into
//! pixels (and into the `ColorImage` egui uploads from), and [`prepare`]'s
//! worker runs the highlighter and the markdown parser. This module only ever
//! receives finished work and hands it to egui. Every one of those workers
//! rings the same [`crate::Wake`] bell every other worker rings (PLAN §1).
//! [`body`] is the same rule for the two cards that do not come through this
//! pane at all — an archive entry and a remote row.
//!
//! **Every answer is checked against its token.** df-core cancels a superseded
//! request before it opens anything, and the token check here catches whatever
//! wins the race anyway — so a slow decode of the file you arrowed past can
//! never paint over the file you stopped on.
//!
//! **A new file appears instantly; a new picture of the *same* file
//! crossfades.** PLAN §6 asks for an 80 ms crossfade ([`CROSSFADE`]) and it is
//! still here — for the two swaps that are genuinely one thing turning into
//! another: the cached thumbnail giving way to the full decode, and a document
//! turning a page. Arrowing from one file to the next is not that. The old body
//! is dropped the moment the path changes (there is nothing honest to blend a
//! new file's text against), so a fade-in there was a blank pane followed by a
//! dissolve — 80 ms of nothing on every single arrow key.
//!
//! ## The seam, and what has taken it up
//!
//! Video, PDFs, fonts and models reach [`decode::Job`] with
//! [`decode::Full::Elsewhere`]: what it fetches for them is the *cached
//! thumbnail*, and the real answer comes from somewhere else.
//!
//! **Video and audio are now [`crate::playback`]'s.** The cached thumbnail is
//! still what this module draws — it is the poster the first decoded frame
//! lands on top of — and [`Pane::set_media_mounted`] is how it is told that a
//! transport has the file, at which point the kind badge stands down and the
//! frame, the audio card and the position strip are painted over this pane by
//! the player.
//!
//! **A song's picture is this module's, though, not the player's.** Its sleeve
//! is an `ATTACHED_PIC` stream that dv-media deliberately does not play, so
//! nothing on the player's side will ever put it on screen. [`decode`] fetches
//! it instead ([`decode::Full::CoverArt`]) and this pane draws it exactly as it
//! draws a photograph — fitted, zoomable, under the strip — and
//! [`Pane::poster`] is how the audio card knows to stay out of its way.
//!
//! **PDFs, fonts, models and G-code are [`doc`]'s.** They take the same seam
//! from the other side: [`decode`] still fetches the cached thumbnail so a PDF
//! has a poster the instant the cursor lands on it, and [`doc::Docs`] renders
//! the real page over the top. The four of them share one [`DocView`] because
//! they share one shape — a document with a current page, a zoom and an
//! indicator — even though a specimen sheet has one page and a mesh does not
//! really have pages at all.
//!
//! **Archives are [`listing`]'s.** df-core answers an archive through the same
//! seam, with nothing to decode, so the kind badge goes up on the first frame
//! exactly as it did before the archive had a previewer — and the listing,
//! read on [`body`]'s worker, replaces it when it lands ([`Body::Archive`]).
//! When nothing on the machine can list the file, the badge stays and says why
//! on a line under it. An archive *inside* an archive never gets here: the tab
//! browsing one hands this pane nothing, and draws the entry's card instead.

pub mod body;
pub mod decode;
pub mod doc;
pub mod gesture;
pub mod highlight;
pub mod listing;
pub mod markdown;
mod paint;
pub mod prepare;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use df_core::fs::{Entry, Notifier, Recent, SortOptions};
use df_core::preview::{
    PaneId, Preview, PreviewKind, PreviewToken, PreviewUpdate, Previewer, TargetSize,
};

pub use paint::{fit_rect, nearest_for, oriented_mesh, oriented_size, preview};

/// How long a picture takes to cross into the one it replaces — PLAN §6's
/// "results crossfade in over ~80 ms", and delightviewer's `CROSSFADE` to the
/// millisecond, so the two programs handing a file between them feel like one
/// program.
///
/// Short enough to read as "it was always there", long enough that a
/// placeholder → full-resolution swap does not snap. Only ever between two
/// pictures of the same file: see the module header.
pub const CROSSFADE: Duration = Duration::from_millis(80);

/// How many lines `K` and `J` move the preview (PLAN §4.1's `seek preview ±5`,
/// which is yazi's own binding and its own number).
pub const SEEK_LINES: usize = 5;

/// How long the preview may be blank before it admits it is reading.
///
/// The same 150 ms the list pane uses, for the same reason: the debounce is
/// 40 ms and a local read lands inside a few more, so a label any sooner would
/// be a flash nobody can read and everybody notices.
const LOADING_DELAY: Duration = Duration::from_millis(150);

/// How much bigger the pane has to get before a decoded image is decoded again.
///
/// A drag-resize changes the pane by a pixel per frame, and re-decoding a 40
/// megapixel photo per frame is how a window manager gets blamed for a stutter
/// this program caused. 96 physical pixels is about a centimetre: past that the
/// upscale of the old texture is visible, under it nothing is.
const RESIZE_SLOP: u32 = 96;

/// Which pane asks. There is one preview pane, and the spot panel (PLAN §7)
/// will be the second — the id exists so that the two do not supersede each
/// other's requests when it lands.
const PREVIEW_PANE: PaneId = PaneId(0);

/// How long the page indicator is held after the last page turn.
///
/// PLAN §8's transient chrome: "holds ~2.5–3 s then fades in ~500 ms". The
/// indicator takes the middle of that range, and it is the same 2.5 s the
/// playback strip lingers for — a chip that says where you are in a document
/// and a chip that says where you are in a clip are the same chip.
const CHIP_LINGER: Duration = Duration::from_millis(2500);

/// …then leaves over this. PLAN §8's 500 ms exactly.
const CHIP_FADE: Duration = Duration::from_millis(500);

/// How long the turntable takes to go all the way round.
///
/// Twenty-four seconds — fifteen degrees a second. Slow enough that it reads as
/// a considered look at an object rather than as a spinning icon, and slow
/// enough that the CPU rasteriser behind it is never the reason a frame is
/// late. It runs **only while the pointer is over the pane**: idle discipline
/// beats spin (PLAN §1), so a model nobody is looking at is a still picture
/// that costs nothing.
const TURNTABLE_PERIOD: Duration = Duration::from_secs(24);

/// How many files' reading positions the pane remembers.
///
/// A session's worth of "where was I in that file", and no more: 256 files is
/// far past what anybody arrows back and forth between, and a few words apiece
/// makes the whole map smaller than one line of a preview. It is a *session*
/// memory on purpose — nothing here is written to disk, so a file re-read
/// tomorrow opens at the top like any other.
const PLACES: usize = 256;

/// How far one press of `Ctrl+↑`/`Ctrl+↓` moves a zoomed picture, in logical
/// points.
///
/// A shade over the list's row height, so a press moves about a line of body
/// text at the sizes a PDF is set at — the same "one press, one line" the text
/// body already obeys, in the unit a picture has.
const PAN_STEP: f32 = 24.0;

/// What `+`, `Alt+-` and `0` mean (PLAN §4.3).
///
/// Read by [`gesture::Gestures::zoom_command`], which is what makes the
/// keyboard and the pointer two ways of moving one view rather than two views.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zoom {
    In,
    Out,
    /// `0`: back to fit, which is the only zoom a picture starts at.
    Fit,
}

/// Whether the pane has a picture of the hovered file's own: the question the
/// audio card asks before it takes the pane (`playback::strip::card_shows`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Poster {
    /// A picture is on screen — a photograph, a cached thumbnail, a song's
    /// sleeve.
    Shown,
    /// Not known yet: the preview is still being read, or its decode is still
    /// in flight.
    Pending,
    /// The pane has answered, and it has no picture: a song without a sleeve,
    /// or a body that is not a picture at all.
    Absent,
}

/// A decoded picture living on the GPU.
struct Texture {
    handle: egui::TextureHandle,
    /// The decoded size in physical pixels, which is what the fit is computed
    /// from — a texture is not points and must not be treated as if it were.
    size: (u32, u32),
}

/// An animated image, playing (PLAN §6): a GIF, an animated WebP, an APNG, an
/// AVIF sequence.
///
/// **Clock-driven, not frame-driven.** The frame on screen has a moment it is
/// due to be replaced, and that moment is a repaint deadline — one scheduled
/// wake-up per frame of the animation, never a poll, and never a `request_
/// repaint` that would hold the window at sixty frames a second to show a
/// picture that changes ten times (PLAN §1's idle rule).
///
/// The frames arrive from the decode worker one at a time, so a long loop plays
/// from its first frame while its last is still being decoded, and `index`
/// wraps over however many have landed. That is also what makes the memory cap
/// harmless: a loop cut short by the budget is a short loop, and this plays it.
struct Anim {
    /// Every frame that has landed, in order, with how long each is held.
    frames: Vec<(Texture, Duration)>,
    /// Which one is on screen.
    index: usize,
    /// …and when it stops being.
    due: Instant,
}

impl Anim {
    fn new(now: Instant) -> Anim {
        Anim {
            frames: Vec::new(),
            index: 0,
            due: now,
        }
    }

    /// A frame off the worker. The first one starts the clock.
    fn push(&mut self, texture: Texture, delay: Duration, now: Instant) {
        if self.frames.is_empty() {
            self.due = now + delay;
        }
        self.frames.push((texture, delay));
    }

    /// Advance to the frame `now` is in.
    ///
    /// **Bounded by one pass round the loop**, and then resynchronised to
    /// `now`: a window that was occluded for ten minutes comes back owing six
    /// thousand ten-millisecond steps, and walking them one at a time would be
    /// a visible stall on the frame that restored it. Where the animation
    /// resumes after a gap that long is not a question anybody can answer
    /// wrongly.
    fn tick(&mut self, now: Instant) {
        if self.frames.len() < 2 {
            return;
        }
        for _ in 0..self.frames.len() {
            if now < self.due {
                return;
            }
            self.index = (self.index + 1) % self.frames.len();
            self.due += self.frames[self.index].1;
        }
        if now >= self.due {
            self.due = now + self.frames[self.index].1;
        }
    }

    /// The frame to draw, if any has landed.
    fn texture(&self) -> Option<&Texture> {
        self.frames.get(self.index).map(|(texture, _)| texture)
    }

    /// When the pane is owed its next frame. `None` while there is nothing to
    /// animate — a single frame is a still, and a still asks for nothing.
    fn next_deadline(&self, now: Instant) -> Option<Duration> {
        // Zero filtered out like the pane's other three deadlines: a frame that
        // is already due is a frame [`Anim::tick`] takes on this pass, and
        // asking for a repaint in no time at all is a busy loop spelled as a
        // timer.
        (self.frames.len() >= 2)
            .then(|| self.due.saturating_duration_since(now))
            .filter(|d| !d.is_zero())
    }
}

/// An image-shaped preview, mid-crossfade or arrived.
struct Media {
    kind: PreviewKind,
    /// The yazi cache's frame, shown the instant it decodes.
    thumb: Option<Texture>,
    /// The real pixels, faded in over the thumbnail.
    full: Option<Texture>,
    /// When [`Media::full`] landed — the start of the placeholder crossfade.
    swapped_at: Option<Instant>,
    /// The loop, for the picture formats that have one. Drawn over
    /// [`Media::full`], whose first frame it starts life identical to, so the
    /// still is what a GIF looks like until the second frame lands and nothing
    /// flashes when it does.
    anim: Option<Anim>,
    /// Set when the decode failed, and printed instead of the picture.
    error: Option<String>,
    /// A decode is still running, so "nothing on screen" is not "nothing to
    /// show".
    decoding: bool,
    /// The document previewer's state, for the four kinds that are documents
    /// rather than pictures (PLAN §6). `None` for a photograph, a video and an
    /// archive, which have no pages to turn.
    ///
    /// Boxed: a [`DocView`] is much the largest thing in [`Body`], and an
    /// unboxed one would make every text preview in the program pay its size.
    doc: Option<Box<DocView>>,
    /// One line drawn under the kind badge: why an archive that nothing could
    /// list is still only a badge (`Install 7-Zip to list this archive`).
    note: Option<String>,
}

impl Media {
    /// What a kind with no previewer of its own is called, in the corner of the
    /// pane.
    ///
    /// The four document kinds are absent from this table *unless their reader
    /// is missing* — a rendered page says what it is far better than the word
    /// "pdf" does, and a badge over it would be a caption on a picture of the
    /// thing it names. What is left is the seam the remaining checkboxes
    /// replace.
    fn badge(&self) -> Option<&'static str> {
        if let Some(doc) = &self.doc {
            // A machine with no libpdfium shows the cached thumbnail and says
            // "pdf", which is exactly what it did before pdfium existed here
            // (PLAN §6: a missing library is a missing feature, never a
            // failure).
            return doc.unavailable.then(|| kind_word(&self.kind)).flatten();
        }
        kind_word(&self.kind)
    }
}

fn kind_word(kind: &PreviewKind) -> Option<&'static str> {
    match kind {
        PreviewKind::Video => Some("video"),
        PreviewKind::Audio => Some("audio"),
        PreviewKind::Pdf => Some("pdf"),
        PreviewKind::Font => Some("font"),
        PreviewKind::Model3d => Some("3d model"),
        PreviewKind::Gcode => Some("g-code"),
        PreviewKind::Archive => Some("archive"),
        PreviewKind::Image => None,
        _ => None,
    }
}

/// A document being read: which page, how far in, and the pixels for it.
///
/// One struct for four kinds, because the *interaction* is one interaction.
/// `←`/`→` turn pages (or do nothing, on a one-page document, and then `←`
/// falls through to the list — PLAN §4.3), `↑`/`↓` move within the page (or
/// step G-code layers), `+`/`-`/`0` zoom, and a chip in the corner says where
/// you are and then gets out of the way.
struct DocView {
    /// What the worker found when it opened the file.
    meta: Option<doc::Meta>,
    /// The page, or the G-code layer. Zero-based.
    page: usize,
    /// The **rasterisation** zoom: 1.0 is fit-to-pane, and higher asks the
    /// worker for a bigger page so a magnified one is sharp rather than
    /// blurry.
    ///
    /// Not the transform — that is [`gesture::View`], which every picture in
    /// this pane shares. This follows it, quantised to the `+`/`-` rungs and
    /// only once the gesture has settled, so a smooth wheel zoom costs one
    /// re-render at the end instead of one per frame (see
    /// [`Pane::sync_doc_zoom`]).
    zoom: f32,
    /// The turntable's angle, in radians.
    yaw: f32,
    /// When the turntable started running, or `None` while it is still.
    spinning_since: Option<Instant>,
    /// The page on screen.
    current: Option<Texture>,
    /// The page being left, crossfaded out under the new one.
    previous: Option<Texture>,
    /// When [`DocView::current`] landed.
    swapped_at: Option<Instant>,
    /// What the worker is drawing now, so one view is never asked for twice.
    inflight: Option<doc::View>,
    /// What is actually on screen, which is what a new request is compared to.
    shown: Option<doc::View>,
    /// The reader this kind needs is not installed. Not an error.
    unavailable: bool,
    error: Option<String>,
    /// When the indicator last had something to say, for its linger-then-leave.
    chip_at: Option<Instant>,
}

impl DocView {
    fn new() -> DocView {
        DocView {
            meta: None,
            page: 0,
            zoom: 1.0,
            yaw: 0.0,
            spinning_since: None,
            current: None,
            previous: None,
            swapped_at: None,
            inflight: None,
            shown: None,
            unavailable: false,
            error: None,
            chip_at: None,
        }
    }

    fn pages(&self) -> usize {
        self.meta.as_ref().map(|m| m.pages).unwrap_or(1).max(1)
    }

    /// The indicator's text, or `None` for a document with nothing to count.
    fn counter(&self) -> Option<String> {
        let meta = self.meta.as_ref()?;
        match meta.counter {
            doc::Counter::Page if meta.pages > 1 => {
                Some(format!("{} / {}", self.page + 1, meta.pages))
            }
            doc::Counter::Layer if meta.pages > 1 => {
                Some(format!("Layer {} / {}", self.page + 1, meta.pages))
            }
            _ => None,
        }
    }

    /// How visible the indicator is, 0–1 (PLAN §8's "linger then leave").
    fn chip_alpha(&self, now: Instant) -> f32 {
        let Some(at) = self.chip_at else { return 0.0 };
        let elapsed = now.saturating_duration_since(at);
        if elapsed < CHIP_LINGER {
            return 1.0;
        }
        let over = (elapsed - CHIP_LINGER).as_secs_f32() / CHIP_FADE.as_secs_f32();
        (1.0 - over).clamp(0.0, 1.0)
    }
}

/// What the pane is drawing.
enum Body {
    /// The file has no bytes in it. Its own state, because "empty" and "we
    /// could not read it" must never look alike (`delightful-ui` §11).
    Empty,
    Text {
        lines: Vec<String>,
        syntax: Option<&'static str>,
        truncated: bool,
        /// What each line *starts* inside, from one pass at arrival — so
        /// scrolling costs a screenful of tokenising, not a file's worth.
        states: Vec<highlight::Block>,
    },
    Markdown {
        blocks: Vec<markdown::Block>,
        truncated: bool,
    },
    Directory {
        entries: Vec<Entry>,
        truncated: bool,
    },
    Hex {
        bytes: Vec<u8>,
        truncated: bool,
    },
    Media(Media),
    /// An archive's members, under a header that says it is one
    /// ([`listing`]). Built by [`body_from_listing`].
    Archive {
        /// `ZIP`, `TAR.GZ`, `7Z` — already in capitals, which is how the
        /// header draws it.
        format: String,
        /// The first [`listing::PREVIEW_ENTRIES`] members, in the archive's
        /// own order.
        entries: Vec<listing::ArchiveRow>,
        /// `entries.len()`.
        shown: usize,
        /// Every member the listing found, folders included.
        total: usize,
        /// …of which files: the header's count.
        files: usize,
        /// Uncompressed bytes, across every file whose size is known.
        total_len: u64,
        /// There is more to the archive than `entries`, so the rows end in a
        /// footer that says how much.
        truncated: bool,
        /// Whether the listing reached the archive's end. When it did not,
        /// `total` is a floor and the counts say so.
        complete: bool,
        /// Any member needs a password.
        encrypted: bool,
    },
    /// There is nothing honest to draw; the opener rules are the answer.
    Unsupported {
        kind: PreviewKind,
    },
    /// The read itself failed — permission denied, and the like.
    Failed(String),
}

/// What is on screen.
///
/// No "since when": moving the cursor from one file to the next **switches the
/// preview instantly**. The body it replaces belonged to a different file, so
/// there is nothing for a crossfade to blend — [`Pane::sync`] has already
/// dropped it — and what an 80 ms fade-in actually bought was a blank pane and
/// then a dissolve, on every single arrow key. The crossfades that survive are
/// the ones *inside* one file, where two pictures of the same thing really do
/// overlap: thumbnail → full image, and page → page in a document.
struct Shown {
    body: Body,
}

/// How far into a file the pane had got, kept for the length of the session.
///
/// Both numbers, because "where I was" in a text file is a line and in a PDF is
/// a page, and one file can be neither — a default `Spot` is the top of the
/// first page, which is where an unremembered file opens.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Spot {
    /// [`Pane::scroll`]: the first line drawn.
    scroll: usize,
    /// [`DocView::page`]: the page or layer on screen.
    page: usize,
}

/// The preview pane's whole state.
pub struct Pane {
    previewer: Previewer,
    decoder: decode::Decoder,
    /// The highlighter and the markdown parser, off the paint thread (see
    /// [`prepare`]). df-core hands back lines; this is what turns them into a
    /// [`Body`], and it is not cheap enough to do inside a frame.
    preparer: prepare::Preparer,
    /// The archive lister ([`body`]'s worker, running [`listing`]). The
    /// pane's own instance, not the card bodies': a `.tar.zst` being read to
    /// its end must not hold up the card of an entry inside another archive.
    listings: body::Bodies,
    /// The path the live request is for, or `None` when nothing is wanted
    /// (an empty directory).
    wanted: Option<PathBuf>,
    /// The live request's token; every arrival is checked against it.
    token: Option<PreviewToken>,
    /// The pane size, in physical pixels, the live request asked for.
    requested_target: (u32, u32),
    /// When the live request was made, for [`LOADING_DELAY`].
    requested_at: Instant,
    shown: Option<Shown>,
    /// How far the content has been scrolled, in lines.
    scroll: usize,
    /// The largest [`Pane::scroll`] the last paint could actually use — the
    /// paint is the only thing that knows how tall the content came out.
    max_scroll: usize,
    /// When the scroll last moved, for the scrollbar's linger-then-leave.
    scrolled_at: Option<Instant>,
    /// The wheel's momentum over the line position (PLAN §7.5, §8).
    ///
    /// A document's scroll is a plain line number, so unlike the listing panes
    /// — whose position is already a tween — the coast has to be held
    /// somewhere. `None` when the pane has never been wheeled, which is the
    /// resting state: no fling, no frames.
    fling: Option<crate::mouse::Fling>,
    /// Set by the app each frame: a playback controller is mounted on this
    /// file, so the kind badge stands down (see [`Pane::set_media_mounted`]).
    media_mounted: bool,
    /// Set by the app each frame: the player has a decoded frame over this
    /// pane, so the poster underneath it stands down too (see
    /// [`Pane::set_media_frame`]).
    media_frame: bool,
    /// The document worker: PDF pages, specimen sheets, meshes, toolpaths.
    docs: doc::Docs,
    /// The four colours a worker-thread rasteriser is allowed, refreshed each
    /// frame from the palette (see [`doc::Ink`]).
    ink: doc::Ink,
    /// Where the cursor had got to in each file previewed this session.
    ///
    /// Arrowing off a long file and back on to it is a thing people do
    /// constantly — comparing two files, or checking a directory and returning
    /// — and starting again from line one every time makes the pane feel like
    /// it has forgotten what you were doing (`delightful-ui` §8: the view does
    /// not move under you). Bounded by [`PLACES`], and *not* keyed on the
    /// file's mtime: a file rewritten under a remembered position is rare, and
    /// the position is clamped to what the new content can use on the first
    /// paint, so the worst case is landing further up a file than expected.
    places: Recent<Spot>,
    /// The page a [`DocView`] should be built at, from [`Pane::places`].
    ///
    /// Deferred because the view does not exist yet when the file is asked
    /// for: the body — and with it the page count — arrives from a worker
    /// several frames later.
    resume_page: usize,
    /// The line an archive's listing should open at, from [`Pane::places`].
    ///
    /// Held for the same reason as [`Pane::resume_page`]: the badge an archive
    /// shows while its listing is read has nothing to scroll, so its paint
    /// clamps [`Pane::scroll`] to zero, and the listing that lands a moment
    /// later would open at the top of a file somebody had scrolled.
    resume_scroll: usize,
    /// Whether the pointer is over this pane. The turntable's whole switch:
    /// with nobody looking, a model is a still picture and asks for nothing.
    ///
    /// It used to be "does this pane have the keyboard". Nothing has the
    /// keyboard but the list any more (PLAN §2.1), and the pointer is the
    /// honest replacement — a model turns while somebody is looking at it and
    /// stops the moment they look away, which is the same idle bargain
    /// (PLAN §1) struck against a signal that still exists.
    pointer_over: bool,
    /// Zoom and pan over whatever picture the pane is showing — the pointer's
    /// and the keyboard's, driving one [`gesture::View`] so the two can never
    /// disagree about where the picture is (see [`gesture`]).
    gestures: gesture::Gestures,
    /// The pane's content box and the size the picture is drawn at when the
    /// view is fitted, both in points, as of the last frame that measured them.
    ///
    /// Remembered because the *keyboard* zoom has no geometry of its own: keys
    /// are dispatched before the frame knows how big the video frame it is
    /// about to draw is, and re-deriving the fit in the key path would be the
    /// second copy of a fit this module has already been careful to have only
    /// one of. `None` while the body is not a picture.
    picture: Option<(egui::Rect, egui::Vec2)>,
    /// Whether this pane is on screen and being looked at: no modal surface or
    /// help sheet over it, and the window itself focused.
    ///
    /// The animated image's switch, and the same bargain the turntable strikes
    /// one field up. A GIF behind a scrim is a GIF nobody can see, and stepping
    /// it costs a wake-up ten times a second for a picture the help sheet is
    /// covering — which is exactly the idle cost PLAN §1 is about. Nothing is
    /// lost by stopping: [`Anim::tick`] resynchronises to `now` when the pane
    /// comes back, so the loop resumes where the clock says rather than
    /// replaying the minutes it was hidden for.
    visible: bool,
}

impl Pane {
    /// Start the workers. Called before the window exists, so PLAN §6's
    /// cold-start ordering — decode workers up before the first frame — is
    /// what actually happens.
    pub fn start(notify: Notifier) -> Pane {
        Pane {
            previewer: Previewer::start(std::sync::Arc::clone(&notify)),
            docs: doc::Docs::start(std::sync::Arc::clone(&notify)),
            preparer: prepare::Preparer::start(std::sync::Arc::clone(&notify)),
            listings: body::Bodies::named("df-listing", std::sync::Arc::clone(&notify)),
            decoder: decode::Decoder::start(notify),
            ink: doc::Ink::test(),
            gestures: gesture::Gestures::new(),
            picture: None,
            pointer_over: false,
            visible: true,
            wanted: None,
            token: None,
            requested_target: (0, 0),
            requested_at: Instant::now(),
            shown: None,
            scroll: 0,
            max_scroll: 0,
            scrolled_at: None,
            fling: None,
            media_mounted: false,
            places: Recent::new(PLACES),
            resume_page: 0,
            resume_scroll: 0,
            media_frame: false,
        }
    }

    /// The sort a directory preview is listed in, so a peek matches what
    /// entering the directory shows.
    pub fn set_sort(&mut self, sort: SortOptions) {
        self.previewer.set_sort(sort);
    }

    /// Ask for `path`, if it is not already what is wanted.
    ///
    /// Called once per frame *after* the keymap has been dispatched, so the
    /// request is for where the cursor has ended up rather than for each row
    /// it passed through. df-core's debounce does the rest.
    pub fn sync(&mut self, path: Option<&Path>, target: (u32, u32), now: Instant) {
        let Some(path) = path else {
            // An empty directory, or a filtered-out listing: nothing is
            // hovered, so nothing is previewed.
            if self.wanted.is_some() {
                self.cancel();
            }
            return;
        };
        let same_path = self.wanted.as_deref() == Some(path);
        // Only a picture cares how big the pane is, and only when the pane has
        // grown past what the decode was sized for — a texture scaled *down*
        // is exactly as sharp as one decoded to size.
        let outgrown = self.is_media()
            && (target.0 > self.requested_target.0 + RESIZE_SLOP
                || target.1 > self.requested_target.1 + RESIZE_SLOP);
        if same_path && !outgrown {
            return;
        }
        if !same_path {
            // Where the file being left had got to, before anything about it is
            // dropped — this is the only moment both the path and the position
            // are still in hand.
            self.remember_place();
            // The content on screen belongs to a different file, and a pane
            // that kept it would be labelling file A with file B.
            self.shown = None;
            self.max_scroll = 0;
            self.scrolled_at = None;
            // A coast belongs to the document it was started in, and so does
            // the zoom: a photograph left at 4× and arrived back at is a
            // photograph you cannot see (see `Gestures::reset_for_new_item`).
            self.fling = None;
            self.gestures.reset_for_new_item();
            self.picture = None;
            self.decoder.cancel();
            self.preparer.cancel();
            self.docs.cancel();
            self.listings.cancel(body::Which::Archive);
            // …and back to wherever this file was last read to. The scroll is
            // clamped by the first paint, which is the only thing that knows
            // how tall the content came out.
            let spot = self.places.recall(path).copied().unwrap_or_default();
            self.scroll = spot.scroll;
            self.resume_scroll = spot.scroll;
            self.resume_page = spot.page;
        }
        self.wanted = Some(path.to_path_buf());
        self.requested_target = target;
        self.requested_at = now;
        self.token = Some(self.previewer.preview(
            PREVIEW_PANE,
            path,
            TargetSize::new(target.0, target.1),
        ));
    }

    /// Open `path` at `line` (1-based) the first time it is shown: a content
    /// search's hit, which is read for the line it matched on (PLAN §7.2).
    ///
    /// Only a file the pane has no place for yet. One it remembers opens where
    /// the reader left it, as every file does: that position is the more
    /// recent thing somebody chose. Written into the same memory
    /// [`Pane::sync`] reads a place from, so there is one way a file opens
    /// part way down and not two.
    pub fn open_at(&mut self, path: &Path, line: usize) {
        if self.places.recall(path).is_none() {
            self.places.remember(
                path.to_path_buf(),
                Spot {
                    scroll: line.saturating_sub(1),
                    page: 0,
                },
            );
        }
    }

    /// The first line drawn ([`Pane::scroll`]'s field), for the tests that
    /// check where a file opened.
    #[cfg(test)]
    pub fn scroll(&self) -> usize {
        self.scroll
    }

    /// The path the live request is for, if any: what the pane has been asked
    /// to show, whether or not it has arrived.
    #[cfg(test)]
    pub fn wanted(&self) -> Option<&Path> {
        self.wanted.as_deref()
    }

    /// Stop previewing anything — a tab switch, a directory with nothing in it.
    pub fn cancel(&mut self) {
        // A tab switch is leaving the file, not losing it: the position is
        // remembered here for the same reason it is when the cursor moves.
        self.remember_place();
        self.previewer.cancel(PREVIEW_PANE);
        self.decoder.cancel();
        self.preparer.cancel();
        self.docs.cancel();
        self.listings.cancel(body::Which::Archive);
        self.wanted = None;
        self.token = None;
        self.shown = None;
        self.scroll = 0;
        self.max_scroll = 0;
        self.scrolled_at = None;
        self.fling = None;
        self.gestures.reset_for_new_item();
        self.picture = None;
        self.resume_page = 0;
        self.resume_scroll = 0;
    }

    /// Record how far into the file on screen the reader had got.
    ///
    /// A file at the very top is recorded like any other: it is a real answer
    /// to "where was I", and skipping it would mean scrolling down, going away,
    /// coming back, scrolling to the top and going away again left the *old*
    /// position behind to pounce on the next visit.
    fn remember_place(&mut self) {
        let Some(path) = self.wanted.clone() else {
            return;
        };
        let page = self.doc_ref().map(|view| view.page).unwrap_or(0);
        self.places.remember(
            path,
            Spot {
                scroll: self.scroll,
                page,
            },
        );
    }

    /// `K` / `J`: move the preview by [`SEEK_LINES`] without leaving the list.
    ///
    /// Clamped to what the last paint said would fit, so `J` on a three-line
    /// file scrolls nothing rather than scrolling it off the top.
    pub fn seek(&mut self, down: bool, now: Instant) {
        let was = self.scroll;
        self.scroll = if down {
            (self.scroll + SEEK_LINES).min(self.max_scroll)
        } else {
            self.scroll.saturating_sub(SEEK_LINES)
        };
        if self.scroll != was {
            self.scrolled_at = Some(now);
        }
        self.fling = None;
    }

    /// Scroll by `delta` lines — `Ctrl+↑`/`Ctrl+↓`, `Ctrl+Shift+u`/`Ctrl+Shift+d`
    /// and `Shift+Space` (PLAN §4.3).
    ///
    /// Same clamp as [`Pane::seek`], and the same scrollbar linger: one
    /// scrolling behaviour in this pane, whichever key asked for it.
    pub fn scroll_by(&mut self, delta: isize, now: Instant) {
        let was = self.scroll;
        self.scroll = if delta >= 0 {
            self.scroll
                .saturating_add(delta as usize)
                .min(self.max_scroll)
        } else {
            self.scroll.saturating_sub(delta.unsigned_abs())
        };
        if self.scroll != was {
            self.scrolled_at = Some(now);
        }
        // The keyboard wins over a coast the wheel started: two things moving
        // one number is one of them losing, and it must not be the key.
        self.fling = None;
    }

    /// `g g` / `G`: the top, or as far down as the content goes.
    pub fn scroll_to(&mut self, line: usize, now: Instant) {
        let was = self.scroll;
        self.scroll = line.min(self.max_scroll);
        if self.scroll != was {
            self.scrolled_at = Some(now);
        }
        // A keyboard jump ends any coast: two things moving one number is one
        // of them losing.
        self.fling = None;
    }

    /// A wheel roll over a **text-shaped** body (PLAN §7.5), in lines.
    ///
    /// Text, markdown, a listing and a hexdump scroll; every picture takes the
    /// wheel as a zoom instead (see [`gesture`]), and a multi-page document
    /// turns pages with it while it is fitted — which is the same split
    /// `Ctrl+↑`/`Ctrl+↓` make from the list (PLAN §4.3), moved to the pointer.
    pub fn wheel(&mut self, delta_lines: f32, now: Instant) -> bool {
        // **A picture never reaches here.** Over a photograph, a clip, a GIF or
        // a rendered page the wheel is the zoom (`preview::gesture`), and that
        // pass runs later in the frame — where the video frame's own size is
        // finally known. The caller checks [`Pane::is_picture`] first; this is
        // the belt to that pass's braces, so a routing mistake scrolls nothing
        // rather than scrolling a photograph by lines.
        if self.is_picture() {
            return false;
        }
        // Every other way of moving this pane clears the fling, so a coast that
        // exists here started from the current position and is still the only
        // thing driving it.
        let fling = self
            .fling
            .get_or_insert_with(|| crate::mouse::Fling::at(self.scroll as f32, now));
        fling.kick(delta_lines, 0.0, self.max_scroll as f32, now)
    }

    /// Sample the coast and put the pane where it says. Called once a frame.
    /// Step the animated image, if there is one, to the frame `now` is in.
    ///
    /// Called once a frame beside [`Pane::tick_fling`], and — like it — it is
    /// the *deadline* in [`Pane::next_deadline`] that brings the frame this
    /// runs in. Nothing here asks for a repaint.
    pub fn tick_anim(&mut self, now: Instant) {
        // …and only while somebody is looking at it. A loop under a scrim is a
        // loop nobody sees, and stepping it is a wake-up a second bought for
        // nothing (PLAN §1). See [`Pane::visible`] for why resuming is safe.
        if !self.visible {
            return;
        }
        if let Some(anim) = self.anim_mut() {
            anim.tick(now);
        }
    }

    fn anim(&self) -> Option<&Anim> {
        match self.shown.as_ref().map(|shown| &shown.body) {
            Some(Body::Media(media)) => media.anim.as_ref(),
            _ => None,
        }
    }

    fn anim_mut(&mut self) -> Option<&mut Anim> {
        match self.shown.as_mut().map(|shown| &mut shown.body) {
            Some(Body::Media(media)) => media.anim.as_mut(),
            _ => None,
        }
    }

    pub fn tick_fling(&mut self, now: Instant) {
        let Some(fling) = &self.fling else { return };
        let at = fling.value(now).round().max(0.0) as usize;
        let done = fling.finished(now);
        let was = self.scroll;
        self.scroll = at.min(self.max_scroll);
        if self.scroll != was {
            self.scrolled_at = Some(now);
        }
        if done {
            // A spent coast is dropped, not kept at its target: `animating`
            // reads the option, and an option that is never `None` is a pane
            // that never stops asking for frames (PLAN §1).
            self.fling = None;
        }
    }

    /// Whether a transport is mounted on what this pane is showing.
    ///
    /// The seam this module's header promised: with a controller behind it, a
    /// video's own frame is drawn over the pane by [`crate::playback`] and the
    /// "video" badge would be labelling a picture that is plainly a video and
    /// has a transport strip under it.
    pub fn set_media_mounted(&mut self, mounted: bool) {
        self.media_mounted = mounted;
        if !mounted {
            // Unmounting hands the pane back its own picture in the same
            // breath, so `↓` off a clip and back onto it never shows an empty
            // pane between the source going away and the poster returning.
            self.media_frame = false;
        }
    }

    /// Whether the player has a **decoded frame** on screen over this pane.
    ///
    /// The poster and the frame are the same picture, and drawing both means
    /// drawing one of them twice — visibly so the instant they disagree about
    /// their footprint, which is exactly what a rotated clip does. So the
    /// cached thumbnail stands in only until the real frame lands, and the
    /// swap back is [`Pane::set_media_mounted`]'s job.
    pub fn set_media_frame(&mut self, showing: bool) {
        self.media_frame = showing;
    }

    /// Whether this pane has a picture of the hovered file's own, or may still
    /// get one.
    ///
    /// The audio card draws over this pane, so it asks first: a song whose
    /// sleeve is on screen keeps it, and a card laid across it would be a
    /// caption on the album cover. `Pending` is the part that needs saying —
    /// the preview is still being read, or the sleeve still decoding — because
    /// the transport usually mounts *before* either has finished, and a card
    /// drawn in that gap is a flash of text on every song that has art.
    pub fn poster(&self) -> Poster {
        match self.shown.as_ref().map(|s| &s.body) {
            Some(Body::Media(media)) => {
                if media.thumb.is_some() || media.full.is_some() || media.anim.is_some() {
                    Poster::Shown
                } else if media.decoding {
                    Poster::Pending
                } else {
                    Poster::Absent
                }
            }
            Some(_) => Poster::Absent,
            // Asked for and not arrived: the answer is still coming.
            None if self.wanted.is_some() => Poster::Pending,
            None => Poster::Absent,
        }
    }

    /// Take whatever the workers finished. `ctx` is where decoded pixels
    /// become textures — the one thing in this file that has to happen on the
    /// thread egui lives on. Returns whether anything changed.
    pub fn poll(&mut self, ctx: Option<&egui::Context>, now: Instant) -> bool {
        let mut changed = false;
        for update in self.previewer.drain() {
            if Some(update.token()) != self.token {
                // A superseded answer that won its race. Dropped here rather
                // than trusted, which is what the token is for.
                continue;
            }
            changed = true;
            self.apply(update, now);
        }
        for prepared in self.preparer.drain() {
            if Some(prepared.token) != self.token {
                continue;
            }
            changed = true;
            self.apply_prepared(prepared);
        }
        // The two stages of one picture usually arrive a whole file-read
        // apart, which is what the crossfade is for. When they arrive in the
        // *same* poll — a small JPEG, a warm cache — the placeholder has
        // nothing to place-hold: uploading it would be a second texture and a
        // second crossfade for a picture that was already there. So the batch
        // is looked at whole, and a thumb with its own full behind it is
        // dropped unopened.
        let batch: Vec<decode::Decoded> = self
            .decoder
            .drain()
            .into_iter()
            .filter(|decoded| Some(decoded.token) == self.token)
            .collect();
        // A full decode that found *nothing* — a song with no sleeve — does not
        // count: the thumb is then the only picture there is.
        let full_here = batch
            .iter()
            .any(|d| d.stage == decode::Stage::Full && matches!(d.result, Ok(Some(_))));
        for decoded in batch {
            changed = true;
            if full_here && decoded.stage == decode::Stage::Thumb {
                continue;
            }
            self.apply_decoded(decoded, ctx, now);
        }
        for update in self.docs.drain() {
            if Some(update.token) != self.token || self.wanted.as_deref() != Some(&update.path) {
                continue;
            }
            changed = true;
            self.apply_doc(update, ctx, now);
        }
        // The worker's own token has already dropped a listing for an archive
        // the cursor left; the path check in `apply_listing` catches whatever
        // won the race.
        for listed in self.listings.drain() {
            if let body::Body::Archive { path, listing } = listed {
                changed |= self.apply_listing(&path, listing);
            }
        }
        changed
    }

    /// An archive's listing, in place of the badge that stood for it.
    ///
    /// Only onto that badge: a listing for a file the pane has moved off, or
    /// one that arrives after the body was replaced for some other reason, is
    /// dropped. A failure keeps the badge and adds the reason under it.
    fn apply_listing(&mut self, path: &Path, listing: Result<listing::Listing, String>) -> bool {
        if self.wanted.as_deref() != Some(path) {
            return false;
        }
        let Some(shown) = &mut self.shown else {
            return false;
        };
        let Body::Media(media) = &mut shown.body else {
            return false;
        };
        if media.kind != PreviewKind::Archive {
            return false;
        }
        match listing {
            Ok(listing) => {
                shown.body = body_from_listing(listing);
                // The badge's paint clamped the scroll to its own nothing; the
                // place this archive was last read to is the one to open at,
                // and the listing's first paint clamps it to what exists.
                self.scroll = self.resume_scroll;
            }
            Err(reason) => media.note = Some(reason),
        }
        true
    }

    fn apply_doc(&mut self, update: doc::Update, ctx: Option<&egui::Context>, now: Instant) {
        let Some(shown) = &mut self.shown else { return };
        let Body::Media(media) = &mut shown.body else {
            return;
        };
        let Some(view) = &mut media.doc else { return };
        view.inflight = None;
        match update.result {
            Ok(doc::Payload::Meta(meta)) => {
                // The page the request was made for may not exist in the file
                // that turned out to be there.
                view.page = view.page.min(meta.pages.saturating_sub(1));
                view.meta = Some(meta);
                // The indicator introduces itself once, on arrival, and then
                // leaves: a person who has just moved the cursor onto a
                // 400-page PDF wants to know that, once.
                if view.counter().is_some() {
                    view.chip_at = Some(now);
                }
            }
            Ok(doc::Payload::Unavailable) => {
                view.unavailable = true;
            }
            Ok(doc::Payload::Page { view: drawn, image }) => {
                let texture = upload(ctx, "df-preview-doc", &image);
                if texture.is_some() {
                    // The outgoing page is kept only while something is coming
                    // in over it; a crossfade from nothing is a flash.
                    view.previous = view.current.take();
                    view.current = texture;
                    view.swapped_at = Some(now);
                    view.shown = Some(drawn);
                    view.page = drawn.page;
                    view.error = None;
                    // The turntable's angle is carried on the *drawn* frame and
                    // the clock restarts from it, so the pointer leaving the
                    // pane and coming back picks the model up where it stopped
                    // instead of snapping back to where it started.
                    view.yaw = drawn.yaw;
                    if view.spinning_since.is_some() {
                        view.spinning_since = Some(now);
                    }
                }
            }
            Err(message) => {
                view.error = Some(message);
            }
        }
    }

    /// The four colours the document rasterisers draw with, and whether the
    /// pointer is over this pane. Both are set once a frame by the app, because
    /// both live on the paint side and are needed on the worker side.
    pub fn set_ink(&mut self, ink: doc::Ink) {
        self.ink = ink;
    }

    /// Whether anybody can see this pane. See [`Pane::visible`].
    pub fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }

    pub fn set_pointer_over(&mut self, pointer_over: bool) {
        if self.pointer_over == pointer_over {
            return;
        }
        self.pointer_over = pointer_over;
        if let Some(view) = self.doc_mut() {
            // The turntable's clock starts when the pointer arrives and is
            // thrown away when it leaves, so the model holds the angle it was
            // last drawn at rather than snapping back to where it started.
            view.spinning_since = None;
        }
    }

    /// The rect this pane's *own* picture occupies at fit, in points.
    ///
    /// The one fit in the program (`paint::fit_rect` for a photograph,
    /// `paint::doc_fit_rect` for a rendered page — a page is enlarged to fill
    /// the pane where a photograph is not), asked here so the gesture layer and
    /// the paint cannot disagree about where the picture is.
    ///
    /// `None` for a body that is not a picture, and for a picture that has not
    /// decoded yet — a video whose poster and first frame are both still in
    /// flight has no footprint to zoom.
    pub fn fitted_rect(&self, content: egui::Rect, ppp: f32) -> Option<egui::Rect> {
        let Body::Media(media) = &self.shown.as_ref()?.body else {
            return None;
        };
        if let Some(view) = &media.doc {
            if let Some(texture) = view.current.as_ref().or(view.previous.as_ref()) {
                return Some(paint::doc_fit_rect(content, texture.size));
            }
        }
        // The still, in the order the paint stacks them: the loop's frame over
        // the photograph over the cached thumbnail. They are the same picture
        // at the same fit, so any of them answers — the order only decides
        // which one answers first.
        let texture = media
            .anim
            .as_ref()
            .and_then(Anim::texture)
            .or(media.full.as_ref())
            .or(media.thumb.as_ref())?;
        Some(paint::fit_rect(content, texture.size, ppp))
    }

    fn doc_mut(&mut self) -> Option<&mut DocView> {
        match self.shown.as_mut().map(|s| &mut s.body) {
            Some(Body::Media(media)) => media.doc.as_deref_mut(),
            _ => None,
        }
    }

    fn doc_ref(&self) -> Option<&DocView> {
        match self.shown.as_ref().map(|s| &s.body) {
            Some(Body::Media(media)) => media.doc.as_deref(),
            _ => None,
        }
    }

    /// Ask the document worker for whatever should be on screen.
    ///
    /// Called once a frame, after the keys have been dispatched and after
    /// [`Pane::poll`], so it asks for where the user has ended up. One request
    /// is in flight at a time: a held `→` through a long PDF renders the page
    /// it stopped on, not every page it passed.
    ///
    /// **This is also the turntable's whole engine.** While the pointer is over
    /// the pane a model's wanted angle moves with the clock, so each finished
    /// frame rings the wake bell, which brings a paint, which asks for the next
    /// angle. With the pointer away, the wanted angle is the angle on screen,
    /// nothing is asked for, and the loop stops dead (PLAN §1).
    pub fn sync_doc(&mut self, now: Instant) {
        let (Some(path), Some(token)) = (self.wanted.clone(), self.token) else {
            return;
        };
        let target = self.requested_target;
        let (ink, turning) = (self.ink, self.pointer_over);
        let kind = match self.shown.as_ref().map(|s| &s.body) {
            Some(Body::Media(media)) => media.kind.clone(),
            _ => return,
        };
        // **Only a mesh turns.** A page, a specimen and a toolpath are still
        // pictures; giving them a moving angle would mean re-rendering a PDF
        // sixty times a second to draw the same page, which is the exact
        // opposite of PLAN §1's idle rule.
        let turntable = kind == PreviewKind::Model3d;
        let Some(view) = self.doc_mut() else { return };
        if view.unavailable || view.inflight.is_some() {
            return;
        }
        // …and only while the keyboard is here.
        if turntable && turning && view.meta.is_some() && view.spinning_since.is_none() {
            view.spinning_since = Some(now);
        }
        let yaw = match view.spinning_since {
            Some(at) => {
                let turns = now.saturating_duration_since(at).as_secs_f32()
                    / TURNTABLE_PERIOD.as_secs_f32();
                view.yaw + turns * std::f32::consts::TAU
            }
            None => view.yaw,
        };
        let wanted = doc::View {
            page: view.page,
            target: (target.0.max(1), target.1.max(1)),
            zoom: view.zoom,
            yaw,
        };
        let settled = view.shown.as_ref().is_some_and(|shown| {
            shown.page == wanted.page
                && shown.target == wanted.target
                && (shown.zoom - wanted.zoom).abs() < f32::EPSILON
                && (shown.yaw - wanted.yaw).abs() < f32::EPSILON
        });
        if settled && view.error.is_none() {
            return;
        }
        if view.error.is_some() && view.meta.is_some() {
            // A page that would not render is not retried every frame; the
            // next key press asks again by changing what is wanted.
            return;
        }
        view.inflight = Some(wanted);
        self.docs.request(doc::Job {
            token,
            path,
            kind,
            view: wanted,
            ink,
        });
    }

    /// `→`, or `←`: the next or previous page. `false` when there is nowhere to
    /// go, which is what lets `←` at the first page fall through to the list
    /// (PLAN §4.3).
    pub fn turn_page(&mut self, forward: bool, now: Instant) -> bool {
        let Some(view) = self.doc_mut() else {
            return false;
        };
        let pages = view.pages();
        let next = if forward {
            (view.page + 1).min(pages.saturating_sub(1))
        } else {
            view.page.saturating_sub(1)
        };
        if next == view.page {
            return false;
        }
        view.page = next;
        view.chip_at = Some(now);
        // A new page starts at the edge you are arriving from — the top going
        // forward, the bottom going back. Carrying the pan across would land
        // you in the middle of a page you have not seen the start of, and
        // zeroing it would send you to the middle of it instead
        // (`Gestures::land_on_page`).
        if let Some(input) = self.gesture_input(now) {
            self.gestures
                .land_on_page(if forward { 1 } else { -1 }, &input);
        }
        true
    }

    /// `↑`/`↓` inside a document: step a G-code layer, or move an oversized
    /// page. `false` when the body is not a document, so the caller falls back
    /// to scrolling text.
    pub fn doc_scroll(&mut self, delta: isize, now: Instant) -> bool {
        let layered = self
            .doc_ref()
            .and_then(|v| v.meta.as_ref())
            .is_some_and(|m| m.counter == doc::Counter::Layer);
        if layered {
            // A toolpath's `↑`/`↓` is its layer stepping, which is the one
            // thing a person opens a G-code file to do.
            return self.turn_page(delta > 0, now);
        }
        // Everything else moves the *picture*, which is the gesture's job now:
        // a page, a photograph and a paused video frame all pan the same way,
        // by the same clamp, whether the hand or the keyboard asked.
        let Some(input) = self.gesture_input(now) else {
            return false;
        };
        // Content down is the view moving up, which is what `↓` means.
        let moved = self
            .gestures
            .pan_by(egui::vec2(0.0, -(delta as f32) * PAN_STEP), &input);
        if moved {
            if let Some(view) = self.doc_mut() {
                view.chip_at = Some(now);
            }
        }
        moved
    }

    /// `+` / `-` / `0`, on **every** picture the pane draws — a photograph and
    /// a video frame as much as a page.
    ///
    /// It drives the same [`gesture::View`] the pointer does, on the same
    /// curve, so the keyboard and the wheel can never end up arguing about
    /// where the picture is. `false` when there is no picture to zoom, which is
    /// how the key stays inert over a text body.
    pub fn zoom(&mut self, step: Zoom, now: Instant) -> bool {
        let Some(input) = self.gesture_input(now) else {
            return false;
        };
        self.gestures.zoom_command(step, &input);
        if let Some(view) = self.doc_mut() {
            view.chip_at = Some(now);
        }
        true
    }

    /// Is the zoom moving — a wheel blend, a double-click, a fling, the
    /// sub-fit spring?
    ///
    /// Its own wake source rather than a line inside [`Pane::animating`], so
    /// `DF_FRAME_LOG` can name it: a zoom that never settles is exactly the
    /// kind of stuck animation PLAN §1's audit exists to catch, and "preview"
    /// would not say which half of the pane was holding the frame rate up.
    /// Every one of the four *does* settle — each is a sampled animation that
    /// drops itself the frame it arrives.
    pub fn gesture_animating(&self) -> bool {
        self.gestures.is_animating()
    }

    /// Is a drag on the picture live? Once it is, the pointer belongs to it
    /// until the release, wherever it wanders — the same capture the seek bar
    /// takes.
    pub fn gesture_live(&self) -> bool {
        self.gestures.phase() != gesture::Phase::Idle
    }

    /// The pane's content box and the picture's fitted size, as
    /// [`Pane::set_picture`] was last told them.
    pub fn picture_geometry(&self) -> Option<(egui::Rect, egui::Vec2)> {
        self.picture
    }

    /// The transform every picture in this pane is drawn under.
    pub fn view(&self) -> gesture::View {
        self.gestures.view()
    }

    /// Is the view zoomed past fit? The cursor asks, so a zoomed picture can
    /// advertise that it can be dragged (`delightful-ui` §2).
    pub fn is_zoomed(&self) -> bool {
        self.gestures.is_zoomed()
    }

    /// Is the pane showing something zoom and pan act on — a photograph, an
    /// animated image, a poster, a video frame, a rendered page?
    ///
    /// The wheel's fork: a picture takes it as a zoom, everything else keeps
    /// scrolling by lines ([`Pane::wheel`]).
    pub fn is_picture(&self) -> bool {
        self.picture.is_some()
    }

    /// Tell the pane where this frame is drawing the picture: the content box,
    /// and the size the picture occupies at fit — both in points.
    ///
    /// Measured by the caller because only the caller knows about the *video*
    /// frame, which the player owns and draws over this pane. `None` retires
    /// the geometry, and with it the wheel's fork and the keyboard's zoom.
    pub fn set_picture(&mut self, geometry: Option<(egui::Rect, egui::Vec2)>) {
        self.picture = geometry;
    }

    /// The gesture layer's input for a frame with nothing happening in it —
    /// what the keyboard paths build on. `None` when there is no picture.
    fn gesture_input(&self, now: Instant) -> Option<gesture::Input> {
        let (content, fitted) = self.picture?;
        Some(gesture::Input::still(content.size(), fitted, now))
    }

    /// Run one frame of pointer and wheel over the picture.
    ///
    /// Returns whether a plain click landed on it — Task A's play/pause — and
    /// whether that click was the second half of a double, which has *also*
    /// just zoomed.
    pub fn gesture(&mut self, input: &gesture::Input, now: Instant) -> Option<bool> {
        // A wheel roll at fit turns the page of a document that has pages;
        // nothing else in this pane has a "next item" to reach for, so nothing
        // else lets the wheel do anything but zoom (see
        // `Gestures::set_navigates_at_fit`).
        let paged = self.doc_ref().is_some_and(|view| view.pages() > 1);
        self.gestures.set_navigates_at_fit(paged);
        let mut tap = None;
        for event in self.gestures.update(input) {
            match event {
                gesture::GestureEvent::Tap { double } => tap = Some(double),
                gesture::GestureEvent::PageStep(step) => {
                    self.turn_page(step > 0, now);
                }
            }
        }
        self.sync_doc_zoom();
        tap
    }

    /// Point the document rasteriser at the zoom the gesture has settled on, so
    /// a magnified page is *sharp* rather than a magnified texture.
    ///
    /// Two rules keep this from being a re-render per frame, which on a PDF is
    /// a worker thread saturated by a wheel roll:
    ///
    /// * **Only once the gesture has settled.** A zoom in flight is drawn by
    ///   magnifying the page already on screen, which is exactly what the eye
    ///   wants during the 200–300 ms it is moving.
    /// * **Quantised to the `+`/`-` rungs.** A settled 2.3× renders at 2.83×
    ///   (the next √2 rung up) rather than at 2.3, so wheeling in and back out
    ///   lands on renders the worker has already done rather than on a new
    ///   resolution every time.
    fn sync_doc_zoom(&mut self) {
        if self.gestures.is_animating() {
            return;
        }
        let scale = self.gestures.view().scale;
        let mut rung = doc::ZOOM_MIN;
        while rung < scale && rung < doc::ZOOM_MAX {
            rung = doc::zoom_in(rung);
        }
        if let Some(view) = self.doc_mut() {
            view.zoom = rung;
        }
    }

    fn apply(&mut self, update: PreviewUpdate, now: Instant) {
        // The token already proved this answer is the live one; the path is
        // checked too, because a token is a number and a path is the thing the
        // user is actually looking at.
        if self.wanted.as_deref() != Some(update.path()) {
            return;
        }
        let body = match update {
            PreviewUpdate::Failed { path, error, .. } => {
                Body::Failed(readable(&error.to_string(), &path))
            }
            PreviewUpdate::Ready { preview, .. } => match self.body_for(preview, now) {
                Some(body) => body,
                // Handed to the prepare worker instead. The pane keeps showing
                // what it was showing — the same thing it does for the whole
                // read that preceded this — and `apply_prepared` puts the
                // finished body up when it comes back.
                None => return,
            },
        };
        self.shown = Some(Shown { body });
    }

    /// The body this preview becomes, or `None` when the answer has been sent
    /// to a worker and will arrive later.
    fn body_for(&mut self, preview: Preview, _now: Instant) -> Option<Body> {
        let body = match preview {
            Preview::Empty => Body::Empty,
            // The two expensive ones go to [`prepare`]: `block_states` over
            // 20 000 lines and a mebibyte of markdown are both too much to do
            // inside a frame (see that module's essay).
            Preview::Text {
                lines,
                syntax,
                truncated,
            } => {
                let token = self.token?;
                let prepared = self.preparer.request(prepare::Job {
                    token,
                    source: prepare::Source::Text {
                        lines,
                        syntax,
                        truncated,
                    },
                });
                // `None` is the worker having taken it — the `?` returns, and
                // `apply_prepared` puts the body up when it comes back. `Some`
                // is there being no worker to take it, so it was prepared in
                // this frame instead: the jank, in exchange for a preview at
                // all.
                body_from_ready(prepared?.ready)
            }
            Preview::Markdown { source, truncated } => {
                let token = self.token?;
                let prepared = self.preparer.request(prepare::Job {
                    token,
                    source: prepare::Source::Markdown { source, truncated },
                });
                body_from_ready(prepared?.ready)
            }
            Preview::Directory { entries, truncated } => Body::Directory { entries, truncated },
            Preview::Hex { bytes, truncated } => Body::Hex { bytes, truncated },
            Preview::Unsupported { kind } => Body::Unsupported { kind },
            Preview::NeedsDecode {
                kind,
                path,
                target,
                thumb,
            } => {
                let Some(token) = self.token else {
                    return Some(Body::Unsupported { kind });
                };
                // Pictures decode here, and so does a song's sleeve; documents
                // go to their own worker and are drawn over whatever thumbnail
                // the shared cache had. Video takes its poster from the cache
                // and waits for a transport to mount (`decode::plan`).
                let decode::Plan {
                    full,
                    store,
                    decoding,
                } = decode::plan(&kind, thumb.is_some());
                let is_doc = matches!(
                    kind,
                    PreviewKind::Pdf
                        | PreviewKind::Font
                        | PreviewKind::Model3d
                        | PreviewKind::Gcode
                );
                // An archive has nothing to decode, and its badge goes up now
                // as it always has; the listing that replaces it is read on
                // the listing worker. This answer has already waited out
                // df-core's debounce, and the worker waits once more before it
                // opens the file (`body::Which::settle`), so an archive a held
                // `↓` only passed over is never read.
                if kind == PreviewKind::Archive {
                    self.listings
                        .request(body::Job::Archive { path: path.clone() });
                }
                self.decoder.request(decode::Job {
                    token,
                    path,
                    target: (target.width.max(1), target.height.max(1)),
                    thumb,
                    full,
                    store,
                });
                Body::Media(Media {
                    kind,
                    thumb: None,
                    full: None,
                    swapped_at: None,
                    anim: None,
                    error: None,
                    decoding,
                    // The first render is asked for by `sync_doc` on the next
                    // frame, which is where the pane's real pixel size and the
                    // palette are both known.
                    doc: is_doc.then(|| {
                        let mut view = Box::new(DocView::new());
                        // Back to the page this document was last left on.
                        // `apply_doc` clamps it once the real page count
                        // arrives, so a file that has shrunk is not a problem.
                        view.page = self.resume_page;
                        view
                    }),
                    note: None,
                })
            }
        };
        Some(body)
    }

    /// A finished highlight or markdown parse, from [`prepare`].
    fn apply_prepared(&mut self, prepared: prepare::Prepared) {
        let body = body_from_ready(prepared.ready);
        // Shown the instant it lands, at full strength: an item-to-item switch
        // does not fade (PLAN §6 — a preview that eases in reads as slow).
        self.shown = Some(Shown { body });
    }

    fn apply_decoded(
        &mut self,
        decoded: decode::Decoded,
        ctx: Option<&egui::Context>,
        now: Instant,
    ) {
        let Some(shown) = &mut self.shown else { return };
        let Body::Media(media) = &mut shown.body else {
            return;
        };
        match (decoded.stage, decoded.result) {
            (decode::Stage::Thumb, Ok(Some(image))) => {
                // Only while the real thing is still missing: a placeholder
                // that arrives late is worthless (delightviewer's rule).
                if media.full.is_none() {
                    media.thumb = upload_color(ctx, "df-preview-thumb", image);
                }
            }
            (decode::Stage::Full, Ok(Some(image))) => {
                media.full = upload_color(ctx, "df-preview", image);
                media.swapped_at = Some(now);
                media.decoding = false;
            }
            // A song with no sleeve. Nothing to draw and nothing to report —
            // only the wait is over, which is what lets the audio card have
            // the pane (`Pane::poster`).
            (decode::Stage::Full, Ok(None)) => {
                media.decoding = false;
            }
            // Only a full decode ever finds nothing; a thumbnail or a frame
            // that did would be a worker bug, and there is nothing to draw.
            (decode::Stage::Thumb | decode::Stage::Frame { .. }, Ok(None)) => {}
            (decode::Stage::Frame { delay_ms }, Ok(Some(image))) => {
                // A texture apiece rather than one re-uploaded per frame: the
                // worker has already capped the set at 64 MiB of pane-sized
                // pixels, and a preloaded loop costs nothing per frame where a
                // re-upload costs a copy of the picture ten times a second for
                // as long as the cursor sits on the file.
                if let Some(texture) = upload_color(ctx, "df-preview-frame", image) {
                    let anim = media.anim.get_or_insert_with(|| Anim::new(now));
                    anim.push(texture, Duration::from_millis(delay_ms.max(1).into()), now);
                }
            }
            // Logged on the worker; the still is on screen either way.
            (decode::Stage::Frame { .. }, Err(_)) => {}
            (decode::Stage::Full, Err(message)) => {
                media.error = Some(message);
                media.decoding = false;
            }
            // A thumbnail that would not decode is a cache miss; the real
            // decode is still coming and is the answer either way.
            (decode::Stage::Thumb, Err(_)) => {}
        }
    }

    /// Is the body a picture decoded to the pane's size — the one kind of body
    /// a bigger pane is worth asking for again?
    ///
    /// Not an archive's badge: nothing in it depends on the size, and asking
    /// again would put the badge back up and list the archive a second time
    /// for a window that was only dragged wider.
    fn is_media(&self) -> bool {
        matches!(
            self.shown.as_ref().map(|s| &s.body),
            Some(Body::Media(media)) if media.kind != PreviewKind::Archive
        )
    }

    /// Is anything still moving? The `animating()` half of PLAN §1's idle-cost
    /// rule: a settled preview must stop asking for frames.
    pub fn animating(&self, now: Instant) -> bool {
        // The wheel's coast, first: it moves the pane whether or not there is
        // anything decoded to show yet, and it *ends* — `tick_fling` drops it
        // the frame it arrives.
        if self.fling.as_ref().is_some_and(|f| !f.finished(now)) {
            return true;
        }
        let Some(shown) = &self.shown else {
            return false;
        };
        if let Body::Media(media) = &shown.body {
            if media
                .swapped_at
                .is_some_and(|at| media.thumb.is_some() && fade(at, now) < 1.0)
            {
                return true;
            }
            if let Some(view) = &media.doc {
                // A page turn crossfades; the indicator's *fade* is motion and
                // its linger is not (see the scrollbar below, same rule).
                if view.previous.is_some() && view.swapped_at.is_some_and(|at| fade(at, now) < 1.0)
                {
                    return true;
                }
                if matches!(view.chip_alpha(now), a if a > 0.0 && a < 1.0) {
                    return true;
                }
                // **The turntable does not answer here.** It is not an
                // animation the paint thread drives: each rendered frame rings
                // the wake bell, which brings exactly one paint, which asks for
                // the next angle. Claiming to be animating as well would turn
                // that into a busy loop rendering angles nobody sees.
            }
        }
        // The scrollbar's fade-out, and only the fade — a bar that is still
        // being held bright is a constant and asks for nothing.
        matches!(self.scrollbar_alpha(now), a if a > 0.0 && a < 1.0)
    }

    /// When the next frame is owed by something that is *waiting* rather than
    /// moving: the "reading…" label earning its place, and the scrollbar
    /// reaching the end of its linger.
    pub fn next_deadline(&self, now: Instant) -> Option<Duration> {
        let loading = (self.shown.is_none() && self.wanted.is_some())
            .then(|| (self.requested_at + LOADING_DELAY).saturating_duration_since(now))
            .filter(|d| !d.is_zero());
        let bar = crate::scrollbar::deadline(self.scrolled_at, now);
        // The page indicator's one scheduled wake-up: the instant its linger
        // ends and its fade begins.
        let chip = self
            .doc_ref()
            .and_then(|view| view.chip_at)
            .map(|at| (at + CHIP_LINGER).saturating_duration_since(now))
            .filter(|d| !d.is_zero());
        // The animated image's next frame. **This and not `animating`**: a GIF
        // holding each frame for a tenth of a second wants ten wake-ups a
        // second, and saying "still moving" would buy it sixty — six times the
        // frames for the same picture, which is exactly the idle cost PLAN §1
        // is about.
        // …and nothing at all while the pane is covered: a deadline is a
        // wake-up, and a wake-up for a frame nobody can see is the idle cost
        // this whole function exists to keep down.
        let anim = self
            .anim()
            .filter(|_| self.visible)
            .and_then(|anim| anim.next_deadline(now));
        // The sub-fit spring's one scheduled wake-up: the instant the last
        // wheel tick's blend runs out and the picture is owed its way home.
        let spring = self.gestures.next_deadline(now).filter(|d| !d.is_zero());
        [loading, bar, chip, anim, spring]
            .into_iter()
            .flatten()
            .min()
    }

    /// How visible the scrollbar is, 0–1: held after the last scroll, then
    /// eased away — the panes' bar's rule, to the millisecond
    /// ([`crate::scrollbar::alpha`]).
    fn scrollbar_alpha(&self, now: Instant) -> f32 {
        crate::scrollbar::alpha(self.scrolled_at, now)
    }
}

/// How far a crossfade that started at `at` has got, eased.
///
/// `OutQuint` rather than linear: a linear opacity ramp reads as a dissolve
/// with a hard start and stop, and this is the same curve every other motion
/// in the program uses (PLAN §8).
/// What [`prepare`] finished, as the body the pane draws. Split out of
/// `apply_prepared` because the in-frame fallback (see `Preparer::request`)
/// reaches the same translation without going through the channel.
fn body_from_ready(ready: prepare::Ready) -> Body {
    match ready {
        prepare::Ready::Text {
            lines,
            syntax,
            truncated,
            states,
        } => Body::Text {
            lines,
            syntax,
            truncated,
            states,
        },
        prepare::Ready::Markdown { blocks, truncated } => Body::Markdown { blocks, truncated },
    }
}

/// What [`listing`] found, as the body the pane draws.
///
/// `truncated` is the one judgement in it: the rows stop before the archive
/// does, either because the preview kept only its first
/// [`listing::PREVIEW_ENTRIES`] or because the listing itself was cut short —
/// and either way the rows end in a footer rather than looking like the whole
/// archive.
fn body_from_listing(listing: listing::Listing) -> Body {
    let shown = listing.rows.len();
    Body::Archive {
        format: listing.format.to_ascii_uppercase(),
        shown,
        total: listing.total,
        files: listing.files,
        total_len: listing.total_len,
        truncated: listing.total > shown || !listing.complete,
        complete: listing.complete,
        encrypted: listing.encrypted,
        entries: listing.rows,
    }
}

pub fn fade(at: Instant, now: Instant) -> f32 {
    let t = now.saturating_duration_since(at).as_secs_f32() / CROSSFADE.as_secs_f32();
    crate::motion::Easing::OutQuint.apply(t.clamp(0.0, 1.0))
}

/// Put decoded pixels on the GPU.
///
/// `None` when there is no context yet — a decode that finished before the
/// window was mapped, which the next frame's poll will not repeat, so the
/// preview is simply re-requested by the cursor that is already on the row.
fn upload(ctx: Option<&egui::Context>, name: &str, image: &decode::Rgba) -> Option<Texture> {
    // The conversion the decode worker already does for itself; this is the
    // path the *document* rasterisers still come in on, where the page buffer
    // was built on their own thread and the conversion is what is left.
    upload_color(ctx, name, decode::to_color(image).ok()?)
}

/// Put a worker-built [`egui::ColorImage`] on the GPU.
///
/// The **filtering** decision lives here because it is a decision about what
/// the file *is*: a 32-pixel favicon blown up to a 900-pixel pane is pixel art
/// and wants nearest-neighbour; a photograph wants linear (PLAN §6, and
/// `preview::paint::fit_rect`, which is the other half of the same rule).
fn upload_color(
    ctx: Option<&egui::Context>,
    name: &str,
    color: egui::ColorImage,
) -> Option<Texture> {
    let ctx = ctx?;
    let max = ctx.input(|i| i.max_texture_side).max(1);
    let [w, h] = color.size;
    if w == 0 || h == 0 || w > max || h > max {
        log::debug!("{w}×{h} is past this GPU's {max} px texture limit");
        return None;
    }
    let size = (w as u32, h as u32);
    let options = if paint::nearest_for(size) {
        egui::TextureOptions::NEAREST
    } else {
        egui::TextureOptions::LINEAR
    };
    let handle = ctx.load_texture(name, color, options);
    Some(Texture { handle, size })
}

/// Turn a `DfError` string into something a person can act on: the path is
/// already in the list pane, so what is left is the part that says what to do.
fn readable(error: &str, path: &Path) -> String {
    let prefix = format!("{}: ", path.display());
    let tail = error.strip_prefix(&prefix).unwrap_or(error);
    let tail = tail.rsplit_once(": ").map(|(_, t)| t).unwrap_or(tail);
    let mut text = tail.to_string();
    if let Some(first) = text.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A three-frame loop, played by the clock: it holds each frame for its own
    /// delay, wraps at the end, and asks for exactly one wake-up at a time.
    #[test]
    fn an_animation_holds_each_frame_for_its_delay_and_then_wraps() {
        let ctx = egui::Context::default();
        let texture = |name: &str| Texture {
            handle: ctx.load_texture(
                name,
                egui::ColorImage::filled([2, 2], egui::Color32::WHITE),
                egui::TextureOptions::NEAREST,
            ),
            size: (2, 2),
        };
        let t0 = Instant::now();
        let mut anim = Anim::new(t0);
        // One frame is a still: nothing moves and nothing is asked for.
        anim.push(texture("a"), Duration::from_millis(100), t0);
        assert!(anim.texture().is_some());
        assert_eq!(anim.next_deadline(t0), None, "one frame is a still");
        anim.tick(t0 + Duration::from_secs(5));
        assert_eq!(anim.index, 0);

        anim.push(texture("b"), Duration::from_millis(200), t0);
        anim.push(texture("c"), Duration::from_millis(300), t0);
        // The clock started with the first frame, so the second is due 100 ms in.
        assert_eq!(
            anim.next_deadline(t0),
            Some(Duration::from_millis(100)),
            "the wake-up is the frame's own delay, not a poll"
        );
        anim.tick(t0 + Duration::from_millis(99));
        assert_eq!(anim.index, 0, "held for its full delay");
        anim.tick(t0 + Duration::from_millis(100));
        assert_eq!(anim.index, 1);
        assert_eq!(
            anim.next_deadline(t0 + Duration::from_millis(100)),
            Some(Duration::from_millis(200))
        );
        anim.tick(t0 + Duration::from_millis(300));
        assert_eq!(anim.index, 2);
        // …and round it goes.
        anim.tick(t0 + Duration::from_millis(600));
        assert_eq!(anim.index, 0, "the loop wraps");

        // **A window that was hidden for ten minutes** owes six thousand steps.
        // The catch-up is bounded by one pass round the loop and then resyncs,
        // so the frame that restores the window is not the frame that walks
        // them — and the animation is running again from `now`.
        let late = t0 + Duration::from_secs(600);
        anim.tick(late);
        assert!(anim.due > late, "the clock is resynchronised, not chased");
        assert!(anim.due <= late + Duration::from_millis(300));
    }

    #[test]
    fn a_crossfade_starts_at_nothing_and_ends_at_everything() {
        let t0 = Instant::now();
        assert_eq!(fade(t0, t0), 0.0);
        assert!((fade(t0, t0 + CROSSFADE) - 1.0).abs() < 1e-4);
        // …and stays there. A fade that came back would be a flicker.
        assert!((fade(t0, t0 + Duration::from_secs(9)) - 1.0).abs() < 1e-4);
    }

    /// The curve is front-loaded, which is what makes 80 ms read as "it was
    /// already there" rather than as a dissolve.
    #[test]
    fn a_crossfade_is_eased_not_linear() {
        let t0 = Instant::now();
        let quarter = fade(t0, t0 + CROSSFADE / 4);
        assert!(
            quarter > 0.5,
            "a quarter of the way in should be past half, got {quarter}"
        );
        let mut previous = 0.0;
        for step in 0..=16 {
            let value = fade(t0, t0 + CROSSFADE * step / 16);
            assert!(
                value >= previous - 1e-4,
                "the fade went backwards at {step}"
            );
            previous = value;
        }
    }

    /// A `now` from before the start — a stale instant carried into a frame —
    /// must read as "not begun", not panic on a negative duration.
    #[test]
    fn a_crossfade_sampled_early_has_not_begun() {
        let t0 = Instant::now() + Duration::from_secs(1);
        assert_eq!(fade(t0, t0 - Duration::from_millis(500)), 0.0);
    }

    /// PLAN §6's session memory: arrowing off a file and back on to it lands
    /// where the reading was, not at the top.
    #[test]
    fn the_pane_remembers_how_far_into_each_file_it_had_got() {
        let now = Instant::now();
        let mut pane = Pane::start(df_core::fs::no_notifier());
        let (a, b) = (Path::new("/etc/hostname"), Path::new("/etc/hosts"));
        let target = (400, 400);

        pane.sync(Some(a), target, now);
        // The paint is what discovers how far the content goes; this stands in
        // for it so the scroll below is a legal one.
        pane.max_scroll = 40;
        pane.scroll_to(12, now);

        // Onto another file: its own position, which is the top.
        pane.sync(Some(b), target, now);
        assert_eq!(pane.scroll, 0);

        // …and back. The clamp is the next paint's job, so the line is
        // restored as it was.
        pane.sync(Some(a), target, now);
        assert_eq!(pane.scroll, 12);

        // A document remembers its page rather than a line, and the page is
        // handed to the `DocView` the body builds several frames later.
        let mut view = DocView::new();
        view.page = 3;
        pane.shown = Some(Shown {
            body: Body::Media(Media {
                kind: PreviewKind::Pdf,
                thumb: None,
                full: None,
                swapped_at: None,
                anim: None,
                error: None,
                decoding: false,
                doc: Some(Box::new(view)),
                note: None,
            }),
        });
        pane.sync(Some(b), target, now);
        assert_eq!(pane.resume_page, 0);
        pane.sync(Some(a), target, now);
        assert_eq!(pane.resume_page, 3);

        // Leaving the pane entirely — a tab switch — is leaving the file, not
        // losing it.
        pane.scroll_to(0, now);
        pane.max_scroll = 40;
        pane.scroll_to(7, now);
        pane.cancel();
        pane.sync(Some(a), target, now);
        assert_eq!(pane.scroll, 7);
    }

    /// The answer the audio card waits on, through the states a song's pane
    /// really passes through: asked for, reading, decoding the sleeve, and
    /// then either showing it or saying there is none.
    #[test]
    fn the_pane_says_whether_a_songs_sleeve_is_coming() {
        let now = Instant::now();
        let mut pane = Pane::start(df_core::fs::no_notifier());
        assert_eq!(pane.poster(), Poster::Absent, "nothing hovered");

        // Asked for, not arrived: the transport has usually mounted by now,
        // and this is the gap a card would flash in.
        pane.sync(Some(Path::new("/music/song.mp3")), (400, 400), now);
        assert_eq!(pane.poster(), Poster::Pending);

        // The body is up and the sleeve is decoding — exactly what
        // `decode::plan` asks for, for a song.
        let song = |pane: &mut Pane| {
            let decode::Plan { decoding, .. } = decode::plan(&PreviewKind::Audio, false);
            pane.shown = Some(Shown {
                body: Body::Media(Media {
                    kind: PreviewKind::Audio,
                    thumb: None,
                    full: None,
                    swapped_at: None,
                    anim: None,
                    error: None,
                    decoding,
                    doc: None,
                    note: None,
                }),
            });
        };
        song(&mut pane);
        assert_eq!(pane.poster(), Poster::Pending);

        // No sleeve: the wait is over, nothing is printed, and the card may
        // have the pane.
        let answer = |result| decode::Decoded {
            token: PreviewToken(1),
            stage: decode::Stage::Full,
            result,
        };
        pane.apply_decoded(answer(Ok(None)), None, now);
        assert_eq!(pane.poster(), Poster::Absent);
        let Some(Shown {
            body: Body::Media(media),
        }) = &pane.shown
        else {
            panic!("the body is still the song's");
        };
        assert!(media.error.is_none(), "a song without art is not an error");

        // A sleeve: on screen, and the card stays off it.
        song(&mut pane);
        let ctx = egui::Context::default();
        let sleeve = egui::ColorImage::filled([4, 4], egui::Color32::from_rgb(255, 165, 0));
        pane.apply_decoded(answer(Ok(Some(sleeve))), Some(&ctx), now);
        assert_eq!(pane.poster(), Poster::Shown);

        // Text is not a picture, whatever the card thinks of it.
        pane.shown = Some(Shown { body: Body::Empty });
        assert_eq!(pane.poster(), Poster::Absent);
    }

    /// A pane showing a document with `pages` pages and nothing rendered yet.
    fn documented(kind: PreviewKind, pages: usize, counter: doc::Counter) -> (Pane, Instant) {
        let now = Instant::now();
        let mut pane = Pane::start(df_core::fs::no_notifier());
        let mut view = DocView::new();
        view.meta = Some(doc::Meta {
            pages,
            summary: String::new(),
            counter,
        });
        pane.shown = Some(Shown {
            body: Body::Media(Media {
                kind,
                thumb: None,
                full: None,
                swapped_at: None,
                anim: None,
                error: None,
                decoding: false,
                doc: Some(Box::new(view)),
                note: None,
            }),
        });
        (pane, now)
    }

    /// PLAN §4.3: `Ctrl+→` turns the page, `Ctrl+←` turns it back, and **at
    /// the first page it refuses** — the app reads that as "nothing to turn to"
    /// and the key is inert rather than wrapping round.
    #[test]
    fn paging_stops_at_both_ends_and_says_so() {
        let (mut pane, now) = documented(PreviewKind::Pdf, 42, doc::Counter::Page);
        assert!(
            !pane.turn_page(false, now),
            "page one has nowhere back to go"
        );
        assert!(pane.turn_page(true, now));
        assert_eq!(pane.doc_ref().map(|v| v.page), Some(1));
        assert_eq!(
            pane.doc_ref().and_then(DocView::counter).as_deref(),
            Some("2 / 42")
        );
        assert!(pane.turn_page(false, now));
        assert!(!pane.turn_page(false, now), "it walked past page one");

        for _ in 0..100 {
            pane.turn_page(true, now);
        }
        assert_eq!(
            pane.doc_ref().map(|v| v.page),
            Some(41),
            "past the last page"
        );
        assert!(!pane.turn_page(true, now));

        // A one-page document never pages at all, so `←` falls through from the
        // first press — a specimen sheet must not trap the keyboard.
        let (mut single, now) = documented(PreviewKind::Font, 1, doc::Counter::None);
        assert!(!single.turn_page(true, now));
        assert!(!single.turn_page(false, now));
        assert_eq!(single.doc_ref().and_then(DocView::counter), None);
    }

    /// `↑`/`↓` is one key with three meanings, and which one it has depends
    /// entirely on what is in the pane.
    #[test]
    fn the_arrows_step_layers_pan_pages_and_otherwise_scroll_lines() {
        // A toolpath: the arrows are the layer stepping, and they clamp.
        let (mut gcode, now) = documented(PreviewKind::Gcode, 3, doc::Counter::Layer);
        assert!(gcode.doc_scroll(1, now));
        assert_eq!(gcode.doc_ref().map(|v| v.page), Some(1));
        assert_eq!(
            gcode.doc_ref().and_then(DocView::counter).as_deref(),
            Some("Layer 2 / 3")
        );
        for _ in 0..10 {
            gcode.doc_scroll(1, now);
        }
        assert_eq!(
            gcode.doc_ref().map(|v| v.page),
            Some(2),
            "the layer ran off the top"
        );
        for _ in 0..10 {
            gcode.doc_scroll(-1, now);
        }
        assert_eq!(gcode.doc_ref().map(|v| v.page), Some(0));

        // A page: the arrows move the *picture*, which only exists to move
        // once it is zoomed — at fit the key falls through to the text scroll,
        // because a fitted page has nothing left to show.
        let (mut pdf, now) = documented(PreviewKind::Pdf, 2, doc::Counter::Page);
        pdf.set_picture(Some((PANE, PANE.size())));
        assert!(!pdf.doc_scroll(1, now), "a fitted page has nowhere to pan");
        pdf.zoom(Zoom::In, now);
        pdf.zoom(Zoom::In, now);
        settle(&mut pdf, now);
        assert!(pdf.is_zoomed());
        assert!(pdf.doc_scroll(1, now));
        settle(&mut pdf, now);
        assert!(
            pdf.view().pan.y < 0.0,
            "`↓` moves the picture up: {:?}",
            pdf.view().pan
        );
        // …and only as far as the clamp allows, however long the key is held.
        for _ in 0..40 {
            pdf.doc_scroll(1, now);
            settle(&mut pdf, now);
        }
        let floor = pdf.view().pan.y;
        pdf.doc_scroll(1, now);
        settle(&mut pdf, now);
        assert!(
            (pdf.view().pan.y - floor).abs() < 0.01,
            "the pan ran past the clamp"
        );

        // Anything that is not a picture refuses the key, and the caller
        // scrolls text with it instead.
        let mut text = Pane::start(df_core::fs::no_notifier());
        assert!(!text.doc_scroll(1, Instant::now()));
        assert!(!text.turn_page(true, Instant::now()));
    }

    /// A pane, and a picture fitted to exactly fill it.
    const PANE: egui::Rect = egui::Rect {
        min: egui::pos2(0.0, 0.0),
        max: egui::pos2(400.0, 600.0),
    };

    /// Run the gesture layer forward past every animation it could be in the
    /// middle of, with no pointer and no wheel.
    fn settle(pane: &mut Pane, now: Instant) {
        let at = now + Duration::from_millis(2000);
        let (content, fitted) = pane.picture_geometry().expect("a picture");
        pane.gesture(&gesture::Input::still(content.size(), fitted, at), at);
    }

    /// `+` / `-` / `0` drive the same view the wheel does, and the document
    /// rasteriser follows the scale they settle on — the sharpness, not the
    /// transform (`Pane::sync_doc_zoom`).
    #[test]
    fn zooming_a_page_drives_the_shared_view_and_the_raster_follows() {
        let (mut pane, now) = documented(PreviewKind::Pdf, 2, doc::Counter::Page);
        pane.set_picture(Some((PANE, PANE.size())));
        assert_eq!(pane.doc_ref().map(|v| v.zoom), Some(doc::ZOOM_MIN));

        pane.zoom(Zoom::In, now);
        pane.zoom(Zoom::In, now);
        settle(&mut pane, now);
        let scale = pane.view().scale;
        assert!(scale > 1.0, "got {scale}");
        // The rung is the *next* one up from the settled scale, so wheeling in
        // and back out lands on renders the worker has already done.
        let rung = pane.doc_ref().map(|v| v.zoom).unwrap_or(0.0);
        assert!(
            rung >= scale,
            "the raster must not be coarser: {rung} < {scale}"
        );
        assert!(
            rung <= scale * doc::ZOOM_STEP + 1e-4,
            "and no finer than a rung"
        );

        // `0` is fit, and a fitted page is back to one raster.
        pane.zoom(Zoom::Fit, now);
        settle(&mut pane, now);
        assert!((pane.view().scale - 1.0).abs() < 1e-3);
        assert_eq!(pane.doc_ref().map(|v| v.zoom), Some(doc::ZOOM_MIN));

        // `-` at fit is a no-op rather than a page shrinking into the corner:
        // the rubber band belongs to a gesture, which has a release to spring
        // back from, and a key press does not.
        pane.zoom(Zoom::Out, now);
        settle(&mut pane, now);
        assert!((pane.view().scale - 1.0).abs() < 1e-3);
    }

    /// A picture arrives at fit however the last one was left — the zoom is not
    /// remembered per file the way the reading position is.
    #[test]
    fn a_new_file_arrives_fitted() {
        let (mut pane, now) = documented(PreviewKind::Pdf, 2, doc::Counter::Page);
        pane.set_picture(Some((PANE, PANE.size())));
        pane.zoom(Zoom::In, now);
        settle(&mut pane, now);
        assert!(pane.is_zoomed());
        pane.sync(Some(Path::new("/tmp/some-other-file")), (400, 600), now);
        assert!(!pane.is_zoomed());
        assert_eq!(pane.view(), gesture::View::FIT);
        assert!(
            !pane.is_picture(),
            "and it has no footprint until it decodes"
        );
    }

    /// The wheel forks on what the body is: text scrolls by lines, a picture
    /// takes it as a zoom (which the gesture pass, not `wheel`, applies).
    #[test]
    fn the_wheel_declines_a_picture_and_scrolls_text() {
        let (mut pdf, now) = documented(PreviewKind::Pdf, 2, doc::Counter::Page);
        pdf.set_picture(Some((PANE, PANE.size())));
        assert!(pdf.is_picture());
        assert!(!pdf.wheel(-3.0, now), "a picture never scrolls by lines");

        let mut text = Pane::start(df_core::fs::no_notifier());
        assert!(!text.is_picture());
        text.max_scroll = 100;
        assert!(text.wheel(3.0, now), "and text still coasts by lines");
    }

    /// PLAN §8's "linger then leave", and PLAN §1's "one scheduled wake-up in
    /// between": the indicator is held, then fades, and asks for exactly one
    /// frame at the moment the fade begins.
    #[test]
    fn the_page_indicator_lingers_then_leaves() {
        let (mut pane, now) = documented(PreviewKind::Pdf, 42, doc::Counter::Page);
        assert_eq!(pane.doc_ref().map(|v| v.chip_alpha(now)), Some(0.0));
        pane.turn_page(true, now);
        let alpha = |at: Instant| pane.doc_ref().map(|v| v.chip_alpha(at)).unwrap_or(-1.0);
        assert_eq!(alpha(now), 1.0);
        assert_eq!(alpha(now + CHIP_LINGER - Duration::from_millis(1)), 1.0);
        let half = alpha(now + CHIP_LINGER + CHIP_FADE / 2);
        assert!((half - 0.5).abs() < 0.05, "got {half}");
        assert_eq!(alpha(now + CHIP_LINGER + CHIP_FADE), 0.0);
        assert_eq!(alpha(now + CHIP_LINGER + CHIP_FADE * 4), 0.0);

        // The one wake-up, and none after the fade is over.
        assert_eq!(
            pane.next_deadline(now),
            Some(CHIP_LINGER),
            "the fade's start was not scheduled"
        );
        assert!(pane.animating(now + CHIP_LINGER + CHIP_FADE / 2));
        assert!(!pane.animating(now + CHIP_LINGER + CHIP_FADE * 2));
    }

    /// A stored-only zip, byte by byte: `(name, data, encrypted)` per member,
    /// a trailing `/` making a directory. df-core's own fixture builder is
    /// private to its tests, and the format is small enough to write twice.
    fn zip_of(members: &[(&str, &[u8], bool)]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut central = Vec::new();
        for (name, data, encrypted) in members {
            let at = out.len() as u32;
            let flags: u16 = if *encrypted { 0x0001 } else { 0 };
            let external: u32 = if name.ends_with('/') { 0x10 } else { 0 };
            let len = (data.len() as u32).to_le_bytes();
            let name_len = (name.len() as u16).to_le_bytes();
            for chunk in [
                b"PK\x03\x04".as_slice(),
                &20u16.to_le_bytes(),
                &flags.to_le_bytes(),
                &0u16.to_le_bytes(), // stored
                &0u16.to_le_bytes(),
                &0x0021u16.to_le_bytes(),
                &0u32.to_le_bytes(), // crc, which a listing never reads
                &len,
                &len,
                &name_len,
                &0u16.to_le_bytes(),
                name.as_bytes(),
                data,
            ] {
                out.extend_from_slice(chunk);
            }
            for chunk in [
                b"PK\x01\x02".as_slice(),
                &20u16.to_le_bytes(),
                &20u16.to_le_bytes(),
                &flags.to_le_bytes(),
                &0u16.to_le_bytes(),
                &0u16.to_le_bytes(),
                &0x0021u16.to_le_bytes(),
                &0u32.to_le_bytes(),
                &len,
                &len,
                &name_len,
                &0u16.to_le_bytes(),
                &0u16.to_le_bytes(),
                &0u16.to_le_bytes(),
                &0u16.to_le_bytes(),
                &external.to_le_bytes(),
                &at.to_le_bytes(),
                name.as_bytes(),
            ] {
                central.extend_from_slice(chunk);
            }
        }
        let (cd_at, cd_len) = (out.len() as u32, central.len() as u32);
        out.extend_from_slice(&central);
        let count = (members.len() as u16).to_le_bytes();
        for chunk in [
            b"PK\x05\x06".as_slice(),
            &0u16.to_le_bytes(),
            &0u16.to_le_bytes(),
            &count,
            &count,
            &cd_len.to_le_bytes(),
            &cd_at.to_le_bytes(),
            &0u16.to_le_bytes(),
        ] {
            out.extend_from_slice(chunk);
        }
        out
    }

    /// A zip df-core lists becomes a body with its counts, its first
    /// [`listing::PREVIEW_ENTRIES`] rows in the archive's order, a truncation
    /// that says there is more, and the encrypted flag of a member past the
    /// kept rows.
    #[test]
    fn a_listed_zip_becomes_an_archive_body() {
        let tree = df_core::test_support::TempTree::new("preview-archive-body");
        let names: Vec<String> = (0..600).map(|i| format!("proj/f{i:03}.txt")).collect();
        let mut members: Vec<(&str, &[u8], bool)> = vec![("proj/", b"", false)];
        for (i, name) in names.iter().enumerate() {
            members.push((name.as_str(), b"hello", i == 550));
        }
        let path = tree.file("big.zip", &zip_of(&members));
        let listed = df_core::archive::list(&path).expect("the fixture zip lists");

        let Body::Archive {
            format,
            entries,
            shown,
            total,
            files,
            total_len,
            truncated,
            complete,
            encrypted,
        } = body_from_listing(listing::Listing::from_tree(&listed))
        else {
            panic!("a listing is an archive body");
        };
        assert_eq!(format, "ZIP");
        assert_eq!(shown, listing::PREVIEW_ENTRIES);
        assert_eq!(entries.len(), shown);
        assert_eq!(total, 601);
        assert_eq!(files, 600);
        assert_eq!(total_len, 600 * 5);
        assert!(truncated, "600 files do not fit in 500 rows");
        assert!(complete);
        assert!(
            encrypted,
            "member 550 is past the kept rows and still counts"
        );
        // The archive's order, with its single top-level folder kept.
        assert_eq!(entries[0].name, "proj");
        assert!(entries[0].is_dir);
        assert_eq!(entries[0].len, None);
        assert_eq!(entries[1].name, "proj/f000.txt");
        assert_eq!(entries[1].len, Some(5));
        assert_eq!(entries[499].name, "proj/f498.txt");

        // A zip of bare file paths lists what it stores: the folders df-core
        // synthesizes for browsing are not rows, and a listing that fits is
        // not truncated.
        let small = tree.file(
            "small.zip",
            &zip_of(&[("b/one.txt", b"1", false), ("a/two.txt", b"22", false)]),
        );
        let Body::Archive {
            entries,
            total,
            truncated,
            encrypted,
            ..
        } = body_from_listing(listing::Listing::from_tree(
            &df_core::archive::list(&small).expect("the fixture zip lists"),
        ))
        else {
            panic!("a listing is an archive body");
        };
        let names: Vec<&str> = entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["b/one.txt", "a/two.txt"],
            "stored order, no folders"
        );
        assert_eq!(total, 2);
        assert!(!truncated);
        assert!(!encrypted);
    }

    /// The badge goes up first and the listing replaces it; a listing that
    /// could not be read keeps the badge and adds its reason; an answer about
    /// a file the cursor has left is dropped.
    #[test]
    fn a_listing_replaces_the_badge_and_a_failure_keeps_it() {
        let now = Instant::now();
        let mut pane = Pane::start(df_core::fs::no_notifier());
        let path = Path::new("/archives/a.zip");
        pane.sync(Some(path), (400, 400), now);
        let badge = || Shown {
            body: Body::Media(Media {
                kind: PreviewKind::Archive,
                thumb: None,
                full: None,
                swapped_at: None,
                anim: None,
                error: None,
                decoding: false,
                doc: None,
                note: None,
            }),
        };
        let listed = || listing::Listing {
            format: "zip".to_string(),
            rows: Vec::new(),
            total: 0,
            files: 0,
            total_len: 0,
            encrypted: false,
            complete: true,
        };

        pane.shown = Some(badge());
        assert!(!pane.apply_listing(Path::new("/archives/b.zip"), Ok(listed())));
        assert!(matches!(
            pane.shown.as_ref().map(|s| &s.body),
            Some(Body::Media(_))
        ));

        assert!(pane.apply_listing(path, Err("Install 7-Zip to list this archive".into())));
        let Some(Body::Media(media)) = pane.shown.as_ref().map(|s| &s.body) else {
            panic!("a failure keeps the badge");
        };
        assert_eq!(
            media.note.as_deref(),
            Some("Install 7-Zip to list this archive")
        );
        assert_eq!(media.badge(), Some("archive"));

        pane.shown = Some(badge());
        assert!(pane.apply_listing(path, Ok(listed())));
        assert!(matches!(
            pane.shown.as_ref().map(|s| &s.body),
            Some(Body::Archive { .. })
        ));
        // …and a listing is text-shaped: the wheel scrolls it by lines.
        assert!(!pane.is_media());
        assert_eq!(pane.poster(), Poster::Absent);
    }

    #[test]
    fn errors_lose_the_path_and_keep_the_reason() {
        let path = Path::new("/root/secret");
        assert_eq!(
            readable("/root/secret: permission denied (os error 13)", path),
            "Permission denied (os error 13)"
        );
        assert_eq!(readable("gone", path), "Gone");
    }
}
