//! The trash's weight and its clock (PLAN §7.4).
//!
//! Two things the trash view did not know about itself: how much it is
//! holding, and how long it holds it.
//!
//! **The weight** is the chip beside the position counter — `37 items ·
//! 1.2 GB` — and the middle of the Empty trash card's question. It is the du
//! scanner's walk of the trash's `files/` directory, asked for every time the
//! view's rows are rebuilt ([`App::show_trash`]), read off the scanner's one
//! channel, and wearing the size column's `~` until it settles.
//!
//! The channel is the part that needed care. It has three readers now — the
//! size column ([`App::poll_folders`]), "what's big" ([`App::poll_usage`]) and
//! this — and the first two each drain all of it and drop whatever carries a
//! token they do not own. That was harmless while they never ran at once; with
//! a third reader it would have eaten the trash walk's answer on the first
//! frame either of them drained. So every drain goes through
//! [`App::drain_du`], which applies the trash's messages wherever the drain
//! happens and hands back the rest; when only the trash is reading, the rest
//! waits in [`App::du_backlog`] for whichever reader comes next, exactly as it
//! would have waited in the channel.
//!
//! **The clock** is `[mgr] trash_keep_days`: the home trash is listed on a
//! task worker and every item whose record says it was deleted more than that
//! many days ago is destroyed ([`df_core::ops::trash::purge_expired`], and
//! [`df_core::ops::trash::expired`] for what is never chosen). It is a task
//! like any other, so it is in `w` and `x` cancels it.
//!
//! **Once a day per user, not per window.** Every window is a process with a
//! clock of its own, checked on the frame after the first and on every frame
//! after that; the trash's stamp ([`df_core::ops::trash::PURGE_STAMP`]) is what
//! decides. A window only queues a purge when the stamp is a day old or
//! missing, and the purge itself holds an `flock` on the stamp and reads it
//! again under the lock, so of two windows launched together one purges and
//! the other stands down without a word.
//!
//! **When it fires.** The daily check is an instant in [`App::next_deadline`],
//! but an instant a day away is past `REPAINT_HORIZON` and dropped from the
//! window's wake-up, on purpose: an idle window is not woken to check a
//! clock. So the purge runs on the first frame *after* it is due, whatever
//! brings that frame — a key, the pointer, a watcher event. Only a due inside
//! the hour (another window's stamp about to turn a day old) wakes the window
//! by itself. At startup the check waits for the first frame to be on screen
//! and asks for no frame of its own: the frames that follow a window's first
//! (its listing landing, the pointer arriving) run it.
//!
//! It is *not* done while any tab has the trash open — rows vanishing from
//! under the cursor with nobody's hand on a key is not something a list
//! should do — and one already running when `g t` opens the trash is
//! cancelled and owed again. A file dialog never does it at all: the window
//! belongs to another program for a few seconds, and that is no time to be
//! deleting things.
//!
//! A purge is not journalled, for the reason [`crate::trashview`]'s own `D` is
//! not: there is no inverse to record.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use df_core::du::{DuMessage, DuOptions};
use df_core::ops::trash::{self as core_trash, Purged};
use df_core::tasks::{FnJob, Lane, TaskEvent, TaskId};

use super::{App, Dialog};
use crate::chrome;
use crate::dialog::ConfirmKind;
use crate::trashview;

/// How long a running window waits between purges.
pub(super) const PURGE_EVERY: Duration = Duration::from_secs(24 * 60 * 60);

/// Where a purge's answer lands: what it came to, or why it could not start.
type PurgeSlot = Arc<Mutex<Option<std::result::Result<Purged, String>>>>;

/// When the trash is next owed a purge, and the one in flight.
#[derive(Default)]
pub(super) struct Clock {
    /// The home trash — the only one a purge ever lists: a trash on another
    /// volume belongs to whatever is plugged in, and is not this clock's to
    /// empty. `None` is a clock that never runs: a file dialog, `$HOME`
    /// unset, `trash_keep_days = 0`, or a test that did not hand it one.
    root: Option<PathBuf>,
    /// The next purge. `None` exactly when `root` is.
    due: Option<Instant>,
    /// The purge in flight, if any.
    running: Option<(TaskId, PurgeSlot)>,
}

impl Clock {
    /// A window's clock: it purges the home trash at `home` now, and every
    /// [`PURGE_EVERY`] after — or never, in a file dialog (`picker`), with no
    /// home trash to find, or when nothing is ever too old (`keep_days` 0).
    pub(super) fn starting(
        picker: bool,
        home: Option<PathBuf>,
        keep_days: u64,
        now: Instant,
    ) -> Clock {
        match home.filter(|_| !picker && keep_days > 0) {
            Some(root) => Clock {
                root: Some(root),
                due: Some(now),
                running: None,
            },
            None => Clock::default(),
        }
    }

    /// How long until the next purge is owed, for the wake table.
    pub(super) fn deadline(&self, now: Instant) -> Option<Duration> {
        self.due.map(|at| at.saturating_duration_since(now))
    }

    /// The purge in flight.
    pub(super) fn running(&self) -> Option<TaskId> {
        self.running.as_ref().map(|(id, _)| *id)
    }
}

impl App {
    // ── The weight ──────────────────────────────────────────────────────────

    /// Weigh the trash whose `files/` directory is `files` — again, if it was
    /// being weighed: the walk it replaces is about rows that have changed.
    pub(super) fn weigh_trash(&mut self, files: PathBuf) {
        if let Some(token) = self.trash_weight.stop() {
            if let Some(du) = &self.du {
                du.cancel(token);
            }
        }
        // A trash nothing has gone into yet has no `files/`, and nothing to
        // weigh: the chip is not up for an empty trash anyway.
        if !files.is_dir() {
            return;
        }
        // The root alone: the chip is one number, and a walk that reported
        // every directory in a trash full of `node_modules` would be a stream
        // of answers to questions nobody asked.
        let token = self.du().request_with(files, DuOptions::at_depth(0));
        self.trash_weight.begin(token);
    }

    /// Everything the du scanner has said that its readers have not yet
    /// read, less the trash's own messages, which are applied here — see the
    /// module note on why every drain goes through one door.
    pub(super) fn drain_du(&mut self) -> Vec<DuMessage> {
        let mut messages = std::mem::take(&mut self.du_backlog);
        if let Some(du) = &self.du {
            messages.extend(du.drain());
        }
        let Some(token) = self.trash_weight.token() else {
            return messages;
        };
        let (ours, rest): (Vec<DuMessage>, Vec<DuMessage>) = messages
            .into_iter()
            .partition(|message| message.token() == token);
        let mut moved = false;
        for message in ours {
            moved |= self.trash_weight.apply(message);
        }
        if moved {
            self.sync_empty_trash_card();
        }
        rest
    }

    /// Take whatever the trash's walk has said. Returns whether its number
    /// moved.
    ///
    /// Stops the walk when no tab is showing the trash any more: a walk
    /// nobody will read is work nobody asked for (PLAN §1).
    pub(super) fn poll_trash_weight(&mut self) -> bool {
        let Some(token) = self.trash_weight.token() else {
            return false;
        };
        if !self.tabs.iter().any(|tab| tab.trash.is_some()) {
            self.trash_weight.stop();
            if let Some(du) = &self.du {
                du.cancel(token);
            }
            return false;
        }
        let before = self.trash_weight.size();
        let rest = self.drain_du();
        // Put back for the size column and "what's big", which read after
        // this — and which drop what is not theirs, as they always have.
        self.du_backlog = rest;
        self.trash_weight.size() != before
    }

    /// The Empty trash card asks its question with the chip's numbers, and
    /// keeps asking it with them as the walk settles under it.
    fn sync_empty_trash_card(&mut self) {
        let size = self.trash_weight.size();
        if let Some(Dialog::Confirm(confirm)) = &mut self.dialog {
            if confirm.kind == ConfirmKind::EmptyTrash {
                confirm.size = size;
            }
        }
    }

    /// The chip beside the position counter, in the trash view and only
    /// there, and only when there is something in the trash to weigh.
    pub(super) fn trash_chip(&self) -> Option<chrome::TrashChip> {
        let view = self.tab().trash.as_ref()?;
        if view.items.is_empty() {
            return None;
        }
        Some(chrome::TrashChip {
            label: trashview::weight_text(view.items.len(), self.trash_weight.size()),
            tip: trashview::keep_text(self.config.mgr.trash_keep_days),
        })
    }

    // ── The clock ───────────────────────────────────────────────────────────

    /// How long until the clock is owed a check, for [`App::next_deadline`]
    /// — nothing until the first frame is on screen, so the check owed at
    /// startup asks for no frame of its own. See the module note on why a day
    /// away is, in practice, "the first frame after".
    pub(super) fn trash_clock_deadline(&self, now: Instant) -> Option<Duration> {
        self.trash_clock
            .deadline(now)
            .filter(|_| self.logged_first_frame)
    }

    /// The purge, if one is owed now. Called on every frame once the first
    /// is on screen, which costs a comparison until the day is up.
    pub(super) fn tick_trash_clock(&mut self, now: Instant) {
        let Some(due) = self.trash_clock.due else {
            return;
        };
        if now < due {
            return;
        }
        // The next check is a day from now whatever happens to this one: a
        // purge skipped because the trash is open is not retried every frame
        // until it is closed.
        self.trash_clock.due = Some(now + PURGE_EVERY);
        if self.trash_clock.running.is_some() {
            return;
        }
        if self.tabs.iter().any(|tab| tab.trash.is_some()) {
            log::debug!("trash purge skipped: the trash is open");
            return;
        }
        let Some(root) = self.trash_clock.root.clone() else {
            return;
        };
        // The stamp decides, not this window's clock: another window may have
        // purged an hour ago, and then this one waits out the rest of that
        // day. One `stat`, so asked here rather than queued as a task that
        // would sit in `w` saying it did nothing.
        let trash = df_core::ops::Trash::at(root);
        if let Some(wait) = core_trash::purge_due_in(&trash, PURGE_EVERY, SystemTime::now()) {
            self.trash_clock.due = Some(now + wait);
            return;
        }
        self.start_trash_purge();
    }

    /// Stop a purge that is running: the trash is coming on screen. It is
    /// owed again — a cancelled purge does not stamp — and the next check
    /// runs it.
    pub(super) fn hold_trash_purge(&mut self) {
        if let Some(id) = self.trash_clock.running() {
            self.engine.cancel(id);
        }
    }

    /// Queue the purge on the task engine's workers, which nice themselves
    /// (`df_core::thread::lower_priority`), so it shows in `w` and `x` stops
    /// it.
    fn start_trash_purge(&mut self) {
        let Some(root) = self.trash_clock.root.clone() else {
            return;
        };
        let keep_days = self.config.mgr.trash_keep_days;
        let slot: PurgeSlot = Arc::new(Mutex::new(None));
        let sink = Arc::clone(&slot);
        let job = FnJob::new(
            format!(
                "Remove trashed items older than {}",
                trashview::days(keep_days)
            ),
            Lane::Macro,
            move |ctx| {
                let trash = df_core::ops::Trash::at(&root);
                // Under the stamp's lock, and asking the stamp again: of two
                // windows that both found it a day old, one purges and the
                // other stands down here.
                let result = core_trash::purge_expired_if_due(
                    &trash,
                    keep_days,
                    PURGE_EVERY,
                    SystemTime::now(),
                    ctx,
                );
                // Standing down and being cancelled are quiet, as every
                // cancelled task is; anything else is said, once, by whoever
                // reads the slot.
                let answer = match &result {
                    Ok(Some(report)) => Some(Ok(report.clone())),
                    Ok(None) | Err(df_core::DfError::Cancelled) => None,
                    Err(e) => Some(Err(e.to_string())),
                };
                let failure = match &answer {
                    Some(Ok(report)) => trashview::purged_text(report, keep_days)
                        .filter(|(_, error)| *error)
                        .map(|(text, _)| text),
                    _ => None,
                };
                match sink.lock() {
                    Ok(mut guard) => *guard = answer,
                    Err(poisoned) => *poisoned.into_inner() = answer,
                }
                result?;
                // Items that would not go make the task a failure, with the
                // count and the first reason as its text in `w`.
                match failure {
                    Some(text) => Err(df_core::DfError::Op(text)),
                    None => Ok(()),
                }
            },
        );
        let id = self.engine.spawn(job);
        self.trash_clock.running = Some((id, slot));
    }

    /// A task event that may be the purge's last: say what it came to.
    pub(super) fn trash_purge_event(&mut self, event: &TaskEvent, now: Instant) {
        if self.trash_clock.running() != Some(event.id) {
            return;
        }
        // `Failed` is not always the end (a transient failure runs again), so
        // the engine's own word on it is asked rather than the state guessed.
        let over = self.engine.task(event.id).is_none_or(|task| task.terminal);
        if !over {
            return;
        }
        let Some((_, slot)) = self.trash_clock.running.take() else {
            return;
        };
        let answer = match slot.lock() {
            Ok(mut guard) => guard.take(),
            Err(poisoned) => poisoned.into_inner().take(),
        };
        match answer {
            Some(Ok(report)) => {
                if let Some((text, error)) =
                    trashview::purged_text(&report, self.config.mgr.trash_keep_days)
                {
                    if error {
                        self.toasts.error(text, now);
                    } else {
                        self.toasts.notice(text, now);
                    }
                }
                // The trash may have been opened while the purge ran.
                if report.removed > 0 {
                    self.refresh_trash(now);
                }
            }
            Some(Err(message)) => self.toasts.error(message, now),
            None => {}
        }
    }
}
