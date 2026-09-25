//! `A`: the selection packed into a new archive.
//!
//! The writing is df-core's ([`df_core::archive::write`]); this is the part a
//! person meets. One prompt, `Archive as:`, prefilled with a name and nothing
//! else to set: **the extension is the format**. `photos.zip`, `photos.tar.zst`
//! and `photos.7z` are three archives, and a name with no archive extension at
//! all gets `.zip`, because the question the prompt asks is "what is it
//! called" and the zip is the archive everybody can open. A compression level,
//! a password, a list of exclusions — each is a setting somebody has to look
//! past every time for the once a year they want it.
//!
//! The prompt's hint is the list of formats with the one the name picks lit,
//! so the choice is visible without being a control. A format whose program
//! is missing is dimmed, and says what it needs when it is picked — asked once
//! as the prompt opens, because the asking spawns a process.
//!
//! Everything that can be refused is refused *in the prompt*, where the fix is
//! one edit away: a format this cannot write, a missing program, an archive
//! that would be inside what it archives, 7z across folders. A name that is
//! taken asks first, on the same Replace card a save dialog uses, and is never
//! written over silently. What gets past all of that is a job on the macro
//! lane, and when it lands the cursor is on the archive and `u` takes it back.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use df_core::archive::write::{self, Extension, Format, Named, Pack, Packed};
use df_core::input::InputBuffer;
use df_core::ops::journal::{Fingerprint, OpRecord};
use df_core::ops::{OpOutcome, Outcome};
use df_core::tasks::{FnJob, Lane, TaskCtx};

use super::{first_under, plural, App, Dialog};
use crate::dialog::{Confirm, ConfirmKind};
use crate::input::{Ink, InkedHint, PromptKind};

/// What `A` was pressed on, kept from the prompt opening to the job being
/// queued.
///
/// Taken when the prompt opens rather than asked again at `Enter`: the name
/// in the field was made from these, and the selection an archive is of is
/// the one that was there when it was asked for.
pub(super) struct Draft {
    /// The selection, or the row under the cursor.
    sources: Vec<PathBuf>,
    /// The directory the typed name is relative to.
    cwd: PathBuf,
    /// Formats whose program is not installed.
    missing: Vec<Format>,
    /// The archive the Replace card is asking about, while it is up.
    replacing: Option<Pack>,
}

impl Draft {
    /// Stand in for a machine without some programs.
    #[cfg(test)]
    pub(super) fn set_missing(&mut self, missing: Vec<Format>) {
        self.missing = missing;
    }
}

/// How an `Archive as:` name was answered.
pub(super) enum Answer {
    /// Queued, or handed to the Replace card: the prompt closes.
    Done,
    /// Not a name at all. Said beside the field, which stays open.
    Refused(String),
    /// A name that cannot be written as it stands. A toast has said why, and
    /// the field stays as typed.
    Kept,
}

impl App {
    /// `A`: open `Archive as:` on the selection, or on the row under the
    /// cursor, with the stem selected so typing replaces it and keeps `.zip`.
    pub(super) fn open_archive_prompt(&mut self, now: Instant) {
        let sources = self.targets();
        if sources.is_empty() {
            self.toasts.notice("Nothing here to archive", now);
            return;
        }
        let cwd = self.cwd();
        let stem = stem_for(&sources, &cwd);
        let chars = stem.chars().count();
        let mut buffer = InputBuffer::new(format!("{stem}.zip"), chars);
        buffer.set_selection(0, chars);
        let missing = Format::ALL
            .into_iter()
            .filter(|format| !format.is_available())
            .collect();
        self.archive_draft = Some(Draft {
            sources,
            cwd,
            missing,
            replacing: None,
        });
        self.open_prompt_with(PromptKind::Archive, buffer);
    }

    /// The format list for the open `Archive as:` prompt, or `None` when that
    /// is not the prompt that is open.
    pub(super) fn archive_hint(&self) -> Option<InkedHint> {
        let prompt = self
            .prompt
            .as_ref()
            .filter(|prompt| prompt.kind == PromptKind::Archive)?;
        let draft = self.archive_draft.as_ref()?;
        Some(format_hint(prompt.query(), &draft.missing))
    }

    /// `Enter` on `Archive as:`.
    pub(super) fn archive_submit(&mut self, text: &str, now: Instant) -> Answer {
        let Some(draft) = self.archive_draft.as_ref() else {
            return Answer::Done;
        };
        let (name, format) = match write::named(text) {
            Named::Empty => return Answer::Refused("no name given".to_string()),
            Named::Unwritable(extension) => {
                self.toasts.error(
                    format!(
                        "{extension} archives cannot be written — {}",
                        writable_list()
                    ),
                    now,
                );
                return Answer::Kept;
            }
            Named::Archive { name, format } => (name, format),
        };
        if draft.missing.contains(&format) {
            self.toasts.error(
                format!(
                    "{} needs {}, which is not installed",
                    format.label(),
                    format.tool().unwrap_or("a program")
                ),
                now,
            );
            return Answer::Kept;
        }
        let dest = draft.cwd.join(&name);
        let pack = Pack {
            sources: draft.sources.clone(),
            dest: dest.clone(),
            format,
            overwrite: false,
        };
        if let Err(why) = pack.check() {
            self.toasts.error(why, now);
            return Answer::Kept;
        }
        // `symlink_metadata`: a dangling link is a name that is taken too.
        if dest.symlink_metadata().is_ok() {
            if let Some(draft) = &mut self.archive_draft {
                draft.replacing = Some(Pack {
                    overwrite: true,
                    ..pack
                });
            }
            self.dialog = Some(Dialog::Confirm(Confirm::new(
                ConfirmKind::Replace,
                vec![dest],
            )));
            self.sync_context();
            return Answer::Done;
        }
        self.spawn_archive(pack);
        Answer::Done
    }

    /// The Replace card's yes, when the card was asking about an archive's
    /// name: write it over. `false` when it was asking about something else —
    /// a save dialog's pick — which the caller then answers.
    pub(super) fn archive_replace(&mut self, paths: &[PathBuf]) -> bool {
        let ours = self
            .archive_draft
            .as_ref()
            .and_then(|draft| draft.replacing.as_ref())
            .is_some_and(|pack| paths == std::slice::from_ref(&pack.dest));
        if !ours {
            return false;
        }
        let Some(pack) = self
            .archive_draft
            .as_mut()
            .and_then(|draft| draft.replacing.take())
        else {
            return false;
        };
        self.spawn_archive(pack);
        true
    }

    /// Queue the archive, and land on it when it is done.
    fn spawn_archive(&mut self, pack: Pack) {
        let cwd = self
            .archive_draft
            .take()
            .map(|draft| draft.cwd)
            .unwrap_or_else(|| self.cwd());
        let mut dirs = vec![cwd.clone()];
        if let Some(parent) = pack.dest.parent().filter(|parent| *parent != cwd) {
            dirs.push(parent.to_path_buf());
        }
        // The row it appears as here: the archive, or the folder `out/` made
        // for `out/photos.zip`.
        let focus: Vec<PathBuf> = first_under(&cwd, &pack.dest).into_iter().collect();
        let slot: Outcome = Arc::new(Mutex::new(None));
        let sink = Arc::clone(&slot);
        let job = FnJob::new(pack.name(), Lane::Macro, move |ctx: &TaskCtx| {
            let outcome = outcome(&pack, pack.run(ctx));
            match sink.lock() {
                Ok(mut guard) => *guard = Some(outcome),
                Err(poisoned) => *poisoned.into_inner() = Some(outcome),
            }
            Ok(())
        });
        let id = self.engine.spawn(job);
        self.track_focus(id, slot, dirs, focus);
    }
}

/// The name a prompt opens with, less its `.zip`: one folder's own name, one
/// file's name less its extension (`report.pdf` → `report`, and a
/// `backup.tar.gz` → `backup` rather than `backup.tar`), or for several items
/// the folder they are in.
pub(super) fn stem_for(sources: &[PathBuf], cwd: &Path) -> String {
    let stem = match sources {
        [one] => one.file_name().map(|name| {
            let name = name.to_string_lossy();
            if df_core::ops::is_real_dir(one) {
                name.into_owned()
            } else {
                df_core::archive::archive_stem(&name)
            }
        }),
        _ => cwd
            .file_name()
            .map(|name| name.to_string_lossy().into_owned()),
    };
    // `/` has no name to give.
    stem.filter(|stem| !stem.is_empty())
        .unwrap_or_else(|| "archive".to_string())
}

/// `zip · tar · tar.gz · tar.zst · tar.xz · 7z`, with the one `text` picks
/// lit, the ones that cannot be written here dimmed, and a picked one that
/// cannot saying what it needs instead of its name.
pub(super) fn format_hint(text: &str, missing: &[Format]) -> InkedHint {
    let extension = write::extension(text);
    let picked = match &extension {
        Extension::Writes(format) => Some(*format),
        Extension::Bare => Some(Format::Zip),
        Extension::Unwritable(_) => None,
    };
    let mut hint = InkedHint::default();
    if let Extension::Unwritable(extension) = &extension {
        hint.push(&format!("{extension} cannot be written"), Ink::Warn);
        hint.push(" · ", Ink::Quiet);
    }
    for (i, format) in Format::ALL.into_iter().enumerate() {
        if i > 0 {
            hint.push(" · ", Ink::Quiet);
        }
        let label = format.label();
        match (picked == Some(format), missing.contains(&format)) {
            (true, true) => hint.push(
                &format!("{label} needs {}", format.tool().unwrap_or("a program")),
                Ink::Warn,
            ),
            (true, false) => hint.push(label, Ink::Strong),
            (false, true) => hint.push(label, Ink::Absent),
            (false, false) => hint.push(label, Ink::Quiet),
        }
    }
    hint
}

/// `zip, tar, tar.gz, tar.zst, tar.xz or 7z can`, for the toast that turns a
/// `.rar` away.
fn writable_list() -> String {
    let labels: Vec<&str> = Format::ALL.iter().map(|format| format.label()).collect();
    match labels.split_last() {
        Some((last, rest)) => format!("{} or {last} can", rest.join(", ")),
        None => String::new(),
    }
}

/// What the finished job says, and what `u` will undo.
///
/// The record is `a`'s: an archive is a file that was made, and undoing it is
/// removing it — still only while it is the file that was made, and taking
/// with it any folder made for it. The toast counts what was selected, which
/// is the number the person has in mind, and gives the archive's size, which
/// is the number they did not know yet.
fn outcome(pack: &Pack, result: df_core::Result<Packed>) -> OpOutcome {
    let name = pack
        .dest
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    match result {
        Ok(packed) => {
            let record = Fingerprint::of(&packed.path)
                .ok()
                .map(|fingerprint| OpRecord::Create {
                    path: packed.path.clone(),
                    is_dir: false,
                    fingerprint,
                    created_parents: packed.created_parents.clone(),
                });
            let mut message = format!(
                "Archived {} · {}",
                plural(packed.items, "item", "items"),
                crate::format::human_size(packed.size)
            );
            if !packed.skipped.is_empty() {
                message.push_str(&format!(
                    " · {} left out",
                    plural(packed.skipped.len(), "special file", "special files")
                ));
            }
            OpOutcome {
                record,
                message,
                ..OpOutcome::default()
            }
        }
        Err(df_core::DfError::Cancelled) => OpOutcome {
            message: format!("{name} was not written"),
            cancelled: true,
            ..OpOutcome::default()
        },
        Err(e) => OpOutcome {
            message: format!("{name} was not written"),
            errors: vec![(pack.dest.clone(), e.to_string())],
            ..OpOutcome::default()
        },
    }
}
