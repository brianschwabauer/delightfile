//! The window's half of the permissions card (`C`): opening it over the
//! selection, over the row a menu opened on, or over the file the spot panel
//! is about; its keys and its clicks; and carrying it out, on the task engine
//! with `u` to take it back, or through the vfs on a server, which keeps no
//! undo.
//!
//! A child of `app` for the reason `syncing` is: it needs the private fields
//! every surface's glue does. The card itself is [`crate::permissions`].

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Instant;

use df_core::keymap::{Chord, Command};
use df_core::ops::mode::MODE_BITS;
use df_core::ops::ModeJob;
use df_core::tasks::Lane;

use super::{plural, App, Dialog, RemoteDone};
use crate::permissions::{Outcome, PermCard, Subject, APPLY, CANCEL};

impl App {
    /// `C`, and the app menu's "Permissions…": the selection, or the row
    /// under the cursor. The gate in [`App::run`] has already turned away an
    /// archive, the trash and a cloud remote.
    pub(super) fn open_permissions(&mut self, now: Instant) {
        let paths = self.targets();
        self.open_permissions_on(paths, None, now);
    }

    /// The row menu's "Permissions…": the row it opened on, which the right
    /// click made the cursor, and the rest of the selection only when that
    /// row is part of it. A right click on a row outside the selection is a
    /// question about that row.
    pub(super) fn row_permissions(&mut self, now: Instant) {
        let dir = &self.tab().cwd.dir;
        let Some(row) = dir.cursor_entry().map(|entry| entry.path.clone()) else {
            return;
        };
        let selected = dir.selected_paths();
        let paths = if selected.contains(&row) {
            selected
        } else {
            vec![row]
        };
        self.open_permissions_on(paths, None, now);
    }

    /// `Enter` on the spot panel's Permissions row, and `C` over the panel:
    /// the card about the spotted file, over the panel, which comes back when
    /// the card goes.
    pub(super) fn spot_permissions(&mut self, now: Instant) {
        if self.refuse_where_we_are(Command::Permissions, now) {
            return;
        }
        let Some(spot) = self.spot.take() else {
            return;
        };
        let path = spot.facts.path.clone();
        self.open_permissions_on(vec![path], Some(Box::new(spot)), now);
    }

    /// Put the card up over `paths`, holding `spot` to put back when it
    /// closes. Whether it went up: when it did not, `spot` is back where it
    /// was and a toast says why.
    fn open_permissions_on(
        &mut self,
        paths: Vec<PathBuf>,
        spot: Option<Box<crate::spot::Spot>>,
        now: Instant,
    ) -> bool {
        let subjects = match self.permission_subjects(&paths) {
            Ok(subjects) => subjects,
            Err(why) => {
                if let Some(spot) = spot {
                    self.spot = Some(*spot);
                }
                self.toasts.notice(why, now);
                return false;
            }
        };
        let host = self.remote_at().map(|at| at.service);
        let mut card = PermCard::new(subjects, host);
        card.spot = spot;
        self.dialog = Some(Dialog::Permissions(Box::new(card)));
        self.sync_context();
        true
    }

    /// The rows `paths` name, as the card's subjects, links left out.
    ///
    /// From the listing, which read every one of them already and which
    /// inotify's `IN_ATTRIB` keeps current: opening the card touches no disk
    /// and makes no round trip to a server.
    fn permission_subjects(&self, paths: &[PathBuf]) -> Result<Vec<Subject>, &'static str> {
        if paths.is_empty() {
            return Err("Nothing selected");
        }
        let wanted: HashSet<&PathBuf> = paths.iter().collect();
        let entries: Vec<&df_core::fs::Entry> = self
            .tab()
            .cwd
            .dir
            .entries()
            .iter()
            .filter(|entry| wanted.contains(&entry.path))
            .collect();
        let subjects: Vec<Subject> = entries
            .iter()
            .filter(|entry| !entry.is_symlink())
            .map(|entry| Subject::of(entry))
            .collect();
        if subjects.is_empty() {
            // `chmod` follows a link, and there is no `lchmod`: the bits a
            // link shows are its target's, which nobody selected.
            return Err(if entries.is_empty() {
                "Nothing selected"
            } else {
                "Links have no permissions of their own — change the file they point to"
            });
        }
        Ok(subjects)
    }

    /// One keystroke into the card, before the registry sees it. Returns
    /// whether the card took it.
    pub(super) fn permissions_key(&mut self, chord: Chord, now: Instant) -> bool {
        let Some(Dialog::Permissions(card)) = &mut self.dialog else {
            return false;
        };
        match card.key(chord) {
            Outcome::Consumed => true,
            Outcome::Ignored => false,
            Outcome::Submit => {
                self.submit_permissions(now);
                true
            }
            Outcome::Close => {
                self.close_overlay(now);
                true
            }
        }
    }

    /// A press on one of the card's controls.
    pub(super) fn permissions_click(&mut self, index: usize, now: Instant) {
        match index {
            CANCEL => self.close_overlay(now),
            APPLY => self.submit_permissions(now),
            other => {
                if let Some(Dialog::Permissions(card)) = &mut self.dialog {
                    card.click(other);
                }
            }
        }
    }

    /// `Enter`, and the Apply button.
    pub(super) fn submit_permissions(&mut self, now: Instant) {
        let Some(Dialog::Permissions(card)) = &self.dialog else {
            return;
        };
        match card.enter() {
            Outcome::Submit => {}
            // Half a number in the field: the status beside the buttons
            // already says what `Enter` is waiting for.
            Outcome::Consumed | Outcome::Ignored => return,
            Outcome::Close => {
                self.close_overlay(now);
                self.toasts.notice("Nothing to change", now);
                return;
            }
        }
        let Some(Dialog::Permissions(mut card)) = self.dialog.take() else {
            return;
        };
        if let Some(spot) = card.spot.take() {
            self.spot = Some(*spot);
        }
        self.sync_context();
        if let Some(host) = card.host.clone() {
            self.remote_permissions(&card, host, now);
            return;
        }
        let paths = card.paths();
        // The folders the rows are in, re-read when the job lands; inotify's
        // `IN_ATTRIB` usually gets there first. The cursor is not aimed at
        // anything: the rows are where they were, and so is it.
        let dirs = Self::affected(&paths, None);
        // Found from the folder on screen, which every row is below — directly,
        // or further down for a listing whose rows are hits inside it.
        let job = ModeJob::new(self.cwd(), paths, card.grid, card.recursive);
        let slot = job.outcome();
        let id = self.engine.spawn(job);
        self.track(id, slot, dirs);
    }

    /// The card went away with nothing done: `Esc`, `Cancel`, a press on the
    /// scrim. The spot it was opened from comes back.
    pub(super) fn permissions_closed(&mut self, card: &mut PermCard) {
        if let Some(spot) = card.spot.take() {
            self.spot = Some(*spot);
        }
    }

    /// The card's change on a server: one `SETSTAT` per item whose mode is
    /// changing, on the pool, one level only, and no journal — there is no
    /// inverse to keep for a machine this one does not own.
    fn remote_permissions(&mut self, card: &PermCard, host: String, now: Instant) {
        let Some(at) = self.remote_at() else { return };
        let pairs: Vec<(df_core::vfs::VfsPath, u32)> = card
            .subjects
            .iter()
            .filter_map(|subject| {
                let mode = card.grid.apply(subject.mode) & MODE_BITS;
                if mode == subject.mode & MODE_BITS {
                    return None;
                }
                Some((crate::remote::at_of(&subject.path)?, mode))
            })
            .collect();
        if pairs.is_empty() {
            self.toasts.notice("Nothing to change", now);
            return;
        }
        let name = format!(
            "Set permissions on {}",
            plural(pairs.len(), "remote item", "remote items")
        );
        self.spawn_remote(name, Lane::Micro, move |vfs, ctx| {
            let mut done = 0;
            let mut failure: Option<String> = None;
            for (place, mode) in &pairs {
                match vfs.chmod(place, *mode, ctx) {
                    Ok(()) => done += 1,
                    Err(df_core::vfs::VfsError::Cancelled) => {
                        return Err(df_core::DfError::Cancelled.to_string())
                    }
                    // The first failure is the one reported, and the rest
                    // still go: one file owned by somebody else is not a
                    // reason to leave the other thirty as they were.
                    Err(e) => {
                        failure.get_or_insert_with(|| e.to_string());
                    }
                }
            }
            match failure {
                Some(message) if done > 0 => Err(format!(
                    "Permissions set on {} — {message}",
                    plural(done, "item", "items")
                )),
                Some(message) => Err(message),
                None => Ok(RemoteDone {
                    message: format!("Permissions set on {host} · no undo on a server"),
                    invalidate: Some(at),
                    ..RemoteDone::default()
                }),
            }
        });
    }

    /// The spot panel's mode, read again after an operation landed: the card
    /// it opened may have changed it, and the panel has to show the file as
    /// it is. One `stat`, following a link as the panel's own facts do.
    pub(super) fn refresh_spot_mode(&mut self) {
        let Some(spot) = &mut self.spot else { return };
        if crate::remote::is_remote(&spot.facts.path) {
            return;
        }
        if let Ok(meta) = std::fs::metadata(&spot.facts.path) {
            spot.facts.mode = df_core::platform::meta::mode(&meta);
        }
    }
}
