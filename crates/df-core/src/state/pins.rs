//! Pinned places: the folders a person put on the Places list by hand (`g b`).
//!
//! **In the state file, not in config**, and that is the whole reason this
//! module is here rather than beside [`crate::config::Bookmark`]. `[goto]` is
//! the owner's own table, written by hand in `delightfile.toml`, and the
//! program never writes config — a file manager that rewrote the file you
//! keep in a dotfiles repo, reformatting it and dropping your comments, would
//! be a file manager you stopped letting near it. A pin is made by a keystroke,
//! so it is machine-written, and machine-written data belongs to
//! [`super::StateStore`], which already knows how to write a line atomically
//! and survive a line it cannot read.
//!
//! The two layers meet in the app: the `g` chord table is `[goto]` first and
//! the keyed pins after it, and where both want the same key **the hand-written
//! row wins**. The store knows nothing about config, so the only clash it can
//! see is two pins wanting one key, and that one it refuses itself.
//!
//! # The format
//!
//! One line per pin, in the order they were pinned, after the directory
//! records and the tabs:
//!
//! ```text
//! !pin\tpath=~/Work\tkey=w
//! !pin\tpath=sftp://showandtour1/srv
//! ```
//!
//! `!` is not the start of an absolute path, so the key cannot collide with a
//! directory record — the rule `!tabs` already relies on. A line per pin rather
//! than one record holding all of them, so a line that goes bad costs one pin
//! and not the list. The path is **written as given**: `~/Work` stays `~/Work`
//! so `$HOME` can move under it, and an `sftp://` URL is a place the vfs owns,
//! exactly the forms a `[goto]` path takes.

use std::path::Path;

use crate::config::expand_home;
use crate::keymap::{parse_chord, Chord};

use super::{push_field, split_field, text, StateStore};

/// The record key of one pin. See the module header for why it cannot collide
/// with a directory's.
pub const PIN_KEY: &str = "!pin";

/// One pinned place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pin {
    /// Where it is, written as given — see the module header.
    pub path: String,
    /// The key after `g` that goes there, written the way `keymap.toml` writes
    /// one (`w`, `1`, `space`), or none. A keyless pin is still on the list; it
    /// is reached from `g space`, the mount card and the Go menu instead.
    pub key: Option<String>,
}

impl Pin {
    /// The path with `~` replaced by `$HOME`, by the rule `[goto]` uses.
    pub fn expanded_path(&self) -> String {
        expand_home(&self.path)
    }

    /// Whether this pin is the place `path` names, in whichever of its
    /// spellings: `~/Work`, `/home/brian/Work` and `/home/brian/Work/` are one
    /// folder, and pinning it twice under two spellings would be two rows for
    /// it.
    pub fn is(&self, path: &str) -> bool {
        same_place(&self.path, path)
    }
}

/// Why the store would not do what it was asked.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PinRefusal {
    /// That place is on the list already.
    #[error("{0} is already pinned")]
    AlreadyPinned(String),
    /// That place is not on the list, so there is no key to change.
    #[error("{0} is not pinned")]
    NotPinned(String),
    /// Another pin has that key. `holder` is its path as written.
    #[error("g {key} is already {holder}")]
    KeyTaken { key: String, holder: String },
    /// Not one key: `ab`, `g w`, `shift` on its own. The keymap binds one key
    /// after `g`, so anything else would be a pin whose key did nothing.
    #[error("{0} is not a key — one key, like w or space")]
    NotAKey(String),
}

/// Two spellings of one place. By [`Path`] equality after expanding `~`, which
/// is component-wise, so a trailing `/` does not make a second place.
fn same_place(a: &str, b: &str) -> bool {
    Path::new(&expand_home(a)) == Path::new(&expand_home(b))
}

/// A key as the keymap would bind it, or the refusal. Surrounding space is
/// dropped and nothing left is no key at all, which is what `Enter` on an empty
/// prompt means.
fn checked_key(key: Option<String>) -> Result<Option<(String, Chord)>, PinRefusal> {
    let Some(key) = key else { return Ok(None) };
    let key = key.trim();
    if key.is_empty() {
        return Ok(None);
    }
    match parse_chord(key) {
        Ok(chord) => Ok(Some((key.to_string(), chord))),
        Err(_) => Err(PinRefusal::NotAKey(key.to_string())),
    }
}

/// Whether `pin` already answers to `chord`. Compared as chords rather than as
/// text, so `W` and `shift+w` are the one key they are.
fn has_key(pin: &Pin, chord: Chord) -> bool {
    pin.key
        .as_deref()
        .and_then(|key| parse_chord(key).ok())
        .is_some_and(|held| held == chord)
}

impl StateStore {
    /// The pinned places, in the order they were pinned.
    pub fn pins(&self) -> &[Pin] {
        &self.pins
    }

    /// The pin for `path`, in any of its spellings.
    pub fn pinned(&self, path: &str) -> Option<&Pin> {
        self.pins.iter().find(|pin| pin.is(path))
    }

    pub fn is_pinned(&self, path: &str) -> bool {
        self.pinned(path).is_some()
    }

    /// Put `path` at the end of the list, with `key` or none.
    ///
    /// Refused when the place is on the list already, when the key is not one
    /// key, or when another pin has it. Only the pins are checked: a key the
    /// `[goto]` table or a built-in `g` chord holds is the app's to refuse,
    /// because only the app has the keymap to ask.
    pub fn pin(&mut self, path: impl Into<String>, key: Option<String>) -> Result<(), PinRefusal> {
        let path = path.into();
        if let Some(held) = self.pinned(&path) {
            return Err(PinRefusal::AlreadyPinned(held.path.clone()));
        }
        let key = self.free_key(key, None)?;
        self.pins.push(Pin { path, key });
        self.dirty = true;
        Ok(())
    }

    /// Take `path` off the list, and hand back what it was.
    pub fn unpin(&mut self, path: &str) -> Option<Pin> {
        let at = self.pins.iter().position(|pin| pin.is(path))?;
        self.dirty = true;
        Some(self.pins.remove(at))
    }

    /// Give a pin a different key, or take its key away. The pin keeps its
    /// place in the list: a re-key is not a re-pin.
    pub fn set_pin_key(&mut self, path: &str, key: Option<String>) -> Result<(), PinRefusal> {
        let Some(at) = self.pins.iter().position(|pin| pin.is(path)) else {
            return Err(PinRefusal::NotPinned(path.to_string()));
        };
        let key = self.free_key(key, Some(at))?;
        if self.pins[at].key != key {
            self.pins[at].key = key;
            self.dirty = true;
        }
        Ok(())
    }

    /// `key` checked and, if another pin than `except` holds it, refused.
    fn free_key(
        &self,
        key: Option<String>,
        except: Option<usize>,
    ) -> Result<Option<String>, PinRefusal> {
        let Some((key, chord)) = checked_key(key)? else {
            return Ok(None);
        };
        let holder = self
            .pins
            .iter()
            .enumerate()
            .find(|(i, pin)| Some(*i) != except && has_key(pin, chord));
        match holder {
            Some((_, pin)) => Err(PinRefusal::KeyTaken {
                key,
                holder: pin.path.clone(),
            }),
            None => Ok(Some(key)),
        }
    }

    /// The pins' lines, for [`StateStore::render`].
    pub(super) fn render_pins(&self, out: &mut Vec<u8>) {
        for pin in &self.pins {
            out.extend_from_slice(PIN_KEY.as_bytes());
            push_field(out, "path", pin.path.as_bytes());
            if let Some(key) = &pin.key {
                push_field(out, "key", key.as_bytes());
            }
            out.push(b'\n');
        }
    }

    /// One `!pin` line, for [`StateStore::parse`].
    ///
    /// The rules the rest of the file follows: a line with no path is dropped
    /// with a warning, and an unknown field is a newer build's and is let go.
    /// A second line for a place already pinned is dropped, and a key another
    /// pin got to first — or one that is not a key — is dropped *from the
    /// line*, keeping the pin: a hand edit that doubled a key should cost the
    /// key, not the place.
    pub(super) fn parse_pin<'a>(&mut self, fields: impl Iterator<Item = &'a [u8]>, line: usize) {
        let mut path: Option<String> = None;
        let mut key: Option<String> = None;
        for field in fields {
            let Some((name, value)) = split_field(field) else {
                log::warn!(
                    "state: {}:{line}: malformed pin field; skipped",
                    self.path.display()
                );
                continue;
            };
            match name.as_slice() {
                b"path" => path = Some(text(&value)),
                b"key" => key = Some(text(&value)),
                _ => log::debug!(
                    "state: {}:{line}: unknown pin field {}",
                    self.path.display(),
                    text(&name)
                ),
            }
        }
        let Some(path) = path.filter(|path| !path.is_empty()) else {
            log::warn!(
                "state: {}:{line}: a pin with no path; line skipped",
                self.path.display()
            );
            return;
        };
        if self.is_pinned(&path) {
            log::warn!(
                "state: {}:{line}: {path} is pinned twice; line skipped",
                self.path.display()
            );
            return;
        }
        let key = match self.free_key(key, None) {
            Ok(key) => key,
            Err(refusal) => {
                log::warn!(
                    "state: {}:{line}: {refusal}; pinned without it",
                    self.path.display()
                );
                None
            }
        };
        self.pins.push(Pin { path, key });
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

    use super::*;
    use crate::ops::fixture::TempTree;

    fn store_at(tree: &TempTree) -> StateStore {
        StateStore::load_from(tree.join("state"))
    }

    fn paths(store: &StateStore) -> Vec<&str> {
        store.pins().iter().map(|pin| pin.path.as_str()).collect()
    }

    /// Pinned, re-keyed, unpinned — and each state read back from the file,
    /// because a pin that did not survive a restart was never pinned.
    #[test]
    fn pins_round_trip_through_the_file() {
        let tree = TempTree::new("state-pins-trip");
        let mut store = store_at(&tree);
        store.pin("~/Work", Some("w".to_string())).unwrap();
        store.pin("sftp://showandtour1/srv", None).unwrap();
        store.flush().unwrap();

        let mut back = store_at(&tree);
        assert_eq!(
            back.pins(),
            &[
                Pin {
                    path: "~/Work".to_string(),
                    key: Some("w".to_string()),
                },
                Pin {
                    path: "sftp://showandtour1/srv".to_string(),
                    key: None,
                },
            ],
            "written as given, `~` and URL alike"
        );

        back.set_pin_key("~/Work", Some("space".to_string()))
            .unwrap();
        back.set_pin_key("sftp://showandtour1/srv", Some("s".to_string()))
            .unwrap();
        back.flush().unwrap();
        let mut again = store_at(&tree);
        assert_eq!(
            again.pinned("~/Work").unwrap().key.as_deref(),
            Some("space")
        );
        assert_eq!(
            again
                .pinned("sftp://showandtour1/srv")
                .unwrap()
                .key
                .as_deref(),
            Some("s")
        );

        let gone = again.unpin("~/Work").unwrap();
        assert_eq!(gone.key.as_deref(), Some("space"), "hands back what it was");
        again.flush().unwrap();
        assert_eq!(paths(&store_at(&tree)), vec!["sftp://showandtour1/srv"]);
    }

    /// The list is in the order things were pinned, in memory and on disk,
    /// and a re-key does not move a pin.
    #[test]
    fn the_order_is_the_order_they_were_pinned_in() {
        let tree = TempTree::new("state-pins-order");
        let mut store = store_at(&tree);
        for path in ["/srv/c", "/srv/a", "/srv/b"] {
            store.pin(path, None).unwrap();
        }
        store.set_pin_key("/srv/c", Some("c".to_string())).unwrap();
        assert_eq!(paths(&store), vec!["/srv/c", "/srv/a", "/srv/b"]);
        store.flush().unwrap();
        assert_eq!(paths(&store_at(&tree)), vec!["/srv/c", "/srv/a", "/srv/b"]);
        let rendered = String::from_utf8(store.render()).unwrap();
        let lines: Vec<&str> = rendered
            .lines()
            .filter(|l| l.starts_with(PIN_KEY))
            .collect();
        assert_eq!(
            lines,
            vec![
                "!pin\tpath=/srv/c\tkey=c",
                "!pin\tpath=/srv/a",
                "!pin\tpath=/srv/b",
            ]
        );
    }

    /// A state file from before pins existed is every other record it holds
    /// and an empty list.
    #[test]
    fn a_file_from_before_pins_loads_with_none() {
        let tree = TempTree::new("state-pins-old");
        std::fs::write(
            tree.join("state"),
            "# delightfile state v1\n\
             /tmp/a\tsort=size\tsort_reverse=0\tt=10\n\
             !tabs\t0=/tmp/a\tactive=0\tt=10\n",
        )
        .unwrap();
        let store = store_at(&tree);
        assert!(store.pins().is_empty());
        assert_eq!(store.tabs(), &[std::path::PathBuf::from("/tmp/a")]);
        assert!(store.sort(Path::new("/tmp/a")).is_some());
        assert!(!store.is_dirty());
    }

    /// Two pins cannot share a key, one place cannot be pinned twice, and a
    /// key has to be one key. The same key spelled two ways is one key.
    #[test]
    fn the_store_refuses_what_would_collide() {
        let tree = TempTree::new("state-pins-clash");
        let mut store = store_at(&tree);
        store.pin("/srv/one", Some("w".to_string())).unwrap();
        store.pin("/srv/two", Some("W".to_string())).unwrap();

        assert_eq!(
            store.pin("/srv/three", Some("w".to_string())),
            Err(PinRefusal::KeyTaken {
                key: "w".to_string(),
                holder: "/srv/one".to_string(),
            })
        );
        assert_eq!(
            store.pin("/srv/three", Some("shift+w".to_string())),
            Err(PinRefusal::KeyTaken {
                key: "shift+w".to_string(),
                holder: "/srv/two".to_string(),
            }),
            "`W` and `shift+w` are one key"
        );
        assert_eq!(
            store.pin("/srv/one/", None),
            Err(PinRefusal::AlreadyPinned("/srv/one".to_string())),
            "a trailing slash is not a second place"
        );
        for not_a_key in ["ab", "g w", "shift"] {
            assert_eq!(
                store.pin("/srv/three", Some(not_a_key.to_string())),
                Err(PinRefusal::NotAKey(not_a_key.to_string())),
                "{not_a_key}"
            );
        }
        assert_eq!(
            paths(&store),
            vec!["/srv/one", "/srv/two"],
            "no refusal pinned anything"
        );

        // A re-key is refused on the same terms, except against itself.
        assert_eq!(
            store.set_pin_key("/srv/two", Some("w".to_string())),
            Err(PinRefusal::KeyTaken {
                key: "w".to_string(),
                holder: "/srv/one".to_string(),
            })
        );
        store
            .set_pin_key("/srv/one", Some("w".to_string()))
            .unwrap();
        assert_eq!(
            store.set_pin_key("/srv/nowhere", None),
            Err(PinRefusal::NotPinned("/srv/nowhere".to_string()))
        );
        // Blank is no key, not a bad one: `Enter` on an empty prompt.
        store.pin("/srv/three", Some("  ".to_string())).unwrap();
        assert_eq!(store.pinned("/srv/three").unwrap().key, None);
    }

    /// `~/Work` and `$HOME/Work` are one folder, whichever was pinned.
    #[test]
    fn a_place_is_found_in_either_spelling() {
        let tree = TempTree::new("state-pins-home");
        let mut store = store_at(&tree);
        store.pin("~/Work", None).unwrap();
        let expanded = expand_home("~/Work");
        if expanded != "~/Work" {
            assert!(store.is_pinned(&expanded));
            assert!(store.is_pinned(&format!("{expanded}/")));
        }
        assert!(store.is_pinned("~/Work"));
        assert!(!store.is_pinned("~/Workshop"));
    }

    /// Pinning dirties the store and a flush cleans it; unpinning something
    /// that was never pinned changes nothing and so dirties nothing.
    #[test]
    fn only_a_real_change_is_dirt() {
        let tree = TempTree::new("state-pins-dirt");
        let mut store = store_at(&tree);
        assert!(store.unpin("/srv/never").is_none());
        assert!(!store.is_dirty());
        store.pin("/srv/one", None).unwrap();
        assert!(store.is_dirty());
        store.flush().unwrap();
        assert!(!store.is_dirty());
        store.set_pin_key("/srv/one", None).unwrap();
        assert!(!store.is_dirty(), "the key it already had");
        store.unpin("/srv/one").unwrap();
        assert!(store.is_dirty());
    }

    /// A path with the file's structure in it survives, and a hand-damaged
    /// line costs what it has to and no more: a pin with no path is dropped,
    /// a doubled place keeps its first line, and a doubled or bad key keeps
    /// the pin without it.
    #[test]
    fn a_damaged_pin_line_costs_only_itself() {
        let tree = TempTree::new("state-pins-damage");
        let mut store = store_at(&tree);
        store
            .pin("/tmp/tab\there=and\\more", Some("t".to_string()))
            .unwrap();
        store.flush().unwrap();
        assert_eq!(paths(&store_at(&tree)), vec!["/tmp/tab\there=and\\more"]);

        std::fs::write(
            tree.join("state"),
            "# delightfile state v1\n\
             !pin\tpath=/srv/a\tkey=a\n\
             !pin\tkey=b\n\
             !pin\tpath=/srv/a/\tkey=c\n\
             !pin\tpath=/srv/b\tkey=a\n\
             !pin\tpath=/srv/c\tkey=ab\tfuture=1\n",
        )
        .unwrap();
        let store = store_at(&tree);
        assert_eq!(
            store.pins(),
            &[
                Pin {
                    path: "/srv/a".to_string(),
                    key: Some("a".to_string()),
                },
                Pin {
                    path: "/srv/b".to_string(),
                    key: None,
                },
                Pin {
                    path: "/srv/c".to_string(),
                    key: None,
                },
            ]
        );
    }
}
