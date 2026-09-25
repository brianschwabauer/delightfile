//! The one fuzzy overlay, and the lists that go in it (PLAN §4.4, §7.2).
//!
//! `Ctrl+p` is a command palette, `z` is a fuzzy jump over everywhere you have
//! been, `Z` is zoxide's frecency list, and `g space` is the places somebody
//! pinned or bookmarked. They look identical on screen and
//! they behave identically under the hand, because they *are* identical: one
//! card, one query, one ranked list, one `Enter`. Only the rows differ, and a
//! row is `(what it says, what it does)`.
//!
//! Writing them as three overlays would have been three cursors to keep in
//! range, three scroll rules and three `Esc` ladders — and the second one would
//! have drifted from the first inside a week. So this module owns the state and
//! the ranking, [`crate::chrome`] owns the painting, and [`Source`] is the only
//! thing that differs between the three.
//!
//! ## What the palette lists
//!
//! Everything the registry knows, because the registry is the only thing that
//! knows it (PLAN §4: "the keymap documents itself in three places from one
//! table"). A command's *name* in the palette is its description, its chord is
//! rendered dim on the right, and a command that is not reachable right now is
//! **absent rather than greyed** — a palette is a list of things you can do,
//! and a disabled row is a thing you cannot do taking up space that a thing you
//! can do wanted.
//!
//! On top of that: the `[goto]` bookmarks as places, the directories this tab
//! has been through, and the open tabs. All four kinds are the same [`Row`].

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use df_core::config::Bookmark;
use df_core::fs::Span;
use df_core::input::InputBuffer;
use df_core::keymap::Command;
use df_core::state::Pin;
use df_core::zoxide::ZoxideDir;

use crate::fuzzy;

/// How many rows the card shows before it scrolls.
///
/// Twelve. Below about eight the list stops being a list and becomes a
/// peephole you have to scroll to read; past about sixteen the card is tall
/// enough to cover the window it is a shortcut into, and the rows past the
/// tenth are never read anyway — if the answer is not in the first few, the
/// thing to do is type another letter, not scroll.
pub const ROWS: usize = 12;

/// How many command ids the session-local most-recently-used list holds.
///
/// Thirty-two is comfortably more than the handful of commands a person
/// actually reaches for by palette, and small enough that the list is a
/// `Vec<String>` searched linearly without anybody having to think about it.
/// It is deliberately **not persisted**: a palette that remembers last
/// Tuesday's ordering is a palette whose first row moves for reasons you cannot
/// see.
const MRU_LIMIT: usize = 32;

/// Which list is in the card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// `Ctrl+p` — every command, plus bookmarks, recent directories and tabs.
    Commands,
    /// `z` — pins, history, bookmarks and zoxide merged into one place list.
    Jump,
    /// `Z` — zoxide's frecency order, unmerged.
    Zoxide,
    /// `g space` — the Places list: the pins, then `[goto]`, then home
    /// (`crate::app::places`). Short, chosen by hand, and servers included.
    Places,
}

impl Source {
    /// The card's heading.
    pub fn title(self) -> &'static str {
        match self {
            Source::Commands => "Command palette",
            Source::Jump => "Jump to",
            Source::Zoxide => "Frecent directories",
            Source::Places => "Places",
        }
    }

    /// What the field says when nothing has been typed into it yet.
    pub fn placeholder(self) -> &'static str {
        match self {
            Source::Commands => "Type a command…",
            Source::Jump => "Type part of a path…",
            Source::Zoxide => "Type part of a path…",
            Source::Places => "Type part of a path…",
        }
    }

    /// The context this overlay's keys are matched in. All three are the
    /// palette's — one card, one set of keys, one place in the help sheet.
    pub fn empty_message(self) -> &'static str {
        match self {
            Source::Commands => "Nothing matches. Backspace to widen the search.",
            Source::Jump => "No directory matches. Backspace to widen the search.",
            Source::Zoxide => "zoxide has not been anywhere matching that yet.",
            Source::Places => "No place matches. Backspace to widen the search.",
        }
    }
}

/// What choosing a row does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    /// Dispatch a registry command down **the same path a keystroke takes**, so
    /// there is exactly one implementation of every command and the palette
    /// cannot drift from the key that runs it.
    Run(Command),
    /// Go there.
    Cd(PathBuf),
    /// Switch to tab *n*, 0-based.
    Tab(usize),
    /// Flip this tab between the list and the grid.
    ///
    /// Not a [`Command`] because df-core's enum has no variant for it (see this
    /// module's note in the Phase 5 report): the keymap cannot bind a view
    /// toggle until that variant exists, so for now the palette is the only
    /// door to it and this is the row behind that door.
    ToggleView,
}

/// What kind of thing a row is, which decides its glyph and its colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Command,
    Place,
    Tab,
    View,
}

/// One row of the card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The bright half: what this row *is*. A command's description, a
    /// bookmark's label, a directory's name.
    pub label: String,
    /// The dim half, right-aligned: a chord, or the path a place is at.
    pub detail: String,
    pub kind: Kind,
    pub choice: Choice,
}

/// One row that survived the query, and where it matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hit {
    /// Index into the pool the hits were ranked from.
    pub index: usize,
    pub score: i32,
    /// Byte ranges into [`Row::label`] and [`Row::detail`] — the same shape the
    /// file listing highlights its filter matches with, so one painter draws
    /// both.
    pub label: Vec<Span>,
    pub detail: Vec<Span>,
}

/// Narrow `rows` by `query` and put the best first.
///
/// Both halves of a row are searched, because both are things a person types:
/// "palette" is in the label and `ctrl` is in the chord, and `Work` is in a
/// bookmark's path but not always in its name. The row's score is the better of
/// the two — a label hit and a detail hit are both hits, and adding them would
/// let a row that matched mediocrely twice beat one that matched perfectly
/// once.
///
/// Ties keep the pool's order, which is what makes the most-recently-used
/// ordering survive a query that does not distinguish two rows (see [`Mru`]).
pub fn rank(rows: &[Row], query: &str) -> Vec<Hit> {
    let mut hits: Vec<Hit> = rows
        .iter()
        .enumerate()
        .filter_map(|(index, row)| {
            let label = fuzzy::score(&row.label, query);
            let detail = fuzzy::score(&row.detail, query);
            let score = match (&label, &detail) {
                (None, None) => return None,
                (a, b) => a
                    .as_ref()
                    .map(|m| m.score)
                    .max(b.as_ref().map(|m| m.score))
                    .unwrap_or(0),
            };
            Some(Hit {
                index,
                score,
                label: label.map(|m| m.spans).unwrap_or_default(),
                detail: detail.map(|m| m.spans).unwrap_or_default(),
            })
        })
        .collect();
    // Stable, so equal scores stay in pool order.
    hits.sort_by_key(|hit| std::cmp::Reverse(hit.score));
    hits
}

/// The commands a person has actually run from the palette, most recent first.
///
/// Session-local and unpersisted on purpose (see [`MRU_LIMIT`]). It exists for
/// one behaviour: an empty palette opens on what you did last, so the second
/// use of a command is `Ctrl+p Enter` rather than `Ctrl+p` and a word.
#[derive(Debug, Default, Clone)]
pub struct Mru {
    ids: Vec<String>,
}

impl Mru {
    /// Record a use. Moves an id already in the list to the front rather than
    /// adding a second copy of it.
    pub fn touch(&mut self, id: impl Into<String>) {
        let id = id.into();
        self.ids.retain(|held| *held != id);
        self.ids.insert(0, id);
        self.ids.truncate(MRU_LIMIT);
    }

    /// Where `id` sits, or `None` if it has never been run. Lower is more
    /// recent.
    pub fn rank_of(&self, id: &str) -> Option<usize> {
        self.ids.iter().position(|held| held == id)
    }

    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }
}

/// Sort `rows` so the recently-used ones come first, keeping everything else in
/// the order it arrived.
///
/// A stable sort on `(recency, arrival)`, which is the only way to get "your
/// four favourites, then the shipped order" without inventing a second ordering
/// for the tail.
pub fn by_recency(rows: &mut [Row], mru: &Mru) {
    if mru.is_empty() {
        return;
    }
    rows.sort_by_key(|row| match &row.choice {
        Choice::Run(command) => mru.rank_of(&command.id()).unwrap_or(usize::MAX),
        _ => usize::MAX,
    });
}

/// The `z` overlay's list: everywhere you pinned, everywhere you have been,
/// everywhere you bookmarked, and everywhere zoxide remembers — as one list
/// with no duplicates.
///
/// Order is **pins, then history, then bookmarks, then zoxide**, and it is an
/// order rather than a score for a reason: the sources answer different
/// questions. The pins are first because somebody asked for exactly that —
/// they are the places chosen by hand *and* asked to be at the top — and in
/// the order they were pinned, which is theirs. Then the one that is nearly
/// always right, "somewhere I was a moment ago". Bookmarks come next because
/// they are places a person chose by hand. zoxide's own ranking is preserved
/// within its own tail, and `Z` is the overlay for when zoxide's answer is the
/// one you want.
///
/// Deduplication is by path, first occurrence winning, so a bookmark that is
/// also in the history keeps the history's position and the bookmark's *label*
/// is lost — which is the right way round: you are looking for the place, and
/// the place is already at the top.
pub fn merge_places(
    pins: &[Pin],
    history: &[PathBuf],
    bookmarks: &[Bookmark],
    zoxide: &[ZoxideDir],
    home: Option<&Path>,
) -> Vec<Row> {
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut out: Vec<Row> = Vec::new();
    let mut push = |path: PathBuf, label: Option<String>, seen: &mut HashSet<PathBuf>| {
        if !seen.insert(path.clone()) {
            return;
        }
        out.push(place_row(&path, label, home));
    };
    for pin in pins {
        let path = pin.expanded_path();
        // `z` goes to directories on this machine; a pinned server is
        // `g space`'s, which can reach one (PLAN §7.6).
        if path.contains("://") {
            continue;
        }
        push(PathBuf::from(path), None, &mut seen);
    }
    // The history arrives oldest-first; the useful end is the recent one.
    for path in history.iter().rev() {
        push(path.clone(), None, &mut seen);
    }
    for bookmark in bookmarks {
        let path = bookmark.expanded_path();
        // The SFTP bookmarks are destinations the vfs owns, not directories
        // this overlay can `cd` to (PLAN §7.6).
        if path.contains("://") {
            continue;
        }
        push(
            PathBuf::from(path),
            Some(bookmark.description.clone()),
            &mut seen,
        );
    }
    for dir in zoxide {
        push(dir.path.clone(), None, &mut seen);
    }
    out
}

/// zoxide's list on its own, in the order [`df_core::zoxide::query`] returned.
pub fn zoxide_rows(matches: &[df_core::zoxide::Match], home: Option<&Path>) -> Vec<Row> {
    matches
        .iter()
        .map(|m| place_row(&m.dir.path, None, home))
        .collect()
}

/// One directory as a row: its name in the bright half, its path in the dim
/// one.
fn place_row(path: &Path, label: Option<String>, home: Option<&Path>) -> Row {
    let name = label.unwrap_or_else(|| {
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            // The filesystem root has no file name, and "/" is what it is
            // called.
            .unwrap_or_else(|| path.to_string_lossy().into_owned())
    });
    Row {
        label: name,
        detail: shorten_home(path, home),
        kind: Kind::Place,
        choice: Choice::Cd(path.to_path_buf()),
    }
}

/// `/home/brian/Work` → `~/Work`.
///
/// Not cosmetic: `/home/brian/` is a prefix every path on this machine shares,
/// so it carries no information and spends a third of the column saying
/// nothing. `~` says the same thing in one character, and the painter dims what
/// is left of the directory part so the eye lands on the last component.
pub fn shorten_home(path: &Path, home: Option<&Path>) -> String {
    let text = path.to_string_lossy();
    let Some(home) = home else {
        return text.into_owned();
    };
    let home = home.to_string_lossy();
    if home.is_empty() {
        return text.into_owned();
    }
    match text.strip_prefix(home.as_ref()) {
        Some("") => "~".to_string(),
        // Only at a component boundary: `/home/brianne` is not inside `~`.
        Some(rest) if rest.starts_with('/') => format!("~{rest}"),
        _ => text.into_owned(),
    }
}

/// The overlay's own state: what is in it, what has been typed, and where the
/// cursor is.
pub struct Finder {
    pub source: Source,
    /// The query, on df-core's vi line editor — the same one rename and filter
    /// use (PLAN §4.2). A second line editor in this program would be a second
    /// set of Unicode edge cases.
    pub buffer: InputBuffer,
    /// Every candidate, in the order an empty query shows them.
    pub pool: Vec<Row>,
    /// The pool narrowed and ranked by the query. Rebuilt on every keystroke —
    /// the pool is a few hundred rows and the ranker is linear in each, so a
    /// cache here would buy nothing and cost an invalidation bug.
    pub hits: Vec<Hit>,
    pub cursor: usize,
    pub first: usize,
}

impl Finder {
    pub fn new(source: Source, pool: Vec<Row>) -> Finder {
        let hits = rank(&pool, "");
        // The empty query is the same for every source, so the initial rank
        // needs no `ranking_query` and the field can be built after it.
        Finder {
            source,
            buffer: InputBuffer::new(String::new(), 0),
            pool,
            hits,
            cursor: 0,
            first: 0,
        }
    }

    pub fn query(&self) -> &str {
        self.buffer.text()
    }

    /// What the ranker is given, which is not always what was typed.
    ///
    /// `Z` is the one exception, and it is the whole point of that overlay:
    /// zoxide has already matched *and scored* the query, and its order —
    /// match quality, then frecency — is the answer the user asked for by
    /// pressing `Z` rather than `z`. Re-ranking it by subsequence score would
    /// throw away the frecency and leave `Z` as a worse `z`. So its pool
    /// arrives pre-filtered and is ranked against an empty query, which
    /// [`rank`] defines as "keep the pool's order".
    fn ranking_query(&self) -> &str {
        match self.source {
            Source::Zoxide => "",
            Source::Commands | Source::Jump | Source::Places => self.buffer.text(),
        }
    }

    /// Re-rank after a keystroke. The cursor goes back to the top, because the
    /// row it was on is usually not in the new list and the first row of the
    /// narrowed list is the row the narrowing was *for*.
    pub fn requery(&mut self) {
        let query = self.ranking_query().to_string();
        self.hits = rank(&self.pool, &query);
        self.cursor = 0;
        self.first = 0;
    }

    /// Replace the candidates without disturbing what has been typed — how the
    /// zoxide overlay re-queries its database as the query changes.
    pub fn set_pool(&mut self, pool: Vec<Row>) {
        self.pool = pool;
        let query = self.ranking_query().to_string();
        self.hits = rank(&self.pool, &query);
        self.cursor = self.cursor.min(self.hits.len().saturating_sub(1));
    }

    /// `↑` / `↓`, **wrapping**.
    ///
    /// Wrapping rather than clamping, which only this list and the `M` card
    /// ([`crate::mounts::Card::move_cursor`]) do. The file panes clamp because
    /// a directory is a place with a top and a bottom and running off the end
    /// of it would lose your place. A palette is a short ranked menu you are
    /// stepping around, and `↑` from the first row meaning "the last one" is
    /// what every palette does and what the hand expects.
    pub fn move_cursor(&mut self, delta: isize) {
        let len = self.hits.len();
        if len == 0 {
            self.cursor = 0;
            return;
        }
        let len = len as isize;
        self.cursor = (self.cursor as isize + delta).rem_euclid(len) as usize;
    }

    /// The row under the cursor.
    pub fn chosen(&self) -> Option<&Row> {
        let hit = self.hits.get(self.cursor)?;
        self.pool.get(hit.index)
    }

    /// Keep the cursor on screen, by the same scrolloff rule the panes use.
    pub fn scroll_into_view(&mut self, scrolloff: usize) {
        self.first = crate::viewport::first_visible(
            self.first,
            self.cursor,
            self.hits.len(),
            ROWS,
            scrolloff,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command_row(description: &str, chord: &str, command: Command) -> Row {
        Row {
            label: description.to_string(),
            detail: chord.to_string(),
            kind: Kind::Command,
            choice: Choice::Run(command),
        }
    }

    fn pool() -> Vec<Row> {
        vec![
            command_row("Copy the path", "c c", Command::CopyPath),
            command_row("Command palette", "Ctrl+p", Command::CommandPalette),
            command_row("Toggle hidden files", ".", Command::ToggleHidden),
        ]
    }

    /// The label and the chord are both searched — `ctrl` is not in any
    /// description, and it still has to find the row.
    #[test]
    fn both_halves_of_a_row_are_searched() {
        let rows = pool();
        let ids = |query: &str| -> Vec<String> {
            rank(&rows, query)
                .into_iter()
                .map(|hit| rows[hit.index].label.clone())
                .collect()
        };
        assert_eq!(ids("ctrl"), vec!["Command palette"]);
        assert!(ids("copy").contains(&"Copy the path".to_string()));
        assert!(ids("zzz").is_empty());
    }

    /// An empty query is every row, in pool order — the order the caller built,
    /// which is what carries the most-recently-used ranking.
    #[test]
    fn an_empty_query_is_the_pool_in_order() {
        let rows = pool();
        let hits = rank(&rows, "");
        assert_eq!(hits.len(), rows.len());
        assert_eq!(
            hits.iter().map(|h| h.index).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
    }

    /// A use moves a command to the front, a second use of the same one does
    /// not add a second copy, and the list is bounded.
    #[test]
    fn the_mru_is_ordered_deduplicated_and_capped() {
        let mut mru = Mru::default();
        mru.touch("copy-path");
        mru.touch("command-palette");
        mru.touch("copy-path");
        assert_eq!(mru.rank_of("copy-path"), Some(0));
        assert_eq!(mru.rank_of("command-palette"), Some(1));
        assert_eq!(mru.rank_of("quit"), None);
        for n in 0..MRU_LIMIT * 2 {
            mru.touch(format!("id-{n}"));
        }
        assert_eq!(mru.ids.len(), MRU_LIMIT);
        assert_eq!(mru.rank_of("copy-path"), None, "aged out");
    }

    /// The recently-used commands float to the top and everything else keeps
    /// the order it was declared in — a palette whose tail reshuffles is a
    /// palette you cannot learn.
    #[test]
    fn recency_reorders_the_head_and_leaves_the_tail_alone() {
        let mut rows = pool();
        let mut mru = Mru::default();
        mru.touch(Command::ToggleHidden.id());
        by_recency(&mut rows, &mru);
        let labels: Vec<&str> = rows.iter().map(|r| r.label.as_str()).collect();
        assert_eq!(
            labels,
            vec!["Toggle hidden files", "Copy the path", "Command palette"]
        );
    }

    fn bookmark(key: &str, path: &str, description: &str) -> Bookmark {
        Bookmark {
            key: key.to_string(),
            path: path.to_string(),
            description: description.to_string(),
        }
    }

    fn zoxide_dir(path: &str) -> ZoxideDir {
        ZoxideDir {
            path: PathBuf::from(path),
            rank: 1.0,
            last_accessed: 0,
        }
    }

    /// The three sources become one list, newest history first, with each path
    /// appearing exactly once wherever it first appeared.
    #[test]
    fn the_three_place_sources_merge_without_duplicates() {
        let history = vec![PathBuf::from("/tmp"), PathBuf::from("/home/brian/Work")];
        let bookmarks = vec![
            bookmark("w", "/home/brian/Work", "Go to Work"),
            bookmark("d", "/home/brian/Downloads", "Go to Downloads"),
        ];
        let zoxide = vec![zoxide_dir("/tmp"), zoxide_dir("/etc")];
        let rows = merge_places(
            &[],
            &history,
            &bookmarks,
            &zoxide,
            Some(Path::new("/home/brian")),
        );
        let paths: Vec<&str> = rows.iter().map(|r| r.detail.as_str()).collect();
        assert_eq!(paths, vec!["~/Work", "/tmp", "~/Downloads", "/etc"]);
        // The history's copy of `~/Work` won, so it is *not* wearing the
        // bookmark's label.
        assert_eq!(rows[0].label, "Work");
        assert_eq!(rows[2].label, "Go to Downloads");
        assert!(matches!(&rows[1].choice, Choice::Cd(p) if p == Path::new("/tmp")));
    }

    /// A bookmark that names a remote service is not a directory this overlay
    /// can go to, so it is not offered (PLAN §7.6).
    #[test]
    fn remote_bookmarks_are_left_out_of_the_jump_list() {
        let rows = merge_places(
            &[Pin {
                path: "sftp://showandtour1/srv".to_string(),
                key: None,
            }],
            &[],
            &[bookmark("1", "sftp://showandtour1/", "Go to server one")],
            &[],
            None,
        );
        assert!(rows.is_empty());
    }

    /// The pins come first, in the order they were pinned — ahead of the
    /// history — and a pinned place the history also holds is listed once,
    /// where the pin put it.
    #[test]
    fn the_pins_head_the_jump_list() {
        let pins = vec![
            Pin {
                path: "/srv/b".to_string(),
                key: Some("b".to_string()),
            },
            Pin {
                path: "/tmp".to_string(),
                key: None,
            },
        ];
        let history = vec![PathBuf::from("/tmp"), PathBuf::from("/etc")];
        let rows = merge_places(
            &pins,
            &history,
            &[bookmark("w", "/work", "Go to Work")],
            &[zoxide_dir("/srv/b")],
            None,
        );
        let paths: Vec<&str> = rows.iter().map(|r| r.detail.as_str()).collect();
        assert_eq!(paths, vec!["/srv/b", "/tmp", "/etc", "/work"]);
    }

    /// `~` is only a prefix at a component boundary — `/home/brianne` is not
    /// inside `/home/brian`.
    #[test]
    fn the_home_prefix_shortens_only_whole_components() {
        let home = Some(Path::new("/home/brian"));
        assert_eq!(shorten_home(Path::new("/home/brian"), home), "~");
        assert_eq!(shorten_home(Path::new("/home/brian/Work"), home), "~/Work");
        assert_eq!(
            shorten_home(Path::new("/home/brianne/Work"), home),
            "/home/brianne/Work"
        );
        assert_eq!(shorten_home(Path::new("/etc"), None), "/etc");
    }

    /// `↑` from the top is the bottom. The one wrapping list in the program,
    /// and the reason is on [`Finder::move_cursor`].
    #[test]
    fn the_cursor_wraps_at_both_ends() {
        let mut finder = Finder::new(Source::Commands, pool());
        assert_eq!(finder.cursor, 0);
        finder.move_cursor(-1);
        assert_eq!(finder.cursor, 2);
        finder.move_cursor(1);
        assert_eq!(finder.cursor, 0);
        finder.move_cursor(2);
        assert_eq!(finder.cursor, 2);
    }

    /// `Z` keeps zoxide's frecency order whatever is typed into it; `z` and
    /// the palette re-rank as you type. Re-ranking `Z` would make it a worse
    /// `z` rather than a different thing.
    #[test]
    fn the_zoxide_overlay_does_not_re_rank_what_zoxide_already_ranked() {
        let rows = vec![
            place_row(Path::new("/aaa/exact"), None, None),
            place_row(Path::new("/zzz/e-x-a-c-t"), None, None),
        ];
        let mut z = Finder::new(Source::Zoxide, rows.clone());
        let _ = z.buffer.insert_text("exact");
        z.requery();
        assert_eq!(
            z.hits.len(),
            2,
            "zoxide already filtered; nothing is dropped"
        );
        assert_eq!(z.hits[0].index, 0, "zoxide's order survives");
        let mut j = Finder::new(Source::Jump, rows);
        let _ = j.buffer.insert_text("exact");
        j.requery();
        assert_eq!(j.hits[0].index, 0, "the tighter match wins on `z`");
        assert!(j.hits[0].score > j.hits[1].score);
    }

    /// An empty list has nowhere to move to and must not divide by zero on the
    /// way to finding that out.
    #[test]
    fn an_empty_list_has_no_cursor_to_move() {
        let mut finder = Finder::new(Source::Commands, Vec::new());
        finder.move_cursor(1);
        assert_eq!(finder.cursor, 0);
        assert!(finder.chosen().is_none());
    }
}
