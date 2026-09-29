//! The window's half of the sync card (`alt+p`): opening it, the two jobs it
//! starts, and what each owes the window when it lands.
//!
//! A child of `app` rather than more of `app.rs`, which is where it would
//! otherwise go: it needs the same private fields every other surface's glue
//! does, and a child module can reach them. The card itself — its stages, its
//! layout, its paint — is [`crate::sync`], beside [`crate::dialog`]; `app.rs`
//! only hands it the keyboard, the pointer and a frame.
//!
//! With a server at either end the comparison and the run are `rsync`'s
//! ([`df_core::sync::rsync`]); what is built here is the transfer, from the
//! display paths the clipboard and the pane carry and the vfs's own service
//! table, so a host that the remote pane reaches is reached the same way.
//!
//! Nothing here is journalled. The card is the confirmation, an update only
//! adds, and a mirror's removals go to the trash — whose own view is where
//! they are put back — or were announced on the card's button as deletes.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use df_core::keymap::{Chord, Key};
use df_core::ops::paste::PasteMode;
use df_core::sync::rsync::{Direction, Host, Transfer};
use df_core::sync::{Root, SyncOptions, SyncReport};

use super::{home, plural, App, Dialog, RemoteDone};
use crate::sync::{self, Remote, Running, SyncCard};

impl App {
    /// `alt+p`, and the menus' "Sync here…": sync what is yanked into the
    /// folder on screen.
    ///
    /// The gate in [`App::run`] has already turned away an empty clipboard,
    /// an archive and the trash. What is left to refuse is what only the paths
    /// can say: a folder synced into itself, two servers at once, and a server
    /// with no `rsync` here to reach it.
    pub(super) fn paste_sync(&mut self, now: Instant) {
        if self.clipboard.is_empty() {
            self.toasts.notice("Nothing yanked", now);
            return;
        }
        let sources = self.clipboard.paths.clone();
        let dest = self.cwd();
        use crate::remote::Transfer as T;
        let direction = match T::of(&sources, &dest) {
            T::Local => None,
            T::Download => Some(Direction::Download),
            T::Upload => Some(Direction::Upload),
            // The paste's two refusals, in the sync's words.
            T::Across => {
                self.toasts.notice(
                    "Remote to remote would come through this machine — download it first",
                    now,
                );
                return;
            }
            T::Mixed => {
                self.toasts.notice(
                    "Local and remote files in one sync — yank one or the other",
                    now,
                );
                return;
            }
        };
        let remote = match direction {
            None => {
                // Lexical, and so instant; the planner asks again with every
                // symlink resolved, and says so from the pool if that is what
                // catches it.
                if sources
                    .iter()
                    .any(|src| df_core::ops::is_ancestor(src, &dest))
                {
                    self.toasts.notice("cannot sync a folder into itself", now);
                    return;
                }
                None
            }
            Some(direction) => match self.remote_sync(&sources, &dest, direction) {
                Ok(remote) => Some(remote),
                Err(message) => {
                    self.toasts.notice(message, now);
                    return;
                }
            },
        };
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
            remote.clone(),
        );
        let mut card = SyncCard::new(sources, dest, title, id, slot);
        card.remote = remote;
        self.dialog = Some(Dialog::Sync(Box::new(card)));
        self.sync_context();
    }

    /// The `rsync` transfer a sync with a server is, or why there cannot be
    /// one: no `rsync` here, two servers in one clipboard, or a service the
    /// vfs reaches with something other than `ssh`.
    fn remote_sync(
        &mut self,
        sources: &[PathBuf],
        dest: &Path,
        direction: Direction,
    ) -> Result<Remote, String> {
        // A sync with a server is rsync over ssh, and a cloud remote is reached
        // through rclone with no ssh behind it. Refused first, so the reason
        // given is the real one rather than "needs rsync" on a machine without
        // it, or an ssh to a host called `r2`.
        let cloud = std::iter::once(dest)
            .chain(sources.iter().map(PathBuf::as_path))
            .filter_map(crate::remote::at_of)
            .find(|at| at.kind == df_core::vfs::ServiceKind::Rclone);
        if let Some(at) = cloud {
            return Err(format!(
                "Sync needs ssh, and {} is an rclone remote",
                at.service
            ));
        }
        if !df_core::sync::rsync::available() {
            return Err("Sync to a server needs rsync".to_string());
        }
        let far: Vec<df_core::vfs::VfsPath> = match direction {
            Direction::Upload => crate::remote::at_of(dest).into_iter().collect(),
            Direction::Download => sources
                .iter()
                .filter_map(|source| crate::remote::at_of(source))
                .collect(),
        };
        let Some(service) = far.first().map(|at| at.service.clone()) else {
            return Err("Nothing here is on a server".to_string());
        };
        if far.iter().any(|at| at.service != service) {
            return Err("Sync with one server at a time — yank from one".to_string());
        }
        let vfs = self.vfs();
        let (host, root) = match vfs.service(&service) {
            // The refusal above, for an `sftp://` URL that names a service the
            // config says is an rclone remote.
            Some(found) if found.kind == df_core::vfs::ServiceKind::Rclone => {
                return Err(format!("Sync needs ssh, and {service} is an rclone remote"));
            }
            Some(found) if found.program.is_some() => {
                return Err(format!("Sync needs ssh to reach {service}"));
            }
            Some(found) => (
                Host {
                    destination: found.destination(),
                    port: (found.port != df_core::vfs::DEFAULT_SSH_PORT).then_some(found.port),
                    key: found.key_path(),
                    program: None,
                },
                found.root_path().to_string(),
            ),
            // Not in `vfs.toml`: the name is an alias `~/.ssh/config` knows,
            // which is what an `sftp://` service name is everywhere else.
            None => (Host::alias(service.clone()), ".".to_string()),
        };
        #[cfg(test)]
        let host = Host {
            program: TEST_SHELL.with(|shell| shell.borrow().clone()),
            ..host
        };
        remote_sync(sources, dest, direction, host, &root)
    }

    /// Start the planner on the pool, named for the `w` panel after the card:
    /// the walk here, or `rsync`'s dry run when a server is at one end.
    fn compare(
        &mut self,
        title: &str,
        sources: Vec<PathBuf>,
        dest: PathBuf,
        options: SyncOptions,
        remote: Option<Remote>,
    ) -> (df_core::tasks::TaskId, sync::PlanSlot) {
        let name = title.replacen("Sync", "Compare", 1);
        match remote {
            Some(remote) => {
                let (job, slot) = sync::remote_plan_job(name, remote, dest, options);
                (self.engine.spawn(job), slot)
            }
            None => {
                let (job, slot) = sync::plan_job(name, sources, dest, options);
                (self.engine.spawn(job), slot)
            }
        }
    }

    /// The card's own keys: `m`, `v` and `c`. Matched by hand, as the disks card's
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
            Key::Char('m') => {
                card.toggle_mode();
                true
            }
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
            card.remote.clone(),
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
        let (mode, verify) = (card.mode, card.run_verify());
        let title = card.title.clone();
        let dest = card.dest.clone();
        self.dialog = None;
        self.sync_context();
        let focus = sync::focus(&plan);
        let (job, slot) = sync::sync_job(title.clone(), plan, mode, verify);
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
        let remote = crate::remote::at_of(&running.dest);
        let focus: Vec<PathBuf> = match &remote {
            // A server's listing is stale until it is fetched again.
            Some(at) => {
                self.apply_remote_done(
                    RemoteDone {
                        invalidate: Some(at.clone()),
                        ..RemoteDone::default()
                    },
                    now,
                );
                running.focus
            }
            None => {
                self.rescan(&running.dest, now);
                running
                    .focus
                    .into_iter()
                    .filter(|path| path.symlink_metadata().is_ok())
                    .collect()
            }
        };
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
thread_local! {
    /// A stand-in for `ssh` in this thread's syncs with a server: df-core's
    /// [`Host::program`] seam, reached from a test of the window, so `y` on a
    /// remote row and `alt+p` can run real `rsync` with no server.
    static TEST_SHELL: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

/// A path in a `sftp://` display path, as the server's `rsync` must be handed
/// it: the vfs's own rule (`vfs::conn`'s `wire_path`). Empty is the service's
/// root — the login directory, unless `vfs.toml` names another; absolute is
/// absolute; relative is under the root.
fn server_path(path: &str, root: &str) -> PathBuf {
    let raw = path.trim();
    if raw.is_empty() || raw == "/" {
        return PathBuf::from(root);
    }
    if raw.starts_with('/') || root == "." {
        return PathBuf::from(raw);
    }
    PathBuf::from(format!("{}/{raw}", root.trim_end_matches('/')))
}

/// The transfer and the card's roots for a sync with `host`: each source
/// lands at `dest/<its name>`, named on the card by display path, and handed
/// to `rsync` by the path its own side knows it by.
fn remote_sync(
    sources: &[PathBuf],
    dest: &Path,
    direction: Direction,
    host: Host,
    root: &str,
) -> Result<Remote, String> {
    let far = |path: &Path| crate::remote::at_of(path).map(|at| server_path(&at.path, root));
    let (paths, into) = match direction {
        Direction::Upload => (
            sources.to_vec(),
            far(dest).ok_or_else(|| format!("{} is not on a server", dest.display()))?,
        ),
        Direction::Download => (
            sources
                .iter()
                .map(|source| far(source))
                .collect::<Option<Vec<_>>>()
                .ok_or_else(|| "Some of what is yanked is not on a server".to_string())?,
            dest.to_path_buf(),
        ),
    };
    let mut names = HashSet::new();
    let mut roots = Vec::with_capacity(sources.len());
    for source in sources {
        let name = source
            .file_name()
            .ok_or_else(|| format!("{} has no name to sync under", source.display()))?;
        if !names.insert(name.to_os_string()) {
            return Err(format!(
                "two of the yanked items are called {}, and a sync would put both in one place",
                name.to_string_lossy()
            ));
        }
        roots.push(Root {
            src: source.clone(),
            dst: dest.join(name),
        });
    }
    Ok(Remote {
        transfer: Transfer {
            host,
            direction,
            sources: paths,
            dest: into,
        },
        roots,
    })
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
    use super::{remote_sync, server_path, SyncCard};
    use df_core::sync::rsync::{Direction, Host};

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
        assert!(card(&app).status().contains("· verify everything ·"));
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
    fn m_switches_to_a_mirror_and_the_button_says_what_it_removes() {
        let (tree, mut app) = yanked("sync-app-mirror");
        tree.file("dest/photos/stray.txt", b"only here");
        tree.file("dest/photos/old/gone.jpg", b"gone");
        app.run(Command::PasteSync, 10, Instant::now());
        compared(&mut app);
        assert_eq!(card(&app).labels(), ["Cancel", "Sync"]);
        key(&mut app, Key::Char('m'));
        let shown = card(&app);
        assert_eq!(shown.mode, df_core::sync::Mode::Mirror);
        let verb = match shown.plan().unwrap().removal {
            df_core::sync::Removal::Trash => "trash",
            df_core::sync::Removal::Delete => "delete",
        };
        assert_eq!(
            shown.labels(),
            ["Cancel".to_string(), format!("Sync and {verb} 2")]
        );
        assert_eq!(shown.removal_line(), Some(format!("2 to {verb}")));
        assert!(shown
            .rows()
            .iter()
            .any(|row| row.text == "photos/stray.txt"));
        // `m` again, and the extras are nobody's business.
        key(&mut app, Key::Char('m'));
        assert_eq!(card(&app).labels(), ["Cancel", "Sync"]);
        assert!(!card(&app)
            .rows()
            .iter()
            .any(|row| row.text.contains("stray")));
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
    #[cfg(unix)]
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
    #[cfg(unix)]
    fn a_folder_that_could_not_be_read_opens_the_card_and_fails_the_task() {
        use std::os::unix::fs::PermissionsExt;
        let (tree, mut app) = yanked("sync-app-unreadable");
        let locked = tree.dir("src/photos/locked");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        app.run(Command::PasteSync, 10, Instant::now());
        compared(&mut app);
        key(&mut app, Key::Enter);
        let id = app.syncs[0].id;
        synced(&mut app);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
        let shown = card(&app);
        assert_eq!(shown.heading(), "Sync finished with 1 problem");
        assert_eq!(
            shown.summary(),
            "Synced 2 files · 3 B · 2 files verified · 1 could not be read"
        );
        assert!(shown.rows().iter().any(|row| row.text.ends_with("locked")));
        assert!(matches!(
            app.engine.task(id).map(|task| task.state),
            Some(TaskState::Failed { .. })
        ));
    }

    #[test]
    #[cfg(unix)]
    fn a_socket_is_left_out_in_the_toast_and_is_no_problem() {
        let (tree, mut app) = yanked("sync-app-special");
        let _listener =
            std::os::unix::net::UnixListener::bind(tree.join("src/photos/sock")).unwrap();
        app.run(Command::PasteSync, 10, Instant::now());
        compared(&mut app);
        key(&mut app, Key::Enter);
        let id = app.syncs[0].id;
        synced(&mut app);
        assert!(app.dialog.is_none(), "no card for a socket");
        assert_eq!(
            toast(&app).as_deref(),
            Some("Synced 2 files · 3 B · verified · 1 special file left out")
        );
        assert_eq!(
            app.engine.join(id, Duration::from_secs(10)),
            Some(TaskState::Done)
        );
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
    fn a_server_path_is_what_the_remote_pane_would_have_read() {
        assert_eq!(server_path("", "."), PathBuf::from("."));
        assert_eq!(server_path("/", "/srv"), PathBuf::from("/srv"));
        assert_eq!(
            server_path("/home/brian/photos", "."),
            PathBuf::from("/home/brian/photos")
        );
        assert_eq!(server_path("photos", "."), PathBuf::from("photos"));
        assert_eq!(server_path("photos", "/srv/"), PathBuf::from("/srv/photos"));
    }

    #[test]
    fn a_sync_with_a_server_is_an_rsync_transfer_named_like_the_panes() {
        let host = Host::alias("showandtour1");
        let up = remote_sync(
            &[
                PathBuf::from("/home/brian/Photos"),
                PathBuf::from("/home/brian/notes.txt"),
            ],
            Path::new("sftp://showandtour1/backups"),
            Direction::Upload,
            host.clone(),
            ".",
        )
        .unwrap();
        assert_eq!(up.transfer.dest, PathBuf::from("/backups"));
        assert_eq!(up.transfer.sources[0], PathBuf::from("/home/brian/Photos"));
        assert_eq!(
            up.roots[0].dst,
            PathBuf::from("sftp://showandtour1/backups/Photos")
        );
        assert_eq!(
            up.roots[1].dst,
            PathBuf::from("sftp://showandtour1/backups/notes.txt")
        );

        let down = remote_sync(
            &[PathBuf::from("sftp://showandtour1/srv/shoot")],
            Path::new("/home/brian/Pictures"),
            Direction::Download,
            host.clone(),
            ".",
        )
        .unwrap();
        assert_eq!(down.transfer.sources, [PathBuf::from("/srv/shoot")]);
        assert_eq!(down.transfer.dest, PathBuf::from("/home/brian/Pictures"));
        assert_eq!(
            down.roots[0].dst,
            PathBuf::from("/home/brian/Pictures/shoot")
        );

        let twice = remote_sync(
            &[
                PathBuf::from("sftp://a/x/shoot"),
                PathBuf::from("sftp://a/y/shoot"),
            ],
            Path::new("/tmp"),
            Direction::Download,
            host,
            ".",
        );
        assert!(twice.unwrap_err().contains("called shoot"));
    }

    /// `y` on a row of a remote pane, then `alt+p` in a local folder: the
    /// clipboard carries the row's `sftp://` path as it is, the sync takes the
    /// rsync branch, the card fills from rsync's dry run, and `Enter` brings
    /// the folder down and verifies it against the server's `sha256sum` —
    /// the server being this machine, through a stand-in for `ssh`.
    #[test]
    #[cfg(unix)]
    fn y_on_a_server_row_then_alt_p_syncs_it_down_through_rsync() {
        use super::TEST_SHELL;
        use std::os::unix::fs::PermissionsExt;
        if !df_core::sync::rsync::available() {
            eprintln!("rsync is not installed; skipping");
            return;
        }
        let now = Instant::now();
        let tree = TempTree::new("sync-app-remote-yank");
        tree.file("server/photos/a.jpg", b"aa");
        let dest = tree.dir("dest");
        let shell = tree.file("bin/fake-ssh", b"#!/bin/sh\nshift\nexec sh -c \"$*\"\n");
        std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut app = app_on(&tree, &dest);
        // A vfs with no services, so nothing reads this machine's vfs.toml:
        // `fake` is then an ssh alias, and the stand-in answers for it.
        app.vfs = Some(Arc::new(df_core::vfs::Vfs::with_config(
            Default::default(),
            Vec::new(),
            Arc::new(|| {}),
        )));

        // A remote pane on the server folder, its listing from the cache.
        let at = df_core::vfs::VfsPath::new("fake", tree.join("server").to_string_lossy());
        let row = at.join("photos");
        let mut session = crate::remote::Session::new(at.clone(), dest.clone());
        session.store(
            &at,
            vec![df_core::fs::Entry {
                is_hidden: false,
                name: "photos".to_string(),
                path: crate::remote::display(&row),
                kind: df_core::fs::Kind::Dir,
                len: 0,
                mtime: None,
                btime: None,
                mode: 0o040_755,
                uid: 0,
                gid: 0,
                mime: df_core::fs::mime::DIR_MIME,
                file_kind: df_core::fs::classify(
                    df_core::fs::Kind::Dir,
                    "photos",
                    df_core::fs::mime::DIR_MIME,
                    0o040_755,
                ),
                tags: Vec::new(),
            }],
        );
        let (mgr, sort) = (app.mgr.clone(), app.sort());
        let _ = app.tabs.active_mut().show_remote(session, &mgr, sort, now);

        app.run(Command::Yank, 10, now);
        let url = crate::remote::display(&row);
        assert!(url.starts_with("sftp://fake/"), "{}", url.display());
        assert_eq!(app.clipboard.paths, [url], "the row's URL, as it is");

        app.navigate(dest.clone(), now);
        assert!(app.tab().remote.is_none());
        TEST_SHELL.with(|slot| *slot.borrow_mut() = Some(shell));
        app.run(Command::PasteSync, 10, now);
        let opened = card(&app);
        let remote = opened.remote.as_ref().expect("the rsync branch");
        assert_eq!(remote.transfer.direction, Direction::Download);
        assert_eq!(remote.transfer.sources, [tree.join("server/photos")]);

        compared(&mut app);
        assert_eq!(
            card(&app).summary(),
            "1 new · 0 changed · 0 unchanged · 2 B to copy"
        );
        key(&mut app, Key::Enter);
        synced(&mut app);
        assert_eq!(
            toast(&app).as_deref(),
            Some("Synced 1 file · 2 B · verified")
        );
        assert_eq!(std::fs::read(dest.join("photos/a.jpg")).unwrap(), b"aa");
    }

    #[test]
    fn a_clipboard_that_is_half_on_a_server_is_refused() {
        let (tree, mut app) = yanked("sync-app-mixed");
        // Built by hand: `Clipboard::yank` normalizes its paths, which turns a
        // `sftp://` display path into a local one.
        app.clipboard = Clipboard {
            mode: PasteMode::Copy,
            paths: vec![
                tree.join("src/photos"),
                PathBuf::from("sftp://showandtour1/photos"),
            ],
        };
        app.run(Command::PasteSync, 10, Instant::now());
        assert!(app.dialog.is_none());
        assert_eq!(
            toast(&app).as_deref(),
            Some("Local and remote files in one sync — yank one or the other")
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
        // The app menu's is in its Edit list.
        let edit = |app: &App| {
            app.menu
                .as_ref()
                .and_then(|menu| menu.items.iter().find(|item| item.label == "Edit"))
                .and_then(|edit| edit.submenu.clone())
                .expect("an Edit list")
        };
        app.open_app_menu();
        let items = edit(&app);
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
        let items = edit(&app);
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
