//! An SVG as a picture on Linux: FFmpeg's, as it always was. The preview's
//! decoder hands a picture the `image` crate cannot read to FFmpeg
//! ([`crate::preview::decode`]), and the system's FFmpeg reads an SVG
//! through the librsvg it is built with. macOS and Windows carry an FFmpeg
//! without librsvg and draw one with resvg instead (`platform/svg.rs`,
//! `plans/other-platforms/04-windows.md` W4.44); nothing of that is built
//! here.

/// Nothing, so FFmpeg answers: an SVG is its to draw here.
pub fn render(_bytes: &[u8], _target: (u32, u32)) -> Option<image::RgbaImage> {
    None
}
