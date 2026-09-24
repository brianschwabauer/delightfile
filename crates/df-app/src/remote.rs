//! Remote directories browsed as directories (PLAN §7.6).
//!
//! ## The same trick archives use
//!
//! `g 1` does not open a *transfer window*; it puts a different set of rows in
//! the list pane. Those rows are [`df_core::fs::Entry`] values that
//! [`df_core::vfs`] built from an SFTP `READDIR` reply and that arrive here in
//! batches, and they go into the pane's [`DirState`](df_core::fs::DirState)
//! through [`DirState::begin_external`](df_core::fs::DirState::begin_external)
//! and its siblings — so the cursor, the scroll tween, `gg`/`G`, the dir-first
//! sort, `,` re-sorts with their FLIP, `f`, `/`, `Space`, visual mode, the
//! icons, the linemode column, the grid and the breadcrumb all keep working
//! over a machine in another building, exactly as [`crate::archive`] made them
//! work inside a zip. There is one list pane, and it has one set of rules.
//!
//! ## The paths are `sftp://` URLs, in a `PathBuf`
//!
//! A row in `sftp://showandtour1/srv/www` gets the path
//! `sftp://showandtour1/srv/www/index.html`, carried in an [`df_core::fs::Entry::path`] like
//! any other. It is a *display* path and a *key*: [`crate::remote::at_of`] turns it back into
//! the [`df_core::vfs::VfsPath`] the vfs is addressed by, and nothing in this module ever
//! hands one to the local filesystem. The only local paths a remote session
//! produces are the temporary downloads [`crate::remote::Temps`] accounts for.
//!
//! ## What works remotely in v1, and what does not
//!
//! Works: browsing, preview of small text files, `o`/`Enter` (download to a
//! temp file, then the local opener), `r` rename, `a` mkdir, `d` delete,
//! `y`-then-`p` download, and `p` of locally-yanked files as an upload.
//!
//! Does not, and says so: `x` cut, `D`, symlink and hardlink, `;`/`:` shell,
//! `s`/`S` (they are `fd`/`rg` against a directory that is not on this
//! machine), `O` (the opener picker launches a child process on the row's
//! path, and a URL is not a file any viewer can open — `o` downloads first and
//! still works), `Y`, `c t`, `u`, and "what's big". Every one of them is on
//! [`crate::remote::inert_remotely`], because a key that silently does nothing is a key the
//! user presses twice.
//!
//! ### The clipboard holds remote paths
//!
//! `y` inside a remote directory fills the ordinary
//! [`Clipboard`](df_core::ops::paste::Clipboard) with `sftp://…` paths, because
//! the clipboard is a list of paths and these *are* the paths of those rows.
//! `p` then reads the ladder in one place — [`crate::remote::Transfer::of`]: remote paths into
//! a local directory are a download, local paths into a remote directory are an
//! upload, remote-to-remote is refused in v1 (it would be a download and an
//! upload through this machine, and the user should be told that is what they
//! are asking for rather than discovering it from the progress bar), and
//! local-to-local is the paste engine that already exists. One clipboard, one
//! `p`, and the decision made from the two ends rather than from a mode.
//!
//! ### And an upload asks before it destroys
//!
//! Both directions claim their name. A download climbs the `name_1` ladder so
//! it cannot silently overwrite what is in the local folder (PLAN §5); an
//! upload stats its destinations first and, when one is taken, builds the same
//! [`PastePlan`](df_core::ops::paste::PastePlan) a local paste builds — so the
//! collision is answered by the one conflict dialog, with the server's own size
//! and date on the right-hand side of the comparison. See [`plan_upload`], and
//! [`df_core::vfs::Vfs::upload_new`] for the ladder that is climbed again on
//! the server immediately before the bytes move.
//!
//! ## Nothing hangs
//!
// VERIFY-LIVE: everything in this module past the pure functions needs a real
// service. The headless tests cover the addressing, the navigation stack, the
// paste decision table, the temp ledger and the card's layout; what they cannot
// cover is a connection. The per-function markers in `crate::app` say what to
// look for against showandtour1/2.
//!
//! [`df_core::vfs`] guarantees every wait has a deadline, and this module keeps
//! the UI honest about the one wait the user can see: the first listing of a
//! service puts up a **sticky** "Connecting to …" toast, and the first batch —
//! or the failure — takes it down. A sticky toast has no clock, so it cannot
//! expire while the connection is still being made, and it is cleared by the
//! event that resolves it rather than by a timer.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use df_core::fs::Entry;
use df_core::ops::paste::{Conflict, PasteItem, PastePlan};
use df_core::vfs::VfsPath;

/// How big a remote text file may be before the preview stops downloading it.
///
/// 2 MiB. The number is defended from the other side than
/// [`crate::archive::PREVIEW_LIMIT`]: an archive member is a local seek and an
/// inflate, and this is *bytes over a link* — on a 2 MB/s uplink a 2 MiB file
/// is a one-second hover, which is about as long as a preview may take before
/// it stops feeling like a preview. Anything larger gets the facts card, which
/// is also what a non-text file gets, so there is one "no body" picture rather
/// than two.
pub const PREVIEW_LIMIT: u64 = 2 * 1024 * 1024;

/// How long the cursor has to rest on a remote file before its preview is
/// downloaded.
///
/// 220 ms. A local preview is debounced too (see `df_core::preview`), but this
/// one is spending a round trip and somebody's bandwidth, so it is deliberately
/// longer than the local debounce: a held `↓` through a hundred remote rows
/// must cost nothing at all, and 220 ms is past the interval a repeating arrow
/// key produces while comfortably under the pause that means "I am looking at
/// this one".
pub const PREVIEW_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(220);

// ── Addressing ──────────────────────────────────────────────────────────────

/// The display path for a remote place: the URL, in a [`PathBuf`].
pub fn display(at: &VfsPath) -> PathBuf {
    PathBuf::from(at.to_url())
}

/// The remote place a display path names, or `None` when it is a local path.
///
/// This is the one question everything asks — "is this row remote at all?" —
/// and it is a string parse rather than a flag on the pane, so a path that
/// escapes into the clipboard or a drag still answers it correctly wherever it
/// lands.
pub fn at_of(display: &Path) -> Option<VfsPath> {
    VfsPath::parse(&display.to_string_lossy())
}

/// Where `←` goes from a remote directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Leave {
    /// Up one remote directory.
    Up(VfsPath),
    /// Out of the service altogether, back to the local directory the session
    /// came from. `←` at the service root is the way out, and it returns to
    /// where you *were* rather than to `~` — a jump you can take back is a jump
    /// people will make.
    Out(PathBuf),
}

/// `←` from `at`, given the local directory this remote session started in.
///
/// Pure, and the whole navigation stack for a remote pane: there is no history
/// of its own to keep, because "up" is path arithmetic and "out" is one
/// remembered directory. Anything more would be a second history that
/// `Alt+←` did not know about.
pub fn leave(at: &VfsPath, origin: &Path) -> Leave {
    match at.parent() {
        Some(parent) => Leave::Up(parent),
        None => Leave::Out(origin.to_path_buf()),
    }
}

/// The remote service the list pane is on, and everything a tab has to remember
/// to be on it.
///
/// Held on the [`crate::tab::Tab`] rather than on the app, for the reason
/// [`crate::archive::Browse`] is: a second tab can be in a local directory, or
/// on a different service, and switching between them switches this with them.
pub struct Session {
    /// The remote directory the list pane is showing.
    pub at: VfsPath,
    /// The local directory this session started in — where `←` at the root
    /// goes, and where a download with no other destination lands.
    pub origin: PathBuf,
    /// The listing in flight, if one is: its token and the place it is for.
    ///
    /// The token is what makes a late reply harmless. Arrowing quickly through
    /// remote directories leaves several listings in the air at once, and every
    /// one of them that is not this token is dropped on arrival.
    pub pending: Option<(df_core::vfs::VfsToken, VfsPath)>,
    /// Directories already listed this session, by URL.
    ///
    /// Two things fall out of it, and both matter on a link with latency.
    /// **`←` is instant**, because the parent's rows were in hand a moment ago;
    /// and the **parent column is free**, because a miller layout would
    /// otherwise cost a second round trip per navigation for a column nobody
    /// asked to read.
    ///
    /// It is never refreshed on a timer. A remote listing is expensive and a
    /// pane that silently re-fetched what you are looking at would spend
    /// somebody's bandwidth on their behalf; `R` refreshes, and every operation
    /// this program performs invalidates what it changed (see
    /// [`Session::invalidate`]).
    cache: HashMap<String, Vec<Entry>>,
}

impl Session {
    pub fn new(at: VfsPath, origin: PathBuf) -> Session {
        Session {
            at,
            origin,
            pending: None,
            cache: HashMap::new(),
        }
    }

    pub fn cached(&self, at: &VfsPath) -> Option<&Vec<Entry>> {
        self.cache.get(&at.to_url())
    }

    pub fn store(&mut self, at: &VfsPath, rows: Vec<Entry>) {
        self.cache.insert(at.to_url(), rows);
    }

    /// Forget a directory, so the next visit re-reads it. What every operation
    /// owes the cache about the place it changed.
    pub fn invalidate(&mut self, at: &VfsPath) {
        self.cache.remove(&at.to_url());
    }

    pub fn forget_all(&mut self) {
        self.cache.clear();
    }
}

/// The breadcrumb for a remote place: the service as a chip, then the path.
///
/// The service is a chip rather than a segment because it is not a directory —
/// clicking it goes to the service root, but it *names a machine*, and a bar
/// that read `showandtour1 › srv › www` in one colour would look like a folder
/// called `showandtour1` on this computer. Which is exactly the mistake the
/// whole feature must not invite.
pub fn crumbs(at: &VfsPath) -> Vec<crate::chrome::Crumb> {
    let root = VfsPath::new(&at.service, "");
    let mut out = vec![crate::chrome::Crumb {
        label: at.service.clone(),
        path: display(&root),
        accent: true,
    }];
    let mut here = root;
    for segment in at.path.split('/').filter(|s| !s.is_empty()) {
        here = here.join(segment);
        out.push(crate::chrome::Crumb {
            label: segment.to_string(),
            path: display(&here),
            accent: false,
        });
    }
    out
}

// ── What a remote pane can and cannot do ────────────────────────────────────

/// Which commands are inert while the list pane is on a remote service.
///
/// The rule is "everything whose subject would have to be a local path, or
/// whose implementation is a local process". So `u` is here (the journal
/// records local inverses and a remote delete has none), the two search
/// commands are here (`fd` and `rg` would search this machine while the pane
/// showed another), and `p` is deliberately **not** here — a paste into a
/// remote directory is an upload, which is the feature.
///
/// This list is a **guard**, not a courtesy: it is consulted in one place, at
/// the top of the command dispatch, and every command on it is one that would
/// otherwise hand an `sftp://…` display path to something local. `O` is here
/// for exactly that reason — the opener picker launches a child process with
/// the row's path as an argument, and `sftp://showandtour1/srv/a.png` is not a
/// file any viewer can open. `o` is not, because it downloads first.
pub fn inert_remotely(command: df_core::keymap::Command) -> bool {
    use df_core::keymap::Command as C;
    matches!(
        command,
        // A cut is a move, and a move whose two ends are on different machines
        // is a copy and a delete that has to be undoable. Not in v1.
        C::YankCut
            // `D` is "no trash, no undo" — which is what remote `d` already is.
            // Two keys for one irreversible thing is one key too many.
            | C::DeletePermanently
            | C::SymlinkAbsolute
            | C::SymlinkRelative
            | C::Hardlink
            | C::CopyToClipboard
            | C::CopyFileText
            | C::Shell
            | C::ShellBlock
            | C::SearchName
            | C::SearchContent
            | C::Undo
            | C::DiskUsage
            | C::YankToggle
            | C::ArchiveExtractHere
            | C::ArchiveExtractSubfolder
            // `O` hands the row's path to a child process as an argument; a
            // URL there opens nothing, or creates a file with a colon in its
            // name. `o` downloads first and is deliberately still live.
            | C::OpenInteractive
            | C::OpenTrash
    )
}

/// Is this a remote display path rather than a path on this machine?
///
/// The one question a local-only consumer asks. It is [`at_of`] with the answer
/// thrown away, named for the way it is used: `if is_remote(&path) { refuse }`
/// reads as a guard, and a guard is what these call sites are.
pub fn is_remote(path: &Path) -> bool {
    at_of(path).is_some()
}

/// What `p` means, decided from the two ends rather than from a mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transfer {
    /// Local paths into a local directory: the paste engine that already
    /// exists.
    Local,
    /// Remote paths into a local directory.
    Download,
    /// Local paths into a remote directory.
    Upload,
    /// Both ends remote. Refused in v1 — see the module note.
    Across,
    /// A mixed clipboard. Refused, because half a paste is worse than none.
    Mixed,
}

impl Transfer {
    /// `sources` are whatever is in the clipboard; `dest` is the directory `p`
    /// was pressed in.
    pub fn of(sources: &[PathBuf], dest: &Path) -> Transfer {
        let remote_dest = at_of(dest).is_some();
        let remote_sources = sources.iter().filter(|p| at_of(p).is_some()).count();
        let mixed = remote_sources > 0 && remote_sources < sources.len();
        match (mixed, remote_sources > 0, remote_dest) {
            (true, _, _) => Transfer::Mixed,
            (_, true, true) => Transfer::Across,
            (_, true, false) => Transfer::Download,
            (_, false, true) => Transfer::Upload,
            (_, false, false) => Transfer::Local,
        }
    }
}

// ── An upload is a paste, and it asks the same question ─────────────────────

/// What uploading `sources` into `dest` would do, given what the server says is
/// already in that directory.
///
/// **The symmetry that was missing.** A download claims its local name through
/// [`df_core::ops::paste::unique_name`], so it can never silently overwrite
/// (PLAN §5); an upload used to be a bare `TRUNC` open, so the same paste in
/// the other direction destroyed the file that was there without a word. This
/// builds the *same* [`PastePlan`](df_core::ops::paste::PastePlan) a local
/// paste builds — ready items, conflicts, suggested names — so the collision is
/// answered by the one conflict dialog and its one state machine, with
/// `sftp://…` display paths where a local plan has local ones.
///
/// Pure: `taken` is the listing the probe brought back, so what the plan says
/// is a table test rather than something that needs a server.
pub fn plan_upload(sources: &[PathBuf], dest: &VfsPath, taken: &[Entry]) -> PastePlan {
    let mut plan = PastePlan {
        // Never `Cut`: a cut whose two ends are on different machines is a copy
        // and a delete, which is exactly what `inert_remotely` refuses.
        mode: df_core::ops::paste::PasteMode::Copy,
        dest_dir: display(dest),
        ready: Vec::new(),
        conflicts: Vec::new(),
        no_ops: Vec::new(),
    };
    // Names this same upload has already spoken for but not yet written — the
    // local planner's `claimed`, for the same reason: two files called
    // `notes.txt` from two directories must not both get the one slot.
    let mut claimed: Vec<String> = Vec::new();
    for src in sources {
        let Some(name) = src.file_name().map(|n| n.to_string_lossy().into_owned()) else {
            continue;
        };
        let on_server = taken.iter().any(|entry| entry.name == name);
        if !on_server && !claimed.contains(&name) {
            claimed.push(name.clone());
            plan.ready.push(PasteItem {
                src: src.clone(),
                dst: display(&dest.join(&name)),
                overwrite: false,
            });
            continue;
        }
        let free = free_name(&name, taken, &claimed);
        if !on_server {
            // Taken only by this paste's own earlier item: auto-named, exactly
            // as a duplicate is locally, because the user asked for both.
            if let Some(free) = &free {
                claimed.push(free.clone());
                plan.ready.push(PasteItem {
                    src: src.clone(),
                    dst: display(&dest.join(free)),
                    overwrite: false,
                });
            }
            continue;
        }
        // A conflict with no free name left still has to be *asked about* —
        // overwrite and skip are both still answerable — so the suggestion
        // falls back to the name itself rather than dropping the item.
        let suggested = free.unwrap_or_else(|| name.clone());
        plan.conflicts.push(Conflict {
            src: src.clone(),
            dst: display(&dest.join(&name)),
            suggested: display(&dest.join(&suggested)),
        });
    }
    plan
}

/// The first free `name`, `name_1`, `name_2`… in a remote directory, over the
/// listing rather than over a filesystem.
///
/// The same ladder — literally the same `suffixed` — that the trash, the local
/// paste and the archive extractor climb, so a file that lands as `notes_1.txt`
/// locally lands as `notes_1.txt` on the server. It is a *suggestion*: the
/// server is asked again immediately before the bytes move (see
/// [`df_core::vfs::Vfs::upload_new`]), because a listing is a photograph.
fn free_name(name: &str, taken: &[Entry], claimed: &[String]) -> Option<String> {
    let occupied = |candidate: &str| {
        taken.iter().any(|entry| entry.name == candidate) || claimed.iter().any(|c| c == candidate)
    };
    if !occupied(name) {
        return Some(name.to_string());
    }
    let as_os = std::ffi::OsString::from(name);
    (1..df_core::ops::trash::MAX_TRASH_COLLISIONS)
        .map(|n| {
            df_core::ops::trash::suffixed(&as_os, n)
                .to_string_lossy()
                .into_owned()
        })
        .find(|candidate| !occupied(candidate))
}

// ── Temporary downloads ─────────────────────────────────────────────────────

/// Every local file a remote session has produced, and the one place they are
/// removed from.
///
/// [`df_core::vfs::Vfs::download_to_temp`] writes into
/// `$TMPDIR/delightfile-vfs-<pid>/`, which means a session that opened forty
/// remote files leaves forty files behind unless somebody accounts for them.
/// This is that somebody: the app holds one of these, every download is
/// remembered in it, and quitting calls [`Temps::clear`].
///
/// It is keyed by the remote URL rather than by the local path because that is
/// the question every caller asks — "have I already got this file?" — and
/// answering it is what stops `o` on the same row twice costing two downloads.
#[derive(Debug, Default)]
pub struct Temps {
    files: HashMap<String, PathBuf>,
}

impl Temps {
    /// The local file already downloaded for `url`, if it is still there.
    ///
    /// The existence check is the point: an opener that moved the file, a
    /// `/tmp` cleaner, or a user tidying up would otherwise leave this handing
    /// back a path to nothing.
    pub fn get(&self, url: &str) -> Option<&Path> {
        let path = self.files.get(url)?;
        path.exists().then_some(path.as_path())
    }

    /// Remember a download. Replacing an entry removes the file it replaced —
    /// a re-download of a file that changed on the server must not leave the
    /// stale copy behind.
    pub fn remember(&mut self, url: impl Into<String>, local: PathBuf) {
        if let Some(old) = self.files.insert(url.into(), local.clone()) {
            if old != local {
                remove(&old);
            }
        }
    }

    /// Drop what is known about `url` and delete its file — what an upload
    /// over, a rename of, or a delete of the remote original owes the cache.
    pub fn forget(&mut self, url: &str) {
        if let Some(path) = self.files.remove(url) {
            remove(&path);
        }
    }

    /// How many downloads are being held. The `w` panel's "and n temporary
    /// files" line, and what the tests count.
    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Remove every downloaded file, and the directory they were in when it
    /// empties. Returns how many files went.
    ///
    /// Called on quit. Failures are logged and not reported: the program is
    /// leaving, and a toast nobody will see is not worth the path it names.
    pub fn clear(&mut self) -> usize {
        let mut dirs: Vec<PathBuf> = Vec::new();
        let mut gone = 0;
        for (_, path) in self.files.drain() {
            if let Some(dir) = path.parent().map(Path::to_path_buf) {
                if !dirs.contains(&dir) {
                    dirs.push(dir);
                }
            }
            if remove(&path) {
                gone += 1;
            }
        }
        for dir in dirs {
            // `remove_dir`, never `remove_dir_all`: the directory is shared with
            // any other delightfile that happens to have the same pid on
            // another boot, and a recursive delete of a path built from
            // `$TMPDIR` is not a thing to be casual about. An empty one goes; a
            // non-empty one stays.
            let _ignored = std::fs::remove_dir(&dir);
        }
        gone
    }
}

fn remove(path: &Path) -> bool {
    match std::fs::remove_file(path) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => {
            log::warn!("could not remove {}: {e}", path.display());
            false
        }
    }
}

// ── The preview ─────────────────────────────────────────────────────────────

/// Whether a remote row is worth spending a download on for the preview pane.
///
/// Text only, and small only — and, unlike the archive's version of this
/// question, the answer is about *bandwidth* rather than about time. A media
/// file or a PDF gets the facts card in v1: the preview pipeline wants a local
/// path, and handing it one would mean downloading a 4 GB video because a
/// cursor passed over it. Downloading media on demand behind an explicit key is
/// the obvious Phase 7 follow-up; guessing at it from a hover is not.
pub fn previewable(entry: &Entry) -> bool {
    !entry.is_dir()
        && entry.len > 0
        && entry.len <= PREVIEW_LIMIT
        && (entry.mime.starts_with("text/") || is_texty(entry.mime))
}

/// The handful of `application/*` types that are text in every way that matters
/// to a preview pane. The same list the local text preview treats as text.
fn is_texty(mime: &str) -> bool {
    matches!(
        mime,
        "application/json"
            | "application/xml"
            | "application/javascript"
            | "application/x-shellscript"
            | "application/toml"
            | "application/x-yaml"
    )
}

/// The facts card for a remote row, as label/value pairs.
///
/// Pure, so what the card says is a table test. What it does *not* say is as
/// deliberate as what it does: there is no "created" row, because SFTP version
/// 3 has no birth time at all and a column echoing the modification time would
/// be a fabrication.
pub fn card_rows(entry: &Entry, service: &str) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    rows.push((
        "Kind".to_string(),
        if entry.is_dir() {
            format!("Folder on {service}")
        } else {
            format!("{} on {service}", entry.mime)
        },
    ));
    if !entry.is_dir() {
        rows.push(("Size".to_string(), crate::format::human_size(entry.len)));
    }
    if let Some(mtime) = entry.mtime {
        rows.push((
            "Modified".to_string(),
            crate::format::long_stamp(Some(mtime)),
        ));
    }
    if entry.mode != 0 {
        rows.push(("Permissions".to_string(), entry.permissions_string()));
        rows.push(("Owner".to_string(), entry.owner_label()));
    }
    rows.push((
        "Path".to_string(),
        entry.path.to_string_lossy().into_owned(),
    ));
    rows
}

/// Why a remote row has no preview body, in the words the pane shows.
///
/// Three different facts, and the user can act on two of them — which is the
/// whole argument for saying which one it is (`delightful-ui` §11).
pub fn no_body_reason(entry: &Entry) -> &'static str {
    if entry.is_dir() {
        ""
    } else if entry.len > PREVIEW_LIMIT {
        "too big to preview over the link — press o to download and open it"
    } else if entry.len == 0 {
        "empty file"
    } else {
        // v1: images, video and PDF are facts-only on a remote service. See
        // `previewable`.
        "no preview over the link — press o to download and open it"
    }
}

// ── The card ────────────────────────────────────────────────────────────────

/// The card's inner padding, in logical points. The archive card's numbers,
/// because the two are the same picture in the same pane and a reader moving
/// between them must not see the metrics move.
const CARD_PAD: f32 = 12.0;
const CARD_ROW: f32 = 19.0;
const CARD_FONT: f32 = 13.0;
/// The card's title. `CARD_FONT + 2`, which is the step every other titled
/// surface in the window uses (`dialog`'s `FONT + 2.0`) — it was a bare `15.0`
/// that would not have tracked a change to the face below it.
const CARD_TITLE_FONT: f32 = CARD_FONT + 2.0;
/// The title's row: its own line plus the gap down to the rule under it.
const CARD_TITLE_ROW: f32 = 24.0;
const CARD_LABEL: f32 = 86.0;
const BODY_LINE: f32 = 16.0;
const BODY_FONT: f32 = 12.0;

/// Draw the facts card for the row under the cursor on a remote service.
///
/// `loading` is "a preview download is in flight", so the pane can say
/// "reading…" rather than sitting on the facts as if that were the answer.
pub fn card(
    paint: &crate::ui::Painting<'_>,
    pane: egui::Rect,
    entry: &Entry,
    service: &str,
    body: Option<&str>,
    loading: bool,
) {
    let palette = paint.palette;
    let content = crate::ui::content_rect(pane);
    if !content.is_positive() {
        return;
    }
    let painter = paint.painter.with_clip_rect(content);

    let mut y = content.top() + CARD_PAD;
    painter.text(
        egui::pos2(content.left() + CARD_PAD, y),
        egui::Align2::LEFT_TOP,
        &entry.name,
        egui::FontId::proportional(CARD_TITLE_FONT),
        palette.text,
    );
    y += CARD_TITLE_ROW;

    painter.rect_filled(
        egui::Rect::from_min_max(
            egui::pos2(content.left() + CARD_PAD, y),
            egui::pos2(content.right() - CARD_PAD, y + 1.0),
        ),
        0,
        palette.surface0,
    );
    y += CARD_PAD;

    for (label, value) in card_rows(entry, service) {
        if y + CARD_ROW > content.bottom() {
            return;
        }
        painter.text(
            egui::pos2(content.left() + CARD_PAD, y + CARD_ROW / 2.0),
            egui::Align2::LEFT_CENTER,
            &label,
            egui::FontId::proportional(CARD_FONT),
            palette.overlay1,
        );
        let room = content.right() - CARD_PAD - (content.left() + CARD_PAD + CARD_LABEL);
        let mut job = egui::text::LayoutJob::single_section(
            value,
            egui::TextFormat {
                font_id: egui::FontId::proportional(CARD_FONT),
                color: palette.text,
                ..Default::default()
            },
        );
        job.wrap = egui::text::TextWrapping {
            max_width: room.max(0.0),
            max_rows: 1,
            break_anywhere: true,
            overflow_character: Some('…'),
        };
        let galley = painter.layout_job(job);
        painter.galley(
            egui::pos2(
                content.left() + CARD_PAD + CARD_LABEL,
                y + CARD_ROW / 2.0 - galley.size().y / 2.0,
            ),
            galley,
            palette.text,
        );
        y += CARD_ROW;
    }

    y += CARD_PAD;
    match body {
        Some(text) => {
            for line in text.lines() {
                if y + BODY_LINE > content.bottom() {
                    return;
                }
                painter.text(
                    egui::pos2(content.left() + CARD_PAD, y),
                    egui::Align2::LEFT_TOP,
                    line,
                    egui::FontId::monospace(BODY_FONT),
                    palette.subtext0,
                );
                y += BODY_LINE;
            }
        }
        None if entry.is_dir() => {}
        None => {
            // No spinner: `delightful-ui` §9 asks for feedback at the point of
            // action, and this is the point of action — one line of text that
            // changes when the answer arrives, rather than a wheel that turns
            // whether or not anything is happening (PLAN §1's idle rule agrees:
            // a spinner is a window that never sleeps).
            let why = if loading {
                "reading over the link…"
            } else {
                no_body_reason(entry)
            };
            if !why.is_empty() {
                painter.text(
                    egui::pos2(content.left() + CARD_PAD, y),
                    egui::Align2::LEFT_TOP,
                    why,
                    egui::FontId::proportional(CARD_FONT),
                    palette.overlay0,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use df_core::fs::Kind;

    fn entry(name: &str, dir: &str, len: u64, mime: &'static str, is_dir: bool) -> Entry {
        Entry {
            is_hidden: name.starts_with('.'),
            name: name.to_string(),
            path: PathBuf::from(format!("{dir}/{name}")),
            kind: if is_dir { Kind::Dir } else { Kind::File },
            len,
            mtime: None,
            btime: None,
            mode: 0o100_644,
            uid: 0,
            gid: 0,
            mime,
            file_kind: df_core::fs::classify(
                if is_dir { Kind::Dir } else { Kind::File },
                name,
                mime,
                0o100_644,
            ),
        }
    }

    /// A remote URL survives the round trip through the `PathBuf` the panes
    /// carry paths in — which is the whole reason the fiction works.
    #[test]
    fn display_paths_and_remote_paths_round_trip() {
        let at = VfsPath::new("showandtour1", "/srv/www");
        assert_eq!(display(&at), PathBuf::from("sftp://showandtour1/srv/www"));
        assert_eq!(at_of(&display(&at)), Some(at.clone()));
        // The root, which has no path at all.
        let root = VfsPath::new("showandtour1", "");
        assert_eq!(display(&root), PathBuf::from("sftp://showandtour1"));
        assert_eq!(at_of(&display(&root)), Some(root));
        // A local path is not remote, which is how every command asks.
        assert_eq!(at_of(Path::new("/home/brian/Work")), None);
    }

    /// The navigation stack: up while there is somewhere up, and out of the
    /// service at the root — back to where the session came from.
    #[test]
    fn leaving_walks_up_and_then_out() {
        let origin = Path::new("/home/brian/Work");
        let deep = VfsPath::new("showandtour1", "/srv/www");
        assert_eq!(
            leave(&deep, origin),
            Leave::Up(VfsPath::new("showandtour1", "/srv"))
        );
        assert_eq!(
            leave(&VfsPath::new("showandtour1", "/srv"), origin),
            Leave::Up(VfsPath::new("showandtour1", "/"))
        );
        // …and the root goes home, not to `~`: a jump you can take back.
        assert_eq!(
            leave(&VfsPath::new("showandtour1", ""), origin),
            Leave::Out(origin.to_path_buf())
        );
        assert_eq!(
            leave(&VfsPath::new("showandtour1", "/"), origin),
            Leave::Out(origin.to_path_buf())
        );
    }

    /// The breadcrumb is a chip and then segments, and every segment goes
    /// somewhere.
    #[test]
    fn the_breadcrumb_is_a_service_chip_and_a_path() {
        let bar = crumbs(&VfsPath::new("showandtour1", "/srv/www"));
        let labels: Vec<&str> = bar.iter().map(|c| c.label.as_str()).collect();
        assert_eq!(labels, vec!["showandtour1", "srv", "www"]);
        // Only the machine wears the chip.
        assert!(bar[0].accent);
        assert!(bar[1..].iter().all(|c| !c.accent));
        assert_eq!(bar[0].path, PathBuf::from("sftp://showandtour1"));
        assert_eq!(bar[1].path, PathBuf::from("sftp://showandtour1/srv"));
        assert_eq!(bar[2].path, PathBuf::from("sftp://showandtour1/srv/www"));
        // At the root there is nothing but the chip.
        assert_eq!(crumbs(&VfsPath::new("showandtour1", "")).len(), 1);
    }

    /// `p`'s whole decision table, read off the two ends.
    #[test]
    fn a_paste_reads_its_meaning_off_both_ends() {
        let local = PathBuf::from("/home/brian/a.txt");
        let remote = PathBuf::from("sftp://showandtour1/srv/a.txt");
        let here = Path::new("/home/brian/Work");
        let there = Path::new("sftp://showandtour1/srv");
        assert_eq!(
            Transfer::of(std::slice::from_ref(&local), here),
            Transfer::Local
        );
        assert_eq!(
            Transfer::of(std::slice::from_ref(&local), there),
            Transfer::Upload
        );
        assert_eq!(
            Transfer::of(std::slice::from_ref(&remote), here),
            Transfer::Download
        );
        assert_eq!(
            Transfer::of(std::slice::from_ref(&remote), there),
            Transfer::Across
        );
        // Half a paste is worse than none.
        assert_eq!(Transfer::of(&[local, remote], here), Transfer::Mixed);
        // An empty clipboard reads as local, which is what the existing "there
        // is nothing yanked" notice is already about.
        assert_eq!(Transfer::of(&[], there), Transfer::Upload);
    }

    /// **The bug this pins**: an upload was a bare `TRUNC` open, so `p` of a
    /// local `index.html` into a remote folder that already had one destroyed
    /// the server's copy without a word — while the download half had always
    /// claimed a free name "so a download never silently overwrites (PLAN §5)".
    /// The plan an upload builds is now the same plan a paste builds, and a
    /// taken name is a *conflict*, for the same dialog to answer.
    #[test]
    fn an_upload_onto_a_taken_name_is_a_conflict_not_an_overwrite() {
        use df_core::ops::paste::Resolution;
        let dest = VfsPath::new("showandtour1", "/srv/www");
        let taken = [
            entry(
                "index.html",
                "sftp://showandtour1/srv/www",
                4096,
                "text/html",
                false,
            ),
            entry(
                "index_1.html",
                "sftp://showandtour1/srv/www",
                10,
                "text/html",
                false,
            ),
        ];
        let sources = vec![
            PathBuf::from("/home/brian/site/index.html"),
            PathBuf::from("/home/brian/site/new.css"),
        ];
        let mut plan = plan_upload(&sources, &dest, &taken);

        // The free name goes straight through, addressed as a URL.
        assert_eq!(plan.ready.len(), 1);
        assert_eq!(plan.ready[0].src, PathBuf::from("/home/brian/site/new.css"));
        assert_eq!(
            plan.ready[0].dst,
            PathBuf::from("sftp://showandtour1/srv/www/new.css")
        );
        assert!(!plan.ready[0].overwrite);

        // The taken one is asked about, and the suggestion skips the `_1` that
        // is *also* on the server.
        assert!(!plan.is_settled());
        assert_eq!(plan.conflicts.len(), 1);
        assert_eq!(
            plan.conflicts[0].dst,
            PathBuf::from("sftp://showandtour1/srv/www/index.html")
        );
        assert_eq!(
            plan.conflicts[0].suggested,
            PathBuf::from("sftp://showandtour1/srv/www/index_2.html")
        );

        // And the ordinary state machine answers it: rename lands beside the
        // original, in the remote directory rather than in a local one.
        let src = plan.conflicts[0].src.clone();
        plan.resolve(&src, &Resolution::Rename(PathBuf::from("index_2.html")))
            .expect("a bare name is a usable answer");
        assert!(plan.is_settled());
        let renamed = plan
            .ready
            .iter()
            .find(|item| item.src == src)
            .expect("the renamed item is ready");
        assert_eq!(
            renamed.dst,
            PathBuf::from("sftp://showandtour1/srv/www/index_2.html")
        );
        assert!(!renamed.overwrite);

        // Overwrite is the *deliberate* answer, and only it sets the flag the
        // upload reads to allow a replacement.
        let mut plan = plan_upload(&sources, &dest, &taken);
        let src = plan.conflicts[0].src.clone();
        plan.resolve(&src, &Resolution::Overwrite)
            .expect("answered");
        let over = plan
            .ready
            .iter()
            .find(|item| item.src == src)
            .expect("ready");
        assert!(over.overwrite);
        assert_eq!(
            over.dst,
            PathBuf::from("sftp://showandtour1/srv/www/index.html")
        );

        // Skip leaves the server's file alone and uploads nothing for it.
        let mut plan = plan_upload(&sources, &dest, &taken);
        let src = plan.conflicts[0].src.clone();
        plan.resolve(&src, &Resolution::Skip).expect("answered");
        assert!(plan.is_settled());
        assert!(plan.ready.iter().all(|item| item.src != src));
    }

    /// Two files with one name, in one upload: the second is auto-named the way
    /// a duplicate is locally, not turned into a dialog about a file that is
    /// not on the server at all.
    #[test]
    fn two_sources_with_one_name_do_not_both_claim_it() {
        let dest = VfsPath::new("showandtour1", "/srv");
        let sources = vec![
            PathBuf::from("/home/brian/a/notes.txt"),
            PathBuf::from("/home/brian/b/notes.txt"),
        ];
        let plan = plan_upload(&sources, &dest, &[]);
        assert!(plan.is_settled(), "nothing is on the server to ask about");
        let dsts: Vec<PathBuf> = plan.ready.iter().map(|i| i.dst.clone()).collect();
        assert_eq!(
            dsts,
            vec![
                PathBuf::from("sftp://showandtour1/srv/notes.txt"),
                PathBuf::from("sftp://showandtour1/srv/notes_1.txt"),
            ]
        );
        // …and every destination really is a remote address again, which is
        // what `spawn_paste` routes on.
        assert!(dsts.iter().all(|d| at_of(d).is_some()));
        assert_eq!(at_of(&plan.dest_dir), Some(dest));
    }

    /// Only small text is downloaded on a hover; everything else is the facts
    /// card, with a reason a person can act on.
    #[test]
    fn only_small_text_is_worth_a_hover_download() {
        assert!(previewable(&entry(
            "notes.md",
            "sftp://s",
            400,
            "text/markdown",
            false
        )));
        assert!(previewable(&entry(
            "pkg.json",
            "sftp://s",
            400,
            "application/json",
            false
        )));
        let big = entry(
            "dump.txt",
            "sftp://s",
            PREVIEW_LIMIT + 1,
            "text/plain",
            false,
        );
        assert!(!previewable(&big));
        assert!(no_body_reason(&big).contains("too big"));
        let media = entry("clip.mp4", "sftp://s", 400, "video/mp4", false);
        assert!(!previewable(&media));
        assert!(no_body_reason(&media).contains("no preview"));
        let empty = entry("new.txt", "sftp://s", 0, "text/plain", false);
        assert!(!previewable(&empty));
        assert_eq!(no_body_reason(&empty), "empty file");
        let dir = entry("srv", "sftp://s", 0, "inode/directory", true);
        assert!(!previewable(&dir));
        assert_eq!(no_body_reason(&dir), "");
    }

    /// The card says what a row cannot, and never invents a creation time SFTP
    /// version 3 does not carry.
    #[test]
    fn the_card_reports_what_the_row_cannot() {
        let mut e = entry(
            "index.html",
            "sftp://showandtour1/srv",
            4096,
            "text/html",
            false,
        );
        e.mode = 0o100_640;
        let rows = card_rows(&e, "showandtour1");
        let find = |label: &str| {
            rows.iter()
                .find(|(l, _)| l == label)
                .map(|(_, v)| v.clone())
        };
        assert_eq!(find("Kind").as_deref(), Some("text/html on showandtour1"));
        assert_eq!(find("Size").as_deref(), Some("4.0 KB"));
        assert_eq!(
            find("Path").as_deref(),
            Some("sftp://showandtour1/srv/index.html")
        );
        assert!(find("Created").is_none());
        assert!(find("Modified").is_none(), "no mtime means no row");
    }

    /// The whole temp-file ledger: remembered, found, replaced, forgotten, and
    /// swept — with the files themselves following each step.
    #[test]
    fn the_temp_ledger_owns_every_file_it_names() {
        let tree = df_core::test_support::TempTree::new("remote-temps");
        let dir = tree.path().join("delightfile-vfs-1");
        std::fs::create_dir_all(&dir).expect("temp dir");
        let write = |name: &str| {
            let path = dir.join(name);
            std::fs::write(&path, b"body").expect("write");
            path
        };

        let mut temps = Temps::default();
        assert!(temps.is_empty());
        assert_eq!(temps.get("sftp://s/a.txt"), None);

        let a = write("0-a.txt");
        temps.remember("sftp://s/a.txt", a.clone());
        assert_eq!(temps.get("sftp://s/a.txt"), Some(a.as_path()));
        assert_eq!(temps.len(), 1);

        // A second download of the same row replaces the first *and* removes
        // it: a stale copy left on disk is a file the ledger no longer names.
        let a2 = write("1-a.txt");
        temps.remember("sftp://s/a.txt", a2.clone());
        assert!(!a.exists(), "the replaced download is gone");
        assert_eq!(temps.get("sftp://s/a.txt"), Some(a2.as_path()));
        assert_eq!(temps.len(), 1);

        // A file that vanished underneath is not handed back as if it were
        // there — an opener that moved it must not become a stale hit.
        std::fs::remove_file(&a2).expect("remove");
        assert_eq!(temps.get("sftp://s/a.txt"), None);
        // …and it is still accounted for, so the sweep still knows about it.
        assert_eq!(temps.len(), 1);

        let b = write("2-b.txt");
        temps.remember("sftp://s/b.txt", b.clone());
        temps.forget("sftp://s/b.txt");
        assert!(!b.exists());
        assert_eq!(temps.len(), 1);

        let c = write("3-c.txt");
        temps.remember("sftp://s/c.txt", c.clone());
        // Two entries, one of which no longer has a file: the sweep reports the
        // files it actually removed.
        assert_eq!(temps.clear(), 1);
        assert!(!c.exists());
        assert!(temps.is_empty());
        // The directory went with the last file in it.
        assert!(!dir.exists());
    }

    /// The remote read-only contract, as a list. `p` is deliberately not on it.
    #[test]
    fn the_commands_that_cannot_work_remotely_are_named() {
        use df_core::keymap::Command as C;
        for c in [
            C::YankCut,
            C::DeletePermanently,
            C::Hardlink,
            C::Shell,
            C::SearchName,
            C::Undo,
            C::CopyToClipboard,
            // **The bug this pins**: `O` had no remote guard at all, so the
            // opener picker launched a child process with the row's
            // `sftp://…` display path as its argument.
            C::OpenInteractive,
        ] {
            assert!(inert_remotely(c), "{c:?} should be inert");
        }
        for c in [
            C::Paste,
            C::Yank,
            C::Trash,
            C::Rename,
            C::Create,
            C::Open,
            C::Leave,
            C::EnterDirectory,
            C::CommandPalette,
            C::SortSize,
            C::Filter,
            C::Spot,
            C::CopyPath,
        ] {
            assert!(!inert_remotely(c), "{c:?} should still work");
        }
    }

    /// Every state of the card lays out and paints without panicking — the
    /// same smoke test every other painter in this crate carries.
    #[test]
    fn the_card_paints_in_every_state() {
        let theme = df_core::config::Theme::default();
        let palette = crate::theme::Palette::from_theme(&theme);
        let ctx = egui::Context::default();
        let rows = [
            entry("notes.md", "sftp://s", 400, "text/markdown", false),
            entry("clip.mp4", "sftp://s", 9_000_000_000, "video/mp4", false),
            entry("srv", "sftp://s", 0, "inode/directory", true),
        ];
        let _ = ctx.run_ui(Default::default(), |ui| {
            let paint = crate::ui::Painting {
                tips: None,
                painter: ui.painter(),
                palette: &palette,
                theme: &theme,
                nerd: false,
                show_symlink: true,
                now: std::time::Instant::now(),
            };
            let pane = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(420.0, 600.0));
            for e in &rows {
                card(&paint, pane, e, "showandtour1", None, false);
                card(&paint, pane, e, "showandtour1", None, true);
                card(&paint, pane, e, "showandtour1", Some("one\ntwo"), false);
            }
            // A pane with no room at all draws nothing rather than dividing by
            // its own height.
            card(
                &paint,
                egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(0.0, 0.0)),
                &rows[0],
                "showandtour1",
                None,
                false,
            );
        });
    }
}
