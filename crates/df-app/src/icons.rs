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
//! and the user's own font directory — each platform's list is
//! [`crate::platform::fonts::dirs`]) for a regular-weight patched face, and
//! registers it as a **fallback** on egui's proportional and monospace families
//! rather than as a replacement. Two consequences, both wanted: the UI keeps
//! egui's own text face, and every other glyph the chrome needs that a plain
//! Latin font might be missing — the `→` on a symlink row, the arrows in the
//! help sheet — resolves through the same fallback. Where there is no patched
//! face to fall back on, [`crate::glyphs`] draws those few itself.
//!
//! **When no patched font is installed the icons are drawn** ([`paint`],
//! [`crate::marks`]): a folder with a tab for a directory, a page with its
//! corner folded for a file, the page with an arrow for a link and with `▶`
//! for something that runs — the four things `ls -F`'s classifiers told
//! apart, which is what the column fell back to before, and which on a stock
//! Mac or Windows, never having a Nerd Font, put a `/` before every folder's
//! name (`plans/other-platforms/04-windows.md` W4.42). The **colours** do
//! not change — they are the half of the treatment that needs no font — so a
//! fontless machine still reads a Downloads folder by hue.
//!
//! ## What a row's icon says
//!
//! Two channels, and they answer two different questions:
//!
//! - **The glyph says what the file is** — [`df_core::fs::FileKind`] decides,
//!   and an extension the set has a logo for (`.rs`, `.py`, `.pdf`) replaces
//!   the family picture with its own.
//! - **The colour says which family it belongs to** — eight hues, one per group
//!   of kinds, so a folder of photos is one colour whatever the photos are
//!   called. See [`kind_color`] for the table and for why it is eight.
//!
//! The *name* beside it is almost always `palette.text`. That is deliberate and
//! it is the difference between a legible list and a paint chart: colour is
//! spent once, on the icon, and the two exceptions in [`name_color`] are facts
//! about salience rather than about type.
//!
//! ## No bold, and why
//!
//! PLAN §8 would give directories weight as well as colour. egui draws text
//! through the families in [`egui::FontDefinitions`], and the default
//! definition ships exactly one Latin face — there is no bold sibling of the UI
//! font to register, and egui has no synthetic emboldening. The patched icon
//! font *does* ship a bold cut, but it is a **monospace** face: using it for
//! directory names would change their typeface, not their weight, and a list
//! where every folder is in a different font is worse than one where every
//! folder is merely blue. So weight is not part of the vocabulary; a directory
//! is said with the palette's brightest name colour, its own glyph and — in the
//! sort — its position.

use std::path::PathBuf;

use df_core::config::{Color, Theme};
use df_core::fs::{Entry, FileKind};
#[cfg(test)]
use df_core::fs::{Kind, LinkTarget};

use crate::theme::Palette;

/// The egui family the icon column is drawn in. Its own family rather than the
/// default one: an icon is drawn at a different size from the name beside it,
/// and giving it a name means the fallback chain for icons can never be
/// re-ordered by a change to the UI font.
pub const ICON_FAMILY: &str = "df-icons";

/// Preferred faces, most wanted first. The first four are common Arch
/// packages and all are patched with the same icon set, so the choice is
/// only about which Latin face rides along in the fallback chain. The last is
/// what Homebrew's `font-symbols-only-nerd-font` cask installs on macOS.
const PREFERRED: &[&str] = &[
    "JetBrainsMonoNerdFont-Regular",
    "FiraCodeNerdFont-Regular",
    "CaskaydiaMonoNerdFont-Regular",
    "HackNerdFont-Regular",
    "SymbolsNerdFont-Regular",
    "SymbolsNerdFontMono-Regular",
];

/// Find a Nerd Font on this machine, if there is one.
///
/// Returns the bytes rather than the path because the caller hands them
/// straight to egui, and a font that vanished between the scan and the load
/// would be a failure with no useful recovery.
fn find_nerd_font() -> Option<(PathBuf, Vec<u8>)> {
    find_nerd_font_in(crate::platform::fonts::dirs())
}

/// [`find_nerd_font`] over the given directories, in order.
fn find_nerd_font_in(dirs: Vec<PathBuf>) -> Option<(PathBuf, Vec<u8>)> {
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
        log::info!("no Nerd Font found; row icons are drawn");
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
    /// The icon face's picture.
    pub glyph: char,
    pub color: egui::Color32,
    /// The drawing that stands in for it where no icon face is loaded.
    pub mark: Mark,
}

pub use crate::marks::Mark;

/// Draw `icon` at `pos`, anchored as a line of text is: the face's glyph in
/// `font` when an icon face is loaded (`nerd`) — exactly the text each
/// caller drew before there was another way, so the icon column of a
/// machine with a Nerd Font is the pixels it was — and otherwise its drawn
/// [`Mark`], `font.size` tall as the glyph would be. The rect drawn in.
pub fn paint(
    painter: &egui::Painter,
    pos: egui::Pos2,
    anchor: egui::Align2,
    icon: Icon,
    font: egui::FontId,
    nerd: bool,
    color: egui::Color32,
) -> egui::Rect {
    if nerd {
        painter.text(pos, anchor, icon.glyph, font, color)
    } else {
        crate::marks::paint(painter, pos, anchor, icon.mark, font.size, color)
    }
}

/// The face a row's icon is set in when an icon face is loaded; with none,
/// [`paint`] draws and no face is asked (egui panics on a named family
/// nothing is bound to, so this must not be built without one).
pub fn family() -> egui::FontFamily {
    egui::FontFamily::Name(ICON_FAMILY.into())
}

/// The generic glyphs, for everything the theme has no rule about. All from the
/// Nerd Font set that `theme.toml`'s directory icons come from, so one row's
/// icon is never from a different drawing than the next one's.
const GENERIC_DIR: char = '\u{f4d4}'; // nf-oct-file_directory_fill
const GENERIC_FILE: char = '\u{f4a5}'; // nf-oct-file
const GENERIC_LINK: char = '\u{f0c1}'; // nf-fa-link
const BROKEN_LINK: char = '\u{f127}'; // nf-fa-chain_broken

/// One glyph per [`FileKind`], all from FontAwesome 4 (`U+F000`–`U+F2E0`) apart
/// from the two Octicons the file already used.
///
/// One family, on purpose. Nerd Fonts carry six icon sets drawn by six
/// different hands at six different weights, and a column that mixes them reads
/// as a ransom note — the *set* has to be consistent even when the pictures are
/// not. FontAwesome 4 is the one every patched font has had since the
/// beginning, so this degrades on the widest range of installs.
fn kind_glyph(kind: FileKind) -> char {
    match kind {
        FileKind::Directory => GENERIC_DIR,
        FileKind::BrokenLink => BROKEN_LINK,
        FileKind::Special => '\u{f1e6}',    // nf-fa-plug
        FileKind::Image => '\u{f03e}',      // nf-fa-picture_o
        FileKind::Video => '\u{f008}',      // nf-fa-film
        FileKind::Audio => '\u{f001}',      // nf-fa-music
        FileKind::Archive => '\u{f1c6}',    // nf-fa-file_archive_o
        FileKind::DiskImage => '\u{f0a0}',  // nf-fa-hdd_o
        FileKind::Document => '\u{f15c}',   // nf-fa-file_text
        FileKind::Markdown => '\u{f48a}',   // nf-oct-markdown
        FileKind::Text => '\u{f0f6}',       // nf-fa-file_text_o
        FileKind::Code => '\u{f121}',       // nf-fa-code
        FileKind::Data => '\u{f1c0}',       // nf-fa-database
        FileKind::Config => '\u{f013}',     // nf-fa-cog
        FileKind::Executable => '\u{f120}', // nf-fa-terminal
        FileKind::Font => '\u{f031}',       // nf-fa-font
        FileKind::Model3d => '\u{f1b2}',    // nf-fa-cube
        FileKind::Binary => GENERIC_FILE,
    }
}

/// The colour of each kind, as a *name* on the palette rather than a value.
///
/// ## Eight hues, and why not more
///
/// A colour only means something if the eye can hold the whole set at once. Past
/// about eight, "which hue was audio again" stops being answerable at a glance
/// and the column becomes decoration — `ui-anti-slop`'s rainbow, the thing that
/// makes a file manager look busy and read no faster. So the kinds are grouped
/// by *what you would do with the file*, and the groups get the hues:
///
/// | hue     | kinds                | why                                     |
/// |---------|----------------------|-----------------------------------------|
/// | blue    | directories          | already the colour of a folder's name   |
/// | mauve   | images               | the flavour's most saturated hue, for the kind you scan a folder *looking* for |
/// | pink    | video                | next to images, because they sit next to each other in a media folder |
/// | teal    | audio                | far from pink, so an album beside a film is not a judgement call |
/// | peach   | archives, disk images| things that are sealed and must be opened |
/// | maroon  | documents            | the colour a PDF has worn for twenty years, one step off `red` |
/// | yellow  | code                 | the flavour's "type" colour in the syntax theme, so source is source in both panes |
/// | green   | executables          | `ls` has coloured a runnable file green since 1996 |
/// | sky     | data, fonts, models  | assets a program eats rather than a person reads |
///
/// Everything else is a **neutral** — prose, markdown, configuration and
/// unrecognised bytes are the rows that must not shout, and giving them a tenth
/// hue would be spending the vocabulary on the files nobody is hunting for.
///
/// `red` is deliberately absent: it is the window's word for *broken*, and a
/// kind that wore it would make a working file look like a failure.
fn kind_color(kind: FileKind, palette: &Palette) -> egui::Color32 {
    match kind {
        FileKind::Directory => palette.blue,
        FileKind::Image => palette.mauve,
        FileKind::Video => palette.pink,
        FileKind::Audio => palette.teal,
        FileKind::Archive | FileKind::DiskImage => palette.peach,
        FileKind::Document => palette.maroon,
        FileKind::Code => palette.yellow,
        FileKind::Executable => palette.green,
        FileKind::Data | FileKind::Font | FileKind::Model3d => palette.sky,
        FileKind::BrokenLink => palette.red,
        // The neutrals, on the palette's own grey ramp: markdown and prose sit
        // one step brighter than configuration, which sits one step brighter
        // than "we could not say".
        FileKind::Markdown | FileKind::Text => crate::theme::louder(palette),
        FileKind::Config => palette.quiet,
        FileKind::Binary | FileKind::Special => palette.overlay2,
    }
}

/// Extension → glyph, for the formats a person recognises by their *mark*
/// rather than by their family.
///
/// The kind table above already draws every row correctly; this is the layer
/// that makes a directory of source read like a directory of source, with Rust
/// and Python and Go each wearing their own logo. Only formats whose mark is
/// genuinely recognisable are here — an extension that would draw the same
/// picture its kind already draws is left out rather than restated, because a
/// second table saying the same thing is a second table to keep in step.
///
/// Colour is **not** overridden: the hue is the kind's, so a `.rs` and a `.py`
/// are two marks in one colour and the column still reads as "these are all
/// code". Every codepoint here was checked against an installed patched font;
/// on a machine with none, none of this runs (see the module header).
///
/// ## The third column is a guard, not a fact
///
/// Each row also names the [`FileKind`] the glyph *assumes*, and the glyph is
/// only drawn when the row really is that kind. Without it this table is a
/// second, dumber classifier arguing with df-core's: `00001.ts` is a transport
/// stream that got the TypeScript logo, and `package-lock.json` — configuration
/// by name — got the JSON mark instead of the cog every other lockfile wears.
/// The mime table has already done that thinking (see
/// [`df_core::fs::kind`]); this column is how the glyphs defer to it, and
/// `extension_glyph_agrees_with_the_kind_table` is the test that keeps the two
/// from drifting apart.
const EXTENSION_GLYPHS: &[(&str, char, FileKind)] = &[
    // Source, from Devicons and Seti — the logos, which is the whole point.
    ("rs", '\u{e7a8}', FileKind::Code),
    ("py", '\u{e73c}', FileKind::Code),
    ("go", '\u{e724}', FileKind::Code),
    ("rb", '\u{e7b0}', FileKind::Code),
    ("php", '\u{e73d}', FileKind::Code),
    ("java", '\u{e738}', FileKind::Code),
    ("kt", '\u{e738}', FileKind::Code),
    ("lua", '\u{e620}', FileKind::Code),
    ("c", '\u{e61e}', FileKind::Code),
    ("h", '\u{e61e}', FileKind::Code),
    ("cc", '\u{e61d}', FileKind::Code),
    ("cpp", '\u{e61d}', FileKind::Code),
    ("hpp", '\u{e61d}', FileKind::Code),
    ("swift", '\u{e755}', FileKind::Code),
    ("js", '\u{e781}', FileKind::Code),
    ("mjs", '\u{e781}', FileKind::Code),
    ("cjs", '\u{e781}', FileKind::Code),
    ("jsx", '\u{e781}', FileKind::Code),
    ("ts", '\u{e628}', FileKind::Code),
    ("tsx", '\u{e628}', FileKind::Code),
    ("vue", '\u{e6a0}', FileKind::Code),
    ("svelte", '\u{e697}', FileKind::Code),
    ("html", '\u{f13b}', FileKind::Code),
    ("htm", '\u{f13b}', FileKind::Code),
    ("css", '\u{e749}', FileKind::Code),
    ("scss", '\u{e749}', FileKind::Code),
    ("md", '\u{e73e}', FileKind::Markdown),
    ("markdown", '\u{e73e}', FileKind::Markdown),
    // Data and configuration, where the *shape* of the file is the useful fact.
    ("json", '\u{e60b}', FileKind::Data),
    ("toml", '\u{e615}', FileKind::Data),
    ("yaml", '\u{e615}', FileKind::Data),
    ("yml", '\u{e615}', FileKind::Data),
    ("ini", '\u{e615}', FileKind::Config),
    ("conf", '\u{e615}', FileKind::Config),
    ("cfg", '\u{e615}', FileKind::Config),
    // nf-fa-lock: a file you do not edit.
    ("lock", '\u{f023}', FileKind::Config),
    // Documents, which are four different things wearing one word.
    ("pdf", '\u{f1c1}', FileKind::Document),
    ("doc", '\u{f1c2}', FileKind::Document),
    ("docx", '\u{f1c2}', FileKind::Document),
    ("odt", '\u{f1c2}', FileKind::Document),
    ("rtf", '\u{f1c2}', FileKind::Document),
    ("xls", '\u{f1c3}', FileKind::Document),
    ("xlsx", '\u{f1c3}', FileKind::Document),
    ("ods", '\u{f1c3}', FileKind::Document),
    // Spreadsheet-shaped, but *data* to the classifier — the hue stays the
    // kind's, so these read as data wearing a table rather than as documents.
    ("csv", '\u{f1c3}', FileKind::Data),
    ("tsv", '\u{f1c3}', FileKind::Data),
    ("ppt", '\u{f1c4}', FileKind::Document),
    ("pptx", '\u{f1c4}', FileKind::Document),
    ("odp", '\u{f1c4}', FileKind::Document),
    // nf-fa-book.
    ("epub", '\u{f02d}', FileKind::Document),
    ("mobi", '\u{f02d}', FileKind::Document),
    // The one picture format that is a picture of something else.
    ("svg", '\u{e698}', FileKind::Image),
];

/// The glyph an extension asks for, or `None` to use the kind's.
///
/// A leading dot is the hidden marker, not an extension — df-core's rule,
/// followed here so `.ts` and `.gitignore` mean what they mean everywhere else
/// in the program. The comparison is case-insensitive against the slice rather
/// than against a lowercased copy: this is asked for every visible row on every
/// frame, and a `String` per row per frame is a `String` per row per frame.
///
/// `kind` is the answer df-core already gave for this row, and it is a veto: a
/// `.ts` that classified as video is a transport stream, and the TypeScript
/// logo would be the icon column stating something the rest of the program
/// disagrees with.
fn extension_glyph(name: &str, kind: FileKind) -> Option<char> {
    let ext = df_core::fs::extension_of(name)?;
    EXTENSION_GLYPHS
        .iter()
        .find(|(e, _, k)| e.eq_ignore_ascii_case(ext) && *k == kind)
        .map(|(_, glyph, _)| *glyph)
}

/// A chrome glyph: the patched font's icon when [`install`] found one, and a
/// character the stock faces are known to carry otherwise.
///
/// The chrome used to reach for Unicode symbols (`⑂`, `⌕`, `▤`) on the theory
/// that they need no patched font — but egui's own faces do not have them
/// either, and what showed was the missing-glyph box. Every fallback here has
/// to be a character those faces actually draw; `the_chrome_glyphs_all_render`
/// holds them to it.
pub fn glyph(nerd: bool, patched: char, plain: &'static str) -> String {
    if nerd {
        patched.to_string()
    } else {
        plain.to_string()
    }
}

/// The plain file glyph, for a card that stands for several files at once —
/// the ghost of the whole clipboard, dragged off the yank chip (PLAN §7.1).
///
/// A clipboard carries whatever it carries; a ghost wearing the first file's
/// icon would claim they are all that kind of thing.
pub fn generic(palette: &Palette) -> Icon {
    Icon {
        glyph: GENERIC_FILE,
        color: palette.text,
        mark: Mark::File,
    }
}

/// The archive kind's glyph and hue, for the preview's archive header: the
/// same mark an archive's row wears when no logo outranks it, so the header
/// and the kind column agree about what the file is.
pub fn archive(palette: &Palette) -> Icon {
    Icon {
        glyph: kind_glyph(FileKind::Archive),
        color: kind_color(FileKind::Archive, palette),
        mark: Mark::Archive,
    }
}

/// A padlock, for an archive member that needs a password
/// (nf-fa-lock — the glyph the `lock` extension already wears).
pub const LOCK: char = '\u{f023}';

/// The padlock as an icon, drawn where there is no patched font.
pub fn lock(color: egui::Color32) -> Icon {
    Icon {
        glyph: LOCK,
        color,
        mark: Mark::Lock,
    }
}

/// The plain directory glyph, for a card that stands for a *place* rather than
/// for a file: the ghost of a tab being dragged out of the strip (PLAN §2).
pub fn folder(palette: &Palette) -> Icon {
    Icon {
        glyph: GENERIC_DIR,
        color: palette.blue,
        mark: Mark::Folder,
    }
}

/// A place on another machine — an `sftp://` pin or bookmark — where
/// [`folder`] is one on this disk (nf-fa-server). The Places rows of the mount
/// card wear one or the other, so a folder that is a network round trip away
/// does not look like one that is not.
pub fn network(palette: &Palette) -> Icon {
    Icon {
        glyph: NETWORK_PLACE,
        color: palette.teal,
        mark: Mark::Server,
    }
}

const NETWORK_PLACE: char = '\u{f233}'; // nf-fa-server

/// A disk on the Places card: a USB plug for one that comes out, a drive for
/// one that does not — the question a person scanning for the stick they
/// just plugged in is asking (nf-fa-usb, nf-fa-hdd_o).
pub fn drive(palette: &Palette, removable: bool) -> Icon {
    let (glyph, mark) = if removable {
        (USB, Mark::Usb)
    } else {
        (DRIVE, Mark::Drive)
    };
    Icon {
        glyph,
        color: palette.lavender,
        mark,
    }
}

/// A phone gvfs reached over MTP (nf-fa-mobile).
pub fn phone(palette: &Palette) -> Icon {
    Icon {
        glyph: PHONE,
        color: palette.green,
        mark: Mark::Phone,
    }
}

/// A camera gvfs reached over PTP (nf-fa-camera).
pub fn camera(palette: &Palette) -> Icon {
    Icon {
        glyph: CAMERA,
        color: palette.green,
        mark: Mark::Camera,
    }
}

/// The Places card's "Connect to server…" row: a plus, since the row adds a
/// share rather than being one (nf-fa-plus).
pub fn connect(palette: &Palette) -> Icon {
    Icon {
        glyph: PLUS,
        color: palette.subtext0,
        mark: Mark::Plus,
    }
}

const DRIVE: char = '\u{f0a0}'; // nf-fa-hdd_o
const USB: char = '\u{f287}'; // nf-fa-usb
const PHONE: char = '\u{f10b}'; // nf-fa-mobile
const CAMERA: char = '\u{f030}'; // nf-fa-camera
const PLUS: char = '\u{f067}'; // nf-fa-plus

/// The icon for one row.
///
/// `theme` supplies the user's rules — `[[icon.dir]]`'s nineteen and whatever
/// `[[icon.file]]` adds; `palette` supplies the colours for everything else.
/// Whether the glyph or its [`Mark`] is drawn is [`paint`]'s to decide.
///
/// ## The order of the four answers
///
/// 1. **A themed directory** — `~/Work` is the Work folder, and stays the Work
///    folder when it is reached through a symlink.
/// 2. **A user `[[icon.file]]` rule**, first match wins. Written rules outrank
///    the built-in table by definition; that is what they are for.
/// 3. **An extension the set has a logo for** — the glyph only, so the hue
///    stays the kind's and a folder of source still reads as one thing.
/// 4. **The kind**, which always answers.
///
/// A **broken** link short-circuits all four: nothing about what a name claims
/// is true of a link that points at nothing.
pub fn icon_for(entry: &Entry, theme: &Theme, palette: &Palette) -> Icon {
    let broken = entry.is_broken_symlink();
    let link = entry.is_symlink();
    let dir = entry.is_dir();
    // Read, not computed: the scan settled it (see [`Entry::file_kind`]), so
    // the icon column and the name colour no longer classify the same row twice
    // on every frame.
    let kind = entry.file_kind;

    // A themed directory icon outranks the generic ones — including for a
    // symlink *to* a directory, because `~/Work` being a link does not make it
    // less the Work folder.
    let themed = if dir && !broken {
        theme.dir_icon(&entry.path.to_string_lossy(), &entry.name)
    } else {
        None
    };
    let ruled = if dir || broken {
        None
    } else {
        theme.file_icon(&entry.name)
    };

    let color = if broken {
        palette.red
    } else if let Some(fg) = themed.and_then(|i| i.fg) {
        // A shipped colour was picked for a dark ground; on a light one it
        // gives way to the flavour's accent of the same hue (see
        // [`df_core::config::DirIcon::light`]). A written one is the writer's.
        themed
            .and_then(|i| i.light)
            .filter(|_| palette.light)
            .and_then(|name| palette.named(name))
            .unwrap_or_else(|| to_color32(fg))
    } else if let Some(fg) = ruled.and_then(|i| i.fg) {
        to_color32(fg)
    } else if link && !dir {
        // A link keeps the link colour even though its glyph says what it
        // points at: the glyph answers "what is this" and the colour answers
        // "is it really here", and both questions are worth one channel.
        palette.sky
    } else {
        kind_color(kind, palette)
    };

    // What `ls -F` told apart, drawn ([`crate::marks`]): a folder, a link, a
    // thing that runs, and everything else a page.
    let mark = if dir {
        Mark::Folder
    } else if link {
        Mark::Link
    } else if kind.is_runnable() {
        Mark::Exec
    } else {
        Mark::File
    };

    let glyph = if broken {
        BROKEN_LINK
    } else if let Some(icon) = themed {
        icon.text
    } else if let Some(icon) = ruled {
        icon.text
    } else if dir {
        GENERIC_DIR
    } else if link && kind == FileKind::Binary {
        // Nothing about the name said what this is — but it *is* a link, and
        // that is a better thing to draw than the shrug the generic file glyph
        // would be.
        GENERIC_LINK
    } else {
        extension_glyph(&entry.name, kind).unwrap_or_else(|| kind_glyph(kind))
    };

    Icon { glyph, color, mark }
}

/// The colour a row's *name* is drawn in.
///
/// yazi's rules, which are the ones Brian's eye is trained on — directories
/// blue, broken links red — plus two of this program's own, and the two are the
/// reason this is not just `palette.text`:
///
/// - **Executables are green.** `ls` has said so for thirty years, and it is
///   the one property of a file that changes what pressing `Enter` on it does.
/// - **Configuration is a step quieter.** A directory listing is half lockfiles
///   and dotfiles, and they are never what you came for. `subtext0` is one step
///   down the palette's own ramp: legible at a glance, and not competing with
///   the row above it.
///
/// Everything else stays `text`. That restraint is the point (PLAN §8): the
/// *icon* carries the kind, in colour, and a name tinted to match would say the
/// same thing twice at the cost of a listing that reads like a paint chart.
pub fn name_color(entry: &Entry, palette: &Palette) -> egui::Color32 {
    // A name is type, so its accent is the accent as ink: itself on a dark
    // palette, a quiet navy, wine or forest on a light one (crate::theme::ink).
    if entry.is_broken_symlink() {
        return crate::theme::ink(palette, palette.red);
    }
    if entry.is_dir() {
        return crate::theme::ink(palette, palette.blue);
    }
    match entry.file_kind {
        // A socket, fifo or device node: real, listed, and not openable. Given
        // its own colour so `→` on one is visibly not going to do anything.
        FileKind::Special => palette.overlay2,
        FileKind::Executable => crate::theme::ink(palette, palette.green),
        FileKind::Config => palette.subtext0,
        _ => palette.text,
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
            len: 10,
            mtime: None,
            btime: None,
            mode: 0o644,
            uid: 0,
            gid: 0,
            is_hidden: name.starts_with('.'),
            mime: df_core::fs::mime::hint_for_name(name),
            file_kind: df_core::fs::classify(
                kind,
                name,
                df_core::fs::mime::hint_for_name(name),
                0o644,
            ),
            tags: Vec::new(),
        }
    }

    fn file(name: &str) -> Entry {
        entry(name, Kind::File)
    }

    /// One frame at `ppp`, and the shapes `draw` painted in it.
    pub(super) fn frame(
        ctx: &egui::Context,
        ppp: f32,
        mut draw: impl FnMut(&egui::Painter),
    ) -> Vec<egui::Shape> {
        let mut input = egui::RawInput::default();
        input.viewports.insert(
            egui::ViewportId::ROOT,
            egui::ViewportInfo {
                native_pixels_per_point: Some(ppp),
                ..Default::default()
            },
        );
        ctx.run_ui(input, |ui| draw(ui.painter()))
            .shapes
            .into_iter()
            .map(|clipped| clipped.shape)
            .filter(|shape| !matches!(shape, egui::Shape::Noop))
            .collect()
    }

    /// The face is picked by name from the platform's font directories: the
    /// most preferred first whichever directory it is in, a patched face the
    /// list does not name only when nothing it names is there, and an
    /// unpatched one never.
    #[test]
    fn the_icon_face_is_found_by_preference_across_directories() {
        let tree = df_core::test_support::TempTree::new("icon-font");
        let user = tree.dir("user");
        let system = tree.dir("system");
        let found = || find_nerd_font_in(vec![user.clone(), system.clone()]).map(|(path, _)| path);

        tree.file("system/Helvetica.ttf", b"x");
        assert_eq!(found(), None, "nothing patched");

        tree.file("system/SomeOtherNerdFont-Regular.otf", b"x");
        assert_eq!(found(), Some(system.join("SomeOtherNerdFont-Regular.otf")));

        // What `brew install --cask font-symbols-only-nerd-font` puts in
        // `~/Library/Fonts`.
        tree.file("user/SymbolsNerdFontMono-Regular.ttf", b"x");
        assert_eq!(found(), Some(user.join("SymbolsNerdFontMono-Regular.ttf")));

        tree.file("system/JetBrainsMonoNerdFont-Regular.ttf", b"x");
        assert_eq!(
            found(),
            Some(system.join("JetBrainsMonoNerdFont-Regular.ttf")),
            "a later directory's more preferred face wins"
        );
    }

    /// The nineteen ported rules have to actually reach a row — that is the
    /// whole reason `theme.toml` was transcribed.
    #[test]
    fn a_themed_directory_gets_its_own_glyph_and_colour() {
        let theme = Theme::default();
        let palette = Palette::from_theme(&theme, df_core::config::Appearance::Dark);
        let icon = icon_for(&entry("Work", Kind::Dir), &theme, &palette);
        assert_eq!(icon.glyph, '\u{f0b1}');
        assert_eq!(icon.color, egui::Color32::from_rgb(0xf7, 0x76, 0x8e));

        // On the light side the yazi colour gives way to latte's own red, and
        // a rule somebody wrote keeps the colour they wrote.
        let light = Palette::from_theme(&theme, df_core::config::Appearance::Light);
        let icon = icon_for(&entry("Work", Kind::Dir), &theme, &light);
        assert_eq!(icon.glyph, '\u{f0b1}');
        assert_eq!(icon.color, light.red);
        let (written, _) = Theme::parse(
            "[[icon.dir]]\nname = \"Work\"\ntext = \"W\"\nfg = \"#f7768e\"\n",
            std::path::Path::new("theme.toml"),
        );
        let icon = icon_for(&entry("Work", Kind::Dir), &written, &light);
        assert_eq!(icon.color, egui::Color32::from_rgb(0xf7, 0x76, 0x8e));
    }

    #[test]
    fn a_plain_directory_falls_back_to_the_generic_glyph_in_blue() {
        let theme = Theme::default();
        let palette = Palette::from_theme(&theme, df_core::config::Appearance::Dark);
        let icon = icon_for(&entry("scratch", Kind::Dir), &theme, &palette);
        assert_eq!(icon.glyph, GENERIC_DIR);
        assert_eq!(icon.color, palette.blue);
    }

    /// A broken link is red everywhere — icon and name both, the way yazi
    /// draws it.
    #[test]
    fn a_broken_symlink_is_red() {
        let theme = Theme::default();
        let palette = Palette::from_theme(&theme, df_core::config::Appearance::Dark);
        let e = entry("gone", Kind::Symlink { target: None });
        assert_eq!(icon_for(&e, &theme, &palette).color, palette.red);
        assert_eq!(name_color(&e, &palette), palette.red);
    }

    /// The feature in one assertion: a folder of mixed downloads draws a
    /// different picture, in a different colour, for every family in it.
    #[test]
    fn a_downloads_folder_is_legible_at_a_glance() {
        let theme = Theme::default();
        let p = Palette::from_theme(&theme, df_core::config::Appearance::Dark);
        let cases: &[(&str, egui::Color32)] = &[
            ("holiday.jpg", p.mauve),
            ("IMG_4821.HEIC", p.mauve),
            ("clip.mov", p.pink),
            ("song.flac", p.teal),
            ("release.zip", p.peach),
            ("arch.iso", p.peach),
            ("invoice.pdf", p.maroon),
            ("main.rs", p.yellow),
            ("Zed.AppImage", p.green),
            ("package.json", p.sky),
            ("notes.txt", p.subtext0),
            (".zshrc", p.quiet),
        ];
        let mut glyphs = std::collections::HashSet::new();
        for (name, colour) in cases {
            let icon = icon_for(&file(name), &theme, &p);
            assert_eq!(icon.color, *colour, "colour for {name}");
            glyphs.insert(icon.glyph);
        }
        assert!(glyphs.len() >= 10, "only {} distinct glyphs", glyphs.len());
    }

    /// The hues are a closed set, and `red` is not in it: red means broken.
    #[test]
    fn the_kind_palette_stays_small_and_never_claims_red() {
        let p = Palette::default();
        let kinds = [
            FileKind::Directory,
            FileKind::Image,
            FileKind::Video,
            FileKind::Audio,
            FileKind::Archive,
            FileKind::DiskImage,
            FileKind::Document,
            FileKind::Markdown,
            FileKind::Text,
            FileKind::Code,
            FileKind::Data,
            FileKind::Config,
            FileKind::Executable,
            FileKind::Font,
            FileKind::Model3d,
            FileKind::Binary,
            FileKind::Special,
        ];
        let mut hues = std::collections::HashSet::new();
        for kind in kinds {
            let colour = kind_color(kind, &p);
            assert_ne!(colour, p.red, "{kind:?} took the broken colour");
            hues.insert(colour);
        }
        assert!(
            hues.len() <= 12,
            "{} distinct colours is a rainbow",
            hues.len()
        );
    }

    /// A language's own mark, in its kind's colour — so a directory of source
    /// is still one colour with several pictures in it.
    #[test]
    fn an_extension_changes_the_glyph_and_not_the_hue() {
        let theme = Theme::default();
        let p = Palette::from_theme(&theme, df_core::config::Appearance::Dark);
        let rust = icon_for(&file("main.rs"), &theme, &p);
        let python = icon_for(&file("train.py"), &theme, &p);
        assert_ne!(rust.glyph, python.glyph);
        assert_eq!(rust.color, python.color);
        assert_eq!(rust.color, p.yellow);
        // A language the table has no logo for still gets the kind's glyph.
        assert_eq!(
            icon_for(&file("build.zig"), &theme, &p).glyph,
            kind_glyph(FileKind::Code)
        );
    }

    /// `[[icon.file]]` outranks the built-in table — glyph and colour both.
    #[test]
    fn a_user_file_rule_wins() {
        let (theme, warnings) = Theme::parse(
            "[[icon.file]]\nname = \"*.rs\"\ntext = \"R\"\nfg = \"#ffffff\"\n",
            std::path::Path::new("theme.toml"),
        );
        assert!(warnings.is_empty(), "{warnings:?}");
        let p = Palette::from_theme(&theme, df_core::config::Appearance::Dark);
        let icon = icon_for(&file("main.rs"), &theme, &p);
        assert_eq!(icon.glyph, 'R');
        assert_eq!(icon.color, egui::Color32::WHITE);
        // …and only for the rows it matches.
        assert_eq!(icon_for(&file("train.py"), &theme, &p).color, p.yellow);
    }

    /// A link's glyph says what it points at and its colour says it is a link:
    /// two facts, two channels, neither one spent on the other.
    #[test]
    fn a_link_keeps_its_colour_and_borrows_its_targets_glyph() {
        let theme = Theme::default();
        let p = Palette::from_theme(&theme, df_core::config::Appearance::Dark);
        let link = entry(
            "shot.png",
            Kind::Symlink {
                target: Some(LinkTarget::File),
            },
        );
        assert_eq!(icon_for(&link, &theme, &p).color, p.sky);
        assert_eq!(
            icon_for(&link, &theme, &p).glyph,
            kind_glyph(FileKind::Image)
        );

        // …and when the name says nothing at all, the link glyph is the best
        // thing left to draw.
        let opaque = entry(
            "socket-ish",
            Kind::Symlink {
                target: Some(LinkTarget::File),
            },
        );
        assert_eq!(icon_for(&opaque, &theme, &p).glyph, GENERIC_LINK);
    }

    /// Names stay `text` almost always — the icon carries the kind. The two
    /// exceptions are the two facts a *name* is the right place for.
    #[test]
    fn only_salience_tints_a_name() {
        let p = Palette::default();
        assert_eq!(name_color(&file("holiday.jpg"), &p), p.text);
        assert_eq!(name_color(&file("invoice.pdf"), &p), p.text);
        assert_eq!(name_color(&file("main.rs"), &p), p.text);
        assert_eq!(name_color(&file("deploy.sh"), &p), p.green);
        assert_eq!(name_color(&file(".gitignore"), &p), p.subtext0);
        assert_eq!(name_color(&file("Cargo.lock"), &p), p.subtext0);
        assert_eq!(name_color(&entry("src", Kind::Dir), &p), p.blue);
    }

    /// Without a patched font a row is drawn as what `ls -F` told apart: a
    /// folder, a link (a broken one too), a thing that runs, and a page for
    /// everything else, a socket and a pipe included.
    #[test]
    fn a_row_is_drawn_as_what_the_classifier_told_apart() {
        let theme = Theme::default();
        let palette = Palette::from_theme(&theme, df_core::config::Appearance::Dark);
        for (name, kind, mark) in [
            ("Work", Kind::Dir, Mark::Folder),
            ("scratch", Kind::Dir, Mark::Folder),
            ("notes.txt", Kind::File, Mark::File),
            ("holiday.jpg", Kind::File, Mark::File),
            ("deploy.sh", Kind::File, Mark::Exec),
            (
                "link",
                Kind::Symlink {
                    target: Some(LinkTarget::File),
                },
                Mark::Link,
            ),
            ("gone", Kind::Symlink { target: None }, Mark::Link),
            (
                "to-dir",
                Kind::Symlink {
                    target: Some(LinkTarget::Dir),
                },
                Mark::Folder,
            ),
        ] {
            let icon = icon_for(&entry(name, kind), &theme, &palette);
            assert_eq!(icon.mark, mark, "{name}");
        }
    }

    /// …and the themed rule still colours the row when its glyph cannot be
    /// drawn, so a fontless machine keeps the folder colours, and the kinds'
    /// too — the half of the treatment that does not need a font at all.
    #[test]
    fn colours_survive_a_missing_font() {
        let theme = Theme::default();
        let palette = Palette::from_theme(&theme, df_core::config::Appearance::Dark);
        let icon = icon_for(&entry("Work", Kind::Dir), &theme, &palette);
        assert_eq!(icon.mark, Mark::Folder);
        assert_eq!(icon.color, egui::Color32::from_rgb(0xf7, 0x76, 0x8e));
        assert_eq!(
            icon_for(&file("holiday.jpg"), &theme, &palette).color,
            palette.mauve
        );
    }

    /// With an icon face, [`paint`] draws exactly the text the icon column
    /// drew before there was any other way: the same shapes as the
    /// `painter.text` call each caller made, at a list's size and a tile's,
    /// at every scale. The face here is egui's own monospace one bound to
    /// the icon family, so this holds on a machine with no Nerd Font too;
    /// [`super::glyph_tests`] repeats it with the installed face where there
    /// is one.
    #[test]
    fn with_a_face_the_icon_is_the_text_it_was() {
        let mut fonts = egui::FontDefinitions::default();
        let mono = fonts.families[&egui::FontFamily::Monospace][0].clone();
        fonts
            .families
            .insert(egui::FontFamily::Name(ICON_FAMILY.into()), vec![mono]);
        let ctx = egui::Context::default();
        ctx.set_fonts(fonts);
        let theme = Theme::default();
        let palette = Palette::from_theme(&theme, df_core::config::Appearance::Dark);
        let icon = icon_for(&entry("scratch", Kind::Dir), &theme, &palette);
        for ppp in [1.0, 1.25, 2.0] {
            for (anchor, size) in [
                (egui::Align2::LEFT_CENTER, 12.5),
                (egui::Align2::CENTER_CENTER, 44.0),
            ] {
                let at = egui::pos2(7.0, 11.0);
                let font = egui::FontId::new(size, family());
                let ours = frame(&ctx, ppp, |painter| {
                    paint(painter, at, anchor, icon, font.clone(), true, icon.color);
                });
                let before = frame(&ctx, ppp, |painter| {
                    painter.text(at, anchor, icon.glyph, font.clone(), icon.color);
                });
                assert_eq!(ours, before, "{size} at {ppp}");
                assert!(!ours.is_empty());
            }
        }
    }

    /// Every extension the glyph table names is spelled the way the lookup
    /// spells it, and named once.
    #[test]
    fn the_glyph_table_is_lowercase_and_unique() {
        let mut seen = std::collections::HashSet::new();
        for (ext, _, _) in EXTENSION_GLYPHS {
            assert!(seen.insert(*ext), "{ext} appears twice");
            assert_eq!(*ext, ext.to_lowercase(), "{ext} is not lowercase");
        }
    }

    /// **The contract of the third column.** Every row's declared kind is the
    /// kind df-core actually gives a plain `file.<ext>`, so the glyph table can
    /// never state something the classifier disagrees with. A row that drifts
    /// fails here rather than in a listing.
    #[test]
    fn the_glyph_table_agrees_with_the_kind_table() {
        let mut wrong = Vec::new();
        for (ext, _, declared) in EXTENSION_GLYPHS {
            let name = format!("sample.{ext}");
            let actual = df_core::fs::kind_of(&file(&name));
            if actual != *declared {
                wrong.push(format!(
                    "{ext}: table says {declared:?}, df-core says {actual:?}"
                ));
            }
        }
        assert!(wrong.is_empty(), "{wrong:#?}");
    }

    /// The three rows the third column exists for: a name whose *kind* is not
    /// what its extension would suggest keeps its kind's glyph.
    #[test]
    fn a_glyph_never_overrules_the_classifier() {
        let theme = Theme::default();
        let palette = Palette::from_theme(&theme, df_core::config::Appearance::Dark);
        let glyph = |name: &str| icon_for(&file(name), &theme, &palette).glyph;

        // `app.ts` is TypeScript; `00001.ts` is a transport stream, and the
        // TypeScript logo on it was the bug.
        assert_eq!(glyph("app.ts"), '\u{e628}');
        assert_eq!(glyph("00001.ts"), kind_glyph(FileKind::Video));

        // A lockfile is configuration whatever it is written in.
        assert_eq!(glyph("package-lock.json"), kind_glyph(FileKind::Config));
        assert_eq!(glyph("tsconfig.json"), '\u{e60b}');

        // …and the hidden-file case, which is where the two rules meet.
        // df-core demotes an unrecognised dotfile to `Config`, and that
        // demotion used to catch `Code` as well — so `.eslintrc.js` classified
        // as configuration, the `("js", …, Code)` row of `EXTENSION_GLYPHS`
        // refused to match a `Config`, and the icon column drew a cog on a file
        // whose whole name says JavaScript. A dotfile with a known *code*
        // extension keeps its language.
        assert_eq!(glyph(".eslintrc.js"), '\u{e781}');
        assert_eq!(glyph(".babelrc.ts"), '\u{e628}');
        // A dotfile with nothing to go on is still configuration — the rule
        // that demotion exists for, and the one this must not undo.
        assert_eq!(glyph(".zshrc"), kind_glyph(FileKind::Config));
        assert_eq!(glyph(".gitconfig"), kind_glyph(FileKind::Config));
        // …and the all-digit stem stays a transport stream, hidden or not:
        // `media_kind` is asked before the language table and before any of
        // this (`00001.ts`, above).
        assert_eq!(glyph(".2.ts"), kind_glyph(FileKind::Video));
    }
}

#[cfg(test)]
mod glyph_tests {
    /// Every glyph the chrome sets in the stock faces has to be drawn, patched
    /// font or not: by a face, or — for the few symbols no stock face has — by
    /// [`crate::glyphs`]' stand-in. The missing-glyph box is what `⑂` and `⌕`
    /// used to show, and this is the test that would have caught them. Both
    /// families are asked, since keys are set in the monospace one. The
    /// patched-font icons are checked too when a Nerd Font is on the machine,
    /// and skipped when it is not; they are pictures, so only a face will do.
    #[test]
    fn the_chrome_glyphs_all_render() {
        let ctx = egui::Context::default();
        let nerd = super::install(&ctx);
        let _ = ctx.run_ui(Default::default(), |_| {});
        let plain = [
            'Y', 'f', '⊞', '☰', '✓', '☐', '•', '◂', '▣', '▸', '›', '…', '×', '·', '→', '↑', '↓',
            '←', '⇧', '≈', '≡', '⟷',
            // The permissions card's mixed octal digit, and its `0–7` hint.
            '–',
        ];
        for font in [
            egui::FontId::proportional(14.0),
            egui::FontId::monospace(14.0),
        ] {
            for c in plain {
                assert!(
                    crate::glyphs::drawn(&ctx, &font, c),
                    "nothing draws {c:?} (U+{:04X}) in {:?}",
                    c as u32,
                    font.family
                );
            }
        }
        let patched = [
            '\u{f418}',
            '\u{f0b0}',
            '\u{f01c}',
            super::LOCK,
            '\u{f1c6}',
            // The app menu's button and its ticks.
            '\u{f0c9}',
            '\u{f00c}',
            super::GENERIC_DIR,
            super::GENERIC_FILE,
            super::NETWORK_PLACE,
            // The Places card's rows.
            super::DRIVE,
            super::USB,
            super::PHONE,
            super::CAMERA,
            super::PLUS,
        ];
        let font = egui::FontId::proportional(14.0);
        for c in patched.iter().filter(|_| nerd) {
            assert!(
                crate::glyphs::drawn_by_a_face(&ctx, &font, *c),
                "no face draws {c:?} (U+{:04X})",
                *c as u32
            );
        }
    }

    /// **A machine with a Nerd Font draws the icon column it drew** (W4.42):
    /// with the installed face, every icon [`super::paint`] draws — a row's,
    /// a tile's, a Places row's in the proportional family — is the same
    /// shapes, pixel for pixel, as the `painter.text` the column drew before
    /// marks existed. Skipped, saying so, where no Nerd Font is installed:
    /// there is nothing to compare, and the marks are what is drawn.
    #[test]
    fn with_a_nerd_font_the_icon_column_is_unchanged() {
        use df_core::fs::{Entry, Kind};
        let ctx = egui::Context::default();
        if !super::install(&ctx) {
            eprintln!("skipped: no Nerd Font on this machine");
            return;
        }
        let theme = df_core::config::Theme::default();
        let palette = crate::theme::Palette::from_theme(&theme, df_core::config::Appearance::Dark);
        let row = |name: &str, kind: Kind| {
            let mime = df_core::fs::mime::hint_for_name(name);
            let entry = Entry {
                name: name.to_string(),
                path: std::path::PathBuf::from("/home/brian").join(name),
                kind,
                len: 10,
                mtime: None,
                btime: None,
                mode: 0o644,
                uid: 0,
                gid: 0,
                is_hidden: false,
                mime,
                file_kind: df_core::fs::classify(kind, name, mime, 0o644),
                tags: Vec::new(),
            };
            super::icon_for(&entry, &theme, &palette)
        };
        let icons = [
            row("Work", Kind::Dir),
            row("scratch", Kind::Dir),
            row("main.rs", Kind::File),
            row("holiday.jpg", Kind::File),
            row("deploy.sh", Kind::File),
            super::drive(&palette, true),
            super::network(&palette),
            super::archive(&palette),
        ];
        for ppp in [1.0, 1.25, 2.0] {
            for icon in icons {
                for (anchor, font) in [
                    (
                        egui::Align2::LEFT_CENTER,
                        egui::FontId::new(12.5, super::family()),
                    ),
                    (
                        egui::Align2::CENTER_CENTER,
                        egui::FontId::new(44.0, super::family()),
                    ),
                    (
                        egui::Align2::CENTER_CENTER,
                        egui::FontId::proportional(14.0),
                    ),
                ] {
                    let at = egui::pos2(7.0, 11.0);
                    let ours = super::tests::frame(&ctx, ppp, |painter| {
                        super::paint(painter, at, anchor, icon, font.clone(), true, icon.color);
                    });
                    let before = super::tests::frame(&ctx, ppp, |painter| {
                        painter.text(at, anchor, icon.glyph, font.clone(), icon.color);
                    });
                    assert_eq!(ours, before, "{:?} at {ppp}", icon.glyph);
                }
            }
        }
    }
}
