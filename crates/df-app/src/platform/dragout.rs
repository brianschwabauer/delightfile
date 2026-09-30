//! What a drag out of the window carries on macOS, and what it looks like
//! there — the two parts of `platform::macos::device`'s drag that are
//! decisions rather than AppKit calls, compiled on macOS and in every
//! target's tests.
//!
//! **One pasteboard item per file.** The drag is offered to the rest of the
//! desktop as `crate::dnd::offer` spells it for Wayland — a `text/uri-list`,
//! the paths as plain text, and this window's own mark — and on macOS that
//! becomes one `NSPasteboardItem` per path, which is what Finder, Mail and
//! every other drop target read a file drag as. Each item carries its file's
//! URL and its path as text, so a text field that takes the drop gets the
//! paths a person could have typed; the text is not an item of its own,
//! because Finder makes a text clipping out of any item that has only text,
//! and a drag of two files onto the Desktop would leave a third thing
//! behind. The window's mark rides on the first item.
//!
//! **The picture** is [`crate::platform::icon::draw`]'s, whose pixels are
//! `wl_shm`'s `Argb8888`: premultiplied, and on a little-endian machine laid
//! out `B G R A`. An `NSBitmapImageRep` with no format flags is
//! premultiplied `R G B A`, so the colours are swapped into place and the
//! alpha is left as it is.

use std::path::PathBuf;

/// One pasteboard item of a drag.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// The file, offered as its URL.
    pub path: PathBuf,
    /// The same file as text: its path.
    pub text: String,
    /// This window's mark ([`crate::dnd::self_mime`]), on the first item
    /// only, as a type with no bytes.
    pub mark: Option<String>,
}

/// The items a drag offering `offers` is made of: one per file in its
/// `text/uri-list`, in order. Empty when it names no file, which is a drag
/// there is nothing to hand over for.
pub fn items(offers: &[(String, Vec<u8>)]) -> Vec<Item> {
    let paths = offers
        .iter()
        .find(|(mime, _)| mime.eq_ignore_ascii_case("text/uri-list"))
        .map(|(_, bytes)| crate::clipboard::parse_uri_list(&String::from_utf8_lossy(bytes)))
        .unwrap_or_default();
    let mark = offers
        .iter()
        .find(|(mime, _)| mime.starts_with(crate::dnd::SELF_MIME))
        .map(|(mime, _)| mime.clone());
    paths
        .into_iter()
        .enumerate()
        .map(|(at, path)| Item {
            text: path.to_string_lossy().into_owned(),
            path,
            mark: if at == 0 { mark.clone() } else { None },
        })
        .collect()
}

/// `Argb8888` pixels, premultiplied and little-endian (`B G R A`), as the
/// premultiplied `R G B A` an `NSBitmapImageRep` takes.
pub fn rgba(bgra: &[u8]) -> Vec<u8> {
    let (pixels, _) = bgra.as_chunks::<4>();
    pixels
        .iter()
        .flat_map(|&[b, g, r, a]| [r, g, b, a])
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A drag of three files is three items, in order, each with its path as
    /// text, and the window's mark on the first alone. The paths are
    /// absolute as the platform spells them, since the items come back
    /// through a `file://` URI.
    #[test]
    fn every_file_is_one_item() {
        let home = std::env::temp_dir().join("me");
        let paths = vec![
            home.join("a b.txt"),
            home.join("ünïcode #1.png"),
            home.join("folder"),
        ];
        let items = items(&crate::dnd::offer(&paths));
        assert_eq!(items.len(), paths.len());
        for (item, path) in items.iter().zip(&paths) {
            assert_eq!(&item.path, path);
            assert_eq!(item.text, path.to_string_lossy());
        }
        assert_eq!(items[0].mark.as_deref(), Some(crate::dnd::self_mime()));
        assert!(items[1..].iter().all(|item| item.mark.is_none()));
    }

    #[test]
    fn a_drag_of_no_files_has_no_items() {
        assert!(items(&crate::dnd::offer(&[])).is_empty());
        assert!(items(&[("text/plain".to_string(), b"/a".to_vec())]).is_empty());
    }

    /// Blue, half-transparent red, and clear, each premultiplied: the colour
    /// channels trade places and the alpha stays.
    #[test]
    fn the_pixels_are_reordered_not_recomputed() {
        let bgra = [
            0xff, 0x00, 0x00, 0xff, // opaque blue
            0x00, 0x00, 0x80, 0x80, // half-transparent red, premultiplied
            0x00, 0x00, 0x00, 0x00, // clear
        ];
        assert_eq!(
            rgba(&bgra),
            [
                0x00, 0x00, 0xff, 0xff, //
                0x80, 0x00, 0x00, 0x80, //
                0x00, 0x00, 0x00, 0x00,
            ]
        );
        let icon = crate::platform::icon::draw(
            3,
            crate::platform::icon::Rgba(30, 30, 46, 255),
            crate::platform::icon::Rgba(205, 214, 244, 255),
        );
        assert_eq!(
            rgba(&icon.pixels).len(),
            (icon.width * icon.height * 4) as usize
        );
    }
}
