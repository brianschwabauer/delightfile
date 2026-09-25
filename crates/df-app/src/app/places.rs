//! Pinned places, and the one list they make with `[goto]`.
//!
//! A pin is a folder put on the list by hand — `g b` on the folder on screen,
//! or "Pin folder" on a row — and kept in the state file
//! ([`df_core::state::pins`]), because a keystroke made it and the program
//! never writes config. `[goto]` is the other half: the owner's own table in
//! `delightfile.toml`, which stays exactly as written and stays in charge.
//! There is **no sidebar**. A column of favourites would be a fourth pane
//! fighting the three for width, and a surface the keyboard would have to be
//! moved into; everything here is a list that comes up when asked for and goes
//! away again.
//!
//! ## The goto table
//!
//! [`Command::Goto`] carries a slot, and the slot indexes one table: `[goto]`
//! first, then every pin that has a key ([`goto_table`]). `[goto]` rows keep
//! the slots they always had, so a `keymap.toml` that says `goto-3` still means
//! what it meant. The pins' `g` rows are laid over the keymap as it stood
//! *before any pin* — the defaults, `[goto]` and `keymap.toml` — by
//! [`Registry::add_bookmarks`], which binds a key only where nothing answers
//! to it yet. So on a clash the hand-written row wins, a pin can never take
//! `g g` from the top of the list, and a pin added, removed or re-keyed is a
//! rebuild from that base rather than a layer on a layer.
//! (`Registry::apply_bookmarks` re-applied in place would not have done: it
//! takes its keys from whatever holds them, which is right for `[goto]` and
//! wrong for a pin, and a second run of it throws away the `g` rows a
//! `keymap.toml` added after the first.)
//!
//! ## The Places list
//!
//! Every surface that lists places reads one list ([`pool`]): the pins in the
//! order they were pinned, then the `[goto]` rows that are not already pinned,
//! then home if nothing above was home. `g space` is that list in the finder,
//! the mount card's Places section is it without the home row, and the app
//! menu's Go list is it with the pin row under it. `z` has its own, wider pool
//! (history and zoxide besides), which starts with the pins for the same reason
//! this one does: they are the places somebody asked to have at the top.
//!
//! A child of `app` rather than a sibling, so the glue can be `impl App`
//! beside the rules it glues, and the file that already holds everything does
//! not grow a second copy of this.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use df_core::config::{expand_home, Bookmark};
use df_core::input::InputBuffer;
use df_core::keymap::{label_sequence, parse_sequence, Command, Context, Registry};
use df_core::state::{Pin, PinRefusal};

use super::{home, App};
use crate::finder::{self, Choice, Finder, Source};
use crate::input::PromptKind;
use crate::tab::Virtual;

/// The goto table and the keymap it was laid over.
pub(super) struct Places {
    /// The keymap as it stood before any pin: every rebuild starts here.
    base: Registry,
    /// `[goto]`, then the keyed pins. What `Goto(n)` indexes.
    table: Vec<Bookmark>,
    /// The folder the open `Pin … as:` prompt is asking about, written as it
    /// will be pinned.
    pub keying: Option<String>,
}

impl Places {
    /// The table for `config` and `pins`, and the keymap with the pins' keys
    /// in it. `base` is the keymap the app would have had without them.
    pub(super) fn start(base: Registry, config: &[Bookmark], pins: &[Pin]) -> (Registry, Places) {
        let mut places = Places {
            base,
            table: Vec::new(),
            keying: None,
        };
        let keymap = places.rebuild(config, pins);
        (keymap, places)
    }

    /// `[goto]` then the keyed pins: what `Goto(n)` indexes.
    pub(super) fn table(&self) -> &[Bookmark] {
        &self.table
    }

    /// The table again, and the keymap again from its base.
    pub(super) fn rebuild(&mut self, config: &[Bookmark], pins: &[Pin]) -> Registry {
        self.table = goto_table(config, pins, home().as_deref());
        let first = config.len().min(self.table.len());
        let mut keymap = self.base.clone();
        for refused in keymap.add_bookmarks(&self.table[first..], first) {
            // Expected, not a fault: a pin keyed before `[goto]` took the same
            // key, or a state file edited by hand. The pin is still on the
            // list; its key waits until nothing else holds it.
            log::debug!("a pin's key is held by something else: {refused}");
        }
        keymap
    }
}

/// `[goto]` followed by every pin with a key, as the bookmarks `Goto(n)`
/// indexes. A pin's description is where it goes, the way the shipped rows
/// say theirs.
pub(super) fn goto_table(config: &[Bookmark], pins: &[Pin], home: Option<&Path>) -> Vec<Bookmark> {
    let mut table = config.to_vec();
    table.extend(pins.iter().filter_map(|pin| {
        Some(Bookmark {
            key: pin.key.clone()?,
            path: pin.path.clone(),
            description: format!("Go to {}", said(&pin.path, home)),
        })
    }));
    table
}

/// A place as the lists show it: expanded, then home shortened back to `~`,
/// so `~/Work` and `/home/brian/Work` read the same whichever was written.
fn shown(written: &str, home: Option<&Path>) -> String {
    finder::shorten_home(Path::new(&expand_home(written)), home)
}

/// How long a place may run inside a sentence — a toast, the prompt's title,
/// a `g` row's description on the which-key card — before its middle goes.
///
/// Those three are one line each with no room to wrap, and a folder four
/// levels under `/tmp` ran the which-key card's description out through the
/// card's side and pushed the prompt's field off the end of the bar.
const BRIEF: usize = 40;

/// …and in a row of its own, the mount card's, which has the card's width.
const ROW_BRIEF: usize = 56;

/// `text` with its middle folders dropped until it fits in `max` characters,
/// the start that says whose it is kept: `~/…/crates/df-app`,
/// `sftp://host/…/www`, `…/files/Projects`. The last folder always stays
/// whole — it is the name the place is known by — so a single enormous name
/// is left to the painter's ellipsis.
fn brief(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_string();
    }
    // What stays at the front: home, or a server, and nothing else.
    let head_len = if text.starts_with("~/") {
        1
    } else if let Some(scheme) = text.find("://") {
        let after = scheme + 3;
        text[after..]
            .find('/')
            .map_or(text.len(), |slash| after + slash)
    } else {
        0
    };
    let (head, rest) = text.split_at(head_len);
    let parts: Vec<&str> = rest.split('/').filter(|part| !part.is_empty()).collect();
    // `…/`, or the head and `/…/`.
    let prefix = if head.is_empty() {
        2
    } else {
        head.chars().count() + 3
    };
    let mut tail = String::new();
    let mut kept = 0;
    for part in parts.iter().rev() {
        let longer = if tail.is_empty() {
            (*part).to_string()
        } else {
            format!("{part}/{tail}")
        };
        if !tail.is_empty() && prefix + longer.chars().count() > max {
            break;
        }
        tail = longer;
        kept += 1;
    }
    // Nothing in the middle to drop: a `…` would only make it longer.
    if kept == parts.len() {
        return text.to_string();
    }
    if head.is_empty() {
        format!("…/{tail}")
    } else {
        format!("{head}/…/{tail}")
    }
}

/// A place as a sentence says it: [`shown`], then [`brief`].
fn said(written: &str, home: Option<&Path>) -> String {
    brief(&shown(written, home), BRIEF)
}

/// How a folder is written into the state file when it is pinned: under `~`
/// when it is under home, so it survives `$HOME` moving, and as its URL when
/// it is on a server.
pub(super) fn written(dir: &Path, home: Option<&Path>) -> String {
    finder::shorten_home(dir, home)
}

/// One entry of the Places list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Place {
    /// As written in the state file or `[goto]`.
    pub path: String,
    /// Where going there goes.
    pub target: PathBuf,
    /// Its row in the goto table, when it has one: every `[goto]` row, and a
    /// pin with a key. What its key is read back from.
    pub slot: Option<u8>,
    /// Pinned by hand, so it can be unpinned.
    pub pinned: bool,
    /// A `[goto]` row's own words.
    pub description: Option<String>,
    /// The home row the list ends on when nothing above it was home. Not a
    /// pin and not a bookmark, so the mount card, which lists those, leaves it
    /// out.
    pub fallback: bool,
}

impl Place {
    /// `~/Work`, `sftp://host/srv`.
    pub fn label(&self, home: Option<&Path>) -> String {
        finder::shorten_home(&self.target, home)
    }

    /// The key that goes there, as the registry teaches it (`g w`), or none.
    pub fn key(&self, keymap: &Registry) -> Option<String> {
        self.slot
            .and_then(|slot| keymap.binding_label(Command::Goto(slot)))
    }

    /// On another machine rather than this one.
    pub fn remote(&self) -> bool {
        crate::remote::is_remote(&self.target)
    }
}

/// The Places list: the pins in the order they were pinned, the `[goto]` rows
/// that are not already there, and home if nothing above was home — each place
/// once, by where it goes.
pub(super) fn pool(config: &[Bookmark], pins: &[Pin], home: Option<&Path>) -> Vec<Place> {
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut out = Vec::new();
    // The keyed pins' slots follow `[goto]`'s, in pin order: `goto_table`'s
    // arithmetic, and it has to be the same arithmetic or a row's key is
    // another row's.
    let mut next = config.len();
    for pin in pins {
        let slot = pin.key.as_ref().and_then(|_| {
            let slot = next;
            next += 1;
            u8::try_from(slot).ok()
        });
        let target = PathBuf::from(pin.expanded_path());
        if seen.insert(target.clone()) {
            out.push(Place {
                path: pin.path.clone(),
                target,
                slot,
                pinned: true,
                description: None,
                fallback: false,
            });
        }
    }
    for (index, bookmark) in config.iter().enumerate() {
        let target = PathBuf::from(bookmark.expanded_path());
        if seen.insert(target.clone()) {
            out.push(Place {
                path: bookmark.path.clone(),
                target,
                slot: u8::try_from(index).ok(),
                pinned: false,
                description: Some(bookmark.description.clone()),
                fallback: false,
            });
        }
    }
    if let Some(home) = home.filter(|home| !seen.contains(*home)) {
        out.push(Place {
            path: "~".to_string(),
            target: home.to_path_buf(),
            slot: None,
            pinned: false,
            description: None,
            fallback: true,
        });
    }
    out
}

/// `g space`'s rows: the list, its keys on the right.
///
/// **URLs included**, unlike `z`'s: every row here is a place somebody named,
/// and `sftp://` ones are the ones that are hardest to type. Choosing one goes
/// through `jump_to`, which hands a URL to the same router `g 1` goes through.
pub(super) fn finder_rows(
    pool: &[Place],
    keymap: &Registry,
    home: Option<&Path>,
) -> Vec<finder::Row> {
    pool.iter()
        .map(|place| finder::Row {
            label: place.label(home),
            detail: place
                .key(keymap)
                .or_else(|| place.description.clone())
                .unwrap_or_default(),
            kind: finder::Kind::Place,
            choice: Choice::Cd(place.target.clone()),
        })
        .collect()
}

/// The mount card's Places section: the list without its home fallback, each
/// row saying its key, or a `[goto]` row's words, or that it is a pin.
pub(super) fn card_places(
    pool: &[Place],
    keymap: &Registry,
    home: Option<&Path>,
) -> Vec<crate::mounts::Place> {
    pool.iter()
        .filter(|place| !place.fallback)
        .map(|place| crate::mounts::Place {
            name: brief(&place.label(home), ROW_BRIEF),
            detail: place
                .key(keymap)
                .or_else(|| place.description.clone())
                .unwrap_or_else(|| "pinned".to_string()),
            target: place.target.clone(),
            remote: place.remote(),
            pinned: place.pinned,
        })
        .collect()
}

/// The sentence `g b` says where it cannot pin, for [`App::refusal`]'s gate —
/// or `None` where it can. A remote folder can be pinned: its URL is a place
/// `g space` and `g <key>` both know how to reach.
pub(super) fn refusal(command: Command, here: Option<Virtual>) -> Option<&'static str> {
    if command != Command::PinToggle {
        return None;
    }
    match here {
        Some(Virtual::Archive) => Some("Only a folder can be pinned — this is inside an archive"),
        Some(Virtual::Trash) => Some("The trash is not a folder to pin — g t goes there"),
        Some(Virtual::Remote) | None => None,
    }
}

impl App {
    /// The Places list as it stands.
    fn places_pool(&self) -> Vec<Place> {
        pool(&self.config.goto, self.state.pins(), home().as_deref())
    }

    /// `g b`: the folder on screen onto the list, by way of the prompt that
    /// asks for its key — or, if it is on the list already, off it.
    pub(super) fn pin_toggle(&mut self, now: Instant) {
        let here = written(&self.cwd(), home().as_deref());
        if self.state.is_pinned(&here) {
            self.unpin(&here, now);
            return;
        }
        self.open_prompt_with(PromptKind::Pin, InputBuffer::new("", 0));
        if let Some(prompt) = &mut self.prompt {
            prompt.label = Some(format!("Pin {} as:", said(&here, home().as_deref())));
        }
        self.places.keying = Some(here);
    }

    /// `Enter` in `Pin … as:`. Returns whether the prompt is done; a key that
    /// cannot be had is said in a toast and the prompt stays, because the fix
    /// is a different letter in the field the caret is already in.
    pub(super) fn pin_submit(&mut self, text: &str, now: Instant) -> bool {
        let Some(path) = self.places.keying.clone() else {
            return true;
        };
        let key = text.trim();
        if !key.is_empty() {
            if let Some(taken) = self.key_taken(key) {
                self.toasts.notice(taken, now);
                return false;
            }
        }
        match self.state.pin(path.clone(), Some(key.to_string())) {
            Ok(()) => {}
            // Pinned from somewhere else since the prompt opened — another
            // row's menu, say. What was asked for is true.
            Err(PinRefusal::AlreadyPinned(_)) => {}
            Err(refusal) => {
                self.toasts.notice(refusal.to_string(), now);
                return false;
            }
        }
        self.places.keying = None;
        self.pins_changed(now);
        let message = self.pinned_message(&path);
        self.toasts.notice(message, now);
        true
    }

    /// Why `g <key>` cannot be this pin's, in the words the toast uses, or
    /// `None` when it can: not a key at all, or a key something already
    /// answers to — a `[goto]` row, another pin, a built-in chord, a
    /// `keymap.toml` line. The keymap is asked rather than the tables, so the
    /// answer is whatever pressing it would do now.
    fn key_taken(&self, key: &str) -> Option<String> {
        let seq = match parse_sequence(&format!("g {key}")) {
            Ok(seq) if seq.len() == 2 => seq,
            _ => return Some(PinRefusal::NotAKey(key.to_string()).to_string()),
        };
        let holder = self.keymap.holder(Context::Files, &seq)?;
        let what = match holder.command {
            Command::Goto(slot) => match self.places.table().get(slot as usize) {
                Some(bookmark) => said(&bookmark.path, home().as_deref()),
                None => holder.description.clone(),
            },
            _ => holder.description.clone(),
        };
        Some(format!("{} is already {what}", label_sequence(&seq)))
    }

    /// "Pinned ~/Work · g w", or with no key of its own, the key that finds
    /// it anyway.
    fn pinned_message(&self, path: &str) -> String {
        let place = self
            .places_pool()
            .into_iter()
            .find(|place| place.pinned && place.path == path);
        let shown = said(path, home().as_deref());
        match place.and_then(|place| place.key(&self.keymap)) {
            Some(key) => format!("Pinned {shown} · {key}"),
            None => match self.keymap.binding_label(Command::GotoInteractive) {
                Some(jump) => format!("Pinned {shown} · {jump} to jump"),
                None => format!("Pinned {shown}"),
            },
        }
    }

    /// Take `path` off the list, and say so.
    fn unpin(&mut self, path: &str, now: Instant) {
        if let Some(pin) = self.state.unpin(path) {
            self.pins_changed(now);
            self.toasts.notice(
                format!("Unpinned {}", said(&pin.path, home().as_deref())),
                now,
            );
        }
    }

    /// Everything a change to the pins has to reach: the goto table and the
    /// keymap (so `g`'s card and `Goto(n)` agree with the list at once), the
    /// state file's write-behind, and the mount card if it is up.
    fn pins_changed(&mut self, now: Instant) {
        self.keymap = self.places.rebuild(&self.config.goto, self.state.pins());
        self.state_changed(now);
        if self.mounts.is_some() {
            let places = self.card_places();
            if let Some(card) = &mut self.mounts {
                card.set_places(places);
            }
        }
    }

    /// "Pin folder" on a directory row's menu: that folder, keyless, without
    /// a prompt — a menu row that opened a question would be a menu row that
    /// did not do what it said. Or, on one that is pinned, off the list.
    pub(super) fn pin_row(&mut self, now: Instant) {
        let Some(dir) = self
            .tab()
            .cwd
            .dir
            .cursor_entry()
            .filter(|entry| entry.is_dir())
            .map(|entry| entry.path.clone())
        else {
            return;
        };
        let path = written(&dir, home().as_deref());
        if self.state.is_pinned(&path) {
            self.unpin(&path, now);
            return;
        }
        if self.state.pin(path.clone(), None).is_ok() {
            self.pins_changed(now);
            let message = self.pinned_message(&path);
            self.toasts.notice(message, now);
        }
    }

    /// Whether the folder under the row menu's pointer is pinned, so the row
    /// can say which way it goes.
    pub(super) fn row_pinned(&self) -> bool {
        self.tab().cwd.dir.cursor_entry().is_some_and(|entry| {
            self.state
                .is_pinned(&written(&entry.path, home().as_deref()))
        })
    }

    /// Whether the folder on screen is pinned.
    pub(super) fn here_pinned(&self) -> bool {
        self.state
            .is_pinned(&written(&self.cwd(), home().as_deref()))
    }

    /// `g space`: the Places list in the finder.
    pub(super) fn open_places(&mut self) {
        let home = home();
        let rows = finder_rows(&self.places_pool(), &self.keymap, home.as_deref());
        self.finder = Some(Finder::new(Source::Places, rows));
        self.sync_context();
    }

    /// The mount card's Places section, as it stands.
    pub(super) fn card_places(&self) -> Vec<crate::mounts::Place> {
        card_places(&self.places_pool(), &self.keymap, home().as_deref())
    }

    /// A new mount card, its Places section filled — the part of `M` that is
    /// this machine's own and needs no udisks2 to answer — and the cursor
    /// where the first disk will be ([`crate::mounts::Card::with_places`]).
    pub(super) fn mount_card(&self) -> crate::mounts::Card {
        crate::mounts::Card::with_places(self.card_places())
    }

    /// `d` on the mount card: unpin the place under the cursor. A `[goto]`
    /// row is the config's, and the key says so rather than doing nothing.
    pub(super) fn unpin_selected_place(&mut self, now: Instant) {
        let Some(place) = self
            .mounts
            .as_ref()
            .and_then(|card| card.selected_place())
            .cloned()
        else {
            return;
        };
        if !place.pinned {
            self.toasts.notice(
                format!("{} is a [goto] bookmark, not a pin", place.name),
                now,
            );
            return;
        }
        self.unpin(&place.target.to_string_lossy(), now);
    }

    /// `Enter` or a click on a Places row of the mount card: go there, the
    /// way its `g` key would.
    pub(super) fn go_selected_place(&mut self, now: Instant) {
        let Some(target) = self
            .mounts
            .as_ref()
            .and_then(|card| card.selected_place())
            .map(|place| place.target.clone())
        else {
            return;
        };
        self.close_overlay(now);
        self.navigate(target, now);
    }

    /// The app menu's Go list, with the pin row under it.
    pub(super) fn go_item(&self) -> crate::menu::Item {
        let home = home();
        let rows: Vec<crate::menu::GoRow> = self
            .places_pool()
            .iter()
            .map(|place| crate::menu::GoRow {
                label: brief(&place.label(home.as_deref()), BRIEF),
                keys: place.key(&self.keymap).unwrap_or_default(),
            })
            .collect();
        crate::menu::go_item(&rows, self.here_pinned(), &self.keymap, |command| {
            self.refusal(command).is_some()
        })
    }

    /// A row of the Go list, chosen: its place, gone to the way its `g` key
    /// would go.
    pub(super) fn go_place(&mut self, index: usize, now: Instant) {
        if let Some(place) = self.places_pool().get(index) {
            let target = place.target.clone();
            self.navigate(target, now);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use df_core::config::{Config, Theme};
    use df_core::fs::LoadState;
    use df_core::keymap::{parse_chord, Chord, ContextStack, Key, WhenFlags};
    use df_core::state::StateStore;

    use super::*;
    use crate::app::Waker;
    use crate::menu::Action;
    use crate::mounts::{Item, Line};

    fn bookmark(key: &str, path: &str) -> Bookmark {
        Bookmark {
            key: key.to_string(),
            path: path.to_string(),
            description: format!("Go to {path}"),
        }
    }

    fn pin(path: &str, key: Option<&str>) -> Pin {
        Pin {
            path: path.to_string(),
            key: key.map(str::to_string),
        }
    }

    fn text(path: &Path) -> String {
        path.to_string_lossy().into_owned()
    }

    // ── The rules, without an app ───────────────────────────────────────────

    /// `[goto]` keeps its slots and the keyed pins follow in pin order; a
    /// keyless pin is not in the table at all.
    #[test]
    fn the_goto_table_is_goto_then_the_keyed_pins() {
        let home = Some(Path::new("/home/me"));
        let config = vec![bookmark("h", "~"), bookmark("w", "~/Work")];
        let pins = vec![
            pin("/srv/a", Some("a")),
            pin("/srv/quiet", None),
            pin("~/b", Some("b")),
        ];
        let table = goto_table(&config, &pins, home);
        let keys: Vec<&str> = table.iter().map(|b| b.key.as_str()).collect();
        assert_eq!(keys, vec!["h", "w", "a", "b"]);
        assert_eq!(table[2].description, "Go to /srv/a");
        assert_eq!(table[3].path, "~/b", "written as given");
    }

    /// Pins first in their order, then `[goto]` without what is already
    /// there, then home only if nothing was home — and each keyed pin's slot
    /// is its row in [`goto_table`].
    #[test]
    fn the_places_list_is_pins_then_goto_then_home() {
        let home = Some(Path::new("/home/me"));
        let config = vec![bookmark("w", "/work"), bookmark("s", "sftp://box/srv")];
        let pins = vec![
            pin("/b", Some("x")),
            pin("/work/", None),
            pin("/c", Some("y")),
        ];
        let list = pool(&config, &pins, home);
        let targets: Vec<&Path> = list.iter().map(|p| p.target.as_path()).collect();
        assert_eq!(
            targets,
            vec![
                Path::new("/b"),
                Path::new("/work/"),
                Path::new("/c"),
                Path::new("sftp://box/srv"),
                Path::new("/home/me"),
            ],
            "the pinned /work/ stands for [goto]'s /work"
        );
        assert_eq!(list[0].slot, Some(2));
        assert_eq!(list[1].slot, None);
        assert_eq!(list[2].slot, Some(3));
        assert_eq!(list[3].slot, Some(1));
        assert!(list[3].remote() && !list[0].remote());
        assert!(list[4].fallback);
        let table = goto_table(&config, &pins, home);
        assert_eq!(table[2].path, "/b");
        assert_eq!(table[3].path, "/c");

        // A [goto] row that is home already makes the fallback unnecessary.
        let with_home = pool(&[bookmark("h", "/home/me")], &[], home);
        assert_eq!(with_home.len(), 1);
        assert!(!with_home[0].fallback);
    }

    /// `g b` is refused inside an archive and in the trash, and nowhere else.
    #[test]
    fn a_pin_is_refused_where_there_is_no_folder() {
        assert!(refusal(Command::PinToggle, Some(Virtual::Archive)).is_some());
        assert!(refusal(Command::PinToggle, Some(Virtual::Trash)).is_some());
        assert_eq!(refusal(Command::PinToggle, Some(Virtual::Remote)), None);
        assert_eq!(refusal(Command::PinToggle, None), None);
        assert_eq!(refusal(Command::Trash, Some(Virtual::Archive)), None);
    }

    /// A `[goto]` table that takes `b` takes `g b` with it: the pin command
    /// has no key, the table's row goes where it says, and a pin still gets
    /// the slot after it.
    #[test]
    fn goto_wins_g_b_itself() {
        let config = vec![bookmark("b", "/b")];
        let mut base = Registry::defaults();
        let warnings = base.apply_bookmarks(&config, Path::new("delightfile.toml"));
        assert_eq!(warnings.len(), 1, "{warnings:?}");
        let (keymap, places) = Places::start(base, &config, &[pin("/p", Some("p"))]);
        assert_eq!(keymap.binding_label(Command::PinToggle), None);
        assert_eq!(
            keymap.binding_label(Command::Goto(0)).as_deref(),
            Some("g b")
        );
        assert_eq!(
            keymap.binding_label(Command::Goto(1)).as_deref(),
            Some("g p")
        );
        assert_eq!(places.table().len(), 2);
    }

    // ── Through the app ─────────────────────────────────────────────────────

    /// An app with a sandbox of its own under `$TMPDIR`: `config` in place of
    /// the user's (its `[goto]` applied to the keymap the way `App::new`
    /// applies it), a state file of its own, and the listing opened on
    /// `files/`, which holds the folders `sub/` and `other/`. `setup` gets
    /// `files/`, the config and the store before the app is built from them.
    struct Sandbox {
        app: App,
        root: PathBuf,
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    impl Sandbox {
        fn new(name: &str, setup: impl FnOnce(&Path, &mut Config, &mut StateStore)) -> Sandbox {
            let root =
                std::env::temp_dir().join(format!("df-places-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            let files = root.join("files");
            for dir in ["sub", "other"] {
                std::fs::create_dir_all(files.join(dir)).expect("make the fixture");
            }
            let mut config = Config::default();
            let mut state = StateStore::load_from(root.join("state").join("state"));
            setup(&files, &mut config, &mut state);
            let mut keymap = Registry::defaults();
            let _ = keymap.apply_bookmarks(&config.goto, Path::new("delightfile.toml"));
            let mut app = App::assemble(
                Waker {
                    ring: Arc::new(|| {}),
                    source: "test",
                },
                crate::cli::Args {
                    start: Some(files),
                    ..Default::default()
                },
                config,
                Theme::default(),
                keymap,
                state,
            );
            // zoxide's own database is this machine's, not the test's.
            app.zoxide = Some(Vec::new());
            let mut sandbox = Sandbox { app, root };
            sandbox.settle();
            sandbox
        }

        fn plain(name: &str) -> Sandbox {
            Sandbox::new(name, |_, _, _| {})
        }

        fn files(&self) -> PathBuf {
            self.root.join("files")
        }

        /// Until the listing on screen has landed.
        fn settle(&mut self) {
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                for update in self.app.scanner.drain() {
                    self.app.tabs.active_mut().apply(&update);
                }
                if self.app.tab().cwd.dir.state() == LoadState::Loaded || Instant::now() > deadline
                {
                    break;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        }

        fn go(&mut self, dir: PathBuf) {
            self.app.navigate(dir, Instant::now());
            self.settle();
        }

        /// Keys pressed, one chord per word (`g b`, `g space`, `down`),
        /// through the router a keystroke takes.
        fn keys(&mut self, keys: &str) {
            for word in keys.split_whitespace() {
                let chord = parse_chord(word).expect("a chord");
                self.app.route_chord(chord, 10, Instant::now());
            }
        }

        /// The open prompt's field replaced by `text`, and `Enter`.
        fn answer(&mut self, text: &str) {
            if let Some(prompt) = &mut self.app.prompt {
                prompt.buffer = InputBuffer::new("", 0);
            }
            if !text.is_empty() {
                self.app.prompt_text(text);
            }
            self.app
                .route_chord(Chord::plain(Key::Enter), 10, Instant::now());
        }

        fn toast(&self) -> String {
            self.app
                .toasts
                .current()
                .map(|toast| toast.message.clone())
                .unwrap_or_default()
        }

        /// A folder as a sentence says it: a toast, the prompt's title, a
        /// menu row. `$TMPDIR` is long enough on some machines for the middle
        /// to go, which is what [`brief`] is for.
        fn said(&self, dir: &Path) -> String {
            said(&text(dir), home().as_deref())
        }

        /// …in full, as the picker lists it for the typing to match.
        fn full(&self, dir: &Path) -> String {
            shown(&text(dir), home().as_deref())
        }

        /// …as a mount card row names it.
        fn row(&self, dir: &Path) -> String {
            brief(&self.full(dir), ROW_BRIEF)
        }
    }

    /// Long places lose their middle, keeping whose they are and the name
    /// they are known by; short ones, and ones with no middle, stay whole.
    #[test]
    fn a_long_place_loses_its_middle_in_a_sentence() {
        assert_eq!(brief("~/Work", 10), "~/Work");
        assert_eq!(
            brief("~/Work/delightfile/crates/df-app/src", 24),
            "~/…/crates/df-app/src"
        );
        assert_eq!(
            brief("/tmp/a-long-one/b/files/Projects", 20),
            "…/b/files/Projects"
        );
        assert_eq!(
            brief("sftp://box/srv/www/site/public", 26),
            "sftp://box/…/site/public"
        );
        assert_eq!(
            brief("/a-single-enormous-folder-name-with-no-middle", 10),
            "/a-single-enormous-folder-name-with-no-middle",
            "nothing to drop"
        );
        let long =
            "/tmp/claude-1000/-home-brian-Work-delightfile/818c1cd4-fa17/live/files/Projects";
        assert!(brief(long, BRIEF).chars().count() <= BRIEF);
    }

    /// `g b` on a folder that is not pinned asks for a key; `Enter` on
    /// nothing pins it keyless and says how to reach it; `g b` again takes it
    /// off. The store is dirty for the write-behind each time.
    #[test]
    fn g_b_pins_the_folder_on_screen_and_g_b_again_unpins_it() {
        let mut s = Sandbox::plain("toggle");
        let files = s.files();
        s.keys("g b");
        let prompt = s.app.prompt.as_ref().expect("the pin prompt");
        assert_eq!(prompt.kind, PromptKind::Pin);
        assert_eq!(prompt.title(), format!("Pin {} as:", s.said(&files)));
        assert_eq!(
            prompt.message(),
            Some(("a key after g, or Enter for none", false))
        );

        s.answer("");
        assert!(s.app.prompt.is_none());
        assert!(s.app.state.is_pinned(&text(&files)));
        assert_eq!(s.app.state.pins()[0].key, None);
        assert!(s.app.state.is_dirty());
        assert!(
            s.app.state_due.deadline(Instant::now()).is_some(),
            "a write is armed"
        );
        assert_eq!(
            s.toast(),
            format!("Pinned {} · g Space to jump", s.said(&files))
        );

        s.app.flush_state();
        s.keys("g b");
        assert!(s.app.prompt.is_none(), "unpinning asks nothing");
        assert!(s.app.state.pins().is_empty());
        assert!(s.app.state.is_dirty());
        assert_eq!(s.toast(), format!("Unpinned {}", s.said(&files)));
    }

    /// A key makes `g <key>` go there through the real registry, the which-key
    /// card lists it under `g`, and the toast teaches it. Unpinned, the chord
    /// is gone: the keymap is rebuilt, not left holding a slot.
    #[test]
    fn a_keyed_pin_is_a_g_chord_at_once() {
        let mut s = Sandbox::plain("keyed");
        let files = s.files();
        let sub = files.join("sub");
        s.go(sub.clone());
        s.keys("g b");
        s.answer("x");
        assert_eq!(s.toast(), format!("Pinned {} · g x", s.said(&sub)));
        let slot = Command::Goto(s.app.config.goto.len() as u8);
        assert_eq!(s.app.keymap.binding_label(slot).as_deref(), Some("g x"));
        let card = s.app.keymap.continuations(
            &ContextStack::browser(),
            WhenFlags::NONE,
            &[Chord::plain(Key::Char('g'))],
        );
        let row = card
            .iter()
            .find(|row| row.command == slot)
            .expect("on the g card");
        assert_eq!(row.description, format!("Go to {}", s.said(&sub)));

        s.go(files.clone());
        s.keys("g x");
        s.settle();
        assert_eq!(s.app.cwd(), sub, "g x went to the pin");

        s.keys("g b");
        assert!(s.app.state.pins().is_empty());
        assert_eq!(s.app.keymap.binding_label(slot), None);
        s.go(files.clone());
        s.keys("g x");
        assert_eq!(s.app.cwd(), files, "g x goes nowhere now");
    }

    /// `[goto]`'s keys and the built-in `g` chords are not a pin's to take:
    /// the toast names what holds the key and the prompt stays for another
    /// try. A key that is not one key is refused the same way, and so is
    /// another pin's.
    #[test]
    fn a_taken_key_is_refused_out_loud_and_the_prompt_stays() {
        let mut s = Sandbox::new("clash", |_, config, _| {
            config.goto = vec![bookmark("w", "/elsewhere/work")];
        });
        s.keys("g b");
        for (key, said) in [
            ("w", "g w is already /elsewhere/work"),
            ("g", "g g is already Go to top"),
            ("space", "g Space is already Jump interactively"),
            ("ab", "ab is not a key — one key, like w or space"),
        ] {
            s.answer(key);
            assert_eq!(s.toast(), said);
            assert!(s.app.prompt.is_some(), "{key}: the prompt stays");
            assert!(s.app.state.pins().is_empty(), "{key}: nothing pinned");
        }
        s.answer("q");
        assert!(s.app.prompt.is_none());
        assert_eq!(s.app.state.pins()[0].key.as_deref(), Some("q"));

        // Another pin's key, from another folder.
        let files = s.files();
        s.go(files.join("sub"));
        s.keys("g b");
        s.answer("q");
        assert_eq!(s.toast(), format!("g q is already {}", s.said(&files)));
        assert!(s.app.prompt.is_some());
        s.keys("esc");
        assert!(s.app.prompt.is_none(), "Esc drops the question");
        assert_eq!(s.app.state.pins().len(), 1);
    }

    /// Pins in the state file when the app starts are keys from the first
    /// keystroke, `Goto(n)` reaching past `[goto]` into them.
    #[test]
    fn pins_in_the_store_are_keys_at_startup() {
        let mut s = Sandbox::new("boot", |files, _, state| {
            state
                .pin(text(&files.join("other")), Some("o".to_string()))
                .expect("pin");
        });
        s.keys("g o");
        assert_eq!(s.app.cwd(), s.files().join("other"));
    }

    /// `g space` lists the pins first with their keys on the right, then
    /// `[goto]` with its own — `sftp://` rows and all — and `Enter` goes to
    /// the one under the cursor.
    #[test]
    fn g_space_lists_the_pins_first_and_goes_on_enter() {
        let mut s = Sandbox::new("picker", |files, config, state| {
            config.goto = vec![bookmark("s", "sftp://box/srv"), bookmark("h", "~")];
            state
                .pin(text(&files.join("other")), Some("o".to_string()))
                .expect("pin");
            state.pin(text(&files.join("sub")), None).expect("pin");
        });
        let files = s.files();
        s.keys("g space");
        let finder = s.app.finder.as_ref().expect("the picker");
        assert_eq!(finder.source, Source::Places);
        let rows: Vec<(String, String)> = finder
            .pool
            .iter()
            .map(|row| (row.label.clone(), row.detail.clone()))
            .collect();
        assert_eq!(
            rows,
            vec![
                (s.full(&files.join("other")), "g o".to_string()),
                (s.full(&files.join("sub")), String::new()),
                ("sftp://box/srv".to_string(), "g s".to_string()),
                ("~".to_string(), "g h".to_string()),
            ],
            "home is [goto]'s here, so there is no fallback row"
        );
        s.keys("enter");
        assert!(s.app.finder.is_none());
        s.settle();
        assert_eq!(s.app.cwd(), files.join("other"));
    }

    /// `z` starts with the pins, before where this tab has been; a pin on a
    /// server is left out of it, as `[goto]`'s are.
    #[test]
    fn z_starts_with_the_pins() {
        let mut s = Sandbox::new("z", |files, _, state| {
            state.pin(text(&files.join("other")), None).expect("pin");
            state.pin("sftp://box/srv", None).expect("pin");
        });
        let files = s.files();
        s.go(files.join("sub"));
        s.go(files.clone());
        s.keys("z");
        let finder = s.app.finder.as_ref().expect("the jump list");
        assert_eq!(finder.source, Source::Jump);
        assert_eq!(finder.pool[0].choice, Choice::Cd(files.join("other")));
        assert_eq!(
            finder.pool[1].choice,
            Choice::Cd(files.join("sub")),
            "then history"
        );
        assert!(finder
            .pool
            .iter()
            .all(|row| !matches!(&row.choice, Choice::Cd(p) if crate::remote::is_remote(p))));
    }

    /// The mount card opens with a Places section over the disks, the cursor
    /// below it where the first disk will be: pins, then `[goto]` rows
    /// not already pinned, each saying its key. `d` unpins a pin and says so,
    /// and on a `[goto]` row says why it will not; `Enter` on a place goes
    /// there. The strip offers `d` only while the cursor is on a pin.
    ///
    /// The card is built as `M` builds it, less the udisks2 request: the
    /// system bus is this machine's, not the test's.
    #[test]
    fn the_mount_card_lists_the_places_and_d_unpins() {
        let mut s = Sandbox::new("mounts", |files, config, state| {
            config.goto = vec![bookmark("s", "sftp://box/srv")];
            state
                .pin(text(&files.join("other")), Some("o".to_string()))
                .expect("pin");
            state.pin(text(&files.join("sub")), None).expect("pin");
        });
        let files = s.files();
        s.app.mounts = Some(s.app.mount_card());
        s.app.sync_context();
        let card = s.app.mounts.as_ref().expect("the card");
        let rows: Vec<(String, String, bool, bool)> = card
            .places
            .iter()
            .map(|p| (p.name.clone(), p.detail.clone(), p.pinned, p.remote))
            .collect();
        assert_eq!(
            rows,
            vec![
                (s.row(&files.join("other")), "g o".to_string(), true, false),
                (s.row(&files.join("sub")), "pinned".to_string(), true, false),
                ("sftp://box/srv".to_string(), "g s".to_string(), false, true),
            ],
            "no home fallback on the card"
        );
        assert_eq!(card.lines()[0], Line::Section("Places"));
        // Where the first disk will be: udisks2 has not answered (and never
        // will, here), so that index is the connect row for now.
        assert_eq!(
            card.cursor,
            card.places.len(),
            "the card opens below the places"
        );
        assert_eq!(card.selected(), Some(Item::Connect));
        let area = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1200.0, 800.0));
        let strip = |app: &App| -> Vec<String> {
            let card = app.mounts.as_ref().expect("the card");
            super::super::overlay_hints(
                &super::super::OverlayGeom::Mounts(crate::mounts::geometry(area, card)),
                &None,
            )
            .iter()
            .map(|hint| hint.keys.to_string())
            .collect()
        };
        assert!(
            !strip(&s.app).contains(&"d".to_string()),
            "no d below the places"
        );

        // Up past the [goto] row to the second pin: `d` is offered, and takes
        // it off the list with the cursor where it was.
        s.keys("up up");
        assert_eq!(
            s.app.mounts.as_ref().and_then(|c| c.selected()),
            Some(Item::Place(1))
        );
        assert_eq!(strip(&s.app)[1], "d", "d is offered on a pin");
        s.keys("d");
        assert_eq!(
            s.toast(),
            format!("Unpinned {}", s.said(&files.join("sub")))
        );
        assert_eq!(s.app.state.pins().len(), 1);
        let card = s.app.mounts.as_ref().expect("still up");
        assert_eq!(card.places.len(), 2);
        assert_eq!(card.selected(), Some(Item::Place(1)), "now the [goto] row");
        assert!(
            !strip(&s.app).contains(&"d".to_string()),
            "no d on a [goto] row"
        );
        s.keys("d");
        assert_eq!(s.toast(), "sftp://box/srv is a [goto] bookmark, not a pin");
        assert_eq!(s.app.state.pins().len(), 1);

        s.keys("up enter");
        assert!(s.app.mounts.is_none(), "the card goes");
        s.settle();
        assert_eq!(s.app.cwd(), files.join("other"));
    }

    /// With nothing pinned and no `[goto]`, the section says what it is for.
    #[test]
    fn an_empty_places_section_says_how_to_fill_it() {
        let s = Sandbox::new("empty", |_, config, _| config.goto = Vec::new());
        let card = s.app.mount_card();
        assert!(card.places.is_empty());
        assert_eq!(
            &card.lines()[..2],
            &[
                Line::Section("Places"),
                Line::Empty("Nothing pinned · g b pins this folder"),
            ]
        );
    }

    /// The app menu's Go list is "Go to path…" and "Jump to…", then the Places
    /// list with its keys and the pin row under it; the folder menu has the
    /// pin row, through `g b`'s own door; a directory row's menu pins that
    /// row's folder, keyless and without a prompt. Each says Unpin once there
    /// is a pin to take off.
    #[test]
    fn the_menus_pin_and_go() {
        let mut s = Sandbox::new("menus", |files, config, _| {
            config.goto = vec![bookmark("h", &text(&files.join("other")))];
        });
        let files = s.files();

        s.app.open_app_menu();
        let menu = s.app.menu.take().expect("the app menu");
        let go = menu.items.iter().find(|i| i.label == "Go").expect("Go");
        let list = go.submenu.as_ref().expect("a list");
        // The two ways of typing where to go, then — after a gap — the
        // places.
        let typed: Vec<&str> = list[..2].iter().map(|i| i.label.as_str()).collect();
        assert_eq!(typed, ["Go to path…", "Jump to…"]);
        let places = &list[2..];
        assert!(
            places[0].gap_before,
            "no gap between the typed and the places"
        );
        assert_eq!(places[0].label, s.said(&files.join("other")));
        assert_eq!(places[0].keys, "g h");
        assert_eq!(places[0].action, Action::Place(0));
        let last = list.last().expect("the pin row");
        assert_eq!(
            (
                last.label.as_str(),
                last.keys.as_str(),
                last.enabled,
                last.gap_before
            ),
            ("Pin this folder", "g b", true, false)
        );

        // The folder menu's row is `g b`: it asks for a key.
        s.app.open_folder_menu(egui::pos2(10.0, 10.0));
        let menu = s.app.menu.take().expect("the folder menu");
        let row = menu.items.last().expect("rows");
        assert_eq!(row.label, "Pin this folder");
        assert_eq!(row.action, Action::Run(Command::PinToggle));
        s.app.menu_action(row.action, 10, Instant::now());
        assert_eq!(s.app.prompt.as_ref().map(|p| p.kind), Some(PromptKind::Pin));
        s.answer("");
        assert!(s.app.here_pinned());
        s.app.open_folder_menu(egui::pos2(10.0, 10.0));
        let menu = s.app.menu.take().expect("the folder menu");
        assert_eq!(menu.items.last().expect("rows").label, "Unpin this folder");
        s.app.open_app_menu();
        let menu = s.app.menu.take().expect("the app menu");
        let go = menu.items.iter().find(|i| i.label == "Go").expect("Go");
        let list = go.submenu.as_ref().expect("a list");
        assert_eq!(list[2].label, s.said(&files), "the pin heads the places");
        assert_eq!(list.last().expect("rows").label, "Unpin this folder");

        // Go ▸ a place goes there.
        s.app.menu_action(Action::Place(1), 10, Instant::now());
        s.settle();
        assert_eq!(s.app.cwd(), files.join("other"));
        s.go(files.clone());

        // A directory row's menu pins that folder, not this one.
        let at = s
            .app
            .tab()
            .cwd
            .dir
            .entries()
            .iter()
            .position(|e| e.name == "sub")
            .expect("sub");
        s.app.dir().set_cursor(at);
        s.app.open_menu(egui::pos2(10.0, 10.0));
        let menu = s.app.menu.take().expect("the row menu");
        let row = menu
            .items
            .iter()
            .find(|i| i.action == Action::PinRow)
            .expect("a pin row on a folder");
        assert_eq!((row.label.as_str(), row.keys.as_str()), ("Pin folder", ""));
        s.app.menu_action(Action::PinRow, 10, Instant::now());
        assert!(s.app.prompt.is_none(), "no question from a menu row");
        let sub = files.join("sub");
        assert_eq!(s.app.state.pinned(&text(&sub)).expect("pinned").key, None);
        assert_eq!(
            s.toast(),
            format!("Pinned {} · g Space to jump", s.said(&sub))
        );
        s.app.open_menu(egui::pos2(10.0, 10.0));
        let menu = s.app.menu.take().expect("the row menu");
        assert!(menu.items.iter().any(|i| i.label == "Unpin folder"));
    }

    /// In the trash, `g b` says why not, the pin rows are grey, and the Go
    /// list is still live — the places are somewhere to go from anywhere.
    /// (An archive is the same gate, asked of [`refusal`] above.)
    #[test]
    fn nothing_is_pinned_from_the_trash() {
        let mut s = Sandbox::plain("trash");
        let origin = s.files();
        s.app.tabs.active_mut().trash = Some(crate::trashview::View {
            items: Vec::new(),
            origin,
        });
        s.keys("g b");
        assert!(s.app.prompt.is_none());
        assert_eq!(
            s.toast(),
            "The trash is not a folder to pin — g t goes there"
        );
        assert!(s.app.state.pins().is_empty());
        s.app.open_app_menu();
        let menu = s.app.menu.take().expect("the app menu");
        let go = menu.items.iter().find(|i| i.label == "Go").expect("Go");
        assert!(go.enabled);
        let rows = go.submenu.as_ref().expect("a list");
        let place = rows
            .iter()
            .find(|i| matches!(i.action, Action::Place(_)))
            .expect("a place");
        assert!(place.enabled, "a place is still somewhere to go");
        assert!(!rows.last().expect("rows").enabled, "the pin row is grey");
    }
}
