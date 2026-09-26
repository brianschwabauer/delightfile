//! Known tags: every tag the `Tags:` prompt has ever put on a file.
//!
//! The tags themselves live on the files ([`crate::fs::tags`]), and there is
//! no index of them — so without a list here, the only tags `Tab` could
//! complete in the prompt, and the only ones a person could be reminded of,
//! would be the ones on files in the folder on screen. A tag used on a folder
//! of invoices last month is exactly the one you have half-forgotten the
//! spelling of.
//!
//! **Written only by the prompt.** A scan never adds to it: every tag on every
//! file ever listed would be a list that grows with every folder visited and
//! fills with tags other programs wrote, which is a record of the disk rather
//! than of what this person has been tagging with.
//!
//! # The format
//!
//! One line per tag, in the order they were first applied, after the pins:
//!
//! ```text
//! !tag\tname=work
//! !tag\tname=invoice 2026
//! ```
//!
//! A line per tag rather than one record holding all of them, for the reason
//! a pin is one: a line that goes bad costs one tag and not the list.

use super::{push_field, split_field, text, StateStore};

/// The record key of one known tag. `!` cannot begin an absolute path, so it
/// cannot collide with a directory's record.
pub const TAG_KEY: &str = "!tag";

/// How many known tags are kept.
///
/// Far past the few dozen a person really uses — the bound exists because the
/// list only grows, and past it the oldest tag is the one let go.
pub const MAX_KNOWN_TAGS: usize = 1000;

impl StateStore {
    /// Every tag the prompt has applied, oldest first, each in the spelling
    /// it was first typed in.
    pub fn known_tags(&self) -> &[String] {
        &self.tags
    }

    /// Remember `tags`, the ones not already known — in any case, since `Red`
    /// and `red` are one tag. Returns whether anything was new.
    pub fn remember_tags(&mut self, tags: &[String]) -> bool {
        let mut new = false;
        for tag in tags {
            let tag = tag.trim();
            if tag.is_empty() || tag.contains(',') || crate::fs::tags::contains(&self.tags, tag) {
                continue;
            }
            self.tags.push(tag.to_string());
            new = true;
        }
        if self.tags.len() > MAX_KNOWN_TAGS {
            let over = self.tags.len() - MAX_KNOWN_TAGS;
            self.tags.drain(..over);
        }
        self.dirty |= new;
        new
    }

    /// The known tags' lines, for [`StateStore::render`].
    pub(super) fn render_tags(&self, out: &mut Vec<u8>) {
        for tag in &self.tags {
            out.extend_from_slice(TAG_KEY.as_bytes());
            push_field(out, "name", tag.as_bytes());
            out.push(b'\n');
        }
    }

    /// One `!tag` line, for [`StateStore::parse`]. A line with no name is
    /// dropped with a warning; an unknown field is a newer build's and is let
    /// go; a second spelling of a tag already known is dropped quietly.
    pub(super) fn parse_tag<'a>(&mut self, fields: impl Iterator<Item = &'a [u8]>, line: usize) {
        let mut name: Option<String> = None;
        for field in fields {
            let Some((key, value)) = split_field(field) else {
                log::warn!(
                    "state: {}:{line}: malformed tag field; skipped",
                    self.path.display()
                );
                continue;
            };
            match key.as_slice() {
                b"name" => name = Some(text(&value)),
                _ => log::debug!(
                    "state: {}:{line}: unknown tag field {}",
                    self.path.display(),
                    text(&key)
                ),
            }
        }
        let Some(name) = name.filter(|name| !name.trim().is_empty()) else {
            log::warn!(
                "state: {}:{line}: a tag with no name; line skipped",
                self.path.display()
            );
            return;
        };
        let dirty = self.dirty;
        self.remember_tags(&[name]);
        // Loading is not a change.
        self.dirty = dirty;
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

    /// Tags go in once each, in the order they were first applied, and come
    /// back out of the file the same — spaces, case and all.
    #[test]
    fn known_tags_round_trip_through_the_file() {
        let tree = TempTree::new("state-tags");
        let mut store = store_at(&tree);
        assert!(store.known_tags().is_empty());
        assert!(!store.is_dirty());

        let typed = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        assert!(store.remember_tags(&typed(&["work", "invoice 2026"])));
        assert!(store.is_dirty());
        assert!(
            !store.remember_tags(&typed(&["Work", "invoice 2026"])),
            "a second spelling of a known tag is not a new tag"
        );
        assert!(store.remember_tags(&typed(&["Ünïcode tag", "work"])));
        assert_eq!(store.known_tags(), ["work", "invoice 2026", "Ünïcode tag"]);

        let text = String::from_utf8_lossy(&store.render()).into_owned();
        assert!(text.contains("!tag\tname=invoice 2026\n"), "{text}");
        store.flush().unwrap();
        assert!(!store.is_dirty());

        let reloaded = store_at(&tree);
        assert_eq!(
            reloaded.known_tags(),
            ["work", "invoice 2026", "Ünïcode tag"]
        );
        assert!(!reloaded.is_dirty(), "loading is not a change");
    }

    /// A line without a name costs itself, and the lines around it load.
    #[test]
    fn a_damaged_tag_line_costs_only_itself() {
        let tree = TempTree::new("state-tags-damaged");
        std::fs::write(
            tree.join("state"),
            "# delightfile state v1\n!tag\tname=red\n!tag\tcolour=blue\n!tag\tname=\n!tag\tname=work\tsince=2026\n",
        )
        .unwrap();
        let store = store_at(&tree);
        assert_eq!(store.known_tags(), ["red", "work"]);
    }

    /// The list is bounded, and what falls off is the oldest.
    #[test]
    fn the_oldest_tag_is_the_one_let_go() {
        let tree = TempTree::new("state-tags-cap");
        let mut store = store_at(&tree);
        let many: Vec<String> = (0..MAX_KNOWN_TAGS + 3).map(|n| format!("t{n}")).collect();
        store.remember_tags(&many);
        assert_eq!(store.known_tags().len(), MAX_KNOWN_TAGS);
        assert_eq!(store.known_tags()[0], "t3");
    }
}
