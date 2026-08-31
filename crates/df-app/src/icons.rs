//! Row icons: which glyph, which colour, and which font can draw it.
//!
//! ## The font problem, and the answer
//!
//! The nineteen directory icons in `theme.toml` (PLAN §3) are **Nerd Font
//! private-use codepoints** — `\u{f0b1}` for `Work`, `\u{e70c}` for `Code`.
//! Nothing in the Private Use Area is in any system font by definition, so
//! either a patched font is loaded or those rows draw nineteen tofu boxes.
//!
//! egui cannot ask fontconfig for "a font containing U+F0B1", so this scans a
//! short list of the places a Nerd Font is installed on Arch (`/usr/share/fonts`
//! and the user's own font directory) for a regular-weight patched face, and
//! registers it as a **fallback** on egui's proportional and monospace families
//! rather than as a replacement. Two consequences, both wanted: the UI keeps
//! egui's own text face, and every other glyph the chrome needs that a plain
//! Latin font might be missing — the `→` on a symlink row, the arrows in the
//! help sheet — resolves through the same fallback.
//!
//! **When no patched font is installed the icons degrade to `ls -F`.** Not to a
//! different pictogram set — there isn't one that is reliably present — but to
//! the classifier suffixes every Unix user already reads: `/` for a directory,
//! `@` for a symlink, nothing at all for a regular file. Those are ASCII, so
//! they cannot fail to render, and a column of them is still a column that says
//! what each row is.

use std::path::PathBuf;

use df_core::config::{Color, Theme};
use df_core::fs::{Entry, Kind, LinkTarget};

use crate::theme::Palette;

/// The egui family the icon column is drawn in. Its own family rather than the
/// default one: an icon is drawn at a different size from the name beside it,
/// and giving it a name means the fallback chain for icons can never be
/// re-ordered by a change to the UI font.
pub const ICON_FAMILY: &str = "df-icons";

/// Directories scanned for a patched font, in order. Deliberately short — this
/// runs on the cold-start path (PLAN §6), and a full recursive walk of
/// `/usr/share/fonts` is the tens of milliseconds delightviewer's own font
/// loader is careful to keep off it.
const FONT_DIRS: &[&str] = &[
    "/usr/share/fonts/TTF",
    "/usr/share/fonts/truetype",
    "/usr/share/fonts/OTF",
    "/usr/share/fonts/nerd-fonts",
    "/usr/local/share/fonts",
];

/// Preferred faces, most wanted first. All three are common Arch packages and
/// all three are patched with the same icon set, so the choice is only about
/// which Latin face rides along in the fallback chain.
const PREFERRED: &[&str] = &[
    "JetBrainsMonoNerdFont-Regular",
    "FiraCodeNerdFont-Regular",
    "CaskaydiaMonoNerdFont-Regular",
    "HackNerdFont-Regular",
    "SymbolsNerdFont-Regular",
];

/// Find a Nerd Font on this machine, if there is one.
///
/// Returns the bytes rather than the path because the caller hands them
/// straight to egui, and a font that vanished between the scan and the load
/// would be a failure with no useful recovery.
fn find_nerd_font() -> Option<(PathBuf, Vec<u8>)> {
    let mut dirs: Vec<PathBuf> = FONT_DIRS.iter().map(PathBuf::from).collect();
    if let Some(home) = std::env::var_os("HOME") {
        dirs.push(PathBuf::from(&home).join(".local/share/fonts"));
        dirs.push(PathBuf::from(home).join(".fonts"));
    }

    let mut best: Option<(usize, PathBuf)> = None;
    for dir in dirs {
        let Ok(reader) = std::fs::read_dir(&dir) else {
            continue;
        };
        for item in reader.flatten() {
            let name = item.file_name().to_string_lossy().into_owned();
            let stem = name.trim_end_matches(".ttf").trim_end_matches(".otf");
            if stem.len() == name.len() {
                continue; // not a font file
            }
            // Rank by position in `PREFERRED`; anything else patched is a
            // last-resort match one step past the end of that list.
            let rank = PREFERRED.iter().position(|p| stem.eq_ignore_ascii_case(p));
            let rank = match rank {
                Some(r) => r,
                None if stem.contains("NerdFont") && stem.ends_with("-Regular") => PREFERRED.len(),
                None => continue,
            };
            if best.as_ref().is_none_or(|(r, _)| rank < *r) {
                best = Some((rank, item.path()));
            }
        }
    }

    let (_, path) = best?;
    match std::fs::read(&path) {
        Ok(bytes) => Some((path, bytes)),
        Err(e) => {
            log::debug!("could not read {}: {e}", path.display());
            None
        }
    }
}

/// Install the icon font, and say whether the real glyphs are available.
///
/// Called once, before the first frame. `set_fonts` replaces egui's whole font
/// definition, so this is the only place that may call it — a second call built
/// from `FontDefinitions::default()` would take this one back out again (the
/// trap delightviewer's `install_fonts` documents).
pub fn install(ctx: &egui::Context) -> bool {
    let Some((path, bytes)) = find_nerd_font() else {
        log::info!("no Nerd Font found; row icons fall back to ls-style classifiers");
        return false;
    };
    log::debug!("icon font: {}", path.display());

    let mut fonts = egui::FontDefinitions::default();
    let data = std::sync::Arc::new(egui::FontData::from_owned(bytes));
    fonts.font_data.insert(ICON_FAMILY.to_string(), data);
    fonts.families.insert(
        egui::FontFamily::Name(ICON_FAMILY.into()),
        vec![ICON_FAMILY.to_string()],
    );
    // Appended, not prepended: the chrome keeps egui's own face for text, and
    // this face only answers for codepoints nothing else has.
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push(ICON_FAMILY.to_string());
    }
    ctx.set_fonts(fonts);
    true
}

/// What to draw in a row's icon column.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Icon {
    pub glyph: char,
    pub color: egui::Color32,
}

/// The generic glyphs, for everything the theme has no rule about. All from the
/// Nerd Font set that `theme.toml`'s directory icons come from, so one row's
/// icon is never from a different drawing than the next one's.
const GENERIC_DIR: char = '\u{f4d4}'; // nf-oct-file_directory_fill
const GENERIC_FILE: char = '\u{f4a5}'; // nf-oct-file
const GENERIC_LINK: char = '\u{f0c1}'; // nf-fa-link
const BROKEN_LINK: char = '\u{f127}'; // nf-fa-chain_broken

/// The `ls -F` classifiers, used when no patched font could be loaded. See the
/// module header: these are ASCII on purpose.
const PLAIN_DIR: char = '/';
const PLAIN_LINK: char = '@';
const PLAIN_FILE: char = ' ';

/// The icon for one row.
///
/// `theme` supplies the per-directory rules (PLAN §3's nineteen, plus whatever
/// the user prepended); `palette` supplies the colours for everything else.
/// `nerd` says whether the private-use glyphs can actually be drawn.
pub fn icon_for(entry: &Entry, theme: &Theme, palette: &Palette, nerd: bool) -> Icon {
    let broken = entry.is_broken_symlink();
    let link = entry.is_symlink();
    let dir = entry.is_dir();

    // A themed directory icon outranks the generic ones — including for a
    // symlink *to* a directory, because `~/Work` being a link does not make it
    // less the Work folder.
    let themed = if dir && !broken {
        theme.dir_icon(&entry.path.to_string_lossy(), &entry.name)
    } else {
        None
    };

    let color = if broken {
        palette.red
    } else if let Some(fg) = themed.and_then(|i| i.fg) {
        to_color32(fg)
    } else if dir {
        palette.blue
    } else if link {
        palette.sky
    } else {
        palette.overlay2
    };

    let glyph = if !nerd {
        if dir {
            PLAIN_DIR
        } else if link {
            PLAIN_LINK
        } else {
            PLAIN_FILE
        }
    } else if broken {
        BROKEN_LINK
    } else if let Some(icon) = themed {
        icon.text
    } else if dir {
        GENERIC_DIR
    } else if link {
        GENERIC_LINK
    } else {
        GENERIC_FILE
    };

    Icon { glyph, color }
}

/// The colour a row's *name* is drawn in — yazi's rules, which are the ones
/// Brian's eye is trained on: directories blue, broken links red, everything
/// else the default foreground.
pub fn name_color(entry: &Entry, palette: &Palette) -> egui::Color32 {
    if entry.is_broken_symlink() {
        palette.red
    } else if entry.is_dir() {
        palette.blue
    } else if matches!(
        entry.kind,
        Kind::Symlink {
            target: Some(LinkTarget::Other)
        }
    ) {
        // A socket, fifo or device node: real, listed, and not openable. Given
        // its own colour so `→` on one is visibly not going to do anything.
        palette.overlay2
    } else {
        palette.text
    }
}

pub fn to_color32(c: Color) -> egui::Color32 {
    egui::Color32::from_rgb(c.r, c.g, c.b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn entry(name: &str, kind: Kind) -> Entry {
        Entry {
            name: name.to_string(),
            path: PathBuf::from("/home/brian").join(name),
            kind,
            len: 0,
            mtime: None,
            btime: None,
            mode: 0,
            uid: 0,
            gid: 0,
            is_hidden: name.starts_with('.'),
            mime: "application/octet-stream",
        }
    }

    /// The nineteen ported rules have to actually reach a row — that is the
    /// whole reason `theme.toml` was transcribed.
    #[test]
    fn a_themed_directory_gets_its_own_glyph_and_colour() {
        let theme = Theme::default();
        let palette = Palette::from_theme(&theme);
        let icon = icon_for(&entry("Work", Kind::Dir), &theme, &palette, true);
        assert_eq!(icon.glyph, '\u{f0b1}');
        assert_eq!(icon.color, egui::Color32::from_rgb(0xf7, 0x76, 0x8e));
    }

    #[test]
    fn a_plain_directory_falls_back_to_the_generic_glyph_in_blue() {
        let theme = Theme::default();
        let palette = Palette::from_theme(&theme);
        let icon = icon_for(&entry("scratch", Kind::Dir), &theme, &palette, true);
        assert_eq!(icon.glyph, GENERIC_DIR);
        assert_eq!(icon.color, palette.blue);
    }

    /// A broken link is red everywhere — icon and name both, the way yazi
    /// draws it.
    #[test]
    fn a_broken_symlink_is_red() {
        let theme = Theme::default();
        let palette = Palette::from_theme(&theme);
        let e = entry("gone", Kind::Symlink { target: None });
        assert_eq!(icon_for(&e, &theme, &palette, true).color, palette.red);
        assert_eq!(name_color(&e, &palette), palette.red);
    }

    /// Without a patched font every glyph must be one a plain Latin face has.
    #[test]
    fn the_fallback_glyphs_are_ascii() {
        let theme = Theme::default();
        let palette = Palette::from_theme(&theme);
        for (name, kind) in [
            ("Work", Kind::Dir),
            ("notes.txt", Kind::File),
            (
                "link",
                Kind::Symlink {
                    target: Some(LinkTarget::File),
                },
            ),
        ] {
            let icon = icon_for(&entry(name, kind), &theme, &palette, false);
            assert!(icon.glyph.is_ascii(), "{name} drew {:?}", icon.glyph);
        }
    }

    /// …and the themed rule still colours the row even when its glyph cannot
    /// be drawn, so a fontless machine keeps the folder colours.
    #[test]
    fn colours_survive_a_missing_font() {
        let theme = Theme::default();
        let palette = Palette::from_theme(&theme);
        let icon = icon_for(&entry("Work", Kind::Dir), &theme, &palette, false);
        assert_eq!(icon.glyph, PLAIN_DIR);
        assert_eq!(icon.color, egui::Color32::from_rgb(0xf7, 0x76, 0x8e));
    }
}
