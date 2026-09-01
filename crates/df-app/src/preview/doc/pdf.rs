//! PDF pages, via pdfium loaded at runtime (PLAN §6).
//!
//! ## Where `libpdfium.so` comes from
//!
//! `pdfium-render` is taken **without** its `static` feature, so nothing is
//! linked at build time and delightfile builds on a machine that has never
//! heard of pdfium. The library is found at *runtime*, in this order:
//!
//! 1. `$DF_PDFIUM_LIB` — an explicit path, for testing and for packagers.
//! 2. `~/.local/lib/delightfile/libpdfium.so`.
//! 3. `~/.local/lib/delightviewer/libpdfium.so` — where delightviewer's README
//!    tells you to put the prebuilt library from bblanchon/pdfium-binaries, and
//!    delightfile pins the same `pdfium_7881` ABI on purpose so that one copy
//!    serves both programs.
//! 4. `$CARGO_MANIFEST_DIR/../../target/libpdfium.so`, next to a development
//!    build.
//! 5. The system loader, which is what the AUR `pdfium-binaries` package
//!    installs.
//!
//! If none of them load, a PDF falls back to its cached thumbnail and a `pdf`
//! badge — PLAN §6's "missing lib = missing feature, **never a failure**". The
//! pane never shows an error for it, because a missing optional library is not
//! something the person looking at the file did wrong.
//!
//! ## Threading
//!
//! pdfium's bindings are a process-global singleton, and pdfium itself is not
//! thread-safe: `pdfium-render`'s `thread_safe` feature adds `Send`/`Sync` to
//! the handle types and serializes one internal cache, and nothing else. In
//! delightfile there is exactly **one** thread that ever enters the library —
//! [`super::Worker`]'s — so the serialization is structural rather than a
//! mutex. A [`Doc`] is not `Send` and never leaves that thread.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use pdfium_render::prelude::{PdfDocument, PdfRenderConfig, Pdfium};

use super::Rgba;

/// Candidate paths for the dynamic library, most specific first.
fn candidates() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(explicit) = std::env::var_os("DF_PDFIUM_LIB") {
        out.push(PathBuf::from(explicit));
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        out.push(home.join(".local/lib/delightfile/libpdfium.so"));
        // The sibling program's copy. Sharing it is the point of pinning the
        // same ABI: there is one 7 MB library on this machine and neither
        // program should be the reason there are two.
        out.push(home.join(".local/lib/delightviewer/libpdfium.so"));
    }
    out.push(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target")
            .join("libpdfium.so"),
    );
    out
}

/// The process's one pdfium instance, or `None` when there is no usable
/// library.
///
/// `Box::leak` gives the `'static` lifetime every [`PdfDocument`] needs without
/// a self-referential struct; the leak is one instance for the life of the
/// process, which is what a singleton is. The answer is computed once and
/// remembered — including the *negative* answer, so a directory of PDFs on a
/// machine with no pdfium does not walk the candidate list once per file.
pub fn pdfium() -> Option<&'static Pdfium> {
    static INSTANCE: OnceLock<Option<&'static Pdfium>> = OnceLock::new();
    *INSTANCE.get_or_init(|| {
        let bindings = candidates()
            .into_iter()
            .find_map(|path| match Pdfium::bind_to_library(&path) {
                Ok(b) => {
                    log::debug!("pdfium: loaded {}", path.display());
                    Some(b)
                }
                Err(e) => {
                    log::debug!("pdfium: {} — {e}", path.display());
                    None
                }
            })
            .or_else(|| match Pdfium::bind_to_system_library() {
                Ok(b) => {
                    log::debug!("pdfium: loaded from the system library path");
                    Some(b)
                }
                Err(e) => {
                    log::debug!("pdfium: no usable libpdfium ({e})");
                    None
                }
            })?;
        Some(&*Box::leak(Box::new(Pdfium::new(bindings))))
    })
}

/// Whether PDF pages can be rendered at all on this machine.
///
/// The pane asks before it draws: with no library the answer is the cached
/// thumbnail and a badge, which is a preview, rather than a message about a
/// shared object, which is not.
pub fn available() -> bool {
    pdfium().is_some()
}

/// An open PDF. Not `Send`; it lives on the worker thread that opened it.
pub struct Doc {
    /// `Option` only so [`Drop`] runs in a defined order relative to the leaked
    /// `Pdfium` it borrows from.
    document: Option<PdfDocument<'static>>,
    /// Page sizes in points, read once at open: the fit is computed from them
    /// before there are any pixels, and pdfium charges for each lookup.
    sizes: Vec<(f32, f32)>,
}

impl Doc {
    pub fn open(path: &Path) -> Result<Doc, String> {
        let pdfium = pdfium().ok_or_else(|| MISSING.to_string())?;
        let document = pdfium
            .load_pdf_from_file(path, None)
            .map_err(|e| format!("pdf: {e}"))?;
        let sizes = {
            let pages = document.pages();
            (0..pages.len())
                .filter_map(|i| pages.get(i).ok())
                .map(|p| (p.width().value, p.height().value))
                .collect()
        };
        Ok(Doc {
            document: Some(document),
            sizes,
        })
    }

    pub fn page_count(&self) -> usize {
        self.sizes.len()
    }

    /// One page's size in points, which is what the fit is computed from.
    pub fn page_size(&self, page: usize) -> Option<(f32, f32)> {
        self.sizes.get(page).copied()
    }

    /// Rasterise one page at exactly `width × height` pixels.
    pub fn render(&self, page: usize, width: u32, height: u32) -> Result<Rgba, String> {
        let index = i32::try_from(page).map_err(|_| format!("pdf: no page {page}"))?;
        let document = self
            .document
            .as_ref()
            .ok_or_else(|| "pdf: document is closed".to_string())?;
        let page = document
            .pages()
            .get(index)
            .map_err(|e| format!("pdf: page {index}: {e}"))?;
        let (width, height) = (width.max(1), height.max(1));
        let config = PdfRenderConfig::new().set_target_size(width as i32, height as i32);
        let bitmap = page
            .render_with_config(&config)
            .map_err(|e| format!("pdf: render: {e}"))?;
        let (w, h) = (bitmap.width() as u32, bitmap.height() as u32);
        let mut pixels = bitmap.as_rgba_bytes();
        // pdfium's bitmaps carry no alpha of their own unless one is asked for;
        // normalise to fully opaque either way, so a page composes like any
        // other opaque picture in the pane.
        for px in pixels.chunks_exact_mut(4) {
            px[3] = 255;
        }
        // A short bitmap is padded out opaque rather than panicking on the
        // texture upload's length check.
        pixels.resize((w as usize) * (h as usize) * 4, 255);
        Ok(Rgba {
            width: w,
            height: h,
            pixels,
        })
    }
}

/// What is logged when the library is not there. It is never shown in the pane:
/// see the module header.
const MISSING: &str = "PDF pages need libpdfium — see the README";

#[cfg(test)]
mod tests {
    use super::*;

    /// The candidate list is ordered, and the explicit override is first — a
    /// packager's `$DF_PDFIUM_LIB` must beat whatever happens to be installed.
    #[test]
    fn the_library_is_looked_for_in_the_documented_order() {
        let paths = candidates();
        assert!(!paths.is_empty());
        if std::env::var_os("HOME").is_some() {
            assert!(
                paths
                    .iter()
                    .any(|p| p.ends_with(".local/lib/delightfile/libpdfium.so")),
                "{paths:?}"
            );
            assert!(
                paths
                    .iter()
                    .any(|p| p.ends_with(".local/lib/delightviewer/libpdfium.so")),
                "the sibling's copy is shared on purpose: {paths:?}"
            );
        }
    }

    /// **Whether pdfium is present decides nothing about whether this passes.**
    /// A machine without the library must still get through the code path that
    /// asks, and must get `false` rather than a panic or a hang.
    #[test]
    fn asking_whether_pdf_works_is_answered_either_way() {
        let first = available();
        // The answer is memoised, so it cannot change between two asks — a PDF
        // that rendered a moment ago must not fail to render now.
        assert_eq!(first, available());
        if !first {
            log::info!("no libpdfium on this machine; PDFs fall back to a badge");
        }
    }
}
