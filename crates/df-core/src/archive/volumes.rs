//! Multi-part archives, recognised by their names.
//!
//! An archive cut into volumes is several rows in a listing and one thing to
//! the person looking at them. `photos.z01`, `photos.z02` and `photos.zip` are
//! one zip; `backup.7z.001` to `.003` are one 7z. Extracting any one of those
//! rows on its own is meaningless, so the extraction code asks this module
//! which rows belong together and which of them to point the extractor at —
//! the *head*.
//!
//! ## Names, not bytes
//!
//! Only the first volume of most schemes has a signature, and the middle ones
//! are raw slices of a stream that begins somewhere else. The only thing that
//! ties `backup.7z.002` to `backup.7z.001` is the name, and every archiver
//! that reads volumes finds the others the same way. So this is string work,
//! pure and table-tested, and it runs over a listing that is already in
//! memory.
//!
//! ## The schemes
//!
//! | Kind                        | Pieces                               | Head          |
//! |-----------------------------|--------------------------------------|---------------|
//! | [`VolumeKind::ZipSplit`]    | `name.z01`, `name.z02`, …, `name.zip` | `name.zip`    |
//! | [`VolumeKind::SevenZip`]    | `name.7z.001`, `.002`, …             | `.001`        |
//! | [`VolumeKind::RarParts`]    | `name.part1.rar`, `.part2.rar`, …    | `part1`       |
//! | [`VolumeKind::RarOld`]      | `name.rar`, `name.r00`, `.r01`, …    | `name.rar`    |
//! | [`VolumeKind::Split`]       | `name.<ext>.001`, `.002`, …          | `.001`        |
//!
//! A zip split writes its `.zip` *last* — the central directory is at the end
//! of the stream — but that is still the file an archiver is pointed at, which
//! is why it is the head. A lone `name.zip` or `name.rar` is an ordinary
//! archive; it only heads a set when a numbered piece sits beside it, and only
//! [`volume_sets`], which sees the whole listing, can say whether one does.

use std::collections::HashMap;

/// How a multi-part archive spells its pieces. See the module table.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum VolumeKind {
    /// `photos.z01`, `photos.z02`, …, `photos.zip`: Info-ZIP's and WinZip's
    /// split.
    ZipSplit,
    /// `backup.7z.001`, `.002`, …: 7-Zip's own volumes, which 7-Zip reads
    /// straight from the first.
    SevenZip,
    /// `movie.part1.rar`, `movie.part2.rar`, … (or `part01`): RAR 3 onwards.
    RarParts,
    /// `movie.rar`, `movie.r00`, `movie.r01`, …: the older RAR naming, where
    /// the `.rar` is the first volume.
    RarOld,
    /// `name.<ext>.001`, `.002`, …: one file cut into pieces byte for byte —
    /// `split`, HJSplit, 7-Zip's `-v` on anything that is not a 7z. The pieces
    /// join into `name.<ext>`, which is then an ordinary archive. `ext` is that
    /// file's extension as written, compound where it is one (`tar.gz`).
    Split { ext: String },
}

impl VolumeKind {
    /// The index of the head: the unnumbered member for the two schemes that
    /// have one, the first numbered piece for the rest.
    pub fn head_index(&self) -> u32 {
        match self {
            VolumeKind::ZipSplit | VolumeKind::RarOld => 0,
            _ => 1,
        }
    }

    /// Whether the head is the one member with no number — which is also a
    /// perfectly ordinary archive when it stands alone.
    fn head_is_unnumbered(&self) -> bool {
        self.head_index() == 0
    }

    /// The format the whole thing is, for a sentence: `zip`, `7z`, `rar`,
    /// `tar.gz`.
    pub fn label(&self) -> String {
        match self {
            VolumeKind::ZipSplit => "zip".to_string(),
            VolumeKind::SevenZip => "7z".to_string(),
            VolumeKind::RarParts | VolumeKind::RarOld => "rar".to_string(),
            VolumeKind::Split { ext } => ext.to_ascii_lowercase(),
        }
    }
}

/// What one name is, as a piece of a multi-part archive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Volume {
    /// The name with every volume and archive extension taken off: `photos`
    /// for `photos.z01`, `backup` for `backup.7z.002`. What a folder extracted
    /// from the whole set is called.
    pub base: String,
    /// The number the name spells. `0` for the unnumbered member of the two
    /// schemes that have one (`photos.zip`, `movie.rar`); `movie.r00` is `1`,
    /// so the old RAR naming counts from its `.rar` in reading order.
    pub index: u32,
    pub kind: VolumeKind,
}

impl Volume {
    /// What the archive would be called in one piece: `photos.zip`,
    /// `backup.7z`, `movie.rar`, `bundle.tar.gz`. Lets a piece be matched by
    /// the opener rules as the archive it is a piece of.
    pub fn whole_name(&self) -> String {
        let ext = match &self.kind {
            VolumeKind::ZipSplit => "zip",
            VolumeKind::SevenZip => "7z",
            VolumeKind::RarParts | VolumeKind::RarOld => "rar",
            VolumeKind::Split { ext } => ext.as_str(),
        };
        format!("{}.{ext}", self.base)
    }
}

/// Read a name as a volume, if it is spelled like one.
///
/// Extensions are matched without regard to case (`PHOTOS.Z01`); the base
/// keeps the case it was written in. `photos.zip` and `movie.rar` come back as
/// index-0 volumes because that is what they would be *if* numbered pieces
/// sat beside them — see the module note; [`volume_sets`] decides.
pub fn volume_of(name: &str) -> Option<Volume> {
    let (rest, last) = name.rsplit_once('.')?;
    if rest.is_empty() || last.is_empty() {
        return None;
    }
    let lower = last.to_ascii_lowercase();

    if lower == "zip" {
        return Some(Volume {
            base: rest.to_string(),
            index: 0,
            kind: VolumeKind::ZipSplit,
        });
    }
    if lower == "rar" {
        // `movie.part1.rar` before `movie.rar`: the part number is inside the
        // name, one extension in.
        if let Some((base, part)) = rest.rsplit_once('.') {
            let part_lower = part.to_ascii_lowercase();
            if let Some(digits) = part_lower.strip_prefix("part") {
                if !base.is_empty() {
                    if let Some(index) = number(digits, 1) {
                        return Some(Volume {
                            base: base.to_string(),
                            index,
                            kind: VolumeKind::RarParts,
                        });
                    }
                }
            }
        }
        return Some(Volume {
            base: rest.to_string(),
            index: 0,
            kind: VolumeKind::RarOld,
        });
    }
    // `.z01` … `.z99`, and WinZip's `.z100` past that.
    if let Some(index) = lower.strip_prefix('z').and_then(|digits| number(digits, 2)) {
        return Some(Volume {
            base: rest.to_string(),
            index,
            kind: VolumeKind::ZipSplit,
        });
    }
    // `.r00` … `.r99`: exactly two digits, which is all the scheme ever had.
    if let Some(digits) = lower.strip_prefix('r') {
        if digits.len() == 2 {
            if let Some(n) = number(digits, 2) {
                return Some(Volume {
                    base: rest.to_string(),
                    index: n + 1,
                    kind: VolumeKind::RarOld,
                });
            }
        }
    }
    // `.001` and up: a 7z volume, or a piece of anything else.
    let index = number(&lower, 3)?;
    let (stem, ext) = rest.rsplit_once('.')?;
    if stem.is_empty() || !plausible_extension(ext) {
        return None;
    }
    if ext.eq_ignore_ascii_case("7z") {
        return Some(Volume {
            base: stem.to_string(),
            index,
            kind: VolumeKind::SevenZip,
        });
    }
    // `bundle.tar.gz.001` joins into `bundle.tar.gz`, whose folder is `bundle`.
    let (base, ext) = match stem.rsplit_once('.') {
        Some((head, tar)) if !head.is_empty() && tar.eq_ignore_ascii_case("tar") => {
            (head, &rest[head.len() + 1..])
        }
        _ => (stem, ext),
    };
    Some(Volume {
        base: base.to_string(),
        index,
        kind: VolumeKind::Split {
            ext: ext.to_string(),
        },
    })
}

/// All ASCII digits, at least `min` of them, and a number that fits.
fn number(digits: &str, min: usize) -> Option<u32> {
    if digits.len() < min || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// Whether `ext` reads as a file extension rather than as part of a name.
///
/// The check that keeps `report.2024.001` from being a split of a file called
/// `report.2024`: an extension has a letter in it, is short, and is plain
/// ASCII. `7z` and `mp4` pass; `2024` does not.
fn plausible_extension(ext: &str) -> bool {
    (1..=8).contains(&ext.len())
        && ext.bytes().all(|b| b.is_ascii_alphanumeric())
        && ext.bytes().any(|b| b.is_ascii_alphabetic())
}

/// One multi-part archive, as it sits in a listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeSet {
    /// What the whole is called without its extensions — the folder name.
    pub base: String,
    /// The member the extractor is pointed at.
    pub head: String,
    /// Every member that is here, head first, then in the order of their
    /// numbers.
    pub parts: Vec<String>,
    pub kind: VolumeKind,
}

impl VolumeSet {
    /// Is `name` one of this set's pieces?
    pub fn contains(&self, name: &str) -> bool {
        self.parts.iter().any(|part| part == name)
    }
}

/// Group a directory's names into the multi-part archives among them.
///
/// A set needs its head: `photos.z01` with no `photos.zip` beside it is a
/// fragment nothing can extract, and it is left out rather than offered. A
/// `.zip` or `.rar` heads a set only when a numbered piece of the same name is
/// here too; alone, it is just an archive. A missing *middle* piece does not
/// stop a set from being one — the extractor says which volume it wanted,
/// which is a better message than anything this could guess.
///
/// Sets come back ordered by their head's name, so the answer does not depend
/// on the order of the listing it was given.
pub fn volume_sets(names: &[String]) -> Vec<VolumeSet> {
    let mut groups: HashMap<(String, VolumeKind), Vec<(u32, &String)>> = HashMap::new();
    for name in names {
        if let Some(volume) = volume_of(name) {
            groups
                .entry((volume.base, volume.kind))
                .or_default()
                .push((volume.index, name));
        }
    }
    let mut sets: Vec<VolumeSet> = groups
        .into_iter()
        .filter_map(|((base, kind), mut members)| {
            members.sort();
            let head = members
                .iter()
                .find(|(index, _)| *index == kind.head_index())?
                .1
                .clone();
            if kind.head_is_unnumbered() && members.iter().all(|(index, _)| *index == 0) {
                return None;
            }
            Some(VolumeSet {
                base,
                head,
                parts: members.into_iter().map(|(_, name)| name.clone()).collect(),
                kind,
            })
        })
        .collect();
    sets.sort_by(|a, b| a.head.cmp(&b.head));
    sets
}

// ── Names for what an extraction makes ──────────────────────────────────────

/// An archive's name with its archive extensions taken off — the name of the
/// folder "Extract to folder" makes.
///
/// `src.tar.gz` → `src`, not `src.tar`, because the `.tar` is half of one
/// compound extension and a folder called `src.tar` full of source would read
/// as a mistake. A piece of a multi-part set answers with the set's base
/// (`backup.7z.002` → `backup`), so every piece names the same folder. A name
/// that is *only* an extension (`.zip`) keeps it, since the alternative is a
/// folder with no name at all.
pub fn archive_stem(name: &str) -> String {
    if let Some(volume) = volume_of(name) {
        return volume.base;
    }
    let stem = match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem,
        _ => return name.to_string(),
    };
    match stem.rsplit_once('.') {
        Some((head, tar)) if !head.is_empty() && tar.eq_ignore_ascii_case("tar") => {
            head.to_string()
        }
        _ => stem.to_string(),
    }
}

/// The one folder "Extract all into one folder" makes for several archives.
///
/// The longest prefix their stems share, with the numbering and separators
/// trimmed off its end — `photos-1.zip` and `photos-2.zip` go into `photos`,
/// `trip_2024_a` and `trip_2024_b` into `trip` — because what a set of
/// archives has in common is its name, and what distinguishes them is the
/// counter. When they share nothing worth naming, the first archive's stem.
pub fn merged_folder_name<S: AsRef<str>>(names: &[S]) -> String {
    let stems: Vec<String> = names.iter().map(|n| archive_stem(n.as_ref())).collect();
    let Some(first) = stems.first() else {
        return "extracted".to_string();
    };
    let mut prefix: &str = first;
    for stem in &stems[1..] {
        prefix = common_prefix(prefix, stem);
    }
    let trimmed =
        prefix.trim_end_matches(|c: char| matches!(c, '-' | '_' | '.' | ' ') || c.is_ascii_digit());
    if trimmed.is_empty() {
        first.clone()
    } else {
        trimmed.to_string()
    }
}

/// The longest prefix of `a` that `b` also starts with, cut on a character
/// boundary.
fn common_prefix<'a>(a: &'a str, b: &str) -> &'a str {
    let end = a
        .char_indices()
        .zip(b.chars())
        .take_while(|((_, x), y)| x == y)
        .last()
        .map(|((at, c), _)| at + c.len_utf8())
        .unwrap_or(0);
    &a[..end]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vol(base: &str, index: u32, kind: VolumeKind) -> Option<Volume> {
        Some(Volume {
            base: base.to_string(),
            index,
            kind,
        })
    }

    fn split(ext: &str) -> VolumeKind {
        VolumeKind::Split {
            ext: ext.to_string(),
        }
    }

    #[test]
    fn every_scheme_is_read_from_its_name() {
        use VolumeKind::*;
        // Zip split: numbered pieces, and the unnumbered `.zip`.
        assert_eq!(volume_of("photos.z01"), vol("photos", 1, ZipSplit));
        assert_eq!(volume_of("photos.z12"), vol("photos", 12, ZipSplit));
        assert_eq!(volume_of("photos.z100"), vol("photos", 100, ZipSplit));
        assert_eq!(volume_of("photos.zip"), vol("photos", 0, ZipSplit));
        assert_eq!(volume_of("PHOTOS.Z01"), vol("PHOTOS", 1, ZipSplit));
        // 7-Zip volumes.
        assert_eq!(volume_of("backup.7z.001"), vol("backup", 1, SevenZip));
        assert_eq!(volume_of("backup.7Z.013"), vol("backup", 13, SevenZip));
        // RAR, both namings.
        assert_eq!(volume_of("movie.part1.rar"), vol("movie", 1, RarParts));
        assert_eq!(volume_of("movie.part01.rar"), vol("movie", 1, RarParts));
        assert_eq!(volume_of("movie.Part12.RAR"), vol("movie", 12, RarParts));
        assert_eq!(volume_of("movie.rar"), vol("movie", 0, RarOld));
        assert_eq!(volume_of("movie.r00"), vol("movie", 1, RarOld));
        assert_eq!(volume_of("movie.r07"), vol("movie", 8, RarOld));
        // Byte splits of anything else, compound extensions kept whole.
        assert_eq!(
            volume_of("bundle.tar.gz.001"),
            vol("bundle", 1, split("tar.gz"))
        );
        assert_eq!(volume_of("photos.zip.002"), vol("photos", 2, split("zip")));
        assert_eq!(volume_of("disk.iso.0003"), vol("disk", 3, split("iso")));
        assert_eq!(
            volume_of("my.notes.tar.001"),
            vol("my.notes", 1, split("tar"))
        );
    }

    #[test]
    fn names_that_only_look_numbered_are_not_volumes() {
        for name in [
            "report.2024.pdf",
            "a.001.txt",
            "report.2024",
            "archive.2024.001",
            "IMG.001",
            ".001",
            ".zip",
            "notes.txt",
            "movie.r1",
            "movie.r001",
            "photos.z1",
            "song.mp3",
            "a.b.c.zst",
            "thing.zst",
            "x.part.rar.001x",
        ] {
            assert_eq!(volume_of(name), None, "{name} is not a volume");
        }
        // `.rar` on its own is an ordinary archive to `volume_sets`, but by
        // name it is still an unnumbered RAR member.
        assert_eq!(
            volume_of("x.part.rar"),
            vol("x.part", 0, VolumeKind::RarOld)
        );
    }

    #[test]
    fn a_listing_groups_into_sets_with_their_heads() {
        let names: Vec<String> = [
            "photos.z02",
            "notes.txt",
            "photos.zip",
            "photos.z01",
            "backup.7z.002",
            "backup.7z.001",
            "backup.7z.003",
            "movie.part2.rar",
            "movie.part1.rar",
            "old.rar",
            "old.r00",
            "bundle.tar.gz.002",
            "bundle.tar.gz.001",
            "plain.zip",
            "lonely.rar",
            "orphan.z01",
            "tail.7z.002",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let sets = volume_sets(&names);
        let heads: Vec<&str> = sets.iter().map(|s| s.head.as_str()).collect();
        assert_eq!(
            heads,
            vec![
                "backup.7z.001",
                "bundle.tar.gz.001",
                "movie.part1.rar",
                "old.rar",
                "photos.zip",
            ],
            "plain zips and rars, and pieces with no head, are not sets"
        );

        let photos = &sets[4];
        assert_eq!(photos.base, "photos");
        assert_eq!(photos.kind, VolumeKind::ZipSplit);
        assert_eq!(photos.parts, vec!["photos.zip", "photos.z01", "photos.z02"]);
        assert!(photos.contains("photos.z02"));
        assert!(!photos.contains("plain.zip"));

        let backup = &sets[0];
        assert_eq!(backup.base, "backup");
        assert_eq!(
            backup.parts,
            vec!["backup.7z.001", "backup.7z.002", "backup.7z.003"]
        );
        assert_eq!(sets[1].kind, split("tar.gz"));
        assert_eq!(sets[1].base, "bundle");
        assert_eq!(sets[3].parts, vec!["old.rar", "old.r00"]);
    }

    #[test]
    fn a_first_piece_alone_is_still_a_set() {
        // `.001` is a piece by definition; the extractor will say what it is
        // missing.
        let sets = volume_sets(&["only.7z.001".to_string()]);
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].head, "only.7z.001");
    }

    #[test]
    fn stems_drop_archive_and_volume_extensions() {
        assert_eq!(archive_stem("src.zip"), "src");
        assert_eq!(archive_stem("src.tar.gz"), "src");
        assert_eq!(archive_stem("src.TAR.ZST"), "src");
        assert_eq!(archive_stem("src.tar"), "src");
        assert_eq!(archive_stem("backup"), "backup");
        assert_eq!(archive_stem(".zip"), ".zip");
        assert_eq!(archive_stem("photos.z01"), "photos");
        assert_eq!(archive_stem("backup.7z.001"), "backup");
        assert_eq!(archive_stem("movie.part2.rar"), "movie");
        assert_eq!(archive_stem("bundle.tar.gz.003"), "bundle");
    }

    #[test]
    fn several_archives_share_a_folder_named_by_what_they_share() {
        assert_eq!(
            merged_folder_name(&["photos-1.zip", "photos-2.zip"]),
            "photos"
        );
        assert_eq!(
            merged_folder_name(&["photos-1.zip", "photos-10.zip"]),
            "photos"
        );
        assert_eq!(
            merged_folder_name(&["trip_2024_a.tar.gz", "trip_2024_b.zip"]),
            "trip"
        );
        assert_eq!(
            merged_folder_name(&["scans part 1.zip", "scans part 2.zip", "scans part 3.7z"]),
            "scans part"
        );
        // Nothing in common: the first archive's stem.
        assert_eq!(merged_folder_name(&["alpha.zip", "beta.zip"]), "alpha");
        // A prefix that is only numbering is nothing in common either.
        assert_eq!(merged_folder_name(&["12.zip", "13.zip"]), "12");
        // A set counts by its base.
        assert_eq!(
            merged_folder_name(&["music.7z.001", "music-extra.zip"]),
            "music"
        );
        // Shared multibyte characters are kept whole, never cut in half.
        assert_eq!(merged_folder_name(&["café-1.zip", "café-2.zip"]), "café");
        assert_eq!(merged_folder_name(&["é.zip", "è.zip"]), "é");
        assert_eq!(merged_folder_name::<&str>(&[]), "extracted");
        assert_eq!(merged_folder_name(&["solo.zip"]), "solo");
    }
}
