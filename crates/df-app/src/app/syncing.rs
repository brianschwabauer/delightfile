//! The window's half of the sync card (`alt+p`): opening it, the two jobs it
//! starts, and what each owes the window when it lands.
//!
//! A child of `app` rather than more of `app.rs`, which is where it would
//! otherwise go: it needs the same private fields every other surface's glue
//! does, and a child module can reach them. The card itself — its stages, its
//! layout, its paint — is [`crate::sync`], beside [`crate::dialog`]; `app.rs`
//! only hands it the keyboard, the pointer and a frame.
//!
//! Nothing here is journalled. The card is the confirmation, a sync only adds
//! and updates, and there is no inverse of "these bytes were verified" to
//! record.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use df_core::keymap::{Chord, Key};
use df_core::ops::paste::PasteMode;
use df_core::sync::{SyncOptions, SyncReport};

use super::{home, plural, App, Dialog};
use crate::sync::{self, Running, SyncCard};

impl App {
    /// `alt+p`, and the menus' "Sync here…": sync what is yanked into the
    /// folder on screen.
    ///
    /// The gate in [`App::run`] has already turned away an empty clipboard,
    /// an archive and the trash. What is left to refuse is what only the paths
    /// can say: a folder synced into itself, and a server at either end.
    pub(super) fn paste_sync(&mut self, now: Instant) {
        if self.clipboard.is_empty() {
            self.toasts.notice("Nothing yanked", now);
            return;
        }
        let sources = self.clipboard.paths.clone();
        let dest = self.cwd();
        if !matches!(
            crate::remote::Transfer::of(&sources, &dest),
            crate::remote::Transfer::Local
        ) {
            self.toasts
                .notice("Sync works between folders on this machine", now);
            return;
        }
        // Lexical, and so instant; the planner asks again with every symlink
        // resolved, and says so from the pool if that is what catches it.
        if sources
            .iter()
            .any(|src| df_core::ops::is_ancestor(src, &dest))
        {
            self.toasts.notice("cannot sync a folder into itself", now);
            return;
        }
        // A sync never removes a source, so a cut is synced as though it were
        // a yank — and said so, since `x` promised a move.
        if self.clipboard.mode == PasteMode::Cut {
            self.toasts
                .notice("Sync copies; the cut stays where it is", now);
        }
        let title = format!(
            "Sync {} → {}",
            plural(sources.len(), "item", "items"),
            crate::finder::shorten_home(&dest, home().as_deref())
        );
        let (id, slot) = self.compare(
            &title,
            sources.clone(),
            dest.clone(),
            SyncOptions::default(),
        );
        let card = SyncCard::new(sources, dest, title, id, slot);
        self.dialog = Some(Dialog::Sync(Box::new(card)));
        self.sync_context();
    }

    /// Start the planner on the pool, named for the `w` panel after the card.
    fn compare(
        &mut self,
        title: &str,
        sources: Vec<PathBuf>,
        dest: PathBuf,
        options: SyncOptions,
    ) -> (df_core::tasks::TaskId, sync::PlanSlot) {
        let name = title.replacen("Sync", "Compare", 1);
        let (job, slot) = sync::plan_job(name, sources, dest, options);
        (self.engine.spawn(job), slot)
    }

    /// The card's own keys: `v` and `c`. Matched by hand, as the disks card's
    /// verbs are, because the `[confirm]` table the card is matched in has no
    /// rows for them — and the card's hint strip is where they are taught.
    pub(super) fn sync_key(&mut self, chord: Chord, _now: Instant) -> bool {
        let Some(Dialog::Sync(card)) = &mut self.dialog else {
            return false;
        };
        if card.is_result() || !chord.mods.is_none() {
            return false;
        }
        match chord.key {
            Key::Char('v') => {
                card.toggle_verify();
                true
            }
            Key::Char('c') => {
                self.compare_again();
                true
            }
            _ => false,
        }
    }

    /// `c`: compare the other way. The comparison under way — or the plan
    /// already in — no longer answers the question, so it is dropped and
    /// the card counts again.
    fn compare_again(&mut self) {
        let mut card = match self.dialog.take() {
            Some(Dialog::Sync(card)) => card,
            other => {
                self.dialog = other;
                return;
            }
        };
        if let Some((id, _)) = card.comparing() {
            self.engine.cancel(id);
        }
        card.toggle_content();
        let (id, slot) = self.compare(
            &card.title,
            card.sources.clone(),
            card.dest.clone(),
            card.options(),
        );
        card.compare_again(id, slot);
        self.dialog = Some(Dialog::Sync(card));
    }

    /// The wheel over the card.
    pub(super) fn sync_wheel(&mut self, points: f32) -> bool {
        match &mut self.dialog {
            Some(Dialog::Sync(card)) => card.wheel(points),
            _ => false,
        }
    }

    /// `Enter`, and the card's answer button: run the plan — or, on the result
    /// card, put it away.
    pub(super) fn submit_sync(&mut self, now: Instant) {
        let Some(Dialog::Sync(card)) = &self.dialog else {
            return;
        };
        if card.is_result() {
            self.close_overlay(now);
            return;
        }
        // Still comparing: there is nothing to run yet, and the veiled button
        // already says so.
        let Some(plan) = card.plan().map(Arc::clone) else {
            return;
        };
        let verify = card.run_verify();
        let title = card.title.clone();
        let dest = card.dest.clone();
        self.dialog = None;
        self.sync_context();
        let focus = sync::focus(&plan);
        let (job, slot) = sync::sync_job(title.clone(), plan, verify);
        let id = self.engine.spawn(job);
        self.syncs.push(Running {
            id,
            slot,
            title,
            dest,
            focus,
        });
    }

    /// The card went away without running anything: `Esc`, `Cancel`, the
    /// `×` or a press on the backdrop.
    pub(super) fn sync_closed(&mut self, card: &SyncCard, now: Instant) {
        if let Some((id, _)) = card.comparing() {
            self.engine.cancel(id);
        }
        if !card.is_result() {
            self.toasts.notice("Sync cancelled", now);
        }
    }

    /// Once a frame: the card's comparison, and the syncs on the pool.
    /// Whether anything changed, so the frame knows to draw.
    pub(super) fn poll_sync(&mut self, now: Instant) -> bool {
        let mut changed = self.poll_comparison(now);
        let mut index = 0;
        while index < self.syncs.len() {
            // The state before the slot: a job fills its slot and *then*
            // ends, so an empty slot on a task already over means it ended
            // without a word — cancelled before it ran.
            let over = self
                .engine
                .task(self.syncs[index].id)
                .is_none_or(|task| task.terminal);
            match sync::take(&self.syncs[index].slot) {
                Some(report) => {
                    let running = self.syncs.remove(index);
                    self.sync_landed(running, report, now);
                    changed = true;
                }
                None if over => {
                    self.syncs.remove(index);
                    self.toasts.notice("Sync cancelled", now);
                    changed = true;
                }
                None => index += 1,
            }
        }
        changed
    }

    fn poll_comparison(&mut self, now: Instant) -> bool {
        let Some(Dialog::Sync(card)) = &mut self.dialog else {
            return false;
        };
        let Some((id, slot)) = card.comparing() else {
            return false;
        };
        let slot = Arc::clone(slot);
        let task = self.engine.task(id);
        let mut changed = false;
        if let Some(progress) = task.as_ref().and_then(|task| task.state.progress()) {
            changed = card.set_seen(progress.files_done);
        }
        match sync::take(&slot) {
            Some(Ok(plan)) => {
                card.land(plan);
                true
            }
            Some(Err(message)) => {
                self.dialog = None;
                self.sync_context();
                self.toasts.error(message, now);
                true
            }
            // Over with no answer: cancelled from the `w` panel.
            None if task.is_none_or(|task| task.terminal) => {
                self.dialog = None;
                self.sync_context();
                self.toasts.notice("Sync cancelled", now);
                true
            }
            None => changed,
        }
    }

    /// A sync is over: re-read where it wrote, put the cursor on what it
    /// wrote, and say how it went — in a toast when it went well, and on the
    /// card again, naming every problem, when it did not.
    fn sync_landed(&mut self, running: Running, report: SyncReport, now: Instant) {
        self.rescan(&running.dest, now);
        let focus: Vec<PathBuf> = running
            .focus
            .into_iter()
            .filter(|path| path.symlink_metadata().is_ok())
            .collect();
        if !focus.is_empty() {
            self.land_on(&focus);
        }
        if report.problems() == 0 {
            self.toasts.notice(sync::outcome(&report), now);
            return;
        }
        let card = SyncCard::result(running.dest, running.title.clone(), report);
        // Never over another card: whatever is up was asked for since, and a
        // card that replaced it would be a card nobody asked for.
        if self.dialog.is_some() || self.overlay_open() {
            self.toasts
                .error(format!("{}: {}", running.title, card.heading()), now);
            return;
        }
        self.dialog = Some(Dialog::Sync(Box::new(card)));
        self.sync_context();
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: a broken fixture should panic

    use std::path::Path;
    use std::time::Duration;

    use df_core::config::Config;
    use df_core::keymap::{Command, Registry};
    use df_core::state::StateStore;
    use df_core::sync::Verify;
    use df_core::test_support::TempTree;

    use super::super::*;
    use super::SyncCard;

    /// An `App` opened on `dir`, with nothing read from this machine: the
    /// default config, keymap and theme, and a state file inside `tree`.
    fn app_on(tree: &TempTree, dir: &Path) -> App {
        let waker = Waker {
            ring: Arc::new(|| {}),
            source: "test",
        };
        let args = crate::cli::Args {
            start: Some(dir.to_path_buf()),
            ..Default::default()
        };
        App::assemble(
            waker,
            args,
            Config::default(),
            Theme::default(),
            Registry::defaults(),
            StateStore::load_from(tree.join("state/state")),
        )
    }

    fn toast(app: &App) -> Option<String> {
        app.toasts.current().map(|toast| toast.message.clone())
    }

    fn card(app: &App) -> &SyncCard {
        match &app.dialog {
            Some(Dialog::Sync(card)) => card,
            _ => panic!("no sync card"),
        }
    }

    fn key(app: &mut App, key: Key) {
        app.overlay_key(Chord::plain(key), 10, Instant::now());
    }

    /// Wait for the card's comparison to land.
    fn compared(app: &mut App) {
        let (id, _) = card(app).comparing().expect("comparing");
        app.engine
            .join(id, Duration::from_secs(10))
            .expect("the comparison ended");
        app.poll_sync(Instant::now());
    }

    /// Wait for every sync on the pool to land.
    fn synced(app: &mut App) {
        let ids: Vec<_> = app.syncs.iter().map(|running| running.id).collect();
        for id in ids {
            app.engine
                .join(id, Duration::from_secs(10))
                .expect("the sync ended");
        }
        app.poll_sync(Instant::now());
    }

    /// A tree with `src/photos` (two files) and an empty `dest`, and an app
    /// in `dest` with the photos yanked.
    fn yanked(label: &str) -> (TempTree, App) {
        let tree = TempTree::new(label);
        tree.file("src/photos/a.jpg", b"aa");
        tree.file("src/photos/b.jpg", b"b");
        let dest = tree.dir("dest");
        let mut app = app_on(&tree, &dest);
        app.clipboard = Clipboard::yank([tree.join("src/photos")]);
        (tree, app)
    }

    #[test]
    fn alt_p_is_paste_sync_in_the_browser() {
        let keymap = Registry::defaults();
        let chord = df_core::keymap::parse_chord("alt+p").unwrap();
        assert_eq!(
            keymap.lookup(Context::Files, chord),
            Some(Command::PasteSync)
        );
    }

    #[test]
    fn with_nothing_yanked_the_sync_is_refused_and_no_card_opens() {
        let tree = TempTree::new("sync-app-empty");
        let dest = tree.dir("dest");
        let mut app = app_on(&tree, &dest);
        app.run(Command::PasteSync, 10, Instant::now());
        assert!(app.dialog.is_none());
        assert_eq!(toast(&app).as_deref(), Some("Nothing yanked"));
    }

    #[test]
    fn the_card_opens_comparing_and_fills_in_with_the_counts() {
        let (_tree, mut app) = yanked("sync-app-open");
        app.run(Command::PasteSync, 10, Instant::now());
        let card_now = card(&app);
        assert!(card_now.comparing().is_some());
        assert!(
            card_now.title.starts_with("Sync 1 item → "),
            "{}",
            card_now.title
        );
        assert!(!card_now.can_commit());

        compared(&mut app);
        let card = card(&app);
        assert_eq!(
            card.summary(),
            "2 new · 0 changed · 0 unchanged · 3 B to copy"
        );
        assert_eq!(card.labels(), ["Cancel", "Sync"]);
        assert_eq!(
            card.rows()
                .iter()
                .map(|row| row.text.as_str())
                .collect::<Vec<_>>(),
            ["photos/", "photos/a.jpg", "photos/b.jpg"]
        );
    }

    #[test]
    fn v_toggles_what_is_verified_and_c_compares_again() {
        let (_tree, mut app) = yanked("sync-app-keys");
        app.run(Command::PasteSync, 10, Instant::now());
        compared(&mut app);
        key(&mut app, Key::Char('v'));
        assert_eq!(card(&app).verify, Verify::Everything);
        assert!(card(&app).status().starts_with("verify everything"));
        key(&mut app, Key::Char('v'));
        assert_eq!(card(&app).verify, Verify::Copied);

        key(&mut app, Key::Char('c'));
        assert!(card(&app).content);
        assert!(
            card(&app).comparing().is_some(),
            "a new question, a new comparison"
        );
        compared(&mut app);
        assert!(card(&app).status().ends_with("by contents"));
        assert!(card(&app).can_commit());
    }

    #[test]
    fn enter_runs_the_sync_as_a_task_named_like_the_card() {
        let (tree, mut app) = yanked("sync-app-run");
        app.run(Command::PasteSync, 10, Instant::now());
        compared(&mut app);
        let title = card(&app).title.clone();
        key(&mut app, Key::Enter);
        assert!(app.dialog.is_none(), "the card goes as the job starts");
        let running = &app.syncs[0];
        assert_eq!(app.engine.task(running.id).unwrap().name, title);

        synced(&mut app);
        assert!(app.syncs.is_empty());
        assert_eq!(
            toast(&app).as_deref(),
            Some("Synced 2 files · 3 B · verified")
        );
        assert_eq!(
            std::fs::read(tree.join("dest/photos/a.jpg")).unwrap(),
            b"aa"
        );
    }

    #[test]
    fn esc_closes_the_card_and_stops_the_comparison() {
        let (_tree, mut app) = yanked("sync-app-esc");
        app.run(Command::PasteSync, 10, Instant::now());
        let (id, _) = card(&app).comparing().unwrap();
        key(&mut app, Key::Escape);
        assert!(app.dialog.is_none());
        assert_eq!(toast(&app).as_deref(), Some("Sync cancelled"));
        let state = app.engine.join(id, Duration::from_secs(10));
        assert!(
            matches!(state, Some(TaskState::Cancelled) | Some(TaskState::Done)),
            "{state:?}"
        );
    }

    #[test]
    fn a_synced_folder_says_already_in_sync_and_offers_to_verify() {
        let (_tree, mut app) = yanked("sync-app-in-sync");
        app.run(Command::PasteSync, 10, Instant::now());
        compared(&mut app);
        key(&mut app, Key::Enter);
        synced(&mut app);

        app.run(Command::PasteSync, 10, Instant::now());
        compared(&mut app);
        assert_eq!(card(&app).summary(), "Already in sync · 2 files");
        assert_eq!(card(&app).labels(), ["Cancel", "Verify"]);
        key(&mut app, Key::Enter);
        synced(&mut app);
        assert_eq!(
            toast(&app).as_deref(),
            Some("Already in sync · 2 files verified")
        );
    }

    #[test]
    fn a_run_with_problems_comes_back_as_a_card_naming_them() {
        use std::os::unix::fs::PermissionsExt;
        let (tree, mut app) = yanked("sync-app-problems");
        // The destination folder exists and nothing may be written into it.
        let locked = tree.dir("dest/photos");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o555)).unwrap();
        app.run(Command::PasteSync, 10, Instant::now());
        compared(&mut app);
        let id = {
            key(&mut app, Key::Enter);
            app.syncs[0].id
        };
        synced(&mut app);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();

        let card = card(&app);
        assert_eq!(card.heading(), "Sync finished with 2 problems");
        assert_eq!(
            card.rows()
                .iter()
                .map(|row| row.text.as_str())
                .collect::<Vec<_>>(),
            ["photos/a.jpg", "photos/b.jpg"]
        );
        assert!(card.rows().iter().all(|row| !row.detail.is_empty()));
        assert!(matches!(
            app.engine.task(id).map(|task| task.state),
            Some(TaskState::Failed { .. })
        ));
        // Esc puts it away without a word: there is nothing to cancel.
        key(&mut app, Key::Escape);
        assert!(app.dialog.is_none());
    }

    #[test]
    fn a_cut_is_synced_as_a_copy_and_says_so() {
        let (tree, mut app) = yanked("sync-app-cut");
        app.clipboard = Clipboard::cut([tree.join("src/photos")]);
        app.run(Command::PasteSync, 10, Instant::now());
        assert!(matches!(app.dialog, Some(Dialog::Sync(_))));
        assert_eq!(
            toast(&app).as_deref(),
            Some("Sync copies; the cut stays where it is")
        );
    }

    #[test]
    fn a_folder_cannot_be_synced_into_itself() {
        let tree = TempTree::new("sync-app-self");
        let photos = tree.dir("photos");
        let mut app = app_on(&tree, &photos);
        app.clipboard = Clipboard::yank([photos.clone()]);
        app.run(Command::PasteSync, 10, Instant::now());
        assert!(app.dialog.is_none());
        assert_eq!(
            toast(&app).as_deref(),
            Some("cannot sync a folder into itself")
        );
    }

    #[test]
    fn the_trash_and_an_archive_refuse_a_sync() {
        let (tree, mut app) = yanked("sync-app-refused");
        app.tabs.active_mut().trash = Some(crate::trashview::View {
            items: Vec::new(),
            origin: tree.join("dest"),
        });
        app.run(Command::PasteSync, 10, Instant::now());
        assert!(app.dialog.is_none());
        assert_eq!(
            toast(&app).as_deref(),
            Some("Not in the trash — Enter restores, D destroys")
        );

        app.tabs.active_mut().trash = None;
        let path = tree.join("dest/a.zip");
        app.tabs.active_mut().archive = Some(crate::archive::Browse {
            path: path.clone(),
            tree: Arc::new(df_core::archive::build(
                path,
                df_core::archive::ArchiveFormat::Zip,
                Vec::new(),
                false,
            )),
        });
        app.run(Command::PasteSync, 10, Instant::now());
        assert!(app.dialog.is_none());
        assert_eq!(
            toast(&app).as_deref(),
            Some("Archives are read-only — press e to extract")
        );
    }

    #[test]
    fn the_menus_offer_sync_here_greyed_with_nothing_yanked() {
        let (_tree, mut app) = yanked("sync-app-menus");
        let row = |items: &[menu::Item]| {
            items
                .iter()
                .find(|item| item.label == "Sync here…")
                .map(|item| (item.enabled, item.action))
        };
        let folder = |app: &App| {
            let facts = menu::FolderFacts {
                clipboard: !app.clipboard.is_empty(),
                rows: true,
                scale: app.scale_here(),
                hidden: false,
                linemode: app.mgr.linemode,
                sort: app.mgr.sort_by,
                reverse: false,
            };
            menu::folder_items(facts, &app.keymap, |command| app.refusal(command).is_some())
        };
        assert_eq!(
            row(&folder(&app)),
            Some((true, menu::Action::Run(Command::PasteSync)))
        );
        app.open_app_menu();
        let items = app.menu.as_ref().unwrap().items.clone();
        assert_eq!(
            row(&items),
            Some((true, menu::Action::Run(Command::PasteSync)))
        );
        let paste = items.iter().position(|item| item.label == "Paste").unwrap();
        assert_eq!(items[paste + 1].label, "Sync here…", "next to Paste");

        app.clipboard.clear();
        assert_eq!(
            row(&folder(&app)),
            Some((false, menu::Action::Run(Command::PasteSync)))
        );
        app.menu = None;
        app.open_app_menu();
        let items = app.menu.as_ref().unwrap().items.clone();
        assert_eq!(row(&items).map(|(enabled, _)| enabled), Some(false));
    }

    #[test]
    fn the_cards_hints_run_what_their_keys_run() {
        let (_tree, mut app) = yanked("sync-app-hints");
        app.run(Command::PasteSync, 10, Instant::now());
        compared(&mut app);
        let now = Instant::now();
        for hint in card(&app).hints() {
            let Ok(chord) = df_core::keymap::parse_chord(&hint.keys.to_lowercase()) else {
                assert!(hint.act.is_none(), "`{}` is not one key", hint.keys);
                continue;
            };
            let dispatched = app.keymap.dispatch(
                &mut KeymapState::new(),
                &app.overlay_stack(),
                WhenFlags::NONE,
                chord,
                now,
            );
            match hint.act {
                Some(chrome::HintAct::Command(command)) => {
                    assert_eq!(dispatched, Dispatch::Match(command), "{}", hint.keys)
                }
                Some(chrome::HintAct::Key(typed)) => {
                    assert_eq!(typed, chord);
                    assert_eq!(dispatched, Dispatch::NoMatch, "{} has a row", hint.keys);
                }
                None => panic!("`{}` is one key and does nothing", hint.keys),
            }
        }
    }
}
