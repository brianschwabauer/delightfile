//! The bytes of Windows' clipboard formats, built and read without the
//! clipboard: a file list (`CF_HDROP`'s `DROPFILES`), text (`CF_UNICODETEXT`)
//! and a device-independent bitmap (`CF_DIB`, `CF_DIBV5`) turned into the PNG
//! the window pastes pictures as. Windows' (`windows/clipboard.rs` does the
//! calls), and compiled in every target's tests so the layouts are checked
//! where they are edited.

use std::io::Cursor;

/// The size of `DROPFILES`: `pFiles`, a `POINT`, `fNC` and `fWide`, four
/// bytes each.
pub const DROPFILES_SIZE: usize = 20;

/// A `CF_HDROP` block for `paths`, each already UTF-16: the `DROPFILES`
/// header saying the names follow it and are wide, then every name ended by
/// a NUL, then one more NUL for the end of the list.
pub fn dropfiles(paths: &[Vec<u16>]) -> Vec<u8> {
    let mut out = Vec::with_capacity(
        DROPFILES_SIZE + paths.iter().map(|p| (p.len() + 1) * 2).sum::<usize>() + 2,
    );
    out.extend_from_slice(&(DROPFILES_SIZE as u32).to_le_bytes()); // pFiles
    out.extend_from_slice(&0i32.to_le_bytes()); // pt.x
    out.extend_from_slice(&0i32.to_le_bytes()); // pt.y
    out.extend_from_slice(&0i32.to_le_bytes()); // fNC
    out.extend_from_slice(&1i32.to_le_bytes()); // fWide
    for path in paths {
        for unit in path.iter().chain(Some(&0)) {
            out.extend_from_slice(&unit.to_le_bytes());
        }
    }
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

/// Text as `CF_UNICODETEXT` wants it: UTF-16, ended by a NUL. Bytes that are
/// not UTF-8 are carried as far as they can be, a replacement character
/// standing where one could not.
pub fn unicode_text(bytes: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(bytes);
    let mut out = Vec::with_capacity((text.len() + 1) * 2);
    for unit in text.encode_utf16().chain(Some(0)) {
        out.extend_from_slice(&unit.to_le_bytes());
    }
    out
}

/// `CF_UNICODETEXT`'s bytes as text, up to the first NUL (a block is often
/// longer than what was written into it).
pub fn text_of_unicode(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .take_while(|&unit| unit != 0)
        .collect();
    String::from_utf16_lossy(&units)
}

/// A `CF_DIB` or `CF_DIBV5` block — a `BITMAPINFOHEADER` (or a larger header
/// that starts like one) and the pixels after it — as a PNG. Uncompressed
/// 24- and 32-bit pictures, and 32-bit ones with the usual masks, which is
/// what a screenshot and every current program put there; anything else is
/// `None`, and the paste says there was no picture to take.
pub fn png_of_dib(dib: &[u8]) -> Option<Vec<u8>> {
    let u32_at = |at: usize| -> Option<u32> {
        Some(u32::from_le_bytes(dib.get(at..at + 4)?.try_into().ok()?))
    };
    let i32_at = |at: usize| u32_at(at).map(|v| v as i32);
    let u16_at = |at: usize| -> Option<u16> {
        Some(u16::from_le_bytes(dib.get(at..at + 2)?.try_into().ok()?))
    };
    const BI_RGB: u32 = 0;
    const BI_BITFIELDS: u32 = 3;

    let header = u32_at(0)? as usize;
    if header < 40 {
        return None;
    }
    let width = i32_at(4)?;
    let height = i32_at(8)?;
    let bits = u16_at(14)?;
    let compression = u32_at(16)?;
    if width <= 0 || height == 0 || !(bits == 24 || bits == 32) {
        return None;
    }
    let masks_after = match compression {
        BI_RGB => 0,
        // The three masks sit at offset 40 either way: after a plain
        // header, which the pixels then follow, or inside a V4 or V5 one.
        BI_BITFIELDS if bits == 32 => {
            let masks = (u32_at(40)?, u32_at(44)?, u32_at(48)?);
            if masks != (0x00FF_0000, 0x0000_FF00, 0x0000_00FF) {
                return None;
            }
            if header == 40 {
                12
            } else {
                0
            }
        }
        _ => return None,
    };
    let (w, h) = (width as usize, height.unsigned_abs() as usize);
    let bottom_up = height > 0;
    let step = usize::from(bits / 8);
    let stride = (w * step).div_ceil(4) * 4;
    let start = header + masks_after;
    let pixels = dib.get(start..start + stride.checked_mul(h)?)?;

    let mut rgba = Vec::with_capacity(w * h * 4);
    let mut any_alpha = false;
    for row in 0..h {
        let source = if bottom_up { h - 1 - row } else { row };
        let line = &pixels[source * stride..source * stride + w * step];
        for px in line.chunks_exact(step) {
            let alpha = if step == 4 { px[3] } else { 255 };
            any_alpha |= step == 4 && alpha != 0;
            rgba.extend_from_slice(&[px[2], px[1], px[0], alpha]);
        }
    }
    // A 32-bit bitmap with every alpha zero is one whose fourth byte is
    // unused, which is most of them: opaque, not invisible.
    if step == 4 && !any_alpha {
        for px in rgba.chunks_exact_mut(4) {
            px[3] = 255;
        }
    }
    let image = image::RgbaImage::from_raw(w as u32, h as u32, rgba)?;
    let mut png = Cursor::new(Vec::new());
    image.write_to(&mut png, image::ImageFormat::Png).ok()?;
    Some(png.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().collect()
    }

    /// The header, then each name and its NUL, then the list's NUL.
    #[test]
    fn a_file_list_is_a_wide_dropfiles_block() {
        let block = dropfiles(&[wide(r"C:\a b.txt"), wide(r"D:\ü")]);
        assert_eq!(&block[0..4], &20u32.to_le_bytes(), "pFiles");
        assert_eq!(&block[16..20], &1i32.to_le_bytes(), "fWide");
        let names: Vec<u16> = block[DROPFILES_SIZE..]
            .chunks_exact(2)
            .map(|p| u16::from_le_bytes([p[0], p[1]]))
            .collect();
        let mut expected = wide(r"C:\a b.txt");
        expected.push(0);
        expected.extend(wide(r"D:\ü"));
        expected.extend([0, 0]);
        assert_eq!(names, expected);
    }

    #[test]
    fn text_round_trips_through_unicode_text() {
        let block = unicode_text("naïve café — ✓ 🎬".as_bytes());
        assert_eq!(&block[block.len() - 2..], &[0, 0], "ends in a NUL");
        let mut padded = block.clone();
        padded.extend_from_slice(&[b'x', 0, b'y', 0]);
        assert_eq!(text_of_unicode(&padded), "naïve café — ✓ 🎬");
    }

    /// A 2×2 bitmap, bottom-up as a DIB is unless its height says otherwise,
    /// comes out as a PNG the right way up.
    #[test]
    fn a_bitmap_becomes_a_png_the_right_way_up() {
        let header = |bits: u16, height: i32| {
            let mut h = Vec::new();
            h.extend_from_slice(&40u32.to_le_bytes());
            h.extend_from_slice(&2i32.to_le_bytes());
            h.extend_from_slice(&height.to_le_bytes());
            h.extend_from_slice(&1u16.to_le_bytes());
            h.extend_from_slice(&bits.to_le_bytes());
            h.extend_from_slice(&[0; 24]);
            h
        };
        // 24-bit: rows padded to four bytes. The bottom row (first in the
        // block) is blue then green; the top row is red then white.
        let mut dib = header(24, 2);
        dib.extend_from_slice(&[255, 0, 0, 0, 255, 0, 0, 0]);
        dib.extend_from_slice(&[0, 0, 255, 255, 255, 255, 0, 0]);
        let png = png_of_dib(&dib).expect("a png");
        let back = image::load_from_memory(&png).expect("decodes").to_rgba8();
        assert_eq!(back.dimensions(), (2, 2));
        assert_eq!(back.get_pixel(0, 0).0, [255, 0, 0, 255], "top left red");
        assert_eq!(back.get_pixel(1, 0).0, [255, 255, 255, 255]);
        assert_eq!(back.get_pixel(0, 1).0, [0, 0, 255, 255], "bottom left blue");
        assert_eq!(back.get_pixel(1, 1).0, [0, 255, 0, 255]);

        // 32-bit, top-down, the fourth byte unused: opaque.
        let mut dib = header(32, -1);
        dib[8..12].copy_from_slice(&(-1i32).to_le_bytes());
        dib.extend_from_slice(&[0, 0, 255, 0, 0, 255, 0, 0]);
        let back = image::load_from_memory(&png_of_dib(&dib).expect("a png"))
            .expect("decodes")
            .to_rgba8();
        assert_eq!(back.dimensions(), (2, 1));
        assert_eq!(back.get_pixel(0, 0).0, [255, 0, 0, 255]);
        assert_eq!(back.get_pixel(1, 0).0, [0, 255, 0, 255]);

        assert_eq!(png_of_dib(&header(8, 2)), None, "a palette is not read");
        assert_eq!(png_of_dib(&[0; 10]), None, "nor is half a header");
    }
}
