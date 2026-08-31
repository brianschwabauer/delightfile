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
//! ## The seam for the next agent
//!
//! Video, audio, PDFs, fonts and models reach [`decode::Job`] with `full:
//! false`: they get their cached thumbnail and a kind badge, and no decoder is
//! called. PLAN §10's "Video/audio playback in pane (dv-playback)" replaces
//! [`Media::badge`] and sets `full` for those kinds; nothing else here has to
//! change.

pub mod decode;
pub mod highlight;
pub mod markdown;
mod paint;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use df_core::fs::{Entry, Notifier, SortOptions};
use df_core::preview::{
    PaneId, Preview, PreviewKind, PreviewToken, PreviewUpdate, Previewer, TargetSize,
};

pub use paint::preview;

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
}

impl Media {
    /// What a kind with no decoder yet is called, in the corner of the pane.
    ///
    /// This is the seam PLAN §10's playback checkbox replaces: today a video
    /// shows its cached frame and the word "video", and tomorrow it plays.
    fn badge(&self) -> Option<&'static str> {
        match self.kind {
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
}

impl Pane {
    /// Start the workers. Called before the window exists, so PLAN §6's
    /// cold-start ordering — decode workers up before the first frame — is
    /// what actually happens.
    pub fn start(notify: Notifier) -> Pane {
        Pane {
            previewer: Previewer::start(std::sync::Arc::clone(&notify)),
            decoder: decode::Decoder::start(notify),
            wanted: None,
            token: None,
            requested_target: (0, 0),
            requested_at: Instant::now(),
            shown: None,
            scroll: 0,
            max_scroll: 0,
            scrolled_at: None,
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
        changed
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
                // Phase 3 decodes pictures. Everything else shows the cached
                // frame yazi left, if there is one, and says what it is — the
                // seam the playback checkbox picks up.
                let full = kind == PreviewKind::Image;
                let store = full && thumb.is_none() && kind.thumbnailable();
                let decoding = full || thumb.is_some();
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
        match (loading, bar) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
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
