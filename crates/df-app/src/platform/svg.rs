//! An SVG as a picture, on macOS and Windows: resvg
//! (`plans/other-platforms/04-windows.md` W4.44).
//!
//! **Why here and not on Linux.** The preview decodes a picture the `image`
//! crate cannot read through FFmpeg ([`crate::preview::decode`]), and Arch's
//! FFmpeg reads an SVG through librsvg. The FFmpeg the Windows zip carries
//! (BtbN's GPL shared build, `build/windows/ffmpeg.lock`) is configured
//! without librsvg — its `--enable-*` list has no `librsvg` — and so is the
//! one the macOS app carries, so there an SVG came back "no decoder". This
//! module answers first on those two; on Linux `platform::svg::render` says
//! nothing and FFmpeg answers as it always did.
//!
//! **What it draws.** The whole document at the size that fits the pane, a
//! vector being as sharp at any size: unlike a photograph, a 24-point icon is
//! drawn as large as the pane rather than left at 24 pixels. Words in the
//! drawing are set in the system's faces, read from the folders a Nerd Font
//! is looked for in ([`crate::platform::fonts::dirs`]) once a process, and
//! only for a drawing that has words in it: most SVGs are icons, which have
//! none, and reading the Fonts folder for one would cost a picture's worth of
//! time for nothing. The faces are memory-mapped (`memmap-fonts`), so only
//! the names of each are read until a word is drawn in it.
//!
//! Embedded raster images, `.svgz` and fontconfig are left out of resvg's
//! features: the first two are rare in a file somebody previews, and the
//! third is Linux's, which does not build this.

use std::sync::{Arc, OnceLock};

use resvg::tiny_skia;
use resvg::usvg;

/// The largest side drawn, whatever the pane: a bound on the pixels one
/// preview may cost, as the decoders' own are.
const MOST: u32 = 8192;

/// `bytes` drawn as a picture that fits `target` (physical pixels), or
/// `None` when they are not an SVG resvg can read.
pub fn render(bytes: &[u8], target: (u32, u32)) -> Option<image::RgbaImage> {
    if !looks_like_svg(bytes) {
        return None;
    }
    let mut options = usvg::Options::default();
    if has_words(bytes) {
        options.fontdb = fonts();
    }
    let tree = match usvg::Tree::from_data(bytes, &options) {
        Ok(tree) => tree,
        Err(e) => {
            log::debug!("svg: {e}");
            return None;
        }
    };
    let size = tree.size();
    let (width, height) = fitted(size.width(), size.height(), target);
    let mut pixmap = tiny_skia::Pixmap::new(width, height)?;
    let transform = tiny_skia::Transform::from_scale(
        width as f32 / size.width(),
        height as f32 / size.height(),
    );
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    // tiny-skia's pixels are premultiplied; a picture here is straight
    // alpha, which is what egui is handed.
    let straight: Vec<u8> = pixmap
        .pixels()
        .iter()
        .flat_map(|pixel| {
            let color = pixel.demultiply();
            [color.red(), color.green(), color.blue(), color.alpha()]
        })
        .collect();
    image::RgbaImage::from_raw(width, height, straight)
}

/// Whether `bytes` begin as an SVG document does: markup — after a byte
/// order mark and white space — with an `<svg` element in its first
/// kilobytes. Cheap, so a photograph the `image` crate turned away is not
/// handed to an XML parser on its way to FFmpeg.
fn looks_like_svg(bytes: &[u8]) -> bool {
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let start = bytes.iter().position(|b| !b.is_ascii_whitespace());
    if start.is_none_or(|at| bytes[at] != b'<') {
        return false;
    }
    let head = &bytes[..bytes.len().min(64 * 1024)];
    head.windows(4).any(|window| window == b"<svg")
}

/// Whether the drawing has words in it, which need the system's faces.
fn has_words(bytes: &[u8]) -> bool {
    bytes.windows(5).any(|window| window == b"<text")
}

/// The size the drawing is drawn at: its own aspect, as large as fits
/// `target`, at least a pixel and at most [`MOST`] a side.
fn fitted(width: f32, height: f32, target: (u32, u32)) -> (u32, u32) {
    let (tw, th) = (
        target.0.clamp(1, MOST) as f32,
        target.1.clamp(1, MOST) as f32,
    );
    if !(width > 0.0 && height > 0.0) {
        return (tw as u32, th as u32);
    }
    let scale = (tw / width).min(th / height);
    (
        ((width * scale).round() as u32).clamp(1, MOST),
        ((height * scale).round() as u32).clamp(1, MOST),
    )
}

/// The system's faces, read once a process from the font folders.
fn fonts() -> Arc<usvg::fontdb::Database> {
    static DB: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    DB.get_or_init(|| {
        let mut db = usvg::fontdb::Database::new();
        for dir in crate::platform::fonts::dirs() {
            db.load_fonts_dir(dir);
        }
        log::debug!("svg: {} faces for a drawing's words", db.len());
        Arc::new(db)
    })
    .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A path, a gradient and a line of text: the three things an SVG is
    /// drawn with, the third in the system's faces.
    const DRAWING: &str = r##"<?xml version="1.0" encoding="UTF-8"?>
<svg xmlns="http://www.w3.org/2000/svg" width="200" height="100" viewBox="0 0 200 100">
  <defs>
    <linearGradient id="g" x1="0" y1="0" x2="1" y2="0">
      <stop offset="0" stop-color="#ff0000"/>
      <stop offset="1" stop-color="#0000ff"/>
    </linearGradient>
  </defs>
  <path d="M0 0 H200 V60 H0 Z" fill="url(#g)"/>
  <text x="10" y="92" font-family="Arial, sans-serif" font-size="30" fill="#000000">Delight</text>
</svg>
"##;

    /// The drawing comes back the size that fits the pane, its gradient red
    /// at the left and blue at the right, and its word drawn in black under
    /// it — which needs a face, so the system's were found.
    #[test]
    fn a_drawing_with_a_path_a_gradient_and_text_is_a_picture() {
        let image = render(DRAWING.as_bytes(), (400, 400)).expect("an SVG");
        assert_eq!(
            image.dimensions(),
            (400, 200),
            "fitted, and enlarged to fit"
        );
        let left = image.get_pixel(4, 40).0;
        let right = image.get_pixel(395, 40).0;
        assert!(left[0] > 200 && left[2] < 60, "left is red: {left:?}");
        assert!(right[2] > 200 && right[0] < 60, "right is blue: {right:?}");
        assert_eq!(left[3], 255);
        assert!(
            !fonts().is_empty(),
            "no system faces in {:?}",
            crate::platform::fonts::dirs()
        );
        let ink = (0..400)
            .flat_map(|x| (130..196).map(move |y| (x, y)))
            .filter(|&(x, y)| {
                let p = image.get_pixel(x, y).0;
                p[3] > 200 && p[0] < 80 && p[1] < 80 && p[2] < 80
            })
            .count();
        assert!(ink > 200, "the word is not drawn: {ink} dark pixels");
        // Below the gradient and outside the word the picture is clear.
        assert_eq!(image.get_pixel(399, 199).0[3], 0);
    }

    /// A drawing with nothing behind it comes back clear where it is clear
    /// — alpha 0 in every corner, not black — and its edge, which tiny-skia
    /// holds premultiplied, comes back in the drawing's own colour at the
    /// coverage it has.
    #[test]
    fn a_transparent_drawing_is_clear_around_its_picture() {
        let disc = br##"<svg xmlns="http://www.w3.org/2000/svg" width="100" height="100">
  <circle cx="50" cy="50" r="30" fill="#89b4fa"/>
</svg>"##;
        let image = render(disc, (100, 100)).expect("an SVG");
        assert_eq!(image.dimensions(), (100, 100));
        for (x, y) in [(0, 0), (99, 0), (0, 99), (99, 99)] {
            assert_eq!(image.get_pixel(x, y).0[3], 0, "the corner {x},{y}");
        }
        assert_eq!(image.get_pixel(50, 50).0, [0x89, 0xb4, 0xfa, 255]);
        // At half coverage and more, where demultiplying loses at most a
        // step or two of each channel to rounding.
        let edge: Vec<[u8; 4]> = image
            .pixels()
            .map(|p| p.0)
            .filter(|p| (128..255).contains(&p[3]))
            .collect();
        assert!(!edge.is_empty(), "no antialiased edge");
        for pixel in edge {
            for (got, want) in pixel[..3].iter().zip([0x89u8, 0xb4, 0xfa]) {
                assert!(
                    got.abs_diff(want) <= 2,
                    "an edge pixel {pixel:?} is darkened"
                );
            }
        }
    }

    /// Not an SVG — a PNG's bytes, text that is not markup, markup that is
    /// not SVG, broken SVG — is no picture here, and costs no font loading.
    #[test]
    fn what_is_not_an_svg_is_left_for_the_next_decoder() {
        assert!(render(b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR", (100, 100)).is_none());
        assert!(render(b"plain words, no markup", (100, 100)).is_none());
        assert!(render(b"<html><body>hi</body></html>", (100, 100)).is_none());
        assert!(render(b"<svg xmlns='http://www.w3.org/2000/svg'", (100, 100)).is_none());
        assert!(!has_words(br#"<svg><path d="M0 0"/></svg>"#));
        assert!(looks_like_svg(b"\xef\xbb\xbf  \n<svg/>"));
    }

    /// The fit keeps the drawing's aspect, enlarges it to the pane, and is
    /// bounded either way.
    #[test]
    fn the_drawing_fits_the_pane() {
        assert_eq!(fitted(24.0, 24.0, (300, 200)), (200, 200));
        assert_eq!(fitted(200.0, 100.0, (100, 100)), (100, 50));
        assert_eq!(fitted(1.0, 100_000.0, (100, 100)), (1, 100));
        assert_eq!(fitted(10.0, 10.0, (100_000, 100_000)), (MOST, MOST));
        assert_eq!(fitted(0.0, 10.0, (50, 40)), (50, 40));
    }
}
