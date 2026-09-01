//! The preview pane: what is under the cursor, drawn (PLAN §6).
//!
//! ```text
//!   cursor moves ──▶ Pane::sync ──▶ df_core::preview::Previewer  (debounced,
//!                                        │                        newest wins)
//!                    Pane::poll ◀── PreviewUpdate ────────────────┘
//!                        │
//!                        ├─ Text / Markdown / Directory / Hex ──▶ paint
//!                        └─ NeedsDecode ──▶ decode::Decoder ──▶ texture ──▶ paint
//! ```
//!
//! Three rules hold the whole thing together.
//!
//! **Nothing decodes on the paint thread.** df-core's workers read the file and
//! [`decode`]'s worker turns bytes into pixels; this module only ever receives
//! finished work and hands it to egui. Both workers ring the same
//! [`crate::Wake`] bell every other worker rings (PLAN §1).
//!
//! **Every answer is checked against its token.** df-core cancels a superseded
//! request before it opens anything, and the token check here catches whatever
//! wins the race anyway — so a slow decode of the file you arrowed past can
//! never paint over the file you stopped on.
//!
//! **Everything arrives with the same 80 ms crossfade** ([`CROSSFADE`], PLAN
//! §6). Not only images: text, a listing and a hexdump fade in on the same
//! curve, because the alternative is a pane that snaps to a new state every
//! time the cursor moves, and forty of those in a second is a strobe.
//!
//! ## The seam, and what has taken it up
//!
//! Video, audio, PDFs, fonts and models reach [`decode::Job`] with `full:
//! false`: what it fetches for them is the *cached thumbnail*, and the real
//! answer comes from somewhere else.
//!
//! **Video and audio are now [`crate::playback`]'s.** The cached thumbnail is
//! still what this module draws — it is the poster the first decoded frame
//! lands on top of — and [`Pane::set_media_mounted`] is how it is told that a
//! transport has the file, at which point the kind badge stands down and the
//! frame, the audio card and the position strip are painted over this pane by
//! the player.
//!
//! **PDFs, fonts, models and G-code are [`doc`]'s.** They take the same seam
//! from the other side: [`decode`] still fetches the cached thumbnail so a PDF
//! has a poster the instant the cursor lands on it, and [`doc::Docs`] renders
//! the real page over the top. The four of them share one [`DocView`] because
//! they share one shape — a document with a current page, a zoom and an
//! indicator — even though a specimen sheet has one page and a mesh does not
//! really have pages at all.

pub mod decode;
pub mod doc;
pub mod highlight;
pub mod markdown;
mod paint;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use df_core::fs::{Entry, Notifier, SortOptions};
use df_core::preview::{
    PaneId, Preview, PreviewKind, PreviewToken, PreviewUpdate, Previewer, TargetSize,
};

pub use paint::{fit_rect, preview};

/// How long a preview takes to fade in — PLAN §6's "results crossfade in over
/// ~80 ms", and delightviewer's `CROSSFADE` to the millisecond, so the two
/// programs handing a file between them feel like one program.
///
/// Short enough to read as "it was always there", long enough that a
/// placeholder → full-resolution swap does not snap.
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
/// late. It runs **only while the preview has the keyboard**: idle discipline
/// beats spin (PLAN §1), so a model the cursor merely passed over is a still
/// picture that costs nothing.
const TURNTABLE_PERIOD: Duration = Duration::from_secs(24);

/// How far one press of `↑`/`↓` moves an oversized page, in logical points.
///
/// A shade over the list's row height, so a press moves about a line of body
/// text at the sizes a PDF is set at — the same "one press, one line" the text
/// body already obeys, in the unit a picture has.
const PAN_STEP: f32 = 24.0;

/// What `+`, `-` and `0` mean (PLAN §4.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Zoom {
    In,
    Out,
    /// `0`: back to fit, which is the only zoom a document starts at.
    Fit,
}

/// A decoded picture living on the GPU.
struct Texture {
    handle: egui::TextureHandle,
    /// The decoded size in physical pixels, which is what the fit is computed
    /// from — a texture is not points and must not be treated as if it were.
    size: (u32, u32),
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
    /// 1.0 is fit-to-pane.
    zoom: f32,
    /// How far an oversized page is scrolled, in logical points.
    pan: f32,
    /// The most [`DocView::pan`] the last paint could actually use — the paint
    /// is the only thing that knows how tall the page came out.
    max_pan: f32,
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
            pan: 0.0,
            max_pan: 0.0,
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
    /// There is nothing honest to draw; the opener rules are the answer.
    Unsupported {
        kind: PreviewKind,
    },
    /// The read itself failed — permission denied, and the like.
    Failed(String),
}

/// What is on screen, and since when.
struct Shown {
    body: Body,
    /// The start of the content crossfade.
    at: Instant,
}

/// The preview pane's whole state.
pub struct Pane {
    previewer: Previewer,
    decoder: decode::Decoder,
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
    /// Set by the app each frame: a playback controller is mounted on this
    /// file, so the kind badge stands down (see [`Pane::set_media_mounted`]).
    media_mounted: bool,
    /// The document worker: PDF pages, specimen sheets, meshes, toolpaths.
    docs: doc::Docs,
    /// The four colours a worker-thread rasteriser is allowed, refreshed each
    /// frame from the palette (see [`doc::Ink`]).
    ink: doc::Ink,
    /// Whether the keyboard is in this pane. The turntable's whole switch:
    /// unfocused, a model is a still picture and asks for nothing.
    focused: bool,
}

impl Pane {
    /// Start the workers. Called before the window exists, so PLAN §6's
    /// cold-start ordering — decode workers up before the first frame — is
    /// what actually happens.
    pub fn start(notify: Notifier) -> Pane {
        Pane {
            previewer: Previewer::start(std::sync::Arc::clone(&notify)),
            docs: doc::Docs::start(std::sync::Arc::clone(&notify)),
            decoder: decode::Decoder::start(notify),
            ink: doc::Ink::test(),
            focused: false,
            wanted: None,
            token: None,
            requested_target: (0, 0),
            requested_at: Instant::now(),
            shown: None,
            scroll: 0,
            max_scroll: 0,
            scrolled_at: None,
            media_mounted: false,
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
            // The content on screen belongs to a different file, and a pane
            // that kept it would be labelling file A with file B.
            self.shown = None;
            self.scroll = 0;
            self.max_scroll = 0;
            self.scrolled_at = None;
            self.decoder.cancel();
            self.docs.cancel();
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

    /// Stop previewing anything — a tab switch, a directory with nothing in it.
    pub fn cancel(&mut self) {
        self.previewer.cancel(PREVIEW_PANE);
        self.decoder.cancel();
        self.docs.cancel();
        self.wanted = None;
        self.token = None;
        self.shown = None;
        self.scroll = 0;
        self.max_scroll = 0;
        self.scrolled_at = None;
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
    }

    /// Scroll by `delta` lines — the preview-focus arrows, `Ctrl+u`/`Ctrl+d`
    /// and `Space` (PLAN §4.3).
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
    }

    /// `g g` / `G`: the top, or as far down as the content goes.
    pub fn scroll_to(&mut self, line: usize, now: Instant) {
        let was = self.scroll;
        self.scroll = line.min(self.max_scroll);
        if self.scroll != was {
            self.scrolled_at = Some(now);
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
        for decoded in self.decoder.drain() {
            if Some(decoded.token) != self.token {
                continue;
            }
            changed = true;
            self.apply_decoded(decoded, ctx, now);
        }
        for update in self.docs.drain() {
            if Some(update.token) != self.token || self.wanted.as_deref() != Some(&update.path) {
                continue;
            }
            changed = true;
            self.apply_doc(update, ctx, now);
        }
        changed
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
                    // the clock restarts from it, so losing and regaining focus
                    // picks the model up where it stopped instead of snapping
                    // back to where it started.
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

    /// The four colours the document rasterisers draw with, and whether this
    /// pane has the keyboard. Both are set once a frame by the app, because
    /// both live on the paint side and are needed on the worker side.
    pub fn set_ink(&mut self, ink: doc::Ink) {
        self.ink = ink;
    }

    pub fn set_focused(&mut self, focused: bool) {
        if self.focused == focused {
            return;
        }
        self.focused = focused;
        if let Some(view) = self.doc_mut() {
            // The turntable's clock starts when the keyboard arrives and is
            // thrown away when it leaves, so the model holds the angle it was
            // last drawn at rather than snapping back to where it started.
            view.spinning_since = None;
        }
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
    /// **This is also the turntable's whole engine.** While the pane has the
    /// keyboard a model's wanted angle moves with the clock, so each finished
    /// frame rings the wake bell, which brings a paint, which asks for the next
    /// angle. Unfocused, the wanted angle is the angle on screen, nothing is
    /// asked for, and the loop stops dead (PLAN §1).
    pub fn sync_doc(&mut self, now: Instant) {
        let (Some(path), Some(token)) = (self.wanted.clone(), self.token) else {
            return;
        };
        let target = self.requested_target;
        let (ink, focused) = (self.ink, self.focused);
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
        if turntable && focused && view.meta.is_some() && view.spinning_since.is_none() {
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
        // A new page starts at its top: carrying the scroll across would land
        // you in the middle of a page you have not seen the start of.
        view.pan = 0.0;
        view.chip_at = Some(now);
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
        let Some(view) = self.doc_mut() else {
            return false;
        };
        let was = view.pan;
        view.pan = (view.pan + delta as f32 * PAN_STEP).clamp(0.0, view.max_pan);
        if (view.pan - was).abs() > f32::EPSILON {
            view.chip_at = Some(now);
        }
        true
    }

    /// `+` / `-` / `0`.
    pub fn zoom(&mut self, step: Zoom, now: Instant) -> bool {
        let Some(view) = self.doc_mut() else {
            return false;
        };
        let was = view.zoom;
        view.zoom = match step {
            Zoom::In => doc::zoom_in(view.zoom),
            Zoom::Out => doc::zoom_out(view.zoom),
            Zoom::Fit => doc::ZOOM_MIN,
        };
        if (view.zoom - was).abs() > f32::EPSILON {
            // Zooming out to fit has nothing left to scroll to.
            view.pan = if view.zoom <= doc::ZOOM_MIN {
                0.0
            } else {
                view.pan
            };
            view.chip_at = Some(now);
        }
        true
    }

    /// How far the last paint found the page overflowing the pane. The paint is
    /// the only thing that knows, exactly as it is for [`Pane::max_scroll`].
    pub(crate) fn set_max_pan(&mut self, max_pan: f32) {
        if let Some(view) = self.doc_mut() {
            view.max_pan = max_pan;
            if view.pan > max_pan {
                view.pan = max_pan;
            }
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
            PreviewUpdate::Ready { preview, .. } => self.body_for(preview, now),
        };
        self.shown = Some(Shown { body, at: now });
    }

    fn body_for(&mut self, preview: Preview, _now: Instant) -> Body {
        match preview {
            Preview::Empty => Body::Empty,
            Preview::Text {
                lines,
                syntax,
                truncated,
            } => {
                let profile = highlight::profile_for(syntax);
                let states = highlight::block_states(&lines, profile);
                Body::Text {
                    lines,
                    syntax,
                    truncated,
                    states,
                }
            }
            Preview::Markdown { source, truncated } => Body::Markdown {
                blocks: markdown::parse(&source),
                truncated,
            },
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
                    return Body::Unsupported { kind };
                };
                // Pictures decode here; documents go to their own worker and
                // are drawn over whatever thumbnail the shared cache had.
                // Video and audio still take their frame from the cache and
                // wait for a transport to mount.
                let full = kind == PreviewKind::Image;
                let store = full && thumb.is_none() && kind.thumbnailable();
                let decoding = full || thumb.is_some();
                let is_doc = matches!(
                    kind,
                    PreviewKind::Pdf
                        | PreviewKind::Font
                        | PreviewKind::Model3d
                        | PreviewKind::Gcode
                );
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
                    error: None,
                    decoding,
                    // The first render is asked for by `sync_doc` on the next
                    // frame, which is where the pane's real pixel size and the
                    // palette are both known.
                    doc: is_doc.then(|| Box::new(DocView::new())),
                })
            }
        }
    }

    fn apply_decoded(&mut self, decoded: decode::Decoded, ctx: Option<&egui::Context>, now: Instant) {
        let Some(shown) = &mut self.shown else { return };
        let Body::Media(media) = &mut shown.body else {
            return;
        };
        match (decoded.stage, decoded.result) {
            (decode::Stage::Thumb, Ok(image)) => {
                // Only while the real thing is still missing: a placeholder
                // that arrives late is worthless (delightviewer's rule).
                if media.full.is_none() {
                    media.thumb = upload(ctx, "df-preview-thumb", &image);
                }
            }
            (decode::Stage::Full, Ok(image)) => {
                media.full = upload(ctx, "df-preview", &image);
                media.swapped_at = Some(now);
                media.decoding = false;
            }
            (decode::Stage::Full, Err(message)) => {
                media.error = Some(message);
                media.decoding = false;
            }
            // A thumbnail that would not decode is a cache miss; the real
            // decode is still coming and is the answer either way.
            (decode::Stage::Thumb, Err(_)) => {}
        }
    }

    fn is_media(&self) -> bool {
        matches!(self.shown.as_ref().map(|s| &s.body), Some(Body::Media(_)))
    }

    /// Is anything still moving? The `animating()` half of PLAN §1's idle-cost
    /// rule: a settled preview must stop asking for frames.
    pub fn animating(&self, now: Instant) -> bool {
        let Some(shown) = &self.shown else {
            return false;
        };
        if fade(shown.at, now) < 1.0 {
            return true;
        }
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
        let bar = self
            .scrolled_at
            .map(|at| (at + SCROLLBAR_LINGER).saturating_duration_since(now))
            .filter(|d| !d.is_zero());
        // The page indicator's one scheduled wake-up: the instant its linger
        // ends and its fade begins.
        let chip = self
            .doc_ref()
            .and_then(|view| view.chip_at)
            .map(|at| (at + CHIP_LINGER).saturating_duration_since(now))
            .filter(|d| !d.is_zero());
        [loading, bar, chip].into_iter().flatten().min()
    }

    /// How visible the scrollbar is, 0–1: held for [`SCROLLBAR_LINGER`] after
    /// the last scroll, then eased away over [`SCROLLBAR_FADE`] (PLAN §8's
    /// "linger then leave").
    fn scrollbar_alpha(&self, now: Instant) -> f32 {
        let Some(at) = self.scrolled_at else {
            return 0.0;
        };
        let elapsed = now.saturating_duration_since(at);
        if elapsed < SCROLLBAR_LINGER {
            return 1.0;
        }
        let over = (elapsed - SCROLLBAR_LINGER).as_secs_f32() / SCROLLBAR_FADE.as_secs_f32();
        (1.0 - over).clamp(0.0, 1.0)
    }
}

/// How long the scrollbar is held after the last `K`/`J`.
///
/// PLAN §8's transient chrome holds ~2.5–3 s; a scrollbar is the quietest thing
/// in that family — it answers "where am I", which is a question asked *while*
/// scrolling — so it takes the short end of the range.
const SCROLLBAR_LINGER: Duration = Duration::from_millis(2000);

/// …then leaves over this. Half PLAN §8's 500 ms, because the bar is 3 points
/// wide and a longer fade on something that small reads as a rendering bug.
const SCROLLBAR_FADE: Duration = Duration::from_millis(250);

/// How far a crossfade that started at `at` has got, eased.
///
/// `OutQuint` rather than linear: a linear opacity ramp reads as a dissolve
/// with a hard start and stop, and this is the same curve every other motion
/// in the program uses (PLAN §8).
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
    let ctx = ctx?;
    let max = ctx.input(|i| i.max_texture_side).max(1);
    let (w, h) = (image.width as usize, image.height as usize);
    if w == 0 || h == 0 || w > max || h > max {
        log::debug!("{w}×{h} is past this GPU's {max} px texture limit");
        return None;
    }
    if image.pixels.len() < w * h * 4 {
        return None;
    }
    let color = egui::ColorImage::from_rgba_unmultiplied([w, h], &image.pixels[..w * h * 4]);
    let handle = ctx.load_texture(name, color, egui::TextureOptions::LINEAR);
    Some(Texture {
        handle,
        size: (image.width, image.height),
    })
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
        assert!(quarter > 0.5, "a quarter of the way in should be past half, got {quarter}");
        let mut previous = 0.0;
        for step in 0..=16 {
            let value = fade(t0, t0 + CROSSFADE * step / 16);
            assert!(value >= previous - 1e-4, "the fade went backwards at {step}");
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
                error: None,
                decoding: false,
                doc: Some(Box::new(view)),
            }),
            at: now,
        });
        (pane, now)
    }

    /// PLAN §4.3: `→` turns the page, `←` turns it back, and **at the first
    /// page `←` refuses** — which is what lets the app fall through to focusing
    /// the list rather than leaving the keyboard stuck in the pane.
    #[test]
    fn paging_stops_at_both_ends_and_says_so() {
        let (mut pane, now) = documented(PreviewKind::Pdf, 42, doc::Counter::Page);
        assert!(!pane.turn_page(false, now), "page one has nowhere back to go");
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
        assert_eq!(pane.doc_ref().map(|v| v.page), Some(41), "past the last page");
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
        assert_eq!(gcode.doc_ref().map(|v| v.page), Some(2), "the layer ran off the top");
        for _ in 0..10 {
            gcode.doc_scroll(-1, now);
        }
        assert_eq!(gcode.doc_ref().map(|v| v.page), Some(0));

        // A page: the arrows pan it, and only as far as it actually overflows.
        let (mut pdf, now) = documented(PreviewKind::Pdf, 2, doc::Counter::Page);
        pdf.set_max_pan(50.0);
        assert!(pdf.doc_scroll(1, now));
        assert_eq!(pdf.doc_ref().map(|v| v.pan), Some(PAN_STEP));
        for _ in 0..10 {
            pdf.doc_scroll(1, now);
        }
        assert_eq!(pdf.doc_ref().map(|v| v.pan), Some(50.0), "it panned off the page");
        pdf.doc_scroll(-100, now);
        assert_eq!(pdf.doc_ref().map(|v| v.pan), Some(0.0));
        // Turning the page starts at the top of it again.
        pdf.doc_scroll(1, now);
        pdf.turn_page(true, now);
        assert_eq!(pdf.doc_ref().map(|v| v.pan), Some(0.0));

        // Anything that is not a document refuses the key, and the caller
        // scrolls text with it instead.
        let mut text = Pane::start(df_core::fs::no_notifier());
        assert!(!text.doc_scroll(1, Instant::now()));
        assert!(!text.turn_page(true, Instant::now()));
    }

    #[test]
    fn zooming_a_page_and_resetting_it_moves_the_pan_with_it() {
        let (mut pane, now) = documented(PreviewKind::Pdf, 2, doc::Counter::Page);
        pane.set_max_pan(100.0);
        pane.zoom(Zoom::In, now);
        let zoomed = pane.doc_ref().map(|v| v.zoom).unwrap_or(0.0);
        assert!(zoomed > 1.0, "got {zoomed}");
        pane.doc_scroll(2, now);
        assert!(pane.doc_ref().map(|v| v.pan).unwrap_or(0.0) > 0.0);
        // `0` is fit, and a fitted page has nothing left to scroll.
        pane.zoom(Zoom::Fit, now);
        assert_eq!(pane.doc_ref().map(|v| v.zoom), Some(doc::ZOOM_MIN));
        assert_eq!(pane.doc_ref().map(|v| v.pan), Some(0.0));
        // `-` at fit is a no-op rather than a page shrinking into the corner.
        pane.zoom(Zoom::Out, now);
        assert_eq!(pane.doc_ref().map(|v| v.zoom), Some(doc::ZOOM_MIN));
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
