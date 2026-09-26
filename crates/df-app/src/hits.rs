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
//! tree. The rows are re-read instead when this program's own operations touch
//! them, the way the trash view re-reads itself after a restore: a rename
//! renames its row and every row beneath it ([`View::follow`]), a trash or a
//! move elsewhere takes the row out when its folder is re-read
//! ([`View::reread`]), and `u` on a trash puts the rows it took out back
//! ([`View::restore`]). Only the rows in the folder an operation touched are
//! read again, never the whole set.
//!
//! The one folder that *is* watched is the root, for the parent column beside
//! the hits. Its events, and a tab coming back on screen, do not re-read
//! anything on the spot: the folders are marked and their rows are read again
//! once the tree has been quiet for [`crate::folders::RESTALE_QUIET`], the
//! quiet period the size column waits out for the same reason. A build writing
//! into the root is a stream of events, and a synchronous stat of up to two
//! thousand rows per event would be the window stalling for the length of the
//! build. A change another program makes elsewhere in the tree shows after the
//! next operation that touches its folder, or after searching again.
//!
//! ## What is refused
//!
//! The verbs that make something *in* the folder on screen — `a`, `p` and its
//! variants, the links, `A` and every extract (here, to a folder, all into
//! one folder) — have no folder here to make it in (and what they made would
//! never be one of these rows), and `g b` has no folder to pin. `.` is refused as well: which files the search saw is a
//! property of the search, and the dotfiles it did not return are not hidden
//! here, they were never found. Each says so, and the menus grey them
//! ([`refusal`]).
//!
//! An empty query in names mode lists everything under the root: the folder,
//! flattened.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

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

/// A row an operation took out of the listing because its file went: what
/// [`View::restore`] needs to put it back when `u` brings the file back.
#[derive(Debug, Clone)]
struct Gone {
    name: String,
    found: Option<Found>,
}

/// Which folders' rows are owed a re-read once the tree is quiet.
#[derive(Debug, Clone, Default)]
enum Stale {
    #[default]
    Nothing,
    /// The rows in these folders.
    In(HashSet<PathBuf>),
    /// Every row: events were lost, or the tab has been off screen.
    All,
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
    /// The rows taken out because their files went, by path.
    gone: HashMap<PathBuf, Gone>,
    /// What the watcher has said since the last re-read, and when it last
    /// said it (see the module note's "No watcher").
    stale: Stale,
    stale_since: Option<Instant>,
}

impl View {
    pub fn new(search: Search, origin: Option<String>) -> View {
        View {
            search,
            taken: 0,
            found: HashMap::new(),
            notes: HashMap::new(),
            origin,
            gone: HashMap::new(),
            stale: Stale::Nothing,
            stale_since: None,
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
    ///
    /// A hit whose file is no longer at its path is dropped. The walk read it
    /// a moment ago, but the listing may have renamed or trashed it since —
    /// and a late batch naming the old path would otherwise put the old name
    /// back as a second, stale row beside the renamed one. One `lstat` per new
    /// row, never per counted line.
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
            if hit.path.symlink_metadata().is_err() {
                continue;
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

    /// Follow renames this program made: every row at or beneath a `from` is
    /// re-pathed, renamed, and read again — a folder's rename takes the hits
    /// inside it along, rather than leaving them at a path that is gone.
    ///
    /// `moved` is the renames in the order they ran, and each is applied to
    /// where the previous ones left a row: a bulk rename renames a folder's
    /// files before the folder, and a swap goes through a temporary name, and
    /// both come out right only played in that order.
    pub fn follow(&mut self, dir: &mut DirState, moved: &[(PathBuf, PathBuf)]) {
        self.revise(dir, moved, |_| false);
    }

    /// Read the rows in `folders` again (every row, for `None`), taking out
    /// the ones whose files went — and every row beneath a folder row that
    /// went with them.
    pub fn reread(&mut self, dir: &mut DirState, folders: Option<&HashSet<PathBuf>>) {
        let gone = self.revise(dir, &[], |entry| match folders {
            Some(folders) => entry.path.parent().is_some_and(|p| folders.contains(p)),
            None => true,
        });
        if !gone.is_empty() {
            self.revise(dir, &[], |entry| {
                gone.iter().any(|folder| entry.path.starts_with(folder))
            });
        }
    }

    /// `u` brought `paths` back: the rows this listing took out when they
    /// went are put back — and the rows beneath a folder that came back —
    /// under the names they had, unmarked. A path the listing never had a row
    /// for is not added: an undo of something done before the search is not a
    /// hit. Returns whether any row came back.
    pub fn restore(&mut self, dir: &mut DirState, paths: &[PathBuf]) -> bool {
        let back: Vec<PathBuf> = self
            .gone
            .keys()
            .filter(|gone| paths.iter().any(|path| gone.starts_with(path)))
            .cloned()
            .collect();
        let mut rows = Vec::new();
        for path in back {
            let Some(gone) = self.gone.remove(&path) else {
                continue;
            };
            let Ok(entry) = Entry::read(&path) else {
                continue;
            };
            if dir.entries().iter().any(|row| row.name == gone.name) {
                continue;
            }
            if let Some(found) = gone.found {
                self.notes.insert(gone.name.clone(), note(&found));
                self.found.insert(gone.name.clone(), found);
            }
            rows.push(Entry {
                name: gone.name,
                ..entry
            });
        }
        if rows.is_empty() {
            return false;
        }
        dir.extend_external(rows);
        true
    }

    /// The watcher saw `folder` change: its rows are owed a re-read once the
    /// tree has been quiet for [`crate::folders::RESTALE_QUIET`].
    pub fn mark_stale(&mut self, folder: &Path, now: Instant) {
        match &mut self.stale {
            Stale::All => {}
            Stale::In(folders) => {
                folders.insert(folder.to_path_buf());
            }
            Stale::Nothing => self.stale = Stale::In(HashSet::from([folder.to_path_buf()])),
        }
        self.stale_since = Some(now);
    }

    /// Every row is owed a re-read, on the same quiet period.
    pub fn mark_all_stale(&mut self, now: Instant) {
        self.stale = Stale::All;
        self.stale_since = Some(now);
    }

    /// The instant the owed re-read is due, when one is — a deadline for the
    /// frame loop, never a poll (PLAN §1).
    pub fn due_at(&self) -> Option<Instant> {
        self.stale_since
            .map(|since| since + crate::folders::RESTALE_QUIET)
    }

    /// Re-read what the watcher marked, if the tree has been quiet long
    /// enough. Returns whether it did.
    pub fn reread_due(&mut self, dir: &mut DirState, now: Instant) -> bool {
        if self.due_at().is_none_or(|at| now < at) {
            return false;
        }
        self.stale_since = None;
        match std::mem::take(&mut self.stale) {
            Stale::Nothing => return false,
            Stale::In(folders) => self.reread(dir, Some(&folders)),
            Stale::All => self.reread(dir, None),
        }
        true
    }

    /// Re-read the rows `wanted` picks and every row `moved` re-paths, taking
    /// out the ones whose files went. Returns the folder rows that went, whose
    /// contents' rows are going too.
    fn revise(
        &mut self,
        dir: &mut DirState,
        moved: &[(PathBuf, PathBuf)],
        mut wanted: impl FnMut(&Entry) -> bool,
    ) -> Vec<PathBuf> {
        let root = self.root().to_path_buf();
        let mut renamed: Vec<(String, String)> = Vec::new();
        let mut gone: Vec<(PathBuf, String, bool)> = Vec::new();
        dir.retain_entries(|entry| {
            let to = moved_to(&entry.path, moved);
            if to.is_none() && !wanted(entry) {
                return true;
            }
            if let Some(to) = to {
                let name = name_under(&root, &to);
                renamed.push((entry.name.clone(), name.clone()));
                entry.path = to;
                entry.name = name;
            }
            match Entry::read(&entry.path) {
                Ok(fresh) => {
                    let name = std::mem::take(&mut entry.name);
                    *entry = Entry { name, ..fresh };
                    true
                }
                Err(_) => {
                    gone.push((entry.path.clone(), entry.name.clone(), entry.is_dir()));
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
        let mut folders = Vec::new();
        for (path, name, is_dir) in gone {
            self.notes.remove(&name);
            let found = self.found.remove(&name);
            if is_dir {
                folders.push(path.clone());
            }
            self.gone.insert(path, Gone { name, found });
        }
        folders
    }
}

/// Where the renames in `moved`, run in order, leave the file at `path` — or
/// `None` when none of them touched it. A rename of the file itself moves it,
/// and so does a rename of any folder it is in.
pub fn moved_to(path: &Path, moved: &[(PathBuf, PathBuf)]) -> Option<PathBuf> {
    let mut at = path.to_path_buf();
    for (from, to) in moved {
        // Component by component, so `src2` is not taken for a file in `src`.
        if let Ok(rest) = at.strip_prefix(from) {
            at = if rest.as_os_str().is_empty() {
                to.clone()
            } else {
                to.join(rest)
            };
        }
    }
    (at != path).then_some(at)
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
// `/`-separated only: see plans/other-platforms/03-paths.md for the port.
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
        // …and `A` and the extracts, which make a file or a folder the way
        // `a` does, in a folder that is not on screen: what they made would
        // never be one of these rows, so nobody would see it land. Extract to
        // folder is also the gate for "all into one folder" (the menu's row)
        // and for the `builtin:extract…` openers `O` offers (`App::launch`).
        C::Create | C::ArchiveCreate | C::ArchiveExtractHere | C::ArchiveExtractSubfolder => {
            "Search results are not a folder — ← goes back to make something there"
        }
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

    /// A hit for `relative`, with the file made under `root` if it is not
    /// there: a row is only taken for a file that is still at its path.
    fn hit(root: &Path, relative: &str, line: Option<usize>, text: &str) -> Hit {
        let path = root.join(relative);
        if path.symlink_metadata().is_err() {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).expect("the hit's folder");
            }
            std::fs::write(&path, b"x").expect("the hit's file");
        }
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
        let tree = df_core::test_support::TempTree::new("hits-names");
        let root = tree.path().to_path_buf();
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
        let tree = df_core::test_support::TempTree::new("hits-content");
        let root = tree.path().to_path_buf();
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

    /// What makes something here is refused — `A` and every extract with the
    /// same sentence as `a` — and a verb on the rows works.
    #[test]
    fn making_something_here_is_refused_and_acting_on_rows_is_not() {
        for command in [
            Command::Create,
            Command::Paste,
            Command::PasteForce,
            Command::PasteSync,
            Command::ToggleHidden,
            Command::ArchiveCreate,
            Command::ArchiveExtractHere,
            Command::ArchiveExtractSubfolder,
        ] {
            assert!(refusal(command).is_some(), "{}", command.id());
        }
        for command in [
            Command::ArchiveCreate,
            Command::ArchiveExtractHere,
            Command::ArchiveExtractSubfolder,
        ] {
            assert_eq!(
                refusal(command),
                refusal(Command::Create),
                "{}",
                command.id()
            );
        }
        for command in [Command::Rename, Command::Trash, Command::Yank] {
            assert_eq!(refusal(command), None, "{}", command.id());
        }
    }

    /// Renames run in order carry every row beneath them: a folder's rename
    /// moves the hits inside it, a file renamed before its folder ends up in
    /// the renamed folder, `src2` is not inside `src`, and a swap through a
    /// temporary name comes out swapped.
    #[test]
    fn a_rename_carries_the_rows_beneath_it() {
        let p = PathBuf::from;
        let folder = [(p("/r/src/deep"), p("/r/src/deeper"))];
        assert_eq!(
            moved_to(Path::new("/r/src/deep/a.txt"), &folder),
            Some(p("/r/src/deeper/a.txt"))
        );
        assert_eq!(
            moved_to(Path::new("/r/src/deep"), &folder),
            Some(p("/r/src/deeper"))
        );
        assert_eq!(moved_to(Path::new("/r/src/deep2/a.txt"), &folder), None);

        let child_then_folder = [
            (p("/r/src/deep/a.txt"), p("/r/src/deep/b.txt")),
            (p("/r/src/deep"), p("/r/src/deeper")),
        ];
        assert_eq!(
            moved_to(Path::new("/r/src/deep/a.txt"), &child_then_folder),
            Some(p("/r/src/deeper/b.txt"))
        );

        let swap = [
            (p("/r/a"), p("/r/b.df-rename-1")),
            (p("/r/b"), p("/r/a")),
            (p("/r/b.df-rename-1"), p("/r/b")),
        ];
        assert_eq!(moved_to(Path::new("/r/a"), &swap), Some(p("/r/b")));
        assert_eq!(moved_to(Path::new("/r/b"), &swap), Some(p("/r/a")));
    }
}
