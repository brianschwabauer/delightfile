//! Command pattern + undo stack (§6.5). Every mutation of the project model
//! is an `EditCommand { apply, revert }` pushed onto the session undo stack.
//! Commands coalesce when the same parameter is nudged repeatedly within
//! 500 ms. Timestamps come from the caller (milliseconds, any monotonic
//! origin) so the stack is deterministic under test.

use std::any::Any;

use crate::model::{Media, MediaId, Project};

pub const COALESCE_WINDOW_MS: u64 = 500;

pub trait EditCommand: Any + Send {
    /// Human label — feeds status-bar feedback and history entry labels (§6.6).
    fn label(&self) -> String;

    fn apply(&mut self, project: &mut Project);
    fn revert(&mut self, project: &mut Project);

    /// Merge `next` (already applied to the model) into `self`, so one undo
    /// reverts the whole burst. Return `false` if the commands don't coalesce;
    /// the stack then pushes `next` as its own entry.
    fn coalesce(&mut self, _next: &dyn EditCommand) -> bool {
        false
    }

    /// How long after this command a coalescable successor may still merge.
    /// Default 500 ms (§6.5); modal edit sessions (§6.4 frame mode) return
    /// `u64::MAX` so the whole session stays one undo step — the session end
    /// calls [`UndoStack::seal`] to close the burst instead.
    fn coalesce_window_ms(&self) -> u64 {
        COALESCE_WINDOW_MS
    }

    fn as_any(&self) -> &dyn Any;
}

struct Entry {
    cmd: Box<dyn EditCommand>,
    at_ms: u64,
    /// `true` once sealed (§6.4 modal session end): no later command may
    /// coalesce into this entry, whatever its window.
    sealed: bool,
}

/// Session undo stack: in-memory, unbounded, cleared on project open (§6.5).
#[derive(Default)]
pub struct UndoStack {
    undo: Vec<Entry>,
    redo: Vec<Entry>,
}

impl UndoStack {
    pub fn new() -> UndoStack {
        UndoStack::default()
    }

    /// Apply `cmd` to the project and record it. Returns the label to show.
    pub fn execute(
        &mut self,
        project: &mut Project,
        mut cmd: Box<dyn EditCommand>,
        now_ms: u64,
    ) -> String {
        cmd.apply(project);
        let label = cmd.label();
        self.redo.clear();
        if let Some(top) = self.undo.last_mut() {
            if !top.sealed
                && now_ms.saturating_sub(top.at_ms) <= top.cmd.coalesce_window_ms()
                && top.cmd.coalesce(&*cmd)
            {
                top.at_ms = now_ms;
                return top.cmd.label();
            }
        }
        self.undo.push(Entry {
            cmd,
            at_ms: now_ms,
            sealed: false,
        });
        label
    }

    /// Close the current coalescing burst: nothing later merges into the top
    /// entry. Called when a modal edit session ends (§6.4 frame mode), so a
    /// re-entered session starts a fresh undo step.
    pub fn seal(&mut self) {
        if let Some(top) = self.undo.last_mut() {
            top.sealed = true;
        }
    }

    /// Undo the newest command. Returns its label, or `None` if empty.
    pub fn undo(&mut self, project: &mut Project) -> Option<String> {
        let mut entry = self.undo.pop()?;
        entry.cmd.revert(project);
        let label = entry.cmd.label();
        self.redo.push(entry);
        Some(label)
    }

    /// Redo the newest undone command. Returns its label, or `None` if empty.
    pub fn redo(&mut self, project: &mut Project) -> Option<String> {
        let mut entry = self.redo.pop()?;
        entry.cmd.apply(project);
        let label = entry.cmd.label();
        self.undo.push(entry);
        Some(label)
    }

    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Cleared on project open (§6.5).
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
    }

    /// Label of the newest undoable command (drives history labels, §6.6).
    pub fn last_label(&self) -> Option<String> {
        self.undo.last().map(|e| e.cmd.label())
    }
}

// ---------------------------------------------------------------------------
// M1 commands. Editing commands (split/trim/ripple…) arrive with M3.
// ---------------------------------------------------------------------------

/// Import one or more media files (§9). One command per import batch, so one
/// undo removes the whole batch.
pub struct ImportMedia {
    pub media: Vec<Media>,
}

impl EditCommand for ImportMedia {
    fn label(&self) -> String {
        if self.media.len() == 1 {
            format!("Import {}", file_label(&self.media[0]))
        } else {
            format!("Import {} files", self.media.len())
        }
    }

    fn apply(&mut self, project: &mut Project) {
        for m in &self.media {
            project.bump_id_counter(m.id.0);
            project.media.push(m.clone());
        }
    }

    fn revert(&mut self, project: &mut Project) {
        let ids: Vec<MediaId> = self.media.iter().map(|m| m.id).collect();
        project.media.retain(|m| !ids.contains(&m.id));
    }

    fn coalesce(&mut self, next: &dyn EditCommand) -> bool {
        // Rapid-fire imports (multi-select in the browser) become one batch.
        if let Some(other) = next.as_any().downcast_ref::<ImportMedia>() {
            self.media.extend(other.media.iter().cloned());
            true
        } else {
            false
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Point an offline media row at a new path (§9 relink; undoable).
pub struct RelinkMedia {
    pub id: MediaId,
    pub old_path: std::path::PathBuf,
    pub new_path: std::path::PathBuf,
}

impl EditCommand for RelinkMedia {
    fn label(&self) -> String {
        format!(
            "Relink {}",
            self.new_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| self.new_path.display().to_string())
        )
    }

    fn apply(&mut self, project: &mut Project) {
        if let Some(m) = project.media_by_id_mut(self.id) {
            m.path = self.new_path.clone();
            m.offline = false;
        }
    }

    fn revert(&mut self, project: &mut Project) {
        if let Some(m) = project.media_by_id_mut(self.id) {
            m.path = self.old_path.clone();
            m.offline = !m.path.exists();
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Restore a history snapshot (§6.6): replaces the whole project model as a
/// regular undoable command. History itself stays append-only — the flush
/// after this records "Restored version from …" as a new head entry.
pub struct RestoreProject {
    pub label: String,
    pub before: Box<Project>,
    pub after: Box<Project>,
}

impl EditCommand for RestoreProject {
    fn label(&self) -> String {
        self.label.clone()
    }

    fn apply(&mut self, project: &mut Project) {
        let floor = project.peek_id_counter();
        *project = (*self.after).clone();
        // Ids allocated since the snapshot must stay burned (§6.5).
        project.bump_id_counter(floor);
    }

    fn revert(&mut self, project: &mut Project) {
        let floor = project.peek_id_counter();
        *project = (*self.before).clone();
        project.bump_id_counter(floor);
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

fn file_label(m: &Media) -> String {
    m.path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| m.path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MediaKind, US_PER_SEC};

    fn media(id: i64, name: &str) -> Media {
        Media {
            id: MediaId(id),
            path: std::path::PathBuf::from(format!("/tmp/{name}")),
            hash: format!("hash-{id}"),
            kind: MediaKind::Video,
            duration_us: Some(10 * US_PER_SEC),
            video_codec: Some("h264".into()),
            audio_codec: Some("aac".into()),
            width: Some(1920),
            height: Some(1080),
            fps_num: Some(30),
            fps_den: Some(1),
            added_at: 0,
            offline: false,
        }
    }

    #[test]
    fn apply_revert_apply_idempotence() {
        let mut p = Project::new("t", 0);
        let mut stack = UndoStack::new();
        stack.execute(
            &mut p,
            Box::new(ImportMedia {
                media: vec![media(1, "a.mp4")],
            }),
            0,
        );
        let after_apply = p.clone();
        stack.undo(&mut p).expect("undo");
        assert!(p.media.is_empty());
        stack.redo(&mut p).expect("redo");
        assert_eq!(p, after_apply, "apply → revert → apply must round-trip");
    }

    #[test]
    fn coalesce_within_window_only() {
        let mut p = Project::new("t", 0);
        let mut stack = UndoStack::new();
        stack.execute(
            &mut p,
            Box::new(ImportMedia {
                media: vec![media(1, "a.mp4")],
            }),
            0,
        );
        stack.execute(
            &mut p,
            Box::new(ImportMedia {
                media: vec![media(2, "b.mp4")],
            }),
            300, // within 500 ms — coalesces
        );
        assert_eq!(p.media.len(), 2);
        stack.undo(&mut p).expect("undo");
        assert!(p.media.is_empty(), "coalesced burst undoes as one step");
        stack.redo(&mut p).expect("redo");

        stack.execute(
            &mut p,
            Box::new(ImportMedia {
                media: vec![media(3, "c.mp4")],
            }),
            2_000, // outside the window — separate entry
        );
        stack.undo(&mut p).expect("undo");
        assert_eq!(p.media.len(), 2, "late command is its own undo step");
    }

    #[test]
    fn redo_cleared_by_new_command() {
        let mut p = Project::new("t", 0);
        let mut stack = UndoStack::new();
        stack.execute(
            &mut p,
            Box::new(ImportMedia {
                media: vec![media(1, "a.mp4")],
            }),
            0,
        );
        stack.undo(&mut p).expect("undo");
        assert!(stack.can_redo());
        stack.execute(
            &mut p,
            Box::new(ImportMedia {
                media: vec![media(2, "b.mp4")],
            }),
            10_000,
        );
        assert!(!stack.can_redo());
    }

    #[test]
    fn relink_round_trips_and_labels() {
        let mut p = Project::new("t", 0);
        let mut m = media(1, "a.mp4");
        m.offline = true;
        p.media.push(m);
        let mut stack = UndoStack::new();
        let label = stack.execute(
            &mut p,
            Box::new(RelinkMedia {
                id: MediaId(1),
                old_path: "/tmp/a.mp4".into(),
                new_path: "/new/a.mp4".into(),
            }),
            0,
        );
        assert_eq!(label, "Relink a.mp4");
        assert_eq!(p.media[0].path, std::path::PathBuf::from("/new/a.mp4"));
        assert!(!p.media[0].offline);
        stack.undo(&mut p).expect("undo");
        assert_eq!(p.media[0].path, std::path::PathBuf::from("/tmp/a.mp4"));
    }
}
