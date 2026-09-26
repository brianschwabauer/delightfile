//! Search hits as a listing (PLAN §7.2): `Enter` in the `s`/`S` panel turns
//! what it found into the tab's rows.
//!
//! ## The same trick, a fourth time
//!
//! The panel is a card, and a card answers one question — where is it — with
//! one key. Everything else a person does with a set of files lives in the
//! list pane: `Space` and visual mode, `y`, `d`, `r` and the bulk card, a drag
//! out, `Tab`, the sorts, `A`, `Y`, the `c` chords, the preview. So the hit set
//! does not grow copies of those; it *becomes* the pane's rows, by the door the
//! archive, the remote pane and the trash already use ([`DirState`]'s external
//! listing), and every one of them works on hits across folders without
//! knowing they are hits.
//!
//! ## The name is the path from the root
//!
//! A row is the file's own [`Entry`], read in the search's worker, with its
//! **real path** — so the preview, the icon table, the spot panel and every
//! operation see a file, because it is one. Only the *name* differs: it is the
//! path relative to where the search ran (`src/app/foo.rs`). That is what keeps
//! names unique, and they have to be, because the pane keys the cursor, the
//! selection and the filter by name: two `mod.rs` rows would be one row to
//! `Space`. The listing's own path is the root, so `root.join(name)` is the
//! file — which is what [`DirState::selected_paths`] and every other "this
//! listing's path plus a name" in the program already compute. A row renamed
//! out of the root keeps its absolute path as its name, which `join` leaves
//! alone as well.
//!
//! The row draws its folders in the quiet ink and its own name in the row's,
//! cut from the front a folder at a time when the column is narrow
//! ([`crate::chrome::elide_segments`]): the end is what tells two rows apart.
//! `f` filters on the whole relative path, so `f preview/` narrows to a folder.
//!
//! ## What the column means here
//!
//! Names mode keeps the linemode column. Contents mode replaces it, whatever
//! `m` says, with the first matching line and — when there is more than one —
//! how many (`3 · let x = …`), the way the trash replaces it with where a file
//! came from: a line of text is what that search was for. The preview opens a
//! content hit at that line, unless it already remembers where you were in the
//! file.
//!
//! ## Streaming
//!
//! `Enter` does not wait for the walk. The [`Search`] moves into the listing
//! still running, and its batches land through the same rebuild a scanner's
//! batch does ([`DirState::extend_external`]): sorted as they come, the cursor
//! held by name. Its reader rings the bell every other worker rings, so nothing
//! polls. `Ctrl+s` stops it and keeps what it found.
//!
//! ## Leaving, and the history
//!
//! `←` returns to the root folder with the cursor where it was when the search
//! was committed, and so does `Esc` once it has nothing smaller left to take
//! back. `→` or `Enter` on a directory row enters it as a normal folder — and
//! the history records the root, not the hits, because a set of results is not
//! a place the back button can rebuild: `Alt+←` from there returns to the root
//! folder, not to the results. `s`, `S` or a click on the chip at the end of the
//! breadcrumb puts the query back in the panel to be refined, and `Enter` there
//! lists the new answer in place of this one.
//!
//! ## No watcher
//!
//! A directory listing is watched; a set of hits spread over a tree is not,
//! because a watch per folder the search touched is a watch per folder in the
//! tree. The rows are re-read instead whenever this program's own operations
//! touch the tree — a rename renames its row, a trash or a move elsewhere takes
//! the row out ([`View::refresh`]) — the way the trash view re-reads itself
//! after a restore. A change another program makes shows after the next
//! operation here, or after searching again.
//!
//! ## What is refused
//!
//! The verbs that make something *in* the folder on screen — `a`, `p` and its
//! variants, the links — have no folder here to make it in, and `g b` has no
//! folder to pin. `.` is refused as well: which files the search saw is a
//! property of the search, and the dotfiles it did not return are not hidden
//! here, they were never found. Each says so, and the menus grey them
//! ([`refusal`]).
//!
//! An empty query in names mode lists everything under the root: the folder,
//! flattened.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use df_core::fs::{DirState, Entry};
use df_core::keymap::Command;

use crate::search::{Hit, Mode, Search};

/// The path the breadcrumb's search chip carries.
///
/// A URL in the spirit of `trash://`: it is not a directory, a click on it is
/// routed to the panel rather than to `navigate`, and a drop on it goes
/// nowhere rather than into a folder called `search:`.
pub const URL: &str = "search://";

/// What the linemode column says for one content hit's file.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Found {
    /// The first matching line's number, 1-based as rg prints it.
    line: usize,
    /// That line, trimmed.
    text: String,
    /// How many matching lines the file has had so far.
    count: usize,
}

/// The hits a tab is showing as its listing.
pub struct View {
    /// The search the rows came from — still running when `Enter` came before
    /// the end of the walk. Dropping the view drops it, which kills the
    /// process ([`Search`]'s `Running`).
    pub search: Search,
    /// How many of `search.hits` have been turned into rows.
    taken: usize,
    /// Contents mode: each row's first matching line, by row name.
    found: HashMap<String, Found>,
    /// The linemode column's replacement in contents mode, by row name —
    /// the map the list painter reads (`ListView::notes`).
    pub notes: HashMap<String, String>,
    /// The row the root folder's cursor was on when the search was
    /// committed, which is where `←` puts it back.
    pub origin: Option<String>,
}

impl View {
    pub fn new(search: Search, origin: Option<String>) -> View {
        View {
            search,
            taken: 0,
            found: HashMap::new(),
            notes: HashMap::new(),
            origin,
        }
    }

    /// Where the search ran, and what every row's name is relative to.
    pub fn root(&self) -> &Path {
        &self.search.root
    }

    pub fn mode(&self) -> Mode {
        self.search.mode
    }

    pub fn query(&self) -> &str {
        self.search.query()
    }

    /// Whether more rows may still arrive.
    pub fn running(&self) -> bool {
        self.search.searching()
    }

    /// The rows for every hit that has arrived since the last call.
    ///
    /// A names hit is a row. A content hit is a *line*, and a file with three
    /// matching lines is one row: the first line it arrives with is the one
    /// the column shows and the preview opens at, and the rest are counted.
    pub fn take(&mut self) -> Vec<Entry> {
        let mut rows = Vec::new();
        let fresh = self.search.hits.get(self.taken..).unwrap_or_default();
        for hit in fresh {
            if self.search.mode == Mode::Content {
                if let Some(found) = self.found.get_mut(&hit.relative) {
                    found.count += 1;
                    self.notes.insert(hit.relative.clone(), note(found));
                    continue;
                }
            }
            let Some(entry) = row(hit) else { continue };
            if self.search.mode == Mode::Content {
                let found = Found {
                    line: hit.line.unwrap_or(1),
                    text: one_line(&hit.text),
                    count: 1,
                };
                self.notes.insert(hit.relative.clone(), note(&found));
                self.found.insert(hit.relative.clone(), found);
            }
            rows.push(entry);
        }
        self.taken = self.search.hits.len();
        rows
    }

    /// The first matching line of a content hit's row, for the preview to
    /// open at. `None` in names mode.
    pub fn line_of(&self, name: &str) -> Option<usize> {
        self.found.get(name).map(|found| found.line)
    }

    /// The breadcrumb's chip for `rows` rows ([`chip`]).
    pub fn chip(&self, rows: usize) -> String {
        chip(
            self.mode(),
            self.query(),
            rows,
            self.running(),
            self.search.capped,
        )
    }

    /// What the tab strip calls a tab showing these hits.
    pub fn title(&self) -> String {
        chip_query(self.mode(), self.query())
    }

    /// Re-read every row after an operation touched the tree: `moved` renames
    /// the rows it names first (old path to new), then each row is read from
    /// the disk again and a row whose file is gone is taken out — its mark
    /// with it, as a finished scan drops a mark on a file that went.
    ///
    /// The trash view's answer to a restore, for a listing that has no
    /// directory to re-read: its rows are the only record of what it holds.
    pub fn refresh(&mut self, dir: &mut DirState, moved: &[(PathBuf, PathBuf)]) {
        let root = self.root().to_path_buf();
        let mut renamed: Vec<(String, String)> = Vec::new();
        let mut gone: Vec<String> = Vec::new();
        dir.retain_entries(|entry| {
            if let Some((_, to)) = moved.iter().find(|(from, _)| *from == entry.path) {
                let name = name_under(&root, to);
                renamed.push((entry.name.clone(), name.clone()));
                entry.path = to.clone();
                entry.name = name;
            }
            match Entry::read(&entry.path) {
                Ok(fresh) => {
                    let name = std::mem::take(&mut entry.name);
                    *entry = Entry { name, ..fresh };
                    true
                }
                Err(_) => {
                    gone.push(entry.name.clone());
                    false
                }
            }
        });
        for (old, new) in renamed {
            if let Some(found) = self.found.remove(&old) {
                self.found.insert(new.clone(), found);
            }
            if let Some(note) = self.notes.remove(&old) {
                self.notes.insert(new, note);
            }
        }
        for name in gone {
            self.found.remove(&name);
            self.notes.remove(&name);
        }
    }
}

/// One hit as a row: the entry its worker read, named by its path from the
/// root. `None` for a file that went away between the search and the stat.
pub fn row(hit: &Hit) -> Option<Entry> {
    let entry = hit.entry.clone()?;
    Some(Entry {
        name: hit.relative.clone(),
        ..entry
    })
}

/// What a row at `path` is called in a listing rooted at `root`: the path
/// from the root, or — for a file renamed out of it — the whole path, which
/// `root.join` leaves as it is.
pub fn name_under(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .ok()
        .filter(|rest| !rest.as_os_str().is_empty())
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

/// A matched line as one line of a column: its leading indentation gone and
/// any tab a space, so `    let x` and `\tlet x` read the same.
fn one_line(text: &str) -> String {
    text.trim().replace('\t', " ")
}

/// The column's text for one file: the line, and how many there are when it
/// is not the only one.
fn note(found: &Found) -> String {
    if found.count > 1 {
        format!("{} · {}", found.count, found.text)
    } else {
        found.text.clone()
    }
}

/// `s query`, or `S query` for contents — which key asked it, and what. An
/// empty names query lists everything, and reads `s *`.
fn chip_query(mode: Mode, query: &str) -> String {
    let key = match mode {
        Mode::Names => 's',
        Mode::Content => 'S',
    };
    let query = if query.is_empty() { "*" } else { query };
    format!("{key} {query}")
}

/// The breadcrumb's chip: the key and the query, how many rows, `…` while the
/// walk is still going and `· truncated` when it was cut off at the cap
/// ([`crate::search::MAX_HITS`]) — `s mouse · 132`, `S TODO · 1,204…`.
pub fn chip(mode: Mode, query: &str, rows: usize, running: bool, capped: bool) -> String {
    let mut label = format!(
        "{} · {}",
        chip_query(mode, query),
        df_core::text::grouped(rows as u64)
    );
    if running {
        label.push('…');
    } else if capped {
        label.push_str(" · truncated");
    }
    label
}

/// Whether a breadcrumb segment is the search chip.
pub fn is_chip(crumb: &crate::chrome::Crumb) -> bool {
    crumb.path == Path::new(URL)
}

/// The breadcrumb's search chip, for the end of the root's crumbs.
pub fn chip_crumb(label: String) -> crate::chrome::Crumb {
    crate::chrome::Crumb {
        label,
        path: PathBuf::from(URL),
        accent: true,
    }
}

/// The sentence a command is refused with in a hits listing, or `None` where
/// it works (see the module note). `g b` is refused by the places gate, with
/// the other listings that are not folders.
pub fn refusal(command: Command) -> Option<&'static str> {
    use Command as C;
    Some(match command {
        C::Create => "Search results are not a folder — ← goes back to make something there",
        C::Paste
        | C::PasteForce
        | C::PasteSync
        | C::SymlinkAbsolute
        | C::SymlinkRelative
        | C::Hardlink => "Search results are not a folder — ← goes back to paste there",
        C::ToggleHidden => "Search again to include hidden files",
        C::DiskUsage => "Disk usage is about one folder — ← goes back to it",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn silent() -> df_core::fs::Notifier {
        std::sync::Arc::new(|| {})
    }

    fn hit(root: &Path, relative: &str, line: Option<usize>, text: &str) -> Hit {
        let path = root.join(relative);
        Hit {
            entry: Some(entry(&path)),
            path,
            relative: relative.to_string(),
            line,
            text: text.to_string(),
            span: None,
        }
    }

    /// An entry that does not need the file to exist: the name the scan
    /// would give it, from its last component.
    fn entry(path: &Path) -> Entry {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        Entry {
            is_hidden: name.starts_with('.'),
            mime: df_core::fs::mime::hint_for_name(&name),
            file_kind: df_core::fs::FileKind::Text,
            name,
            path: path.to_path_buf(),
            kind: df_core::fs::Kind::File,
            len: 1,
            mtime: None,
            btime: None,
            mode: 0o100_644,
            uid: 0,
            gid: 0,
            tags: Vec::new(),
        }
    }

    /// A names hit is a row named by its path from the root, with its real
    /// path under it — the two facts the whole module rests on.
    #[test]
    fn a_names_hit_is_a_row_named_from_the_root() {
        let root = PathBuf::from("/r");
        let mut search = Search::new(Mode::Names, &root, false, silent());
        search.hits = vec![
            hit(&root, "src/app/foo.rs", None, ""),
            hit(&root, "foo.rs", None, ""),
        ];
        let mut view = View::new(search, None);
        let rows = view.take();
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["src/app/foo.rs", "foo.rs"]);
        assert_eq!(rows[0].path, root.join("src/app/foo.rs"));
        assert!(view.notes.is_empty(), "names mode keeps the linemode");
        assert!(view.take().is_empty(), "each hit is taken once");
    }

    /// A content hit is a line; a file is one row, whose column is its first
    /// line — trimmed — and, past one, how many.
    #[test]
    fn content_hits_fold_into_one_row_per_file() {
        let root = PathBuf::from("/r");
        let mut search = Search::new(Mode::Content, &root, false, silent());
        search.hits = vec![
            hit(&root, "a.rs", Some(12), "\tlet x = 1;"),
            hit(&root, "b.rs", Some(3), "  let y"),
        ];
        let mut view = View::new(search, None);
        assert_eq!(view.take().len(), 2);
        assert_eq!(
            view.notes.get("a.rs").map(String::as_str),
            Some("let x = 1;")
        );
        view.search
            .hits
            .push(hit(&root, "a.rs", Some(40), "let x = 2;"));
        view.search
            .hits
            .push(hit(&root, "a.rs", Some(41), "let x = 3;"));
        assert!(view.take().is_empty(), "a second line is not a second row");
        assert_eq!(
            view.notes.get("a.rs").map(String::as_str),
            Some("3 · let x = 1;")
        );
        assert_eq!(
            view.line_of("a.rs"),
            Some(12),
            "the first line, not the last"
        );
        assert_eq!(view.line_of("b.rs"), Some(3));
    }

    /// The chip's three states, and the empty names query that lists
    /// everything.
    #[test]
    fn the_chip_says_what_was_asked_and_how_it_is_going() {
        assert_eq!(
            chip(Mode::Names, "mouse", 132, false, false),
            "s mouse · 132"
        );
        assert_eq!(
            chip(Mode::Content, "TODO", 1204, true, false),
            "S TODO · 1,204…"
        );
        assert_eq!(
            chip(Mode::Names, "a", 2000, false, true),
            "s a · 2,000 · truncated"
        );
        assert_eq!(chip(Mode::Names, "", 7, false, false), "s * · 7");
    }

    /// A row is named from the root; one renamed out of it keeps its whole
    /// path, which `join` onto the root leaves alone.
    #[test]
    fn a_name_is_the_path_from_the_root_or_the_whole_path() {
        let root = Path::new("/r");
        assert_eq!(name_under(root, Path::new("/r/a/b.txt")), "a/b.txt");
        assert_eq!(
            name_under(root, Path::new("/elsewhere/b.txt")),
            "/elsewhere/b.txt"
        );
        assert_eq!(
            root.join(name_under(root, Path::new("/elsewhere/b.txt"))),
            PathBuf::from("/elsewhere/b.txt")
        );
    }

    /// The four refusals the brief names, and a verb that works.
    #[test]
    fn making_something_here_is_refused_and_acting_on_rows_is_not() {
        for command in [
            Command::Create,
            Command::Paste,
            Command::PasteForce,
            Command::PasteSync,
            Command::ToggleHidden,
        ] {
            assert!(refusal(command).is_some(), "{}", command.id());
        }
        for command in [
            Command::Rename,
            Command::Trash,
            Command::Yank,
            Command::ArchiveCreate,
        ] {
            assert_eq!(refusal(command), None, "{}", command.id());
        }
    }
}
