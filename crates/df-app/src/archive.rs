//! Archives browsed as directories (PLAN §7.3).
//!
//! ## The whole trick: an archive is a listing
//!
//! There is almost no new machinery here, and that is the design. `→` on a
//! `.zip` does not open an archive *viewer*; it puts a different set of rows in
//! the list pane. Those rows are [`df_core::fs::Entry`] values built from an
//! [`ArchiveTree`] and handed to a [`df_core::fs::DirState`] through
//! [`df_core::fs::DirState::set_entries`], so every single thing the list pane already does
//! keeps working inside an archive without knowing where the rows came from:
//! the cursor, scrolloff, the scroll tween, `↑`/`↓`, `gg`/`G`, paging, the
//! dir-first sort, `,` re-sorts with their FLIP animation, `f` filter, `/` find,
//! `Space` selection, visual mode, band select, hover, ripples, the icon table,
//! the linemode column, the grid, and the breadcrumb.
//!
//! The alternative — a bespoke overlay with its own cursor and its own painter —
//! would be a second list pane that slowly grows a different answer to every one
//! of those questions. This way there is one list pane and one set of rules.
//!
//! ## The paths are fictions, on purpose
//!
//! A row inside `~/dl/src.zip` at `src/main.rs` gets the path
//! `~/dl/src.zip/src/main.rs`, which does not exist. It is a *display* path and
//! a *key*: it makes the breadcrumb read correctly, it makes `←` and `→` plain
//! path arithmetic, and [`Browse::inner`] turns it back into the archive-inner
//! path the tree is indexed by. Nothing in this module ever hands one of those
//! paths to the filesystem, and [`Browse::real`] is the one function that maps
//! back to something that does exist.
//!
//! ## Read-only, and said out loud
//!
//! v1 has no cross-archive operations: no yank out, no paste in, no rename, no
//! trash. Writing into a zip means rebuilding its central directory, and copying
//! *out* of one means a destination dialog, a conflict flow and a journal record
//! for something that was never a file — a second paste engine, for the case
//! that "extract" already covers. So every mutating command is inert while the
//! list is inside an archive, and inert *with a notice*: a key that silently
//! does nothing is a key the user presses twice.
//!
//! The way out is [`crate::app::App::extract`] — "Extract here", "Extract to
//! folder", or `Enter` on a selection, all of which go through
//! [`df_core::archive::plan_extract`] and the ops job, and therefore land in the
//! journal like every other operation.
//!
//! ## Extracting what cannot be browsed
//!
//! 7z, rar and every multi-part set have no reader here, so they cannot be
//! walked into — but they can be extracted, by 7-Zip or `bsdtar`
//! ([`df_core::archive::external`]). [`units`] is the step that turns a
//! selection into the things to extract: it folds the pieces of a set into
//! one, found by looking at the whole directory rather than only at what was
//! selected.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use df_core::archive::{ArchiveEntry, ArchiveTree};
use df_core::fs::{Entry, Kind};

/// How much of a text entry is decompressed for the preview card.
///
/// 1 MiB, and the cap is the reason the preview can be synchronous with a
/// cursor move: an entry under it is one seek and one inflate, and an entry over
/// it gets the facts card with no body — which is also what a binary entry gets,
/// so there is one "no preview" picture rather than two.
pub const PREVIEW_LIMIT: usize = 1024 * 1024;

/// The archive the list pane is currently inside.
///
/// Held on the [`crate::tab::Tab`] rather than on the app, because it is a
/// property of *where this tab is*: a second tab can be in a real directory, or
/// in a different archive, and switching between them has to switch this too.
pub struct Browse {
    /// The archive file itself. The one path in here that exists.
    pub path: PathBuf,
    /// The listing, shared with the preview card so that hovering a row costs
    /// no re-read. `Arc` because a tab switch and a re-sort both clone it and
    /// the tree of a large archive is megabytes.
    pub tree: Arc<ArchiveTree>,
}

impl Browse {
    /// The display path for `inner` — what the list pane's [`df_core::fs::DirState`] is
    /// called and what the breadcrumb draws.
    pub fn display_path(&self, inner: &str) -> PathBuf {
        if inner.is_empty() {
            self.path.clone()
        } else {
            self.path.join(inner)
        }
    }

    /// The archive-inner path for a display path, or `None` when that path is
    /// not inside this archive.
    ///
    /// The archive's own path maps to `""`, the tree's root.
    pub fn inner(&self, display: &Path) -> Option<String> {
        if display == self.path {
            return Some(String::new());
        }
        let rest = display.strip_prefix(&self.path).ok()?;
        Some(rest.to_string_lossy().replace('\\', "/"))
    }

    /// The real directory the archive lives in — where `←` at the archive's
    /// root goes, and where "Extract here" puts things.
    pub fn real(&self) -> PathBuf {
        self.path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("/"))
    }

    /// The rows of one directory inside the archive.
    pub fn rows(&self, inner: &str) -> Vec<Entry> {
        rows(&self.tree, inner, &self.path)
    }
}

/// One archive directory's rows, as list-pane entries.
///
/// Pure: an [`ArchiveTree`] and a path in, a `Vec<Entry>` out. That is what
/// makes the mapping — sizes, dates, the directory bit, the fictional path —
/// a table test rather than something you have to open a zip to check.
///
/// ## What is faked, and what is not
///
/// - **`mode`** is a plausible `0o644`/`0o755`. Zip stores a real unix mode in
///   its external attributes and tar stores one too, but [`ArchiveEntry`] does
///   not carry it, and the alternative to a plausible constant is a permissions
///   column full of `?????????`. The number is only ever *shown*; nothing acts
///   on it, because nothing inside an archive can be acted on.
/// - **`uid`/`gid`** are zero for the same reason, and the owner linemode
///   renders that as `root`, which is honest about being uninformative.
/// - **`mtime`** is real when the archive carried one. `btime` never is: no
///   archive format records a creation time, so the column is empty rather than
///   a copy of the modification time pretending to be one.
/// - **`len`** is the *uncompressed* size, because that is the size of the thing
///   the row names. The compressed size is on the preview card, where there is
///   room to label it.
pub fn rows(tree: &ArchiveTree, inner: &str, archive: &Path) -> Vec<Entry> {
    tree.entries(inner)
        .into_iter()
        .map(|entry| row(&entry, archive))
        .collect()
}

/// One entry, as a row.
pub fn row(entry: &ArchiveEntry, archive: &Path) -> Entry {
    let path = if entry.path.is_empty() {
        archive.to_path_buf()
    } else {
        archive.join(&entry.path)
    };
    let mime = df_core::fs::mime::hint_for_name(&entry.name);
    let kind = if entry.is_dir { Kind::Dir } else { Kind::File };
    let mode = if entry.is_dir { 0o040_755 } else { 0o100_644 };
    Entry {
        is_hidden: entry.name.starts_with('.'),
        name: entry.name.clone(),
        path,
        kind,
        len: if entry.is_dir { 0 } else { entry.len },
        mtime: entry.mtime.and_then(unix_time),
        btime: None,
        mode,
        uid: 0,
        gid: 0,
        mime,
        file_kind: df_core::fs::classify(kind, &entry.name, mime, mode),
        tags: Vec::new(),
    }
}

/// A unix timestamp as a [`SystemTime`], tolerating the negative ones a 1980s
/// archive can carry.
fn unix_time(secs: i64) -> Option<SystemTime> {
    if secs >= 0 {
        UNIX_EPOCH.checked_add(Duration::from_secs(secs as u64))
    } else {
        UNIX_EPOCH.checked_sub(Duration::from_secs(secs.unsigned_abs()))
    }
}

/// The badge one entry wears in the list and on the card, or nothing.
///
/// Two states, both of which mean "this row is not going to come out of the
/// archive", and they are worth saying before the user selects forty files and
/// presses Enter — not after, in a summary toast.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Badge {
    /// The name is absolute, traverses, or collides with `.git`.
    Unsafe,
    /// Needs a password this build has no way to ask for.
    Encrypted,
}

impl Badge {
    pub fn of(entry: &ArchiveEntry) -> Option<Badge> {
        if entry.unsafe_name {
            Some(Badge::Unsafe)
        } else if entry.encrypted {
            Some(Badge::Encrypted)
        } else {
            None
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Badge::Unsafe => "unsafe path",
            Badge::Encrypted => "encrypted",
        }
    }
}

/// Whether a row in a *real* directory is worth trying to open as an archive.
///
/// A name test, not a bytes test, and deliberately so: this runs on the `→` and
/// `Enter` of every row in the window, and reading the head of a file to find
/// out whether the arrow key should enter it would be a syscall on every
/// keystroke. Being wrong is cheap in both directions — a mis-named archive
/// simply opens with its opener instead, and a `.zip` that is not one produces
/// one "not an archive" notice from the listing job, which is exactly the
/// message the user needs.
///
/// The formats are the ones [`df_core::archive`] can actually list. `.7z`,
/// `.rar` and `.bz2` are left out on purpose: they have no reader here, and a
/// row that walked into a spinner and then said "unsupported" would be worse
/// than one that opens with the system's archiver. They can still be
/// *extracted* — see [`looks_like_archive_name`] — just not browsed.
pub fn looks_like_archive(entry: &Entry) -> bool {
    !entry.is_dir()
        && matches!(
            entry.mime,
            "application/zip"
                | "application/epub+zip"
                | "application/vnd.comicbook+zip"
                | "application/x-tar"
                | "application/gzip"
                | "application/x-xz"
                | "application/zstd"
        )
}

/// Whether a row's *name* says it is something an extractor can open — the
/// half of "is this an archive" that [`looks_like_archive`] leaves out.
///
/// Every extension the shipped opener rule for archives names, plus every
/// piece of a multi-part set (`.z01`, `.r00`, `.7z.001`, `.tar.gz.002`),
/// whose middle pieces have no mime a sniffer could find. Used with
/// [`looks_like_archive`], never instead of it: that one decides what can be
/// *browsed*, and this one widens what can be *extracted* to the formats only
/// 7-Zip or `bsdtar` read.
pub fn looks_like_archive_name(name: &str) -> bool {
    const EXTENSIONS: &[&str] = &[
        "zip", "tar", "tgz", "gz", "bz2", "xz", "zst", "7z", "rar", "cbz", "cbr",
    ];
    let extension = name
        .rsplit_once('.')
        .filter(|(stem, _)| !stem.is_empty())
        .map(|(_, ext)| ext.to_ascii_lowercase());
    extension.is_some_and(|ext| EXTENSIONS.contains(&ext.as_str()))
        || df_core::archive::volume_of(name).is_some()
}

/// Whether "Extract" applies to this row at all: a file, and an archive by its
/// mime or by its name.
pub fn extractable(entry: &Entry) -> bool {
    !entry.is_dir() && (looks_like_archive(entry) || looks_like_archive_name(&entry.name))
}

/// The name of the folder "Extract to folder" makes: the archive's name with
/// its extension taken off, and a multi-part set's pieces all naming the same
/// folder. The rule is [`df_core::archive::archive_stem`]'s.
pub fn subfolder_name(archive: &Path) -> String {
    archive
        .file_name()
        .map(|n| df_core::archive::archive_stem(&n.to_string_lossy()))
        .unwrap_or_else(|| "extracted".to_string())
}

/// The three ways to extract a file in a real directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtractMode {
    /// `E`, `o`: a new folder per archive, named after it.
    Folder,
    /// `e`: into the directory the archive is in; several are merged there.
    Here,
    /// Several archives into one new folder named for what they share.
    Merged,
}

impl ExtractMode {
    /// The `builtin:` opener that asks for this. The names are the shipped
    /// opener ids, and a rule in `delightfile.toml` can use them the same way.
    pub fn of_builtin(builtin: &str) -> Option<ExtractMode> {
        match builtin {
            "extract" => Some(ExtractMode::Folder),
            "extract-here" => Some(ExtractMode::Here),
            "extract-merged" => Some(ExtractMode::Merged),
            _ => None,
        }
    }
}

/// One thing an extraction extracts: an archive, or a whole multi-part set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unit {
    /// The file the extraction is pointed at: the archive, or the set's head.
    pub head: PathBuf,
    /// The set's scheme, when this is a set.
    pub volumes: Option<df_core::archive::VolumeKind>,
    /// The reader here can list it, so it can take the browse-then-plan road.
    /// Never true of a set.
    pub listable: bool,
}

impl Unit {
    pub fn whole(&self) -> df_core::archive::Whole {
        df_core::archive::Whole {
            head: self.head.clone(),
            volumes: self.volumes.clone(),
        }
    }
}

/// A target, as [`units`] needs to know it.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub path: PathBuf,
    pub is_dir: bool,
    /// [`looks_like_archive`]: the reader here can list it.
    pub listable: bool,
    /// [`extractable`]: something can extract it.
    pub archive: bool,
}

/// What a request to extract some targets comes to.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Units {
    /// In the order the targets were given, each set once.
    pub units: Vec<Unit>,
    /// Pieces of a set whose head is not in the directory — nothing can
    /// extract them, and the toast says why.
    pub orphans: Vec<String>,
}

/// Fold targets into the things to extract.
///
/// `listing` is every name in the targets' directory, because the pieces of a
/// set are found by looking at their neighbours: selecting `photos.z02` alone
/// still extracts the whole of `photos.zip` + `.z01` + `.z02`, once, from its
/// head. Folders and files that are not archives drop out.
pub fn units(candidates: &[Candidate], listing: &[String]) -> Units {
    let sets = df_core::archive::volume_sets(listing);
    let mut out = Units::default();
    for candidate in candidates {
        if candidate.is_dir {
            continue;
        }
        let name = candidate
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        if let Some(set) = sets.iter().find(|set| set.contains(&name)) {
            let head = candidate.path.with_file_name(&set.head);
            if !out.units.iter().any(|unit| unit.head == head) {
                out.units.push(Unit {
                    head,
                    volumes: Some(set.kind.clone()),
                    listable: false,
                });
            }
            continue;
        }
        // A numbered piece with no set around it: its head is missing.
        if df_core::archive::volume_of(&name).is_some_and(|v| v.index != v.kind.head_index()) {
            out.orphans.push(name);
            continue;
        }
        if candidate.archive && !out.units.iter().any(|unit| unit.head == candidate.path) {
            out.units.push(Unit {
                head: candidate.path.clone(),
                volumes: None,
                listable: candidate.listable,
            });
        }
    }
    out
}

/// Whether this entry is worth trying to show the contents of.
///
/// Text only, and small only. A preview that decompressed a 300 MB member on a
/// cursor move would make `↓` cost a second, and one that rendered a binary
/// member would be a screen of replacement characters.
pub fn previewable(entry: &ArchiveEntry) -> bool {
    !entry.is_dir
        && !entry.encrypted
        && entry.len > 0
        && entry.len <= PREVIEW_LIMIT as u64
        && df_core::fs::mime::hint_for_name(&entry.name).starts_with("text/")
}

/// The facts card for one entry inside an archive, as label/value rows.
///
/// Pure, so what the card says is a table test. The rows it does *not* emit are
/// as deliberate as the ones it does: no permissions (the tree does not carry
/// them), no owner (likewise), and no "compressed" row for a stored entry,
/// where the number would be the size again.
pub fn card_rows(
    entry: &ArchiveEntry,
    format: df_core::archive::ArchiveFormat,
) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    rows.push((
        "Kind".to_string(),
        if entry.is_dir {
            format!("Folder in a {} archive", format.label())
        } else {
            format!("{} in a {} archive", entry.mime_label(), format.label())
        },
    ));
    if !entry.is_dir {
        rows.push(("Size".to_string(), crate::format::human_size(entry.len)));
        // Only when it means something: a stored entry's compressed size is
        // its size, and a row that repeats the row above it is noise.
        if entry.compressed != entry.len && entry.compressed > 0 {
            let saved = 100.0 - (entry.compressed as f64 / entry.len.max(1) as f64) * 100.0;
            rows.push((
                "Compressed".to_string(),
                format!(
                    "{} ({:.0}% smaller)",
                    crate::format::human_size(entry.compressed),
                    saved.max(0.0)
                ),
            ));
        }
        rows.push(("Method".to_string(), entry.method.label().to_string()));
    }
    if let Some(mtime) = entry.mtime.and_then(unix_time) {
        rows.push((
            "Modified".to_string(),
            crate::format::long_stamp(Some(mtime)),
        ));
    }
    if let Some(target) = &entry.link_target {
        rows.push(("Link to".to_string(), target.clone()));
    }
    rows.push(("In archive".to_string(), entry.path.clone()));
    if let Some(badge) = Badge::of(entry) {
        rows.push((
            "Refused".to_string(),
            format!("{} — this entry will not be extracted", badge.label()),
        ));
    }
    rows
}

/// Which commands are inert while the list pane is inside an archive.
///
/// PLAN §7.3 makes archives read-only in v1, and this is the list that says so.
/// The rule is not "everything that writes" but "everything whose subject would
/// have to be a path inside the archive" — so `u` still works (it is about the
/// journal, not about here), `Ctrl+p` still works, the sorts and the filter
/// still work, and `Enter` is *not* here because inside an archive it means
/// extract.
///
/// The shell commands are on the list for a subtler reason than the rest:
/// nothing stops `;` from running, but it would run in a working directory that
/// does not exist, and `fd`/`rg` would search it. A notice beats an empty result
/// the user has to work out the cause of.
pub fn inert_in_archive(command: df_core::keymap::Command) -> bool {
    use df_core::keymap::Command as C;
    matches!(
        command,
        C::Yank
            | C::YankCut
            | C::Paste
            | C::PasteForce
            | C::PasteSync
            | C::CopyToClipboard
            | C::CopyFileText
            | C::SymlinkAbsolute
            | C::SymlinkRelative
            | C::Hardlink
            | C::Trash
            | C::DeletePermanently
            | C::Create
            | C::Rename
            | C::RenameEmptyStem
            | C::Shell
            | C::ShellBlock
            | C::SearchName
            | C::SearchContent
            | C::OpenInteractive
            | C::DiskUsage
            | C::YankToggle
            // The rows are not files on the disk, and the archive `A` would
            // write goes into a directory this one only looks like.
            | C::ArchiveCreate
    )
}

// ── The preview card ────────────────────────────────────────────────────────

/// The card's inner padding, in logical points.
const CARD_PAD: f32 = 12.0;
/// One label/value row.
const CARD_ROW: f32 = 19.0;
const CARD_FONT: f32 = 13.0;
/// The card's title. `CARD_FONT + 2`, which is the step every other titled
/// surface in the window uses (`dialog`'s `FONT + 2.0`) — it was a bare `15.0`
/// that would not have tracked a change to the face below it.
const CARD_TITLE_FONT: f32 = CARD_FONT + 2.0;
/// The title's row: its own line plus the gap down to the rule under it.
const CARD_TITLE_ROW: f32 = 24.0;
/// How wide the label column is. Fixed rather than measured, so the values line
/// up down the card instead of stepping in and out with the longest word.
const CARD_LABEL: f32 = 86.0;
/// The body's line height and face size.
const BODY_LINE: f32 = 16.0;
const BODY_FONT: f32 = 12.0;

/// Draw the facts card for the entry under the cursor inside an archive.
///
/// The preview pane's job at every other moment is "show me this file"; there is
/// no file here, so it shows what is *known* — and then, when the entry is small
/// text, what is in it. Two states rather than one because the second is not
/// always affordable, and a pane that sometimes has a body and sometimes does
/// not must still read as the same pane (`delightful-ui` §11).
pub fn card(
    paint: &crate::ui::Painting<'_>,
    pane: egui::Rect,
    entry: &ArchiveEntry,
    format: df_core::archive::ArchiveFormat,
    body: Option<&str>,
) {
    let palette = paint.palette;
    let content = crate::ui::content_rect(pane);
    if !content.is_positive() {
        return;
    }
    let painter = paint.painter.with_clip_rect(content);

    // The name, on the pane's own ground: the card is the pane, not a panel
    // floating in it, so there is no second plate to make concentric.
    let mut y = content.top() + CARD_PAD;
    painter.text(
        egui::pos2(content.left() + CARD_PAD, y),
        egui::Align2::LEFT_TOP,
        &entry.name,
        egui::FontId::proportional(CARD_TITLE_FONT),
        palette.text,
    );
    y += CARD_TITLE_ROW;

    // A hairline under the title, inset to the padding on both sides so the
    // rule reads as belonging to the card rather than crossing it.
    painter.rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(content.left() + CARD_PAD, y),
            egui::pos2(content.right() - CARD_PAD, y + 1.0),
        ),
        0,
        palette.surface0,
    );
    y += CARD_PAD;

    let badge = Badge::of(entry);
    for (label, value) in card_rows(entry, format) {
        if y + CARD_ROW > content.bottom() {
            return;
        }
        let refused = label == "Refused";
        painter.text(
            egui::pos2(content.left() + CARD_PAD, y + CARD_ROW / 2.0),
            egui::Align2::LEFT_CENTER,
            &label,
            egui::FontId::proportional(CARD_FONT),
            palette.overlay1,
        );
        let room = content.right() - CARD_PAD - (content.left() + CARD_PAD + CARD_LABEL);
        let mut job = egui::text::LayoutJob::single_section(
            value,
            egui::TextFormat {
                font_id: egui::FontId::proportional(CARD_FONT),
                color: if refused { palette.red } else { palette.text },
                ..Default::default()
            },
        );
        job.wrap = egui::text::TextWrapping {
            max_width: room.max(0.0),
            max_rows: 1,
            break_anywhere: true,
            overflow_character: Some('…'),
        };
        let galley = painter.layout_job(job);
        painter.galley(
            egui::pos2(
                content.left() + CARD_PAD + CARD_LABEL,
                y + CARD_ROW / 2.0 - galley.size().y / 2.0,
            ),
            galley,
            palette.text,
        );
        y += CARD_ROW;
    }

    y += CARD_PAD;
    match body {
        Some(text) => {
            for line in text.lines() {
                if y + BODY_LINE > content.bottom() {
                    return;
                }
                painter.text(
                    egui::pos2(content.left() + CARD_PAD, y),
                    egui::Align2::LEFT_TOP,
                    line,
                    egui::FontId::monospace(BODY_FONT),
                    palette.subtext0,
                );
                y += BODY_LINE;
            }
        }
        None if badge.is_none() && !entry.is_dir => {
            // The honest empty state: say *why* there is no body, since "too
            // big" and "not text" are different facts and the user can act on
            // the first (`delightful-ui` §11).
            let why = if entry.len > PREVIEW_LIMIT as u64 {
                "too big to preview — extract it to open it"
            } else {
                "no text preview — extract it to open it"
            };
            painter.text(
                egui::pos2(content.left() + CARD_PAD, y),
                egui::Align2::LEFT_TOP,
                why,
                egui::FontId::proportional(CARD_FONT),
                palette.overlay0,
            );
        }
        None => {}
    }
}

/// A small extension trait's worth of "what kind of thing is this", spelled as a
/// free function so [`ArchiveEntry`] stays df-core's.
trait MimeLabel {
    fn mime_label(&self) -> &'static str;
}

impl MimeLabel for ArchiveEntry {
    fn mime_label(&self) -> &'static str {
        df_core::fs::mime::hint_for_name(&self.name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use df_core::archive::{ArchiveFormat, RawEntry};

    fn tree(names: &[(&str, u64, bool)]) -> ArchiveTree {
        let raws: Vec<RawEntry> = names
            .iter()
            .map(|(name, len, is_dir)| {
                let mut raw = RawEntry::new(name.to_string());
                raw.len = *len;
                raw.is_dir = *is_dir;
                raw.mtime = Some(1_600_000_000);
                raw
            })
            .collect();
        df_core::archive::build(
            PathBuf::from("/dl/src.zip"),
            ArchiveFormat::Zip,
            raws,
            false,
        )
    }

    fn browse() -> Browse {
        Browse {
            path: PathBuf::from("/dl/src.zip"),
            tree: Arc::new(tree(&[
                ("readme.txt", 12, false),
                ("src/", 0, true),
                ("src/main.rs", 40, false),
                ("src/big.bin", 9_000_000, false),
            ])),
        }
    }

    /// The rows a directory inside an archive produces, and the facts each one
    /// carries — the whole mapping, pinned.
    #[test]
    fn an_archive_directory_becomes_list_pane_rows() {
        let b = browse();
        let root = b.rows("");
        // Directories first, then names — the list's own sort, applied by the
        // tree, and the reason a browsed archive reads like a directory.
        let names: Vec<&str> = root.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["src", "readme.txt"]);

        let readme = &root[1];
        assert_eq!(readme.path, PathBuf::from("/dl/src.zip/readme.txt"));
        assert_eq!(readme.len, 12);
        assert!(!readme.is_dir());
        assert!(readme.mtime.is_some());
        // No archive format records a creation time, so the column stays empty
        // rather than echoing the modification time.
        assert!(readme.btime.is_none());
        assert_eq!(readme.mime, "text/plain");

        let src = &root[0];
        assert!(src.is_dir());
        // Directories weigh nothing, exactly as they do in a real listing.
        assert_eq!(src.len, 0);

        let inside = b.rows("src");
        let names: Vec<&str> = inside.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["big.bin", "main.rs"]);
        assert_eq!(inside[1].path, PathBuf::from("/dl/src.zip/src/main.rs"));
    }

    /// The display path and the inner path are inverses, with the archive's own
    /// path standing for the root.
    #[test]
    fn display_paths_and_inner_paths_round_trip() {
        let b = browse();
        assert_eq!(b.display_path(""), PathBuf::from("/dl/src.zip"));
        assert_eq!(b.display_path("src"), PathBuf::from("/dl/src.zip/src"));
        assert_eq!(b.inner(Path::new("/dl/src.zip")).as_deref(), Some(""));
        assert_eq!(
            b.inner(Path::new("/dl/src.zip/src/main.rs")).as_deref(),
            Some("src/main.rs")
        );
        // Outside the archive: the answer is "not mine", which is how `←` at
        // the root knows it has left.
        assert_eq!(b.inner(Path::new("/dl")), None);
        assert_eq!(b.inner(Path::new("/dl/other.zip/x")), None);
        assert_eq!(b.real(), PathBuf::from("/dl"));
    }

    #[test]
    fn only_small_text_entries_are_worth_previewing() {
        let tree = tree(&[
            ("notes.md", 400, false),
            ("huge.txt", 9_000_000, false),
            ("photo.png", 400, false),
            ("empty.txt", 0, false),
            ("src/", 0, true),
        ]);
        let by = |name: &str| tree.get(name).cloned().expect(name);
        assert!(previewable(&by("notes.md")));
        // Over the cap, so the card shows the facts and no body.
        assert!(!previewable(&by("huge.txt")));
        assert!(!previewable(&by("photo.png")));
        // Nothing to show, and an empty pane that says "empty file" is better
        // than one that says nothing.
        assert!(!previewable(&by("empty.txt")));
        assert!(!previewable(&by("src")));
    }

    /// A traversing name is badged in the listing, not only refused at
    /// extraction time — the user finds out before they select it.
    #[test]
    fn dangerous_entries_are_badged() {
        let tree = tree(&[("../escape.txt", 4, false), ("fine.txt", 4, false)]);
        let bad = tree
            .all()
            .iter()
            .find(|e| e.unsafe_name)
            .expect("the traversing name is flagged");
        assert_eq!(Badge::of(bad), Some(Badge::Unsafe));
        assert_eq!(
            Badge::of(&tree.get("fine.txt").cloned().expect("fine")),
            None
        );
        assert_eq!(Badge::Unsafe.label(), "unsafe path");
    }

    #[test]
    fn a_subfolder_is_named_after_the_archive_without_its_extension() {
        assert_eq!(subfolder_name(Path::new("/dl/src.zip")), "src");
        // One compound extension, not two: `src.tar` would read as a mistake.
        assert_eq!(subfolder_name(Path::new("/dl/src.tar.gz")), "src");
        assert_eq!(subfolder_name(Path::new("/dl/src.tar.zst")), "src");
        assert_eq!(subfolder_name(Path::new("/dl/src.tar")), "src");
        assert_eq!(subfolder_name(Path::new("/dl/backup")), "backup");
        // All extension and no name: keep it, rather than making a folder with
        // no name at all.
        assert_eq!(subfolder_name(Path::new("/dl/.zip")), ".zip");
        // A set is named by its base, from any of its pieces.
        assert_eq!(subfolder_name(Path::new("/dl/photos.z01")), "photos");
        assert_eq!(subfolder_name(Path::new("/dl/backup.7z.001")), "backup");
        assert_eq!(subfolder_name(Path::new("/dl/movie.part01.rar")), "movie");
        assert_eq!(subfolder_name(Path::new("/dl/bundle.tar.gz.002")), "bundle");
    }

    /// What the name alone admits for extraction: the archive rule's
    /// extensions and every piece of a set, and nothing that only has digits.
    #[test]
    fn a_name_can_say_archive_when_the_mime_cannot() {
        for name in [
            "a.7z",
            "a.RAR",
            "a.tar.bz2",
            "comic.cbr",
            "photos.z01",
            "backup.7z.002",
            "bundle.tar.gz.001",
            "movie.r00",
        ] {
            assert!(looks_like_archive_name(name), "{name}");
        }
        for name in ["notes.txt", "report.2024.pdf", "a.001.txt", ".zip", "zip"] {
            assert!(!looks_like_archive_name(name), "{name}");
        }
        assert_eq!(
            ExtractMode::of_builtin("extract"),
            Some(ExtractMode::Folder)
        );
        assert_eq!(
            ExtractMode::of_builtin("extract-here"),
            Some(ExtractMode::Here)
        );
        assert_eq!(
            ExtractMode::of_builtin("extract-merged"),
            Some(ExtractMode::Merged)
        );
        assert_eq!(ExtractMode::of_builtin("reveal"), None);
    }

    fn candidate(path: &str, listable: bool) -> Candidate {
        let name = Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        Candidate {
            path: PathBuf::from(path),
            is_dir: false,
            listable,
            archive: looks_like_archive_name(&name),
        }
    }

    /// Any piece of a set extracts the whole set, once, from its head —
    /// however many of its pieces were selected.
    #[test]
    fn a_selection_folds_into_archives_and_sets() {
        let listing: Vec<String> = [
            "photos.zip",
            "photos.z01",
            "photos.z02",
            "backup.7z.001",
            "backup.7z.002",
            "notes.txt",
            "plain.zip",
            "orphan.z01",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let picked = [
            candidate("/dl/photos.z02", false),
            candidate("/dl/plain.zip", true),
            candidate("/dl/photos.zip", true),
            candidate("/dl/notes.txt", false),
            candidate("/dl/backup.7z.002", false),
            candidate("/dl/backup.7z.001", false),
            candidate("/dl/orphan.z01", false),
            Candidate {
                is_dir: true,
                ..candidate("/dl/folder.zip", false)
            },
        ];
        let folded = units(&picked, &listing);
        assert_eq!(
            folded.units,
            vec![
                Unit {
                    head: PathBuf::from("/dl/photos.zip"),
                    volumes: Some(df_core::archive::VolumeKind::ZipSplit),
                    listable: false,
                },
                Unit {
                    head: PathBuf::from("/dl/plain.zip"),
                    volumes: None,
                    listable: true,
                },
                Unit {
                    head: PathBuf::from("/dl/backup.7z.001"),
                    volumes: Some(df_core::archive::VolumeKind::SevenZip),
                    listable: false,
                },
            ]
        );
        assert_eq!(folded.orphans, vec!["orphan.z01".to_string()]);
    }

    /// The formats the reader actually has. A `.7z` is not offered a door it
    /// cannot walk through.
    #[test]
    fn only_listable_formats_look_like_archives() {
        let of = |name: &str| {
            let mut raw = RawEntry::new(name.to_string());
            raw.len = 10;
            let tree = df_core::archive::build(
                PathBuf::from("/x.zip"),
                ArchiveFormat::Zip,
                vec![raw],
                false,
            );
            row(tree.get(name).expect(name), Path::new("/dl"))
        };
        assert!(looks_like_archive(&of("bundle.zip")));
        assert!(looks_like_archive(&of("book.epub")));
        assert!(looks_like_archive(&of("src.tar")));
        assert!(looks_like_archive(&of("src.tar.gz")));
        assert!(looks_like_archive(&of("src.tar.xz")));
        assert!(looks_like_archive(&of("src.tar.zst")));
        assert!(!looks_like_archive(&of("notes.txt")));
        assert!(!looks_like_archive(&of("thing.7z")));
        assert!(!looks_like_archive(&of("thing.rar")));
    }

    /// The card says the things a listing row cannot: how well the entry
    /// compressed, how it was compressed, and whether it is going to be
    /// refused.
    #[test]
    fn the_card_reports_what_the_row_cannot() {
        let mut raw = RawEntry::new("src/main.rs".to_string());
        raw.len = 1000;
        raw.compressed = 250;
        raw.method = df_core::archive::Method::Deflate;
        raw.mtime = Some(1_600_000_000);
        let tree = df_core::archive::build(
            PathBuf::from("/x.zip"),
            ArchiveFormat::Zip,
            vec![raw],
            false,
        );
        let entry = tree.get("src/main.rs").expect("entry");
        let rows = card_rows(entry, ArchiveFormat::Zip);
        let find = |label: &str| {
            rows.iter()
                .find(|(l, _)| l == label)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(find("Size").as_deref(), Some("1000 B"));
        assert_eq!(find("Compressed").as_deref(), Some("250 B (75% smaller)"));
        assert_eq!(find("Method").as_deref(), Some("deflate"));
        assert_eq!(find("In archive").as_deref(), Some("src/main.rs"));
        assert!(find("Modified").is_some());
        assert!(find("Refused").is_none());

        // A stored entry does not get a "compressed" row that repeats the size.
        let mut stored = RawEntry::new("a.txt".to_string());
        stored.len = 10;
        stored.compressed = 10;
        let tree = df_core::archive::build(
            PathBuf::from("/x.zip"),
            ArchiveFormat::Zip,
            vec![stored],
            false,
        );
        let rows = card_rows(tree.get("a.txt").expect("a"), ArchiveFormat::Zip);
        assert!(rows.iter().all(|(l, _)| l != "Compressed"));
    }

    /// The read-only contract, as a list. `u`, the palette and `Enter` are
    /// deliberately *not* on it.
    #[test]
    fn writing_commands_are_inert_inside_an_archive() {
        use df_core::keymap::Command as C;
        for c in [
            C::Paste,
            C::Trash,
            C::Rename,
            C::Yank,
            C::Create,
            C::DeletePermanently,
            C::Shell,
        ] {
            assert!(inert_in_archive(c), "{c:?} should be inert");
        }
        for c in [
            C::Undo,
            C::Redo,
            C::CommandPalette,
            C::Open,
            C::EnterDirectory,
            C::Leave,
            C::CursorDown,
            C::ToggleSelect,
            C::SortSize,
            C::Filter,
            C::ArchiveExtractHere,
            C::ArchiveExtractSubfolder,
            C::Spot,
            // Not about anything inside the archive: `X` puts down whatever
            // the program is carrying, and `M` is about disks.
            C::Unyank,
            C::MountManager,
        ] {
            assert!(!inert_in_archive(c), "{c:?} should still work");
        }
    }

    /// Every state of the card lays out and paints without panicking — the
    /// same smoke test every other painter in this crate carries.
    #[test]
    fn the_card_paints_in_every_state() {
        let tree = tree(&[
            ("notes.md", 400, false),
            ("huge.bin", 9_000_000, false),
            ("src/", 0, true),
            ("../escape.txt", 4, false),
        ]);
        let theme = df_core::config::Theme::default();
        let palette = crate::theme::Palette::from_theme(&theme);
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(Default::default(), |ui| {
            let paint = crate::ui::Painting {
                tips: None,
                held: None,
                painter: ui.painter(),
                palette: &palette,
                theme: &theme,
                tags: &crate::tags::BUILT_IN,
                nerd: false,
                show_symlink: true,
                now: std::time::Instant::now(),
            };
            let pane = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(420.0, 600.0));
            for entry in tree.all() {
                card(&paint, pane, entry, ArchiveFormat::Zip, None);
                card(
                    &paint,
                    pane,
                    entry,
                    ArchiveFormat::Zip,
                    Some("one\ntwo\nthree"),
                );
            }
            // A pane with no room at all must draw nothing rather than divide
            // by its own height.
            card(
                &paint,
                egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(0.0, 0.0)),
                &tree.all()[0],
                ArchiveFormat::Zip,
                None,
            );
        });
    }

    /// A 1980s DOS timestamp can be negative; it must not panic or vanish.
    #[test]
    fn timestamps_before_the_epoch_still_render() {
        assert!(unix_time(-86_400).is_some());
        assert!(unix_time(0).is_some());
        assert!(unix_time(1_600_000_000).is_some());
    }
}
