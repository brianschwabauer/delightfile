//! `T`: the tags of a file, or of a selection, in one line of text.
//!
//! The tags are df-core's ([`df_core::fs::tags`]) and live on the files; this
//! is the part a person meets. One prompt in the bar, `Tags of notes.txt:`,
//! filled with the tags the file has, comma-separated, and the caret after
//! them, so `T`, a word and `Enter` adds a tag and `T`, `Ctrl+u`, `Enter`
//! takes them all off. No chips, no picker: a tag is a word, and a line of
//! words is the one control that is equally quick for adding one, removing
//! one and retyping them all.
//!
//! **A selection is edited by difference.** The field opens on the tags every
//! selected item shares, and `Enter` applies what changed about that set to
//! each item: a shared tag taken out is taken off every one of them, a tag
//! typed in is put on every one of them, and the tags only some of them have
//! are left alone — the field never showed them, so the edit cannot have
//! been about them. One file gets exactly the line as typed.
//!
//! `Tab` finishes the word under the caret from the tags this program knows:
//! the seven colours, every tag the prompt has ever applied (kept in the state
//! file, [`df_core::state::StateStore::known_tags`]), and the tags on the rows
//! of this listing. Pressed again, it offers the next one.
//!
//! Every `Enter` that changes something is one journal entry
//! ([`OpRecord::Tags`]), so `u` puts every file's tags back as they were.

use std::path::PathBuf;
use std::time::Instant;

use df_core::fs::tags::{self, TagError};
use df_core::input::InputBuffer;
use df_core::ops::journal::{OpRecord, TagChange};

use super::App;
use crate::input::PromptKind;

/// What `T` was pressed on, kept from the prompt opening to `Enter`.
pub(crate) struct Draft {
    /// The selection, or the row under the cursor.
    targets: Vec<PathBuf>,
    /// The tags every target carried when the prompt opened — what the field
    /// was filled with, and what `Enter`'s line is compared against.
    shared: Vec<String>,
    /// The last `Tab`: the letters it completed from and the tag it put in
    /// their place, so a second `Tab` offers the next tag for the same
    /// letters rather than completing the completion.
    cycle: Option<(String, String)>,
}

impl App {
    /// `T`: open `Tags:` on the selection, or on the row under the cursor,
    /// with the tags they share in the field and the caret after them.
    pub(super) fn open_tag_prompt(&mut self, now: Instant) {
        let targets = self.targets();
        if targets.is_empty() {
            self.toasts.notice("Nothing here to tag", now);
            return;
        }
        let sets: Vec<Vec<String>> = targets.iter().map(|path| tags::read(path)).collect();
        let shared = tags::shared(&sets);
        // A trailing separator when there is something to follow, so the
        // next word typed is a new tag rather than the end of the last one.
        let text = if shared.is_empty() {
            String::new()
        } else {
            format!("{}, ", shared.join(", "))
        };
        let label = match targets.as_slice() {
            [one] => format!(
                "Tags of {}:",
                one.file_name().unwrap_or_default().to_string_lossy()
            ),
            many => format!("Tags of {}:", super::plural(many.len(), "item", "items")),
        };
        let caret = text.chars().count();
        self.tag_draft = Some(Draft {
            targets,
            shared,
            cycle: None,
        });
        self.open_prompt_with(PromptKind::Tags, InputBuffer::new(text, caret));
        if let Some(prompt) = &mut self.prompt {
            prompt.label = Some(label);
        }
    }

    /// `Enter` in `Tags:`. Always closes the prompt: what cannot be written
    /// — a drive that holds no tags, a link — is said in a toast, and typing
    /// something else into the field would not change the answer.
    pub(super) fn tags_submit(&mut self, text: &str, now: Instant) {
        let Some(draft) = self.tag_draft.take() else {
            return;
        };
        let typed = tags::parse(text);
        let one = draft.targets.len() == 1;
        let mut changes: Vec<TagChange> = Vec::new();
        let mut failure: Option<String> = None;
        for path in &draft.targets {
            let before = match tags::try_read(path) {
                Ok(before) => before,
                Err(e) => {
                    failure.get_or_insert(e.to_string());
                    continue;
                }
            };
            let after = if one {
                typed.clone()
            } else {
                tags::apply_difference(&before, &draft.shared, &typed)
            };
            if after == before {
                continue;
            }
            match tags::write(path, &after) {
                Ok(()) => changes.push(TagChange {
                    path: path.clone(),
                    before,
                    after,
                }),
                Err(e) => {
                    failure.get_or_insert(refusal(path, &e));
                }
            }
        }

        if !changes.is_empty() && self.state.remember_tags(&typed) {
            self.state_changed(now);
        }
        self.show_tags(&changes);
        let message = tagged_message(&changes);
        if !changes.is_empty() {
            self.journal.record(OpRecord::Tags { changes });
        }
        match (failure, message) {
            (Some(failure), _) => self.toasts.error(failure, now),
            (None, Some(message)) => self.toasts.undo(message, now),
            (None, None) => self.toasts.notice("Tags unchanged", now),
        }
    }

    /// `Esc` in `Tags:`: nothing was written, so there is only the draft to
    /// let go of.
    pub(super) fn tags_cancelled(&mut self) {
        self.tag_draft = None;
    }

    /// Put the tags just written on the rows that show them, now, rather than
    /// when the watcher's rescan lands: the dots are the answer to the `Enter`
    /// and should not trail it by a scan. The rescan still comes, and agrees.
    fn show_tags(&mut self, changes: &[TagChange]) {
        if changes.is_empty() {
            return;
        }
        let tab = self.tabs.active_mut();
        let panes = std::iter::once(&mut tab.cwd).chain(tab.parent.as_mut());
        for pane in panes {
            // `revise_entries` rebuilds the view, so a `#tag` filter that is
            // on lets go of a row whose tag was just taken off.
            pane.dir.revise_entries(|entries| {
                let mut changed = false;
                for entry in entries.iter_mut() {
                    if let Some(change) = changes.iter().find(|c| c.path == entry.path) {
                        entry.tags = change.after.clone();
                        changed = true;
                    }
                }
                changed
            });
        }
    }

    /// `Tab` in `Tags:`: the word under the caret finished from the known
    /// tags, or — pressed again straight after — swapped for the next one
    /// that fits the same letters.
    pub(super) fn complete_tag(&mut self) {
        let known = self.known_tags();
        let (Some(prompt), Some(draft)) = (&mut self.prompt, &mut self.tag_draft) else {
            return;
        };
        let chars: Vec<char> = prompt.buffer.text().chars().collect();
        let caret = prompt.buffer.cursor().min(chars.len());
        // The entry the caret is in: from after the comma before it to the
        // comma after it, less the spaces at either end.
        let mut start = chars[..caret]
            .iter()
            .rposition(|c| *c == ',')
            .map_or(0, |at| at + 1);
        while start < caret && chars[start].is_whitespace() {
            start += 1;
        }
        let mut end = chars[caret..]
            .iter()
            .position(|c| *c == ',')
            .map_or(chars.len(), |at| caret + at);
        while end > start && chars[end - 1].is_whitespace() {
            end -= 1;
        }
        let entry: String = chars[start..end.max(start)].iter().collect();
        let (stem, previous) = match &draft.cycle {
            Some((stem, last)) if *last == entry => (stem.clone(), Some(last.clone())),
            _ => (chars[start..caret.max(start)].iter().collect(), None),
        };

        // The other entries in the field are not offered again.
        let others: Vec<String> = {
            let before: String = chars[..start].iter().collect();
            let after: String = chars[end.max(start)..].iter().collect();
            tags::parse(&format!("{before},{after}"))
        };
        let stem_lower = stem.to_lowercase();
        let mut candidates: Vec<String> = Vec::new();
        for tag in known {
            if tag.to_lowercase().starts_with(&stem_lower)
                && !tags::contains(&others, &tag)
                && !tags::contains(&candidates, &tag)
            {
                candidates.push(tag);
            }
        }
        candidates.sort_by_key(|tag| tag.to_lowercase());
        if candidates.is_empty() {
            return;
        }
        let next = match previous {
            // The one after the last offered, round to the first.
            Some(last) => candidates
                .iter()
                .position(|tag| *tag == last)
                .map_or(0, |at| (at + 1) % candidates.len()),
            // A word already whole is completed to the next one along.
            None if candidates.len() > 1 && tags::same(&candidates[0], &entry) => 1,
            None => 0,
        };
        let chosen = candidates[next].clone();
        prompt.buffer.set_selection(start, end.max(start));
        prompt.insert_text(&chosen);
        draft.cycle = Some((stem, chosen));
    }

    /// Every tag `Tab` may offer: the seven colours, the ones the prompt has
    /// applied before, and the ones on this listing's rows.
    fn known_tags(&self) -> Vec<String> {
        let mut known: Vec<String> = tags::COLOURS.iter().map(|c| c.to_string()).collect();
        known.extend(self.state.known_tags().iter().cloned());
        for entry in self.tab().cwd.dir.entries() {
            known.extend(entry.tags.iter().cloned());
        }
        known
    }
}

/// What a write that did not go through says, naming the file unless the
/// sentence is about the drive or the kind of thing it is.
fn refusal(path: &std::path::Path, e: &TagError) -> String {
    match e {
        TagError::Unsupported | TagError::Link => e.to_string(),
        _ => format!(
            "{}: {e}",
            path.file_name().unwrap_or_default().to_string_lossy()
        ),
    }
}

/// The undo toast for what `Enter` changed, or `None` when it changed
/// nothing: `Tagged notes.txt red, work` for one file, `Tagged 3 items` for
/// several, and `Untagged …` when all it did was take tags away.
pub(super) fn tagged_message(changes: &[TagChange]) -> Option<String> {
    let added = changes
        .iter()
        .any(|c| c.after.iter().any(|tag| !tags::contains(&c.before, tag)));
    let removed = changes
        .iter()
        .any(|c| c.before.iter().any(|tag| !tags::contains(&c.after, tag)));
    let verb = if removed && !added {
        "Untagged"
    } else {
        "Tagged"
    };
    Some(match changes {
        [] => return None,
        [one] => {
            let name = one.path.file_name().unwrap_or_default().to_string_lossy();
            let shown: Vec<&String> = if verb == "Untagged" {
                one.before
                    .iter()
                    .filter(|tag| !tags::contains(&one.after, tag))
                    .collect()
            } else {
                one.after.iter().collect()
            };
            let list: Vec<&str> = shown.iter().map(|tag| tag.as_str()).collect();
            format!("{verb} {name} {}", list.join(", "))
        }
        many => format!("{verb} {}", super::plural(many.len(), "item", "items")),
    })
}
