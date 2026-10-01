//! Deciding what a file is, and producing something to show for it.
//!
//! PLAN §6's preview pipeline, headless half. Four questions in order, each
//! one a module:
//!
//! ```text
//!   Entry (name hint)  ──┐
//!                        ├──▶ sniff ──▶ real mime ──▶ kind ──▶ PreviewKind
//!   first 8 KiB       ───┘                                        │
//!                                                                 ▼
//!                                                    job ──▶ Preview
//!                                                             │
//!                        cache (yazi handshake) ──────────────┘
//! ```
//!
//! - [`mod@sniff`] — what the bytes say it is, since the name lies often enough to
//!   matter and says nothing at all for `LICENSE` or `deploy`.
//! - [`kind`] — which of PLAN §6's previewers that type gets. A pure function
//!   of an [`crate::fs::Entry`] and a mime, so the whole matrix is a table
//!   test.
//! - [`syntax`] — the language name the text highlighter in df-app wants.
//! - [`job`] — the worker pool that reads the file, debounced and capped, in
//!   the [`crate::fs::Scanner`] pattern.
//! - [`cache`] — the yazi-compatible thumbnail cache the two programs share,
//!   so opening a file in delightviewer from delightfile shows the frame that
//!   was already on screen.
//!
//! ## Where df-core stops
//!
//! It does not decode anything. Images, video, audio, PDFs, fonts, 3D models,
//! G-code, archives and Office files come back as
//! [`job::Preview::NeedsDecode`] carrying the kind, the path, the size the pane
//! wants and any thumbnail the shared cache already holds; df-app puts
//! dv-media, pdfium, ttf-parser and its own readers behind that one variant.
//! The split is PLAN §1's — this crate is tested on a machine with no display
//! and no ffmpeg — and it is the reason the preview *decisions* are
//! provable while the pixels are still a separate problem.
//!
//! What df-core does render itself is everything that is only ever text or
//! metadata: source with a language name attached, markdown source, a
//! directory listing built from the same [`crate::fs::Entry`] the list pane
//! draws, and a hexdump for things with no previewer at all. Those never leave
//! the crate, and they are the previews that must never fail — hence the caps
//! in [`job`], every one of them a named constant with a reason.

pub mod affinity;
pub mod cache;
pub mod job;
pub mod kind;
pub mod sniff;
pub mod syntax;
mod xxh3;

pub use cache::{cache_dir, cache_key, cached_thumb, store_thumb, thumb_path, STILL_SKIP};
pub use job::{
    build, PaneId, Preview, PreviewRequest, PreviewToken, PreviewUpdate, Previewer, TargetSize,
    DEBOUNCE, DIR_ENTRIES, HEX_BYTES, LINE_CHARS, PREVIEW_WORKERS, TEXT_BYTES, TEXT_LINES,
};
pub use kind::{kind_for, kind_for_mime, PreviewKind};
pub use sniff::{
    looks_like_text, sniff, sniff_file, sniff_or_hint, CONTROL_RATIO_LIMIT, MIN_RATIO_SAMPLE,
    SNIFF_BYTES,
};
pub use syntax::{syntax_for, syntax_for_name, syntax_for_shebang};
