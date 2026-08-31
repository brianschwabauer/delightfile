//! Project persistence (§8.1–§8.2): a single SQLite file (`*.dv`) is the
//! container. The in-memory model is the runtime structure; saves are full
//! rewrites in one transaction (the model is tiny, so full rewrites are
//! cheap and atomic under the rollback journal).
//! Durability policy: no sidecar backups — `quick_check` on open, salvage
//! from any intact history row on corruption.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension, Transaction};

use crate::graphic::Graphic;
use crate::model::{
    ChannelMode, Clip, ClipId, FitMode, FreeClip, Grade, GradeId, GraphicId, Marker, MarkerId,
    Media, MediaId, Project, Strip, StripId, TrackKind, TrackSettings, Tracks,
};
use crate::snapshot;

/// Bumped on every schema change (§8.1). Forward migrations only.
/// v2 (§17): `clips.graphic_id`.
pub const SCHEMA_VERSION: i64 = 2;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("database error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Snapshot(#[from] snapshot::SnapshotError),
    #[error("project file already exists: {0}")]
    AlreadyExists(PathBuf),
    #[error("not a delightvideo project (no schema_version): {0}")]
    NotAProject(PathBuf),
    #[error("this project was made with a newer delightvideo (schema v{0}, app supports v{SCHEMA_VERSION}) — update the app to open it")]
    TooNew(i64),
    #[error("project file is corrupt and no history snapshot could be recovered")]
    Unsalvageable,
}

/// Result of opening a project (§8.2): either a clean open or a salvage.
pub enum Opened {
    Clean(ProjectDb, Project),
    /// The file failed `quick_check`; the model was recovered from the newest
    /// intact history row, the damaged file renamed to `*.dv.damaged`, and a
    /// fresh file written in place. `recovered_at` is the snapshot's unix time.
    Salvaged {
        db: ProjectDb,
        project: Project,
        recovered_at: i64,
        damaged_path: PathBuf,
    },
}

pub struct ProjectDb {
    conn: Connection,
    pub path: PathBuf,
}

/// Lifecycle state of an export job (§10.1). String forms match the
/// `export_jobs.state` CHECK constraint (§8.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportJobState {
    Queued,
    Running,
    Done,
    Failed,
    Canceled,
}

impl ExportJobState {
    pub fn as_str(self) -> &'static str {
        match self {
            ExportJobState::Queued => "queued",
            ExportJobState::Running => "running",
            ExportJobState::Done => "done",
            ExportJobState::Failed => "failed",
            ExportJobState::Canceled => "canceled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "queued" => Some(ExportJobState::Queued),
            "running" => Some(ExportJobState::Running),
            "done" => Some(ExportJobState::Done),
            "failed" => Some(ExportJobState::Failed),
            "canceled" => Some(ExportJobState::Canceled),
            _ => None,
        }
    }
}

/// One row of the persistent export queue (§10.1), minus the snapshot blob —
/// snapshots are loaded on demand via [`ProjectDb::export_job_snapshot`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportJobRow {
    pub id: i64,
    pub created_at: i64,
    pub finished_at: Option<i64>,
    pub state: ExportJobState,
    pub preset: String,
    pub dest_path: PathBuf,
    pub range_json: Option<String>,
    pub error: Option<String>,
}

type Migration = fn(&Transaction) -> rusqlite::Result<()>;
/// Forward migrations, indexed by from-version (§8.1): opening a file at
/// version `v` runs `MIGRATIONS[v..SCHEMA_VERSION]`. A file created at the
/// current `SCHEMA_VERSION` runs none, so `SCHEMA_SQL` must already contain
/// everything the migrations below add.
const MIGRATIONS: &[Migration] = &[
    // 0 → 1: unreachable. v1 is the first schema ever written; no file can
    // claim version 0. Present only to keep the index = from-version.
    |_tx| Ok(()),
    // 1 → 2 (§17 graphics): clips gain the graphic document reference.
    |tx| {
        tx.execute_batch("ALTER TABLE clips ADD COLUMN graphic_id INTEGER REFERENCES graphics(id)")
    },
];

impl ProjectDb {
    /// Create a new project file. Creation is always explicit (§10.2):
    /// refuses to overwrite an existing path.
    pub fn create(path: &Path, project: &Project) -> Result<ProjectDb, DbError> {
        if path.exists() {
            return Err(DbError::AlreadyExists(path.to_path_buf()));
        }
        let conn = Connection::open(path)?;
        let mut db = ProjectDb {
            conn,
            path: path.to_path_buf(),
        };
        db.init_pragmas()?;
        let tx = db.conn.transaction()?;
        tx.execute_batch(SCHEMA_SQL)?;
        set_meta(&tx, "schema_version", &SCHEMA_VERSION.to_string())?;
        write_model(&tx, project)?;
        tx.commit()?;
        db.append_history("Project created", project, project.meta.created_at)?;
        Ok(db)
    }

    /// Open an existing project (§8.2): `quick_check`, refuse newer schemas,
    /// run forward migrations transactionally, load the model, mark offline
    /// media (§9).
    pub fn open(path: &Path) -> Result<Opened, DbError> {
        match Self::open_clean(path) {
            Ok((db, project)) => Ok(Opened::Clean(db, project)),
            Err(DbError::TooNew(v)) => Err(DbError::TooNew(v)),
            Err(DbError::NotAProject(p)) => Err(DbError::NotAProject(p)),
            // Anything else (corruption, failed quick_check, unreadable
            // schema) drops to salvage — never lose the project (§8.2).
            Err(_) => Self::salvage(path),
        }
    }

    fn open_clean(path: &Path) -> Result<(ProjectDb, Project), DbError> {
        let conn = Connection::open(path)?;
        let mut db = ProjectDb {
            conn,
            path: path.to_path_buf(),
        };
        db.init_pragmas()?;
        let check: String = db.conn.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
        if check != "ok" {
            return Err(DbError::Unsalvageable); // caller falls through to salvage
        }
        let version: Option<String> = {
            let has_meta: bool = db.conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='meta')",
                [],
                |r| r.get(0),
            )?;
            if !has_meta {
                return Err(DbError::NotAProject(path.to_path_buf()));
            }
            db.conn
                .query_row(
                    "SELECT value FROM meta WHERE key='schema_version'",
                    [],
                    |r| r.get(0),
                )
                .optional()?
        };
        let version: i64 = version
            .and_then(|v| v.parse().ok())
            .ok_or_else(|| DbError::NotAProject(path.to_path_buf()))?;
        if version > SCHEMA_VERSION {
            return Err(DbError::TooNew(version));
        }
        if version < SCHEMA_VERSION {
            let tx = db.conn.transaction()?;
            for m in MIGRATIONS
                .iter()
                .take(SCHEMA_VERSION as usize)
                .skip(version as usize)
            {
                m(&tx)?;
            }
            set_meta(&tx, "schema_version", &SCHEMA_VERSION.to_string())?;
            tx.commit()?;
        }
        let mut project = read_model(&db.conn)?;
        for m in &mut project.media {
            m.offline = !m.path.exists();
        }
        // Retention thinning on open (§6.6) — best-effort; a thinning hiccup
        // must never block opening a project.
        let _ = db.thin_history(unix_now());
        Ok((db, project))
    }

    /// Salvage mode (§8.2): recover the newest decodable history snapshot,
    /// move the damaged file aside, recreate fresh.
    fn salvage(path: &Path) -> Result<Opened, DbError> {
        let recovered = Self::newest_intact_snapshot(path)?;
        let (mut project, recovered_at) = recovered.ok_or(DbError::Unsalvageable)?;
        let damaged_path = path.with_extension("dv.damaged");
        std::fs::rename(path, &damaged_path)?;
        // Remove legacy WAL/SHM litter belonging to the damaged file so the
        // fresh DB doesn't inherit it.
        for suffix in ["-wal", "-shm"] {
            let mut os = path.as_os_str().to_owned();
            os.push(suffix);
            let _ = std::fs::remove_file(PathBuf::from(os));
        }
        for m in &mut project.media {
            m.offline = !m.path.exists();
        }
        let db = ProjectDb::create(path, &project)?;
        Ok(Opened::Salvaged {
            db,
            project,
            recovered_at,
            damaged_path,
        })
    }

    /// Best-effort read of the newest decodable `history` snapshot from a
    /// possibly-corrupt file. Each row is independent and self-contained, so
    /// recovering any one recovers the project as of that moment (§8.2).
    fn newest_intact_snapshot(path: &Path) -> Result<Option<(Project, i64)>, DbError> {
        let conn = match Connection::open(path) {
            Ok(c) => c,
            Err(_) => return Ok(None),
        };
        let mut stmt =
            match conn.prepare("SELECT snapshot, created_at FROM history ORDER BY id DESC") {
                Ok(s) => s,
                Err(_) => return Ok(None),
            };
        let rows = match stmt.query_map([], |r| Ok((r.get::<_, Vec<u8>>(0)?, r.get::<_, i64>(1)?)))
        {
            Ok(r) => r,
            Err(_) => return Ok(None),
        };
        for row in rows {
            let Ok((blob, created_at)) = row else {
                continue; // damaged row — keep scanning older ones
            };
            if let Ok(project) = snapshot::decode(&blob) {
                return Ok(Some((project, created_at)));
            }
        }
        Ok(None)
    }

    fn init_pragmas(&self) -> Result<(), DbError> {
        // §8.1: rollback journal (DELETE) + FKs; never weaken synchronous
        // (§8.2 durability). DELETE keeps the `.dv` self-contained at rest —
        // no `-wal`/`-shm` sidecars, and the main file always holds the full
        // model. Setting it on a legacy WAL-mode file checkpoints the WAL
        // into the main file and removes the sidecars.
        self.conn
            .pragma_update(None, "journal_mode", "delete")
            .map_err(DbError::from)?;
        self.conn.pragma_update(None, "foreign_keys", "ON")?;
        // Read-only peeks (recents cards, cache eviction) can briefly hold a
        // shared lock; don't let an autosave commit fail on that race.
        self.conn.pragma_update(None, "busy_timeout", 5000)?;
        Ok(())
    }

    /// Autosave flush (§8.2): full model rewrite in one transaction. Never
    /// touches `history`, `ui_state`, or `export_jobs`.
    pub fn save(&mut self, project: &Project) -> Result<(), DbError> {
        let tx = self.conn.transaction()?;
        write_model(&tx, project)?;
        tx.commit()?;
        Ok(())
    }

    /// Append a history snapshot row (§6.6). If `replace_head`, overwrite the
    /// newest row instead (burst coalescing; full burst semantics land in M3).
    pub fn append_history(
        &mut self,
        label: &str,
        project: &Project,
        created_at: i64,
    ) -> Result<(), DbError> {
        let blob = snapshot::encode(project)?;
        self.conn.execute(
            "INSERT INTO history (created_at, label, snapshot) VALUES (?1, ?2, ?3)",
            params![created_at, label, blob],
        )?;
        Ok(())
    }

    /// Replace the newest history row (burst continuation, §6.6).
    pub fn replace_history_head(
        &mut self,
        label: &str,
        project: &Project,
        created_at: i64,
    ) -> Result<(), DbError> {
        let blob = snapshot::encode(project)?;
        let head: Option<i64> = self
            .conn
            .query_row("SELECT MAX(id) FROM history", [], |r| r.get(0))
            .optional()?
            .flatten();
        match head {
            Some(id) => {
                self.conn.execute(
                    "UPDATE history SET created_at=?1, label=?2, snapshot=?3 WHERE id=?4",
                    params![created_at, label, blob, id],
                )?;
                Ok(())
            }
            None => self.append_history(label, project, created_at),
        }
    }

    pub fn history_entries(&self) -> Result<Vec<(i64, i64, String)>, DbError> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, created_at, label FROM history ORDER BY id DESC")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Newest history row `(id, created_at, label)`, if any (§6.6). Drives the
    /// session's burst-replace decision — the head an incoming flush may merge
    /// into.
    pub fn history_head(&self) -> Result<Option<(i64, i64, String)>, DbError> {
        let row = self
            .conn
            .query_row(
                "SELECT id, created_at, label FROM history ORDER BY id DESC LIMIT 1",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        Ok(row)
    }

    /// Decode the model stored in a history row, for time travel (§6.6). The
    /// History view retargets the preview to this snapshot without touching
    /// the live project.
    pub fn history_snapshot(&self, id: i64) -> Result<Project, DbError> {
        let blob: Vec<u8> = self.conn.query_row(
            "SELECT snapshot FROM history WHERE id=?1",
            params![id],
            |r| r.get(0),
        )?;
        Ok(snapshot::decode(&blob)?)
    }

    /// Retention thinning on project open (§6.6): keep everything from the last
    /// 15 min, then at most 1 row per minute for the last hour, 1 per 10 min
    /// for the last day, 1 per hour beyond; hard cap 1,000 rows total. When a
    /// bucket holds several rows, keep the NEWEST; the newest row overall is
    /// never deleted. Runs in a single transaction.
    pub fn thin_history(&mut self, now_unix: i64) -> Result<(), DbError> {
        // Ascending by (created_at, id): oldest first, newest last.
        let rows: Vec<(i64, i64)> = {
            let mut stmt = self
                .conn
                .prepare("SELECT id, created_at FROM history ORDER BY created_at, id")?;
            let collected = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<Vec<_>, _>>()?;
            collected
        };
        if rows.len() <= 1 {
            return Ok(());
        }
        let mut keep: HashSet<i64> = HashSet::new();
        // Newest survivor per coarse bucket, keyed by (tier, bucket index).
        let mut best: HashMap<(u8, i64), (i64, i64)> = HashMap::new();
        for &(id, created_at) in &rows {
            let age = now_unix.saturating_sub(created_at);
            let key = if age < 900 {
                keep.insert(id); // last 15 min — keep everything
                continue;
            } else if age < 3600 {
                (1u8, created_at / 60) // 1/min for the last hour
            } else if age < 86_400 {
                (2u8, created_at / 600) // 1/10 min for the last day
            } else {
                (3u8, created_at / 3600) // 1/hour beyond
            };
            let slot = best.entry(key).or_insert((created_at, id));
            if (created_at, id) >= (slot.0, slot.1) {
                *slot = (created_at, id);
            }
        }
        for (_, (_, id)) in best {
            keep.insert(id);
        }
        // The newest row overall is never deleted (§6.6).
        if let Some(&(id, _)) = rows.last() {
            keep.insert(id);
        }
        // Hard cap: keep only the newest 1,000. `rows` is ascending, so the
        // oldest kept rows are the first to fall off — the newest stays.
        if keep.len() > 1000 {
            let mut drop_count = keep.len() - 1000;
            for &(id, _) in &rows {
                if drop_count == 0 {
                    break;
                }
                if keep.remove(&id) {
                    drop_count -= 1;
                }
            }
        }
        let tx = self.conn.transaction()?;
        for &(id, _) in &rows {
            if !keep.contains(&id) {
                tx.execute("DELETE FROM history WHERE id=?1", params![id])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    // ---- ui_state (§8.4): best-effort, never dirties the project ----

    pub fn ui_state_get(&self, key: &str) -> Option<String> {
        self.conn
            .query_row("SELECT value FROM ui_state WHERE key=?1", [key], |r| {
                r.get(0)
            })
            .optional()
            .ok()
            .flatten()
    }

    pub fn ui_state_set(&self, key: &str, value: &str) -> Result<(), DbError> {
        self.conn.execute(
            "INSERT INTO ui_state (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    // ---- export_jobs (§10.1): persistent queue; bookkeeping writes like
    // ui_state — never touch `history`, never mark the project dirty ----

    /// All export jobs in creation order (§10.1). Rows whose `state` string
    /// fails to parse are skipped defensively (unreachable through this API,
    /// which only writes CHECK-valid states).
    pub fn load_export_jobs(&self) -> Result<Vec<ExportJobRow>, DbError> {
        let mut stmt = self.conn.prepare(
            "SELECT id, created_at, finished_at, state, preset, dest_path, range_json, error
             FROM export_jobs ORDER BY id",
        )?;
        let rows = stmt
            .query_map([], |r| {
                let state: String = r.get(3)?;
                Ok((
                    ExportJobRow {
                        id: r.get(0)?,
                        created_at: r.get(1)?,
                        finished_at: r.get(2)?,
                        state: ExportJobState::Queued, // placeholder; set below
                        preset: r.get(4)?,
                        dest_path: PathBuf::from(r.get::<_, String>(5)?),
                        range_json: r.get(6)?,
                        error: r.get(7)?,
                    },
                    state,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut out = Vec::with_capacity(rows.len());
        for (mut row, state) in rows {
            match ExportJobState::parse(&state) {
                Some(s) => {
                    row.state = s;
                    out.push(row);
                }
                None => log::warn!("skipping export job {} with bad state {:?}", row.id, state),
            }
        }
        Ok(out)
    }

    /// The stored zstd model snapshot for one job (§10.1), loaded on demand.
    pub fn export_job_snapshot(&self, id: i64) -> Result<Option<Vec<u8>>, DbError> {
        let blob = self
            .conn
            .query_row(
                "SELECT snapshot FROM export_jobs WHERE id=?1",
                params![id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(blob)
    }

    /// Enqueue a new job (§10.1): initial state `queued`, no finish/error yet.
    /// Returns the new rowid.
    pub fn insert_export_job(
        &self,
        created_at: i64,
        preset: &str,
        dest_path: &Path,
        range_json: Option<&str>,
        snapshot: &[u8],
    ) -> Result<i64, DbError> {
        self.conn.execute(
            "INSERT INTO export_jobs
                (created_at, finished_at, state, preset, dest_path, range_json, snapshot, error)
             VALUES (?1, NULL, 'queued', ?2, ?3, ?4, ?5, NULL)",
            params![
                created_at,
                preset,
                dest_path.to_string_lossy(),
                range_json,
                snapshot
            ],
        )?;
        Ok(self.conn.last_insert_rowid())
    }

    /// Update a job's `state`, `finished_at`, and `error` columns (§10.1).
    pub fn set_export_job_state(
        &self,
        id: i64,
        state: ExportJobState,
        finished_at: Option<i64>,
        error: Option<&str>,
    ) -> Result<(), DbError> {
        self.conn.execute(
            "UPDATE export_jobs SET state=?1, finished_at=?2, error=?3 WHERE id=?4",
            params![state.as_str(), finished_at, error, id],
        )?;
        Ok(())
    }

    /// Remove a job row (§10.1 — Delete recovery action, manual purge).
    pub fn delete_export_job(&self, id: i64) -> Result<(), DbError> {
        self.conn
            .execute("DELETE FROM export_jobs WHERE id=?1", params![id])?;
        Ok(())
    }
}

const SCHEMA_SQL: &str = r#"
CREATE TABLE meta      (key TEXT PRIMARY KEY, value TEXT);
CREATE TABLE media     (id INTEGER PRIMARY KEY, path TEXT, hash TEXT,
                        kind TEXT CHECK(kind IN ('video','audio','image')),
                        duration_us INTEGER,
                        video_codec TEXT, audio_codec TEXT,
                        width INTEGER, height INTEGER, fps_num INTEGER, fps_den INTEGER,
                        added_at INTEGER);
CREATE TABLE tracks    (id INTEGER PRIMARY KEY, kind TEXT CHECK(kind IN ('v1','v2','g','a1','a2')),
                        gain REAL DEFAULT 0, pan REAL DEFAULT 0, muted INTEGER DEFAULT 0,
                        solo INTEGER DEFAULT 0, duck INTEGER DEFAULT 0, send REAL DEFAULT 0);
CREATE TABLE strips    (id INTEGER PRIMARY KEY, params_json TEXT);
CREATE TABLE grades    (id INTEGER PRIMARY KEY, params_json TEXT);
CREATE TABLE graphics  (id INTEGER PRIMARY KEY, doc_json TEXT);
CREATE TABLE clips     (id INTEGER PRIMARY KEY, track_id INTEGER REFERENCES tracks,
                        media_id INTEGER REFERENCES media,
                        seq INTEGER,
                        timeline_start_us INTEGER,
                        anchor_clip_id INTEGER REFERENCES clips, anchor_offset_us INTEGER,
                        source_in_us INTEGER, source_out_us INTEGER,
                        speed REAL DEFAULT 1.0, label TEXT, color INTEGER,
                        gain REAL DEFAULT 0, pan REAL DEFAULT 0, muted INTEGER DEFAULT 0,
                        channel_mode TEXT DEFAULT 'stereo',
                        fade_in_us INTEGER DEFAULT 0, fade_out_us INTEGER DEFAULT 0,
                        xfade_us INTEGER DEFAULT 0,
                        vfade_in_us INTEGER DEFAULT 0, vfade_out_us INTEGER DEFAULT 0,
                        strip_id INTEGER REFERENCES strips,
                        crop_l INTEGER DEFAULT 0, crop_r INTEGER DEFAULT 0,
                        crop_t INTEGER DEFAULT 0, crop_b INTEGER DEFAULT 0,
                        nudge_x INTEGER DEFAULT 0, nudge_y INTEGER DEFAULT 0,
                        rotate REAL DEFAULT 0,
                        fit_mode TEXT DEFAULT 'fill',
                        scale REAL, tx REAL, ty REAL,
                        grade_id INTEGER REFERENCES grades,
                        graphic_id INTEGER REFERENCES graphics(id));
CREATE TABLE markers   (id INTEGER PRIMARY KEY, time_us INTEGER, name TEXT, color INTEGER);
CREATE TABLE history   (id INTEGER PRIMARY KEY, created_at INTEGER, label TEXT,
                        snapshot BLOB);
CREATE TABLE export_jobs (id INTEGER PRIMARY KEY, created_at INTEGER, finished_at INTEGER,
                        state TEXT CHECK(state IN ('queued','running','done','failed','canceled')),
                        preset TEXT, dest_path TEXT, range_json TEXT,
                        snapshot BLOB,
                        error TEXT);
CREATE TABLE ui_state  (key TEXT PRIMARY KEY, value TEXT);
"#;

/// Fixed rowids for the fixed track layout (§6.1).
fn track_rowid(kind: TrackKind) -> i64 {
    match kind {
        TrackKind::V1 => 1,
        TrackKind::V2 => 2,
        TrackKind::A1 => 3,
        TrackKind::A2 => 4,
        TrackKind::G => 5,
    }
}

/// Wall-clock unix seconds — used only for retention thinning on open (§6.6);
/// all model timestamps come from callers so the model stays deterministic.
fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

fn set_meta(tx: &Transaction, key: &str, value: &str) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![key, value],
    )?;
    Ok(())
}

/// Serialize the full model into the open transaction (project tables only).
fn write_model(tx: &Transaction, p: &Project) -> Result<(), DbError> {
    for (k, v) in [
        ("name", p.meta.name.clone()),
        ("width", p.meta.width.to_string()),
        ("height", p.meta.height.to_string()),
        ("fps_num", p.meta.fps_num.to_string()),
        ("fps_den", p.meta.fps_den.to_string()),
        ("sample_rate", p.meta.sample_rate.to_string()),
        ("created_at", p.meta.created_at.to_string()),
        ("modified_at", p.meta.modified_at.to_string()),
        (
            "loudness_target_lufs",
            p.meta.loudness_target_lufs.to_string(),
        ),
        (
            "loudness_enabled",
            (p.meta.loudness_enabled as i64).to_string(),
        ),
        (
            "limiter_ceiling_dbtp",
            p.meta.limiter_ceiling_dbtp.to_string(),
        ),
    ] {
        set_meta(tx, k, &v)?;
    }
    for (k, v) in [
        ("bus_comp_json", p.meta.bus_comp_json.clone()),
        ("range_in_us", p.meta.range_in_us.map(|v| v.to_string())),
        ("range_out_us", p.meta.range_out_us.map(|v| v.to_string())),
    ] {
        match v {
            Some(v) => set_meta(tx, k, &v)?,
            None => {
                tx.execute("DELETE FROM meta WHERE key=?1", [k])?;
            }
        }
    }

    tx.execute("DELETE FROM clips", [])?;
    tx.execute("DELETE FROM media", [])?;
    tx.execute("DELETE FROM tracks", [])?;
    tx.execute("DELETE FROM strips", [])?;
    tx.execute("DELETE FROM grades", [])?;
    tx.execute("DELETE FROM graphics", [])?;
    tx.execute("DELETE FROM markers", [])?;

    for m in &p.media {
        tx.execute(
            "INSERT INTO media (id, path, hash, kind, duration_us, video_codec, audio_codec,
                                width, height, fps_num, fps_den, added_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
            params![
                m.id.0,
                m.path.to_string_lossy(),
                m.hash,
                m.kind.as_str(),
                m.duration_us,
                m.video_codec,
                m.audio_codec,
                m.width,
                m.height,
                m.fps_num,
                m.fps_den,
                m.added_at
            ],
        )?;
    }
    for (kind, t) in [
        (TrackKind::V1, &p.tracks.v1),
        (TrackKind::V2, &p.tracks.v2),
        (TrackKind::G, &p.tracks.g),
        (TrackKind::A1, &p.tracks.a1),
        (TrackKind::A2, &p.tracks.a2),
    ] {
        tx.execute(
            "INSERT INTO tracks (id, kind, gain, pan, muted, solo, duck, send)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                track_rowid(kind),
                kind.as_str(),
                t.gain_db,
                t.pan,
                t.muted as i64,
                t.solo as i64,
                t.duck as i64,
                t.send
            ],
        )?;
    }
    for s in &p.strips {
        tx.execute(
            "INSERT INTO strips (id, params_json) VALUES (?1, ?2)",
            params![s.id.0, s.params_json],
        )?;
    }
    for g in &p.grades {
        tx.execute(
            "INSERT INTO grades (id, params_json) VALUES (?1, ?2)",
            params![g.id.0, g.params_json],
        )?;
    }
    for g in &p.graphics {
        tx.execute(
            "INSERT INTO graphics (id, doc_json) VALUES (?1, ?2)",
            params![g.id.0, g.doc_json],
        )?;
    }
    for (i, c) in p.timeline.v1.iter().enumerate() {
        insert_clip(tx, c, TrackKind::V1, Some(i as i64), None, None)?;
    }
    for (kind, list) in [
        (TrackKind::V2, &p.timeline.v2),
        (TrackKind::G, &p.timeline.g),
        (TrackKind::A1, &p.timeline.a1),
        (TrackKind::A2, &p.timeline.a2),
    ] {
        for fc in list {
            // A dangling anchor (its V1 clip deleted; §6.3 fallback is the
            // absolute start) serializes as timeline-anchored — the FK on
            // anchor_clip_id must never see a missing target. Resolve the
            // position through the same fallback the renderer uses.
            let anchor = fc
                .anchor
                .filter(|(id, _)| p.timeline.v1.iter().any(|c| c.id == *id));
            let start = if anchor.is_none() {
                p.timeline.free_start_us(fc)
            } else {
                fc.timeline_start_us
            };
            insert_clip(tx, &fc.clip, kind, None, Some(start), anchor)?;
        }
    }
    for mk in &p.markers {
        tx.execute(
            "INSERT INTO markers (id, time_us, name, color) VALUES (?1,?2,?3,?4)",
            params![mk.id.0, mk.time_us, mk.name, mk.color],
        )?;
    }
    Ok(())
}

fn insert_clip(
    tx: &Transaction,
    c: &Clip,
    track: TrackKind,
    seq: Option<i64>,
    timeline_start_us: Option<i64>,
    anchor: Option<(ClipId, i64)>,
) -> Result<(), DbError> {
    tx.execute(
        "INSERT INTO clips (id, track_id, media_id, seq, timeline_start_us,
                            anchor_clip_id, anchor_offset_us,
                            source_in_us, source_out_us, speed, label, color,
                            gain, pan, muted, channel_mode,
                            fade_in_us, fade_out_us, xfade_us, vfade_in_us, vfade_out_us,
                            strip_id, crop_l, crop_r, crop_t, crop_b, nudge_x, nudge_y,
                            rotate, fit_mode, scale, tx, ty, grade_id, graphic_id)
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,
                 ?21,?22,?23,?24,?25,?26,?27,?28,?29,?30,?31,?32,?33,?34,?35)",
        params![
            c.id.0,
            track_rowid(track),
            c.media_id.map(|m| m.0),
            seq,
            timeline_start_us,
            anchor.map(|(id, _)| id.0),
            anchor.map(|(_, off)| off),
            c.source_in_us,
            c.source_out_us,
            c.speed,
            c.label,
            c.color,
            c.gain_db,
            c.pan,
            c.muted as i64,
            channel_mode_str(c.channel_mode),
            c.fade_in_us,
            c.fade_out_us,
            c.xfade_us,
            c.vfade_in_us,
            c.vfade_out_us,
            c.strip_id.map(|s| s.0),
            c.crop_l,
            c.crop_r,
            c.crop_t,
            c.crop_b,
            c.nudge_x,
            c.nudge_y,
            c.rotate,
            fit_mode_str(c.fit_mode),
            c.scale,
            c.tx,
            c.ty,
            c.grade_id.map(|g| g.0),
            c.graphic_id.map(|g| g.0),
        ],
    )?;
    Ok(())
}

fn channel_mode_str(m: ChannelMode) -> &'static str {
    match m {
        ChannelMode::Stereo => "stereo",
        ChannelMode::Left => "left",
        ChannelMode::Right => "right",
        ChannelMode::Sum => "sum",
    }
}

fn parse_channel_mode(s: &str) -> ChannelMode {
    match s {
        "left" => ChannelMode::Left,
        "right" => ChannelMode::Right,
        "sum" => ChannelMode::Sum,
        _ => ChannelMode::Stereo,
    }
}

fn fit_mode_str(m: FitMode) -> &'static str {
    match m {
        FitMode::Fill => "fill",
        FitMode::Fit => "fit",
        FitMode::Custom => "custom",
    }
}

fn parse_fit_mode(s: &str) -> FitMode {
    match s {
        "fit" => FitMode::Fit,
        "custom" => FitMode::Custom,
        _ => FitMode::Fill,
    }
}

fn get_meta(conn: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0))
        .optional()
}

fn read_model(conn: &Connection) -> Result<Project, DbError> {
    let meta_s = |key: &str, default: &str| -> Result<String, DbError> {
        Ok(get_meta(conn, key)?.unwrap_or_else(|| default.to_string()))
    };
    let meta_i = |key: &str, default: i64| -> Result<i64, DbError> {
        Ok(get_meta(conn, key)?
            .and_then(|v| v.parse().ok())
            .unwrap_or(default))
    };
    let meta_f = |key: &str, default: f64| -> Result<f64, DbError> {
        Ok(get_meta(conn, key)?
            .and_then(|v| v.parse().ok())
            .unwrap_or(default))
    };

    let mut project = Project::new(meta_s("name", "untitled")?, meta_i("created_at", 0)?);
    project.meta.width = meta_i("width", 1920)? as u32;
    project.meta.height = meta_i("height", 1080)? as u32;
    project.meta.fps_num = meta_i("fps_num", 30)? as u32;
    project.meta.fps_den = meta_i("fps_den", 1)? as u32;
    project.meta.sample_rate = meta_i("sample_rate", 48_000)? as u32;
    project.meta.modified_at = meta_i("modified_at", 0)?;
    project.meta.loudness_target_lufs = meta_f("loudness_target_lufs", -14.0)?;
    project.meta.loudness_enabled = meta_i("loudness_enabled", 1)? != 0;
    project.meta.limiter_ceiling_dbtp = meta_f("limiter_ceiling_dbtp", -1.0)?;
    project.meta.bus_comp_json = get_meta(conn, "bus_comp_json")?;
    project.meta.range_in_us = get_meta(conn, "range_in_us")?.and_then(|v| v.parse().ok());
    project.meta.range_out_us = get_meta(conn, "range_out_us")?.and_then(|v| v.parse().ok());

    let mut stmt = conn.prepare(
        "SELECT id, path, hash, kind, duration_us, video_codec, audio_codec,
                width, height, fps_num, fps_den, added_at FROM media ORDER BY id",
    )?;
    let media = stmt
        .query_map([], |r| {
            Ok(Media {
                id: MediaId(r.get(0)?),
                path: PathBuf::from(r.get::<_, String>(1)?),
                hash: r.get(2)?,
                kind: MediaKindSql(r.get::<_, String>(3)?).into(),
                duration_us: r.get(4)?,
                video_codec: r.get(5)?,
                audio_codec: r.get(6)?,
                width: r.get(7)?,
                height: r.get(8)?,
                fps_num: r.get(9)?,
                fps_den: r.get(10)?,
                added_at: r.get(11)?,
                offline: false,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    project.media = media;

    let mut stmt = conn.prepare("SELECT kind, gain, pan, muted, solo, duck, send FROM tracks")?;
    let track_rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                TrackSettings {
                    gain_db: r.get(1)?,
                    pan: r.get(2)?,
                    muted: r.get::<_, i64>(3)? != 0,
                    solo: r.get::<_, i64>(4)? != 0,
                    duck: r.get::<_, i64>(5)? != 0,
                    send: r.get(6)?,
                },
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut tracks = Tracks::default();
    for (kind, t) in track_rows {
        match TrackKind::parse(&kind) {
            Some(TrackKind::V1) => tracks.v1 = t,
            Some(TrackKind::V2) => tracks.v2 = t,
            Some(TrackKind::G) => tracks.g = t,
            Some(TrackKind::A1) => tracks.a1 = t,
            Some(TrackKind::A2) => tracks.a2 = t,
            None => {} // unknown kind — ignore rather than fail the open
        }
    }
    project.tracks = tracks;

    let mut stmt = conn.prepare("SELECT id, params_json FROM strips ORDER BY id")?;
    project.strips = stmt
        .query_map([], |r| {
            Ok(Strip {
                id: StripId(r.get(0)?),
                params_json: r.get(1)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut stmt = conn.prepare("SELECT id, params_json FROM grades ORDER BY id")?;
    project.grades = stmt
        .query_map([], |r| {
            Ok(Grade {
                id: GradeId(r.get(0)?),
                params_json: r.get(1)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    let mut stmt = conn.prepare("SELECT id, doc_json FROM graphics ORDER BY id")?;
    project.graphics = stmt
        .query_map([], |r| {
            Ok(Graphic {
                id: GraphicId(r.get(0)?),
                doc_json: r.get(1)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    let mut stmt = conn.prepare(
        "SELECT id, track_id, media_id, seq, timeline_start_us, anchor_clip_id, anchor_offset_us,
                source_in_us, source_out_us, speed, label, color,
                gain, pan, muted, channel_mode,
                fade_in_us, fade_out_us, xfade_us, vfade_in_us, vfade_out_us,
                strip_id, crop_l, crop_r, crop_t, crop_b, nudge_x, nudge_y,
                rotate, fit_mode, scale, tx, ty, grade_id, graphic_id
         FROM clips ORDER BY seq, timeline_start_us, id",
    )?;
    struct ClipRow {
        clip: Clip,
        track_id: i64,
        timeline_start_us: Option<i64>,
        anchor: Option<(ClipId, i64)>,
    }
    let rows = stmt
        .query_map([], |r| {
            let anchor_id: Option<i64> = r.get(5)?;
            let anchor_off: Option<i64> = r.get(6)?;
            Ok(ClipRow {
                clip: Clip {
                    id: ClipId(r.get(0)?),
                    media_id: r.get::<_, Option<i64>>(2)?.map(MediaId),
                    source_in_us: r.get(7)?,
                    source_out_us: r.get(8)?,
                    speed: r.get(9)?,
                    label: r.get(10)?,
                    color: r.get(11)?,
                    gain_db: r.get(12)?,
                    pan: r.get(13)?,
                    muted: r.get::<_, i64>(14)? != 0,
                    channel_mode: parse_channel_mode(&r.get::<_, String>(15)?),
                    fade_in_us: r.get(16)?,
                    fade_out_us: r.get(17)?,
                    xfade_us: r.get(18)?,
                    vfade_in_us: r.get(19)?,
                    vfade_out_us: r.get(20)?,
                    strip_id: r.get::<_, Option<i64>>(21)?.map(StripId),
                    crop_l: r.get(22)?,
                    crop_r: r.get(23)?,
                    crop_t: r.get(24)?,
                    crop_b: r.get(25)?,
                    nudge_x: r.get(26)?,
                    nudge_y: r.get(27)?,
                    rotate: r.get(28)?,
                    fit_mode: parse_fit_mode(&r.get::<_, String>(29)?),
                    scale: r.get(30)?,
                    tx: r.get(31)?,
                    ty: r.get(32)?,
                    grade_id: r.get::<_, Option<i64>>(33)?.map(GradeId),
                    graphic_id: r.get::<_, Option<i64>>(34)?.map(GraphicId),
                },
                track_id: r.get(1)?,
                timeline_start_us: r.get(4)?,
                anchor: anchor_id.map(|id| (ClipId(id), anchor_off.unwrap_or(0))),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    for row in rows {
        let free = |row: &ClipRow| FreeClip {
            clip: row.clip.clone(),
            timeline_start_us: row.timeline_start_us.unwrap_or(0),
            anchor: row.anchor,
        };
        match row.track_id {
            1 => project.timeline.v1.push(row.clip),
            2 => project.timeline.v2.push(free(&row)),
            3 => project.timeline.a1.push(free(&row)),
            4 => project.timeline.a2.push(free(&row)),
            5 => project.timeline.g.push(free(&row)),
            _ => {}
        }
    }

    let mut stmt = conn.prepare("SELECT id, time_us, name, color FROM markers ORDER BY time_us")?;
    project.markers = stmt
        .query_map([], |r| {
            Ok(Marker {
                id: MarkerId(r.get(0)?),
                time_us: r.get(1)?,
                name: r.get(2)?,
                color: r.get(3)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;

    // Keep the id allocator ahead of everything loaded.
    let max_id: i64 = [
        project.media.iter().map(|m| m.id.0).max().unwrap_or(0),
        project
            .timeline
            .v1
            .iter()
            .map(|c| c.id.0)
            .max()
            .unwrap_or(0),
        project
            .timeline
            .v2
            .iter()
            .chain(&project.timeline.g)
            .chain(&project.timeline.a1)
            .chain(&project.timeline.a2)
            .map(|c| c.clip.id.0)
            .max()
            .unwrap_or(0),
        project.strips.iter().map(|s| s.id.0).max().unwrap_or(0),
        project.grades.iter().map(|g| g.id.0).max().unwrap_or(0),
        project.graphics.iter().map(|g| g.id.0).max().unwrap_or(0),
        project.markers.iter().map(|m| m.id.0).max().unwrap_or(0),
    ]
    .into_iter()
    .max()
    .unwrap_or(0);
    project.bump_id_counter(max_id);

    Ok(project)
}

/// Small adapter so the media `kind` CHECK column parses through one place.
struct MediaKindSql(String);
impl From<MediaKindSql> for crate::model::MediaKind {
    fn from(v: MediaKindSql) -> Self {
        crate::model::MediaKind::parse(&v.0).unwrap_or(crate::model::MediaKind::Video)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MediaKind, US_PER_SEC};

    fn tmp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dv-core-db-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).expect("mkdir");
        d
    }

    fn sample_project() -> Project {
        let mut p = Project::new("sample", 1000);
        let mid = MediaId(p.alloc_id());
        p.media.push(Media {
            id: mid,
            path: "/nonexistent/a.mp4".into(),
            hash: "h1".into(),
            kind: MediaKind::Video,
            duration_us: Some(60 * US_PER_SEC),
            video_codec: Some("h264".into()),
            audio_codec: Some("aac".into()),
            width: Some(1920),
            height: Some(1080),
            fps_num: Some(30000),
            fps_den: Some(1001),
            added_at: 1000,
            offline: false,
        });
        let c1 = Clip::new(ClipId(p.alloc_id()), Some(mid), 0, 10 * US_PER_SEC);
        let c1_id = c1.id;
        let mut c2 = Clip::new(
            ClipId(p.alloc_id()),
            Some(mid),
            15 * US_PER_SEC,
            20 * US_PER_SEC,
        );
        c2.label = Some("second".into());
        c2.color = Some(3);
        p.timeline.v1.push(c1);
        p.timeline.v1.push(c2);
        let v2_id = ClipId(p.alloc_id());
        p.timeline.v2.push(FreeClip {
            clip: Clip::new(v2_id, Some(mid), 0, 3 * US_PER_SEC),
            timeline_start_us: 2 * US_PER_SEC,
            anchor: Some((c1_id, 2 * US_PER_SEC)),
        });
        let marker_id = MarkerId(p.alloc_id());
        p.markers.push(Marker {
            id: marker_id,
            time_us: 5 * US_PER_SEC,
            name: "intro".into(),
            color: Some(1),
        });
        p.tracks.a2.duck = true;
        p.meta.range_in_us = Some(US_PER_SEC);
        p
    }

    /// `sample_project` plus §17 graphics: a V1 title card and a G-track
    /// overlay anchored under the first V1 clip.
    fn graphic_project() -> Project {
        use crate::graphic::{ElementKind, GraphicDoc, GraphicElement, TextBlock};
        let mut p = sample_project();
        let anchor_id = p.timeline.v1[0].id;
        let mut doc = GraphicDoc {
            background: Some([0.0, 0.0, 0.0, 1.0]),
            ..GraphicDoc::default()
        };
        doc.elements.push(GraphicElement {
            kind: ElementKind::Text(TextBlock {
                content: "Title card".into(),
                ..TextBlock::default()
            }),
            ..GraphicElement::default()
        });
        let card_gid = GraphicId(p.alloc_id());
        p.graphics.push(Graphic::new(card_gid, &doc));
        let mut card = Clip::new(ClipId(p.alloc_id()), None, 0, 5 * US_PER_SEC);
        card.graphic_id = Some(card_gid);
        p.timeline.v1.insert(0, card);

        let mut lower = GraphicDoc::default();
        lower.elements.push(GraphicElement {
            anchor: 2,
            kind: ElementKind::Text(TextBlock {
                content: "Lower third".into(),
                ..TextBlock::default()
            }),
            ..GraphicElement::default()
        });
        let lower_gid = GraphicId(p.alloc_id());
        p.graphics.push(Graphic::new(lower_gid, &lower));
        let mut overlay = Clip::new(ClipId(p.alloc_id()), None, 0, 3 * US_PER_SEC);
        overlay.graphic_id = Some(lower_gid);
        p.timeline.g.push(FreeClip {
            clip: overlay,
            timeline_start_us: 6 * US_PER_SEC,
            anchor: Some((anchor_id, US_PER_SEC)),
        });
        p.tracks.g.muted = true; // uniform track row must persist too
        p
    }

    fn assert_model_eq(a: &Project, b: &Project) {
        // `offline` is runtime-only; normalize before comparing.
        let mut a = a.clone();
        let mut b = b.clone();
        for m in a.media.iter_mut().chain(b.media.iter_mut()) {
            m.offline = false;
        }
        assert_eq!(a, b);
    }

    #[test]
    fn create_save_open_roundtrip() {
        let dir = tmp_dir("roundtrip");
        let path = dir.join("p.dv");
        let p = sample_project();
        let mut db = ProjectDb::create(&path, &p).expect("create");

        // Mutate + save (autosave flush shape). Removing the V1 clip leaves
        // the V2 clip's anchor dangling — the writer serializes that as
        // timeline-anchored at the resolved position (§6.3 fallback).
        let mut p2 = p.clone();
        p2.meta.name = "renamed".into();
        p2.timeline.v1.remove(0);
        db.save(&p2).expect("save");
        drop(db);

        let mut expected = p2.clone();
        expected.timeline.v2[0].anchor = None;

        match ProjectDb::open(&path).expect("open") {
            Opened::Clean(_, loaded) => {
                assert_model_eq(&loaded, &expected);
                assert!(loaded.media[0].offline, "missing file marked offline");
            }
            Opened::Salvaged { .. } => panic!("clean file must not salvage"),
        }
    }

    #[test]
    fn intact_anchor_roundtrips() {
        let dir = tmp_dir("anchor");
        let path = dir.join("p.dv");
        let p = sample_project();
        drop(ProjectDb::create(&path, &p).expect("create"));
        match ProjectDb::open(&path).expect("open") {
            Opened::Clean(_, loaded) => {
                assert_model_eq(&loaded, &p);
                assert_eq!(
                    loaded.timeline.v2[0].anchor, p.timeline.v2[0].anchor,
                    "live anchor must survive persistence"
                );
            }
            Opened::Salvaged { .. } => panic!("clean file must not salvage"),
        }
    }

    #[test]
    fn graphics_and_g_track_roundtrip() {
        let dir = tmp_dir("graphics");
        let path = dir.join("p.dv");
        let p = graphic_project();
        drop(ProjectDb::create(&path, &p).expect("create"));
        match ProjectDb::open(&path).expect("open") {
            Opened::Clean(_, loaded) => {
                assert_model_eq(&loaded, &p);
                assert_eq!(loaded.graphics.len(), 2);
                assert_eq!(loaded.timeline.g.len(), 1);
                assert_eq!(loaded.timeline.g[0].anchor, p.timeline.g[0].anchor);
                assert!(loaded.tracks.g.muted, "'g' track row persists");
                assert_eq!(
                    loaded.timeline.v1[0].graphic_id,
                    p.timeline.v1[0].graphic_id
                );
                assert!(loaded.timeline.v1[0].is_graphic());
                assert!(!loaded.timeline.v1[0].is_gap());
                // The allocator clears every loaded id, graphics included.
                let mut loaded = loaded;
                let max = loaded.graphics.iter().map(|g| g.id.0).max().unwrap_or(0);
                assert!(loaded.alloc_id() > max);
            }
            Opened::Salvaged { .. } => panic!("clean file must not salvage"),
        }
    }

    /// A v1 file (no `clips.graphic_id`) migrates forward on open, once.
    #[test]
    fn migrates_v1_file_to_v2() {
        let dir = tmp_dir("migrate-v1");
        let path = dir.join("p.dv");
        let p = sample_project();
        {
            let mut db = ProjectDb::create(&path, &p).expect("create");
            let tx = db.conn.transaction().expect("tx");
            // Rewind the file to exactly what v1 wrote.
            tx.execute_batch("ALTER TABLE clips DROP COLUMN graphic_id")
                .expect("drop column");
            set_meta(&tx, "schema_version", "1").expect("set");
            tx.commit().expect("commit");
        }
        match ProjectDb::open(&path).expect("open") {
            Opened::Clean(db, loaded) => {
                assert_model_eq(&loaded, &p);
                assert!(loaded.timeline.v1.iter().all(|c| c.graphic_id.is_none()));
                let v: String = db
                    .conn
                    .query_row(
                        "SELECT value FROM meta WHERE key='schema_version'",
                        [],
                        |r| r.get(0),
                    )
                    .expect("version");
                assert_eq!(v, SCHEMA_VERSION.to_string());
            }
            Opened::Salvaged { .. } => panic!("a v1 file must migrate, not salvage"),
        }
        // Reopening applies nothing further (the ALTER would fail if it ran
        // twice) and graphics now persist through the migrated file.
        let mut p2 = graphic_project();
        p2.meta.name = "after migration".into();
        match ProjectDb::open(&path).expect("reopen") {
            Opened::Clean(mut db, _) => {
                db.save(&p2).expect("save");
            }
            Opened::Salvaged { .. } => panic!("migrated file must reopen clean"),
        }
        match ProjectDb::open(&path).expect("reopen 2") {
            Opened::Clean(_, loaded) => assert_model_eq(&loaded, &p2),
            Opened::Salvaged { .. } => panic!("migrated file must reopen clean"),
        }
    }

    #[test]
    fn create_refuses_overwrite() {
        let dir = tmp_dir("no-overwrite");
        let path = dir.join("p.dv");
        std::fs::write(&path, b"precious").expect("write");
        match ProjectDb::create(&path, &sample_project()) {
            Err(DbError::AlreadyExists(_)) => {}
            other => panic!("expected AlreadyExists, got {:?}", other.err()),
        }
    }

    #[test]
    fn newer_schema_refused() {
        let dir = tmp_dir("too-new");
        let path = dir.join("p.dv");
        {
            let mut db = ProjectDb::create(&path, &sample_project()).expect("create");
            let tx = db.conn.transaction().expect("tx");
            set_meta(&tx, "schema_version", &(SCHEMA_VERSION + 5).to_string()).expect("set");
            tx.commit().expect("commit");
        }
        match ProjectDb::open(&path) {
            Err(DbError::TooNew(v)) => assert_eq!(v, SCHEMA_VERSION + 5),
            other => panic!("expected TooNew, got {:?}", other.err()),
        }
    }

    #[test]
    fn non_project_sqlite_refused() {
        let dir = tmp_dir("not-project");
        let path = dir.join("random.dv");
        {
            let conn = Connection::open(&path).expect("open");
            conn.execute("CREATE TABLE misc (x INTEGER)", [])
                .expect("ddl");
        }
        match ProjectDb::open(&path) {
            Err(DbError::NotAProject(_)) => {}
            other => panic!("expected NotAProject, got {:?}", other.err()),
        }
    }

    #[test]
    fn dv_file_is_self_contained() {
        let dir = tmp_dir("selfcontained");
        let path = dir.join("p.dv");
        let p = sample_project();
        let wal = PathBuf::from({
            let mut os = path.as_os_str().to_owned();
            os.push("-wal");
            os
        });
        {
            let mut db = ProjectDb::create(&path, &p).expect("create");
            db.save(&p).expect("save");
            let mode: String = db
                .conn
                .query_row("PRAGMA journal_mode", [], |r| r.get(0))
                .expect("mode");
            assert_eq!(mode, "delete");
            assert!(!wal.exists(), "no -wal sidecar while open");
        }
        assert!(!wal.exists(), "no -wal sidecar at rest");

        // A legacy WAL-mode file converts back on open: WAL checkpointed into
        // the main file, sidecars removed. Suppress the close-time checkpoint
        // so the fixture actually leaves a -wal behind, like a crashed app.
        {
            let conn = Connection::open(&path).expect("open raw");
            conn.set_db_config(
                rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
                true,
            )
            .expect("no ckpt on close");
            conn.pragma_update(None, "journal_mode", "wal")
                .expect("wal");
            conn.execute(
                "INSERT INTO markers (time_us, name, color) VALUES (1, 'm', 0)",
                [],
            )
            .expect("write");
        }
        assert!(wal.exists(), "legacy fixture left a -wal behind");
        match ProjectDb::open(&path).expect("reopen") {
            Opened::Clean(..) => {}
            _ => panic!("expected clean open of legacy WAL file"),
        }
        assert!(!wal.exists(), "reopen converted legacy WAL file");
    }

    #[test]
    fn salvage_recovers_from_history() {
        let dir = tmp_dir("salvage");
        let path = dir.join("p.dv");
        let p = sample_project();
        {
            let mut db = ProjectDb::create(&path, &p).expect("create");
            db.append_history("edit burst", &p, 2000).expect("history");
        }
        // Corrupt the middle of the main file (past the header) — enough to
        // fail quick_check but leave some history pages readable. If SQLite
        // can't read anything at all, salvage correctly reports Unsalvageable;
        // this fixture keeps the file mostly intact.
        {
            use std::io::{Seek, SeekFrom, Write};
            let mut f = std::fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .expect("open raw");
            let len = f.metadata().expect("meta").len();
            f.seek(SeekFrom::Start(len / 2)).expect("seek");
            f.write_all(&[0xFF; 512]).expect("scribble");
        }
        match ProjectDb::open(&path) {
            Ok(Opened::Salvaged {
                project,
                damaged_path,
                ..
            }) => {
                assert_model_eq(&project, &p);
                assert!(
                    damaged_path.exists(),
                    "damaged original kept for inspection"
                );
                // Fresh file opens clean afterwards.
                match ProjectDb::open(&path).expect("reopen") {
                    Opened::Clean(_, loaded) => assert_model_eq(&loaded, &p),
                    _ => panic!("salvaged file must reopen clean"),
                }
            }
            Ok(Opened::Clean(_, loaded)) => {
                // SQLite occasionally survives a mid-file scribble (page
                // layout luck). The roundtrip must still hold.
                assert_model_eq(&loaded, &p);
            }
            Err(e) => panic!("open after corruption failed outright: {e}"),
        }
    }

    #[test]
    fn ui_state_is_isolated_from_saves() {
        let dir = tmp_dir("ui-state");
        let path = dir.join("p.dv");
        let p = sample_project();
        let mut db = ProjectDb::create(&path, &p).expect("create");
        db.ui_state_set("playhead", "123456").expect("set");
        db.save(&p).expect("save");
        assert_eq!(db.ui_state_get("playhead").as_deref(), Some("123456"));
        assert_eq!(db.ui_state_get("missing"), None);
    }

    #[test]
    fn history_replace_head() {
        let dir = tmp_dir("history");
        let path = dir.join("p.dv");
        let p = sample_project();
        let mut db = ProjectDb::create(&path, &p).expect("create");
        db.append_history("Split a.mp4", &p, 2000).expect("append");
        db.replace_history_head("Split a.mp4 ×2", &p, 2001)
            .expect("replace");
        let entries = db.history_entries().expect("entries");
        assert_eq!(entries.len(), 2, "created + one burst entry");
        assert_eq!(entries[0].2, "Split a.mp4 ×2");
    }

    #[test]
    fn history_head_reports_newest() {
        let dir = tmp_dir("history-head");
        let path = dir.join("p.dv");
        let p = sample_project();
        let mut db = ProjectDb::create(&path, &p).expect("create");
        db.append_history("Split a.mp4", &p, 2000).expect("append");
        let head = db.history_head().expect("head").expect("some");
        assert_eq!(head.1, 2000);
        assert_eq!(head.2, "Split a.mp4");
    }

    #[test]
    fn history_snapshot_round_trips() {
        let dir = tmp_dir("history-snap");
        let path = dir.join("p.dv");
        let p = sample_project();
        let mut db = ProjectDb::create(&path, &p).expect("create");
        // A distinct model in the row so we know the fetch decodes *this* blob.
        let mut past = p.clone();
        past.meta.name = "as it was".into();
        db.append_history("Trim clip", &past, 2000).expect("append");
        let id = db.history_entries().expect("entries")[0].0;
        let fetched = db.history_snapshot(id).expect("snapshot");
        assert_model_eq(&fetched, &past);
    }

    #[test]
    fn thin_history_buckets_and_keeps_newest() {
        let dir = tmp_dir("thin");
        let path = dir.join("p.dv");
        let p = sample_project();
        let mut db = ProjectDb::create(&path, &p).expect("create");
        // The "Project created" row from create() is ancient relative to `now`;
        // drop it so the fixture below is the whole history.
        db.conn.execute("DELETE FROM history", []).expect("clear");

        let now = 100_000;
        // (created_at, should_survive) — labels double as identity in asserts.
        let fixture: &[(i64, bool)] = &[
            (99_900, true),  // last 15 min — always kept (also newest overall)
            (99_500, true),  // last 15 min — always kept
            (98_970, true),  // last hour, minute bucket 1649: newest of pair
            (98_940, false), // last hour, minute bucket 1649: older of pair → gone
            (98_000, true),  // last hour, minute bucket 1633: alone
            (90_100, true),  // last day, 10-min bucket 150: newest of pair
            (90_000, false), // last day, 10-min bucket 150: older of pair → gone
            (10_000, true),  // beyond a day, hour bucket 2: newest of pair
            (9_000, false),  // beyond a day, hour bucket 2: older of pair → gone
            (3_000, true),   // beyond a day, hour bucket 0: alone
        ];
        for &(created_at, _) in fixture {
            db.append_history(&format!("row {created_at}"), &p, created_at)
                .expect("append");
        }
        db.thin_history(now).expect("thin");

        let survivors: HashSet<i64> = db
            .history_entries()
            .expect("entries")
            .iter()
            .map(|(_, created_at, _)| *created_at)
            .collect();
        for &(created_at, should_survive) in fixture {
            assert_eq!(
                survivors.contains(&created_at),
                should_survive,
                "created_at {created_at} survival"
            );
        }
    }

    #[test]
    fn thin_history_hard_cap() {
        let dir = tmp_dir("thin-cap");
        let path = dir.join("p.dv");
        let p = sample_project();
        let mut db = ProjectDb::create(&path, &p).expect("create");
        db.conn.execute("DELETE FROM history", []).expect("clear");

        let now = 100_000;
        // All within the last 15 min (age 0) so bucket thinning keeps them all;
        // only the hard cap of 1,000 removes any.
        for _ in 0..1_010 {
            db.append_history("recent", &p, now).expect("append");
        }
        let newest_id = db.history_head().expect("head").expect("some").0;
        db.thin_history(now).expect("thin");
        let entries = db.history_entries().expect("entries");
        assert_eq!(entries.len(), 1000, "hard-capped to 1,000 rows");
        assert!(
            entries.iter().any(|(id, _, _)| *id == newest_id),
            "newest row survives the cap"
        );
    }

    // ---- export_jobs (§10.1) ----

    #[test]
    fn export_job_insert_load_roundtrip() {
        let dir = tmp_dir("export-roundtrip");
        let path = dir.join("p.dv");
        let db = ProjectDb::create(&path, &sample_project()).expect("create");

        let dest = PathBuf::from("/home/brian/My Videos/talk final-v1.mp4");
        let id = db
            .insert_export_job(
                4242,
                "youtube-1080p",
                &dest,
                Some(r#"{"in_us":1000,"out_us":9000}"#),
                b"snapshot-bytes",
            )
            .expect("insert");

        let jobs = db.load_export_jobs().expect("load");
        assert_eq!(jobs.len(), 1);
        let job = &jobs[0];
        assert_eq!(job.id, id);
        assert_eq!(job.created_at, 4242);
        assert_eq!(job.finished_at, None);
        assert_eq!(job.state, ExportJobState::Queued);
        assert_eq!(job.preset, "youtube-1080p");
        assert_eq!(job.dest_path, dest, "path with spaces survives");
        assert_eq!(
            job.range_json.as_deref(),
            Some(r#"{"in_us":1000,"out_us":9000}"#)
        );
        assert_eq!(job.error, None);
    }

    #[test]
    fn export_job_snapshot_round_trips() {
        let dir = tmp_dir("export-snap");
        let path = dir.join("p.dv");
        let db = ProjectDb::create(&path, &sample_project()).expect("create");

        let blob: &[u8] = &[0x00, 0xFF, 0x01, 0x7F, 0x80, 0xAB];
        let id = db
            .insert_export_job(1, "preset", Path::new("/out.mp4"), None, blob)
            .expect("insert");
        assert_eq!(
            db.export_job_snapshot(id).expect("snapshot").as_deref(),
            Some(blob)
        );
        assert_eq!(db.export_job_snapshot(9999).expect("missing"), None);
    }

    #[test]
    fn export_job_state_transitions() {
        let dir = tmp_dir("export-state");
        let path = dir.join("p.dv");
        let db = ProjectDb::create(&path, &sample_project()).expect("create");

        // queued → running → done, with finished_at on completion.
        let done_id = db
            .insert_export_job(10, "p", Path::new("/a.mp4"), None, b"s")
            .expect("insert");
        db.set_export_job_state(done_id, ExportJobState::Running, None, None)
            .expect("running");
        db.set_export_job_state(done_id, ExportJobState::Done, Some(99), None)
            .expect("done");

        // a second job that fails, carrying error text.
        let fail_id = db
            .insert_export_job(20, "p", Path::new("/b.mp4"), None, b"s")
            .expect("insert");
        db.set_export_job_state(
            fail_id,
            ExportJobState::Failed,
            Some(50),
            Some("ffmpeg: no such encoder"),
        )
        .expect("failed");

        let jobs = db.load_export_jobs().expect("load");
        let done = jobs.iter().find(|j| j.id == done_id).expect("done row");
        assert_eq!(done.state, ExportJobState::Done);
        assert_eq!(done.finished_at, Some(99));
        assert_eq!(done.error, None);
        let fail = jobs.iter().find(|j| j.id == fail_id).expect("fail row");
        assert_eq!(fail.state, ExportJobState::Failed);
        assert_eq!(fail.finished_at, Some(50));
        assert_eq!(fail.error.as_deref(), Some("ffmpeg: no such encoder"));
    }

    #[test]
    fn export_job_delete_removes_row() {
        let dir = tmp_dir("export-delete");
        let path = dir.join("p.dv");
        let db = ProjectDb::create(&path, &sample_project()).expect("create");

        let id = db
            .insert_export_job(1, "p", Path::new("/a.mp4"), None, b"s")
            .expect("insert");
        db.delete_export_job(id).expect("delete");
        assert!(db.load_export_jobs().expect("load").is_empty());
    }

    #[test]
    fn export_jobs_load_in_creation_order() {
        let dir = tmp_dir("export-order");
        let path = dir.join("p.dv");
        let db = ProjectDb::create(&path, &sample_project()).expect("create");

        // Insert with created_at descending; load order must follow id (creation),
        // not created_at.
        for (i, created_at) in [300, 200, 100].into_iter().enumerate() {
            db.insert_export_job(
                created_at,
                &format!("p{i}"),
                Path::new("/x.mp4"),
                None,
                b"s",
            )
            .expect("insert");
        }
        let presets: Vec<String> = db
            .load_export_jobs()
            .expect("load")
            .into_iter()
            .map(|j| j.preset)
            .collect();
        assert_eq!(presets, vec!["p0", "p1", "p2"]);
    }

    #[test]
    fn export_job_writes_never_add_history() {
        let dir = tmp_dir("export-no-history");
        let path = dir.join("p.dv");
        let db = ProjectDb::create(&path, &sample_project()).expect("create");

        let before = db.history_entries().expect("entries").len();
        let id = db
            .insert_export_job(1, "p", Path::new("/a.mp4"), None, b"s")
            .expect("insert");
        db.set_export_job_state(id, ExportJobState::Running, None, None)
            .expect("running");
        db.set_export_job_state(id, ExportJobState::Done, Some(2), None)
            .expect("done");
        db.delete_export_job(id).expect("delete");
        assert_eq!(
            db.history_entries().expect("entries").len(),
            before,
            "export bookkeeping must not create history rows"
        );
    }
}
