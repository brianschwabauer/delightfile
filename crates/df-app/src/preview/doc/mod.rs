//! The document previewers: PDF pages, font specimens, 3D models and G-code
//! (PLAN §6).
//!
//! These are the four kinds that were left standing at [`crate::preview`]'s
//! seam after images and playback took theirs. They have almost nothing in
//! common as file formats and exactly one thing in common as *previews*: none
//! of them is a picture until somebody makes one. A PDF page is a program, a
//! font is a promise about how text will look, a mesh is a list of triangles
//! and a G-code file is a list of moves. So all four go through the same
//! contract the image decoder already established — **a worker thread hands
//! back pixels** — and the pane draws them with the code that was already
//! there.
//!
//! ```text
//!   Pane::sync_doc ──▶ Worker  ─┬─ pdf   (pdfium, dlopen'd)
//!        ▲                      ├─ font  (ttf-parser + a scanline fill)
//!        │                      ├─ model (stl/obj/ply/3mf + a z-buffer)
//!        └── Update ◀───────────┴─ gcode (a toolpath + a top view)
//! ```
//!
//! ## One thread, one open document
//!
//! There is one preview pane, so there is one open document, and the worker
//! keeps it: arrowing between pages of a PDF must not re-parse the PDF. That is
//! also what makes pdfium safe here without a mutex — the library is entered
//! from this thread and no other (see [`pdf`]).
//!
//! ## Newest wins, again
//!
//! The same rule as [`df_core::preview::Previewer`] and
//! [`crate::preview::decode`]: a held `→` through a 400-page PDF asks for
//! forty renders and wants one. The live token is an `AtomicU64`, checked
//! before the open and again before the send, and the job queue is coalesced to
//! its last entry before any work starts.
//!
//! ## Rendering never blocks a frame
//!
//! Every rasteriser below is a pure function from a parsed document to an
//! [`Rgba`] buffer. Nothing here touches egui, and nothing here reads the
//! palette — the four colours a rasteriser is allowed are handed to it in an
//! [`Ink`], because a worker thread cannot reach the theme.

pub mod font;
pub mod gcode;
pub mod model;
pub mod pdf;

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crossbeam_channel::{unbounded, Receiver, Sender};
use df_core::fs::Notifier;
use df_core::preview::{PreviewKind, PreviewToken};

pub use crate::preview::decode::Rgba;

/// The palette colours a worker-thread rasteriser is given, because it cannot
/// reach the egui palette from off the paint thread.
///
/// Four is deliberately few. A specimen sheet, a mesh and a toolpath are three
/// unrelated pictures, and the thing that makes them look like one program is
/// that all three are drawn out of the same tiny set: a ground, a foreground, a
/// quiet tone for what is context rather than content, and one accent for the
/// thing being pointed at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Ink {
    pub bg: [u8; 4],
    pub fg: [u8; 4],
    pub dim: [u8; 4],
    pub accent: [u8; 4],
}

impl Ink {
    /// The pane's own colours, as bytes.
    pub fn from_palette(palette: &crate::theme::Palette) -> Ink {
        let rgba = |c: egui::Color32| c.to_array();
        Ink {
            // `mantle` is the preview pane's ground, so a rendered page sits on
            // the pane rather than on a rectangle of some other grey.
            bg: rgba(palette.mantle),
            fg: rgba(palette.text),
            dim: rgba(palette.overlay0),
            accent: rgba(palette.blue),
        }
    }

    /// A fixture with four values that are obviously distinguishable, so a test
    /// can assert "something was drawn" without knowing the theme.
    pub fn test() -> Ink {
        Ink {
            bg: [24, 24, 37, 255],
            fg: [205, 214, 244, 255],
            dim: [108, 112, 134, 255],
            accent: [137, 180, 250, 255],
        }
    }
}

/// What the pane wants to see: which page, how big, and at what angle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct View {
    /// The page, or the G-code layer. Zero-based; the chip adds the one.
    pub page: usize,
    /// The pane's size in physical pixels.
    pub target: (u32, u32),
    /// 1.0 is fit-to-pane. Only the PDF reads it.
    pub zoom: f32,
    /// The turntable's angle in radians. Only the model reads it.
    pub yaw: f32,
}

/// How the page indicator reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Counter {
    /// "3 / 42".
    Page,
    /// "Layer 3 / 312".
    Layer,
    /// A single-page document — a specimen sheet, a mesh — counts nothing.
    None,
}

/// What was learned when the document was opened.
#[derive(Debug, Clone, PartialEq)]
pub struct Meta {
    /// Pages, or layers. At least one for anything that opened at all.
    pub pages: usize,
    /// The chip's line: "312 layers · 0.20 mm", "Inter · Regular · 2,548 glyphs".
    pub summary: String,
    pub counter: Counter,
}

/// One answer from the worker.
///
/// Not `Debug`: it carries a decoded page, and a derived formatter on a
/// megabyte of pixels is a debug line nobody wants and a footgun in a log.
pub enum Payload {
    Meta(Meta),
    Page {
        view: View,
        image: Rgba,
    },
    /// The reader this kind needs is not on this machine. Not an error: the
    /// pane falls back to the cached thumbnail and a badge (PLAN §6).
    Unavailable,
}

/// One finished piece of work, checked against the live token by the pane.
pub struct Update {
    pub token: PreviewToken,
    pub path: PathBuf,
    pub result: Result<Payload, String>,
}

/// What the pane asks for.
#[derive(Debug, Clone)]
pub struct Job {
    pub token: PreviewToken,
    pub path: PathBuf,
    pub kind: PreviewKind,
    pub view: View,
    pub ink: Ink,
}

// ── Zoom ────────────────────────────────────────────────────────────────────

/// One press of `+` or `-`, as a ratio.
///
/// The square root of two: two presses double, which is the step size every
/// document viewer on the machine uses and the one a hand already knows. A flat
/// 2× per press overshoots on the first tap of a page that only needed a
/// closer look at a footnote.
pub const ZOOM_STEP: f32 = std::f32::consts::SQRT_2;

/// The smallest `-` will go. Below fit there is nothing to see that fit does
/// not already show, so the floor is fit itself.
pub const ZOOM_MIN: f32 = 1.0;

/// The largest `+` will go: eight times fit, which on a typical pane is about
/// 600 dpi against the page's own 72 — past that pdfium is drawing pixels no
/// screen has and the render cost stops being worth the answer.
pub const ZOOM_MAX: f32 = 8.0;

/// The most pixels one rendered page may cost, as a side length.
///
/// A pane 1200 px wide at zoom 8 asks for 9600, and a page that tall is 350 MB
/// of RGBA before it is a texture — past most GPUs' limit and well past what a
/// preview is worth. The zoom is honoured up to here and clamped after, so `+`
/// keeps working and simply stops getting sharper.
pub const MAX_PAGE_SIDE: u32 = 4096;

/// `+`: the next step up, clamped.
pub fn zoom_in(zoom: f32) -> f32 {
    (zoom * ZOOM_STEP).min(ZOOM_MAX)
}

/// `-`: the next step down, clamped. Never below fit.
pub fn zoom_out(zoom: f32) -> f32 {
    (zoom / ZOOM_STEP).max(ZOOM_MIN)
}

/// How many pixels a page of `page` points should be rasterised at, to fill
/// `target` physical pixels at `zoom`.
///
/// Pure, because this is the arithmetic that decides how much memory a page
/// costs and how sharp it looks, and both are the sort of thing that is wrong
/// on a second monitor for a year before anybody notices.
pub fn page_pixels(page: (f32, f32), target: (u32, u32), zoom: f32) -> (u32, u32) {
    let (tw, th) = (target.0.max(1) as f32, target.1.max(1) as f32);
    let (pw, ph) = (page.0, page.1);
    if !(pw.is_finite() && ph.is_finite()) || pw <= 0.0 || ph <= 0.0 {
        return (1, 1);
    }
    let zoom = if zoom.is_finite() {
        zoom.clamp(ZOOM_MIN, ZOOM_MAX)
    } else {
        ZOOM_MIN
    };
    let fit = (tw / pw).min(th / ph);
    let scale = fit * zoom;
    let w = (pw * scale).round().max(1.0);
    let h = (ph * scale).round().max(1.0);
    // Clamp the *longest* side, so an A0 poster and a business card are both
    // capped without either changing shape.
    let over = (w.max(h) / MAX_PAGE_SIDE as f32).max(1.0);
    (
        ((w / over).round() as u32).max(1),
        ((h / over).round() as u32).max(1),
    )
}

// ── The page cache ──────────────────────────────────────────────────────────

/// How many rendered pages are kept.
///
/// Four: the page you are on, the one you just came from, and one either side —
/// which is exactly the working set of `←` `→` `←` through a document, the way
/// a page is actually read. Past that a cache is holding megabytes to answer a
/// question nobody is asking, and pdfium re-renders a page in single-digit
/// milliseconds anyway.
pub const PAGE_CACHE: usize = 4;

/// What identifies a rendered page. The size is part of it because a page
/// rendered for a narrower pane is the wrong pixels for a wider one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key {
    pub page: usize,
    pub width: u32,
    pub height: u32,
}

/// A tiny least-recently-used cache.
///
/// A `Vec` rather than a map: at [`PAGE_CACHE`] entries a linear scan is faster
/// than a hash, and the order *is* the recency, which makes eviction a `remove`
/// rather than a second structure to keep in step.
#[derive(Debug)]
pub struct PageCache<V> {
    /// Oldest first; the back is the most recently touched.
    entries: Vec<(Key, V)>,
    limit: usize,
}

impl<V> PageCache<V> {
    pub fn new(limit: usize) -> PageCache<V> {
        PageCache {
            entries: Vec::new(),
            limit: limit.max(1),
        }
    }

    /// Look one up, and mark it as the most recent.
    pub fn get(&mut self, key: &Key) -> Option<&V> {
        let at = self.entries.iter().position(|(k, _)| k == key)?;
        let entry = self.entries.remove(at);
        self.entries.push(entry);
        self.entries.last().map(|(_, v)| v)
    }

    /// Insert one, evicting the least recently used if that overflows.
    pub fn insert(&mut self, key: Key, value: V) {
        self.entries.retain(|(k, _)| *k != key);
        self.entries.push((key, value));
        while self.entries.len() > self.limit {
            self.entries.remove(0);
        }
    }

    /// Everything goes: a different document is being looked at.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// How many pages are held. Only the tests ask; the worker never needs to
    /// know, because the eviction is the cache's own business.
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }
}

// ── The worker ──────────────────────────────────────────────────────────────

/// One open document, whichever kind it turned out to be.
enum Doc {
    Pdf(pdf::Doc),
    /// The file's bytes, kept so the specimen can be re-set at a new size.
    Font(Vec<u8>, font::Facts),
    Model(model::Mesh),
    Gcode(gcode::Toolpath),
}

/// The document worker.
///
/// Dropping it closes the channel; the worker finishes the job it is on and
/// exits, and the drop joins it so no thread outlives the window — and so the
/// open PDF is closed before the process tears down the library it came from.
pub struct Docs {
    jobs: Option<Sender<Job>>,
    results: Receiver<Update>,
    live: Arc<AtomicU64>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl Docs {
    pub fn start(notify: Notifier) -> Docs {
        let (job_tx, job_rx) = unbounded::<Job>();
        let (res_tx, res_rx) = unbounded::<Update>();
        let live = Arc::new(AtomicU64::new(0));
        let worker_live = Arc::clone(&live);
        let handle = std::thread::Builder::new()
            .name("df-doc".to_string())
            .spawn(move || {
                // Rasterising a PDF page is tens of milliseconds; it gives way
                // to the paint thread and to nothing else (`df_core::thread`).
                df_core::thread::lower_priority(df_core::thread::NICE_INTERACTIVE);
                run(&job_rx, &res_tx, &worker_live, &notify)
            });
        let worker = match handle {
            Ok(h) => Some(h),
            // A thread that will not spawn costs the document previews and
            // nothing else, exactly as the decode worker's failure costs the
            // pictures and nothing else.
            Err(e) => {
                log::warn!("the document worker did not start: {e}");
                None
            }
        };
        Docs {
            jobs: Some(job_tx),
            results: res_rx,
            live,
            worker,
        }
    }

    /// Queue a render, retiring whatever was in flight.
    pub fn request(&self, job: Job) {
        self.live.store(job.token.0, Ordering::Relaxed);
        if let Some(jobs) = &self.jobs {
            if jobs.send(job).is_err() {
                log::debug!("the document worker is gone");
            }
        }
    }

    /// Stop caring about whatever is in flight.
    pub fn cancel(&self) {
        self.live.store(0, Ordering::Relaxed);
    }

    pub fn drain(&self) -> Vec<Update> {
        self.results.try_iter().collect()
    }
}

impl Drop for Docs {
    fn drop(&mut self) {
        self.cancel();
        self.jobs = None;
        if let Some(handle) = self.worker.take() {
            let _ = handle.join();
        }
    }
}

fn live(state: &AtomicU64, token: PreviewToken) -> bool {
    state.load(Ordering::Relaxed) == token.0
}

/// The worker's whole life: open what is asked for, keep it open, render from
/// it.
fn run(jobs: &Receiver<Job>, out: &Sender<Update>, state: &AtomicU64, notify: &Notifier) {
    let mut open: Option<(PathBuf, Doc)> = None;
    let mut cache: PageCache<Rgba> = PageCache::new(PAGE_CACHE);
    for job in jobs {
        // **Coalesce.** Everything already queued behind this job supersedes
        // it, so only the last one is worth doing — a held `→` or a turntable
        // that outran the renderer would otherwise render every frame it asked
        // for after the pane had stopped wanting them.
        let job = jobs.try_iter().last().unwrap_or(job);
        one(job, &mut open, &mut cache, out, state, notify);
    }
}

fn one(
    job: Job,
    open: &mut Option<(PathBuf, Doc)>,
    cache: &mut PageCache<Rgba>,
    out: &Sender<Update>,
    state: &AtomicU64,
    notify: &Notifier,
) {
    let Job {
        token,
        path,
        kind,
        view,
        ink,
    } = job;
    if !live(state, token) {
        return;
    }
    let send = |result| {
        if live(state, token)
            && out
                .send(Update {
                    token,
                    path: path.clone(),
                    result,
                })
                .is_ok()
        {
            notify();
            true
        } else {
            false
        }
    };

    // Open, unless this is the document that is already open.
    if open.as_ref().map(|(p, _)| p.as_path()) != Some(path.as_path()) {
        cache.clear();
        // Dropped *before* the new one is opened: two PDFs open at once is two
        // documents' worth of pdfium state for no reason.
        *open = None;
        match load(&path, kind) {
            Ok(None) => {
                send(Ok(Payload::Unavailable));
                return;
            }
            Ok(Some(doc)) => {
                let meta = meta_for(&doc);
                if !send(Ok(Payload::Meta(meta))) {
                    return;
                }
                *open = Some((path.clone(), doc));
            }
            Err(e) => {
                send(Err(e));
                return;
            }
        }
    }
    let Some((_, doc)) = open.as_ref() else {
        return;
    };
    if !live(state, token) {
        return;
    }

    match render(doc, &view, &ink, cache) {
        Ok((view, image)) => {
            send(Ok(Payload::Page { view, image }));
        }
        Err(e) => {
            send(Err(e));
        }
    }
}

/// Open one file. `Ok(None)` is "the reader is missing", which is not an error.
fn load(path: &std::path::Path, kind: PreviewKind) -> Result<Option<Doc>, String> {
    match kind {
        PreviewKind::Pdf => {
            if !pdf::available() {
                return Ok(None);
            }
            pdf::Doc::open(path).map(|d| Some(Doc::Pdf(d)))
        }
        PreviewKind::Font => {
            let bytes = read_capped(path, MAX_FONT_BYTES)?;
            // df-core routed here on the *mime*, which for fonts is often a
            // guess from the extension. Checking the magic here means a `.ttf`
            // that is really a zip says so, rather than producing a specimen of
            // nothing (PLAN §6: the preview is never a lie about the file).
            if !font::is_font(&bytes) {
                return Err("not a font file".to_string());
            }
            let facts = font::facts(&bytes)?;
            Ok(Some(Doc::Font(bytes, facts)))
        }
        PreviewKind::Model3d => {
            let bytes = read_capped(path, MAX_MODEL_BYTES)?;
            model::parse(&bytes, path).map(|m| Some(Doc::Model(m)))
        }
        PreviewKind::Gcode => {
            let bytes = read_capped(path, MAX_GCODE_BYTES)?;
            // Same reason as the font above: `text/x.gcode` is a name-based
            // guess, and a text file with a `.gcode` extension and no moves in
            // it should be read as text rather than drawn as an empty bed.
            if !gcode::sniff(&bytes) {
                return Err("no toolpath in this file".to_string());
            }
            let text = String::from_utf8_lossy(&bytes);
            Ok(Some(Doc::Gcode(gcode::parse(&text))))
        }
        // Nothing else reaches this worker; the pane routes by kind.
        _ => Ok(None),
    }
}

/// The largest font file that will be opened: 64 MiB.
///
/// A CJK collection is a few tens of megabytes and everything else is under
/// one. Past this, a file with a font's magic bytes is more likely to be
/// something else wearing them.
const MAX_FONT_BYTES: u64 = 64 * 1024 * 1024;

/// The largest mesh that will be opened: 256 MiB.
///
/// A binary STL is 50 bytes a triangle, so this is about five million
/// triangles — an order of magnitude past what a preview pane can show and
/// still well inside what a machine can read without noticing.
const MAX_MODEL_BYTES: u64 = 256 * 1024 * 1024;

/// The largest G-code file that will be parsed: 512 MiB.
///
/// A big multi-day print is a few hundred megabytes of ASCII, and this is a
/// text file being read line by line, which is the cheapest thing here.
const MAX_GCODE_BYTES: u64 = 512 * 1024 * 1024;

fn read_capped(path: &std::path::Path, cap: u64) -> Result<Vec<u8>, String> {
    let meta = std::fs::metadata(path).map_err(|e| e.to_string())?;
    if meta.len() > cap {
        return Err(format!(
            "{} is too large to preview",
            crate::format::human_size(meta.len())
        ));
    }
    std::fs::read(path).map_err(|e| e.to_string())
}

fn meta_for(doc: &Doc) -> Meta {
    match doc {
        Doc::Pdf(pdf) => Meta {
            pages: pdf.page_count().max(1),
            summary: match pdf.page_count() {
                1 => "1 page".to_string(),
                n => format!("{n} pages"),
            },
            counter: Counter::Page,
        },
        Doc::Font(_, facts) => Meta {
            pages: 1,
            summary: font::summary(facts),
            counter: Counter::None,
        },
        // The mesh is in millimetres whatever the file said; what the unit
        // note adds is *how that was known* — 3MF declares it and the other
        // three formats are unitless numbers the printing world reads as mm.
        Doc::Model(mesh) => Meta {
            pages: 1,
            summary: match mesh.units {
                model::Units::Declared(unit) => format!("{} · {unit}", model::summary(mesh)),
                model::Units::AssumedMillimetres => model::summary(mesh),
            },
            counter: Counter::None,
        },
        Doc::Gcode(toolpath) => Meta {
            pages: toolpath.layer_count().max(1),
            summary: match toolpath.facts.filament_field() {
                Some(filament) => format!("{} · {filament}", gcode::summary(toolpath)),
                None => gcode::summary(toolpath),
            },
            counter: Counter::Layer,
        },
    }
}

/// Rasterise one view. Returns the view that was *actually* drawn, which may
/// have a clamped page in it.
fn render(
    doc: &Doc,
    view: &View,
    ink: &Ink,
    cache: &mut PageCache<Rgba>,
) -> Result<(View, Rgba), String> {
    let target = (view.target.0.max(1), view.target.1.max(1));
    match doc {
        Doc::Pdf(pdf) => {
            let page = view.page.min(pdf.page_count().saturating_sub(1));
            let size = pdf.page_size(page).unwrap_or((612.0, 792.0));
            let (w, h) = page_pixels(size, target, view.zoom);
            let key = Key {
                page,
                width: w,
                height: h,
            };
            let drawn = View {
                page,
                target,
                ..*view
            };
            if let Some(hit) = cache.get(&key) {
                return Ok((drawn, clone_rgba(hit)));
            }
            let image = pdf.render(page, w, h)?;
            cache.insert(key, clone_rgba(&image));
            Ok((drawn, image))
        }
        Doc::Font(bytes, facts) => {
            let image = font::specimen(bytes, facts, target.0, target.1, ink)?;
            Ok((
                View {
                    page: 0,
                    target,
                    ..*view
                },
                image,
            ))
        }
        // **Not cached.** The turntable asks for a different angle every frame,
        // so a cache of them would be a cache that never hits and evicts the
        // one thing that would have.
        Doc::Model(mesh) => Ok((
            View {
                page: 0,
                target,
                ..*view
            },
            model::render(mesh, target.0, target.1, view.yaw, ink),
        )),
        Doc::Gcode(toolpath) => {
            let layer = view.page.min(toolpath.layer_count().saturating_sub(1));
            let key = Key {
                page: layer,
                width: target.0,
                height: target.1,
            };
            let drawn = View {
                page: layer,
                target,
                ..*view
            };
            if let Some(hit) = cache.get(&key) {
                return Ok((drawn, clone_rgba(hit)));
            }
            let image = gcode::render(toolpath, layer, target.0, target.1, ink);
            cache.insert(key, clone_rgba(&image));
            Ok((drawn, image))
        }
    }
}

/// [`Rgba`] is a plain buffer and deliberately not `Clone` — nothing else in
/// the preview ever wants a second copy of a decoded picture. The cache does,
/// once per hit, and this is that one place.
fn clone_rgba(image: &Rgba) -> Rgba {
    Rgba {
        width: image.width,
        height: image.height,
        pixels: image.pixels.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zooming_steps_by_a_ratio_and_stops_at_both_ends() {
        // Two presses double, which is the step every document viewer uses.
        let twice = zoom_in(zoom_in(1.0));
        assert!((twice - 2.0).abs() < 1e-4, "got {twice}");
        // `-` never goes below fit: there is nothing under fit worth seeing.
        assert_eq!(zoom_out(1.0), ZOOM_MIN);
        assert_eq!(zoom_out(0.1), ZOOM_MIN);
        // …and `+` stops rather than running away.
        let mut zoom = 1.0;
        for _ in 0..40 {
            zoom = zoom_in(zoom);
        }
        assert_eq!(zoom, ZOOM_MAX);
        // The two are inverses in the middle of the range, so `+ -` is a
        // round trip and not a slow drift.
        let there_and_back = zoom_out(zoom_in(2.0));
        assert!((there_and_back - 2.0).abs() < 1e-4, "got {there_and_back}");
    }

    #[test]
    fn a_page_is_fitted_to_the_pane_then_scaled_by_the_zoom() {
        // US Letter at 72 dpi in a 400×600 pane: the width binds, and the
        // aspect ratio survives.
        let (w, h) = page_pixels((612.0, 792.0), (400, 600), 1.0);
        assert_eq!(w, 400);
        assert!((h as f32 - 400.0 * 792.0 / 612.0).abs() < 1.5, "got {h}");
        // Zoom multiplies both sides.
        let (w2, h2) = page_pixels((612.0, 792.0), (400, 600), 2.0);
        assert_eq!(w2, 800);
        assert!((h2 as f32 / h as f32 - 2.0).abs() < 0.01);
        // A landscape page in the same pane is bound by the height.
        let (w3, h3) = page_pixels((792.0, 612.0), (400, 600), 1.0);
        assert_eq!(w3, 400, "the wide page still fits the width first");
        assert!(h3 < 400);
    }

    #[test]
    fn a_page_never_costs_more_than_the_cap() {
        let (w, h) = page_pixels((612.0, 792.0), (2000, 3000), ZOOM_MAX);
        assert!(w <= MAX_PAGE_SIDE && h <= MAX_PAGE_SIDE, "got {w}×{h}");
        // …and it is clamped by *scaling*, not by cropping: the shape is kept.
        let ratio = h as f32 / w as f32;
        assert!((ratio - 792.0 / 612.0).abs() < 0.01, "got {ratio}");
    }

    #[test]
    fn page_sizing_survives_nonsense() {
        for page in [(0.0, 0.0), (f32::NAN, 100.0), (-5.0, 10.0)] {
            let (w, h) = page_pixels(page, (400, 600), 1.0);
            assert!(w >= 1 && h >= 1, "{page:?} gave {w}×{h}");
        }
        let (w, h) = page_pixels((612.0, 792.0), (0, 0), f32::NAN);
        assert!(w >= 1 && h >= 1, "got {w}×{h}");
    }

    fn key(page: usize) -> Key {
        Key {
            page,
            width: 400,
            height: 600,
        }
    }

    /// The cache holds exactly [`PAGE_CACHE`] pages and drops the least
    /// recently *used* one — not the least recently inserted, which is the bug
    /// that makes a cache useless for `← → ←`.
    #[test]
    fn the_page_cache_evicts_the_least_recently_used() {
        let mut cache: PageCache<usize> = PageCache::new(PAGE_CACHE);
        for page in 0..PAGE_CACHE {
            cache.insert(key(page), page);
        }
        assert_eq!(cache.len(), PAGE_CACHE);
        // Touch page 0, so page 1 becomes the oldest.
        assert_eq!(cache.get(&key(0)), Some(&0));
        cache.insert(key(PAGE_CACHE), PAGE_CACHE);
        assert_eq!(cache.len(), PAGE_CACHE, "the cache grew past its limit");
        assert_eq!(cache.get(&key(0)), Some(&0), "the touched page was evicted");
        assert_eq!(cache.get(&key(1)), None, "the oldest page survived");
        assert_eq!(cache.get(&key(PAGE_CACHE)), Some(&PAGE_CACHE));
    }

    /// A page re-rendered at a new size replaces the old one rather than
    /// living beside it, and the same page at two sizes is two entries.
    #[test]
    fn the_page_cache_keys_on_the_size_as_well_as_the_page() {
        let mut cache: PageCache<usize> = PageCache::new(PAGE_CACHE);
        cache.insert(key(3), 1);
        cache.insert(key(3), 2);
        assert_eq!(cache.len(), 1, "the same key was stored twice");
        assert_eq!(cache.get(&key(3)), Some(&2));
        cache.insert(
            Key {
                page: 3,
                width: 800,
                height: 1200,
            },
            3,
        );
        assert_eq!(cache.len(), 2);
        // A new document empties it: pages of the file you left are pixels of
        // a file nobody is looking at.
        cache.clear();
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.get(&key(3)), None);
    }

    /// A limit of zero would divide the world by zero; it is floored at one.
    #[test]
    fn a_cache_always_holds_at_least_one_page() {
        let mut cache: PageCache<usize> = PageCache::new(0);
        cache.insert(key(0), 0);
        cache.insert(key(1), 1);
        assert_eq!(cache.len(), 1);
        assert_eq!(cache.get(&key(1)), Some(&1));
    }
}
