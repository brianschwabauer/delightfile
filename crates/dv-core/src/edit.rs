//! Timeline editing (§6.3, §6.4). Every operation is a plain function that
//! mutates the `Project`; `perform` wraps one (or a composition) in a generic
//! snapshot-based `EditCommand`, so apply → revert → apply is idempotent by
//! construction and clip ids never change across undo/redo. The semantic
//! rules that live here and nowhere else: anchoring travel/re-anchoring
//! (§6.3), the crossfade lifecycle ("a crossfade belongs to its cut, and
//! dies with it", §5), and split fade distribution.

use std::any::Any;

use crate::command::{EditCommand, UndoStack};
use crate::graphic::{Graphic, GraphicDoc};
use crate::model::{
    ChannelMode, Clip, ClipId, FitMode, FreeClip, Grade, GradeId, GradeParams, GraphicId, Marker,
    MarkerId, Media, MediaId, Project, ProjectMeta, Strip, StripId, StripParams, TimeUs, Timeline,
    TrackKind, TrackSettings, Tracks, US_PER_SEC,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditError {
    /// Clip id not found on the timeline.
    NoSuchClip,
    /// The operation needs the playhead inside the clip (split, trim-to).
    OutsideClip,
    /// Only V1 supports this operation (lift, reorder) or only free tracks do.
    WrongTrack,
    /// A free-track move/insert would overlap a neighbor (§6.3).
    Collision,
    /// Nothing to do (already at the end, empty clipboard, zero delta…).
    Nothing,
    /// Media row missing for an operation that needs it.
    NoSuchMedia,
}

impl std::fmt::Display for EditError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let s = match self {
            EditError::NoSuchClip => "no clip selected",
            EditError::OutsideClip => "playhead is outside the clip",
            EditError::WrongTrack => "not possible on this track",
            EditError::Collision => "no room on the track",
            EditError::Nothing => "nothing to do",
            EditError::NoSuchMedia => "media not found",
        };
        f.write_str(s)
    }
}

// ---------------------------------------------------------------------------
// Snapshot command (§6.5)
// ---------------------------------------------------------------------------

/// Everything a timeline edit can touch. Media has its own commands;
/// snapshotting just this keeps undo entries tiny. `strips` and `tracks` join
/// as of M5 so strip-linking (§5) and per-track mixing (§5) undo through the
/// same `perform` path; `grades` joins with color grading (§15).
#[derive(Clone, PartialEq)]
struct EditState {
    timeline: Timeline,
    markers: Vec<Marker>,
    meta: ProjectMeta,
    strips: Vec<Strip>,
    grades: Vec<Grade>,
    /// §17 graphic documents — without these, editing a graphic wouldn't undo.
    graphics: Vec<Graphic>,
    tracks: Tracks,
    id_counter_snapshot: i64,
}

fn capture(p: &Project) -> EditState {
    EditState {
        timeline: p.timeline.clone(),
        markers: p.markers.clone(),
        meta: p.meta.clone(),
        strips: p.strips.clone(),
        grades: p.grades.clone(),
        graphics: p.graphics.clone(),
        tracks: p.tracks.clone(),
        id_counter_snapshot: p.peek_id_counter(),
    }
}

fn restore(p: &mut Project, s: &EditState) {
    p.timeline = s.timeline.clone();
    p.markers = s.markers.clone();
    p.meta = s.meta.clone();
    p.strips = s.strips.clone();
    p.grades = s.grades.clone();
    p.graphics = s.graphics.clone();
    p.tracks = s.tracks.clone();
    // Never rewind the allocator — ids must stay unique project-wide even
    // after undoing the edit that allocated them.
    p.bump_id_counter(s.id_counter_snapshot);
}

/// Coalescing identity (§6.5): repeated nudges of the *same parameter on the
/// same clip* merge into one undo step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoalesceKey {
    /// Never coalesces.
    None,
    TrimIn(ClipId),
    TrimOut(ClipId),
    Slip(ClipId),
    Slide(ClipId),
    Gain(ClipId),
    Fade(ClipId, bool),
    /// Repeated nudges of one strip/inspector field (index) on one clip (§5).
    Field(ClipId, u8),
    /// Repeated nudges of one per-track setting (§5).
    Track(TrackKind, u8),
    /// Repeated nudges of one master-bus setting (§5).
    Master(u8),
    /// A frame-mode session on one clip (§6.4): the WHOLE session is one undo
    /// step, however long it lasts — unbounded window; the app seals the
    /// stack on mode exit.
    Framing(ClipId),
    /// Held fine-rotation taps (§6.4 `Alt+r`) — normal 500 ms window.
    Rotate(ClipId),
    /// Repeated speed taps (§6.4 `<`/`>`).
    Speed(ClipId),
    /// Video fade nudges (§6.4; `true` = fade-in edge).
    VideoFade(ClipId, bool),
    /// A graphic-editor session on one clip (§17.6): like `Framing`, the WHOLE
    /// session is one undo step — unbounded window, sealed by the app on
    /// editor exit.
    GraphicEdit(ClipId),
}

struct TimelineEdit {
    label: String,
    key: CoalesceKey,
    count: u32,
    before: EditState,
    after: EditState,
}

impl EditCommand for TimelineEdit {
    fn label(&self) -> String {
        if self.count > 1 {
            format!("{} ×{}", self.label, self.count)
        } else {
            self.label.clone()
        }
    }

    fn apply(&mut self, project: &mut Project) {
        restore(project, &self.after);
    }

    fn revert(&mut self, project: &mut Project) {
        restore(project, &self.before);
    }

    fn coalesce(&mut self, next: &dyn EditCommand) -> bool {
        let Some(other) = next.as_any().downcast_ref::<TimelineEdit>() else {
            return false;
        };
        if self.key == CoalesceKey::None || self.key != other.key {
            return false;
        }
        // Keep our `before`, take their `after`: one undo reverts the burst.
        self.after = other.after.clone();
        self.label = other.label.clone();
        self.count += other.count;
        true
    }

    fn coalesce_window_ms(&self) -> u64 {
        match self.key {
            // §6.4: a frame-mode session is one undo step however long it
            // runs; UndoStack::seal closes it on mode exit.
            // §17.6: likewise, one graphic-editor session = one undo step.
            CoalesceKey::Framing(_) | CoalesceKey::GraphicEdit(_) => u64::MAX,
            _ => crate::command::COALESCE_WINDOW_MS,
        }
    }

    fn as_any(&self) -> &dyn Any {
        self
    }
}

/// Run `f` on the project and record it as one undoable step. On error the
/// project is restored to its pre-`f` state, so a failed composite edit never
/// leaves partial mutations behind. Returns the (possibly coalesced) label.
pub fn perform<F>(
    project: &mut Project,
    undo: &mut UndoStack,
    now_ms: u64,
    key: CoalesceKey,
    f: F,
) -> Result<String, EditError>
where
    F: FnOnce(&mut Project) -> Result<String, EditError>,
{
    let before = capture(project);
    match f(project) {
        Ok(label) => {
            let after = capture(project);
            if after == before {
                return Err(EditError::Nothing);
            }
            let cmd = TimelineEdit {
                label,
                key,
                count: 1,
                before,
                after,
            };
            // The mutation is already applied; execute()'s re-apply restores
            // the identical `after` state — idempotent, not a double edit.
            Ok(undo.execute(project, Box::new(cmd), now_ms))
        }
        Err(e) => {
            restore(project, &before);
            Err(e)
        }
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// One project frame in µs (min clip duration, split/trim granularity floor).
pub fn frame_us(meta: &ProjectMeta) -> TimeUs {
    (US_PER_SEC * meta.fps_den as i64 / meta.fps_num.max(1) as i64).max(1)
}

fn clip_name(p: &Project, clip: &Clip) -> String {
    if let Some(label) = &clip.label {
        return label.clone();
    }
    if clip.is_graphic() {
        // §17.3: a graphic names itself after its first text block.
        return graphic_of(p, clip)
            .map(|doc| doc.title())
            .unwrap_or_else(|| "Graphic".to_string());
    }
    match clip.media_id.and_then(|id| p.media_by_id(id)) {
        Some(m) => m
            .path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| m.path.display().to_string()),
        None => "Gap".to_string(),
    }
}

/// Timeline delta → source-time delta under the clip's speed.
fn to_source(clip: &Clip, timeline_delta: TimeUs) -> TimeUs {
    ((timeline_delta as f64) * clip.speed).round() as TimeUs
}

/// Upper bound for `source_out_us` (images, graphics and gaps are unbounded —
/// §1, §17.2: a graphic has infinite source duration like a still).
fn source_cap(p: &Project, clip: &Clip) -> Option<TimeUs> {
    clip.media_id
        .and_then(|id| p.media_by_id(id))
        .and_then(|m| m.duration_us)
}

/// Resolved [start, end) of every clip on a free track, index-tagged.
fn free_spans(tl: &Timeline, kind: TrackKind) -> Vec<(usize, TimeUs, TimeUs)> {
    let track = tl.free_track(kind).expect("free track");
    let mut spans: Vec<(usize, TimeUs, TimeUs)> = track
        .iter()
        .enumerate()
        .map(|(i, fc)| {
            let s = tl.free_start_us(fc);
            (i, s, s + fc.clip.duration_us())
        })
        .collect();
    spans.sort_by_key(|&(_, s, _)| s);
    spans
}

/// The V1 clip covering absolute time `start`, as an `(anchor_id, offset)`
/// pair (§6.3 default anchoring). `None` when V1 is empty or `start` is at/past
/// the timeline end — the clip anchors to the timeline instead.
fn anchor_under(tl: &Timeline, start: TimeUs) -> Option<(ClipId, TimeUs)> {
    tl.v1_index_at(start).and_then(|i| {
        let cstart = tl.v1_start_us(i);
        let c = &tl.v1[i];
        (start < cstart + c.duration_us()).then(|| (c.id, start - cstart))
    })
}

/// First absolute start ≥ `want` at which a `dur`-long clip fits between the
/// resolved `spans` of a free track (§6.3 shift-right-on-collision).
fn first_free_start(spans: &[(usize, TimeUs, TimeUs)], want: TimeUs, dur: TimeUs) -> TimeUs {
    let mut start = want;
    loop {
        match spans
            .iter()
            .find(|&&(_, s, e)| s < start + dur && e > start)
        {
            Some(&(_, _, e)) => start = e,
            None => break start,
        }
    }
}

/// The `TrackSettings` for `kind` (§5 per-track mixing).
fn track_mut(tracks: &mut Tracks, kind: TrackKind) -> &mut TrackSettings {
    match kind {
        TrackKind::V1 => &mut tracks.v1,
        TrackKind::V2 => &mut tracks.v2,
        TrackKind::G => &mut tracks.g,
        TrackKind::A1 => &mut tracks.a1,
        TrackKind::A2 => &mut tracks.a2,
    }
}

/// Largest crossfade (timeline µs) the cut between `earlier` and `later` can
/// hold (§5). Each side spends `xfade/2` of handle material: the earlier clip
/// needs source beyond its `source_out` (unbounded for images), the later clip
/// needs source before its `source_in`; source amount = timeline × speed. It
/// also may not exceed either clip's own timeline duration.
fn max_xfade(p: &Project, earlier: &Clip, later: &Clip) -> TimeUs {
    let mut m = earlier.duration_us().min(later.duration_us());
    // Earlier side: handle is source past source_out (images = infinite).
    if let Some(cap) = source_cap(p, earlier) {
        let handle_src = (cap - earlier.source_out_us).max(0);
        let handle_tl = ((handle_src as f64) / earlier.speed).round() as TimeUs;
        m = m.min(2 * handle_tl);
    }
    // Later side: handle is source before source_in.
    let handle_src = later.source_in_us.max(0);
    let handle_tl = ((handle_src as f64) / later.speed).round() as TimeUs;
    m = m.min(2 * handle_tl);
    m.max(0)
}

/// Re-point every free clip anchored to `from`, mapping each offset to a new
/// (anchor, offset) pair.
fn remap_anchors(tl: &mut Timeline, from: ClipId, map: impl Fn(TimeUs) -> (ClipId, TimeUs)) {
    for track in [&mut tl.v2, &mut tl.g, &mut tl.a1, &mut tl.a2] {
        for fc in track.iter_mut() {
            if let Some((id, off)) = fc.anchor {
                if id == from {
                    let (nid, noff) = map(off);
                    fc.anchor = Some((nid, noff));
                }
            }
        }
    }
}

/// Convert every free clip anchored to `from` into a timeline anchor at its
/// current resolved position (§6.3 fallback).
fn anchors_to_timeline(tl: &mut Timeline, from: ClipId) {
    let mut resolved: Vec<(ClipId, TimeUs)> = Vec::new();
    for kind in [TrackKind::V2, TrackKind::G, TrackKind::A1, TrackKind::A2] {
        for fc in tl
            .free_track(kind)
            .expect("validated by find_clip/lookup above")
        {
            if matches!(fc.anchor, Some((id, _)) if id == from) {
                resolved.push((fc.clip.id, tl.free_start_us(fc)));
            }
        }
    }
    for (id, start) in resolved {
        for track in [&mut tl.v2, &mut tl.g, &mut tl.a1, &mut tl.a2] {
            if let Some(fc) = track.iter_mut().find(|f| f.clip.id == id) {
                fc.anchor = None;
                fc.timeline_start_us = start;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Operations (§6.4)
// ---------------------------------------------------------------------------

/// Split `clip_id` at absolute timeline time `at_us` (§6.4 `x`). The first
/// part keeps the id (anchors before the split stay put); the second part
/// gets `new` fades per §5: outer edges untouched, interior cut at zero.
/// Returns (first, second) ids.
pub fn split_at(
    p: &mut Project,
    clip_id: ClipId,
    at_us: TimeUs,
) -> Result<(ClipId, ClipId), EditError> {
    let fu = frame_us(&p.meta);
    let (kind, idx) = p.timeline.find_clip(clip_id).ok_or(EditError::NoSuchClip)?;
    let start = match kind {
        TrackKind::V1 => p.timeline.v1_start_us(idx),
        _ => {
            let fc = &p
                .timeline
                .free_track(kind)
                .expect("validated by find_clip/lookup above")[idx];
            p.timeline.free_start_us(fc)
        }
    };
    let clip = p
        .timeline
        .clip(clip_id)
        .expect("validated by find_clip/lookup above")
        .clone();
    let offset = at_us - start;
    if offset < fu || offset > clip.duration_us() - fu {
        return Err(EditError::OutsideClip);
    }
    let second_id = ClipId(p.alloc_id());
    let src_split = clip.source_in_us + to_source(&clip, offset);

    let mut second = clip.clone();
    second.id = second_id;
    second.source_in_us = src_split;
    // Second part owns the original trailing edge: fade-out, vfade-out and
    // the trailing crossfade survive; its leading edge is the new cut (§5).
    second.fade_in_us = 0;
    second.vfade_in_us = 0;
    // A graphic is never shared: the second half gets its own row (§17.2).
    deep_copy_graphic(p, &mut second, None);

    let first_dur = offset;
    {
        let first = p
            .timeline
            .clip_mut(clip_id)
            .expect("validated by find_clip/lookup above");
        first.source_out_us = src_split;
        // First part keeps its leading edge; trailing edge is the new
        // interior cut — fades/crossfade there start at zero (§5).
        first.fade_out_us = 0;
        first.vfade_out_us = 0;
        first.xfade_us = 0;
    }
    match kind {
        TrackKind::V1 => p.timeline.v1.insert(idx + 1, second),
        _ => {
            let anchor = {
                let fc = &p
                    .timeline
                    .free_track(kind)
                    .expect("validated by find_clip/lookup above")[idx];
                fc.anchor.map(|(id, off)| (id, off + first_dur))
            };
            let fc = FreeClip {
                clip: second,
                timeline_start_us: start + first_dur,
                anchor,
            };
            p.timeline
                .free_track_mut(kind)
                .expect("validated by find_clip/lookup above")
                .insert(idx + 1, fc);
        }
    }
    // Anchors past the split travel to the second part, offset re-based.
    if kind == TrackKind::V1 {
        remap_anchors(&mut p.timeline, clip_id, |off| {
            if off >= first_dur {
                (second_id, off - first_dur)
            } else {
                (clip_id, off)
            }
        });
    }
    Ok((clip_id, second_id))
}

/// Ripple-delete a SOURCE-time range out of a V1 clip (§1 silence removal):
/// the clip keeps [source_in, src_start) — original id, fades/xfade at its
/// head preserved — and a NEW clip carries (src_end, source_out] with the
/// same media/settings (like split_at's second half: fresh id, head fades
/// zero, tail fade/xfade moved to it). All later V1 clips ripple earlier by
/// the removed timeline duration. Anchored free clips follow the same rules
/// as split_at + ripple_delete composed:
/// - anchors before the cut stay on the original clip, same offset;
/// - anchors at/past the cut re-base onto the second part (offset relative
///   to its new start), exactly as if the removed span had been split out
///   and ripple-deleted.
///
/// Edge behavior:
/// - src_start <= source_in: this is a START-TRIM to src_end (keep anchors'
///   offsets per the §6.3 start-trim rule; original id kept; no new clip).
/// - src_end >= source_out: this is an END-TRIM to src_start (no new clip).
/// - Both: the whole clip is ripple-deleted (delegate to ripple_delete's
///   re-anchoring policy).
/// - Range clamps to the clip's source window; after clamping, pieces
///   shorter than one project frame (frame_us scaled by speed in source
///   time) are dropped into the adjacent trim case instead of creating
///   sliver clips.
///
/// Returns the label "Cut silence <clip name>" and the id of the SECOND part
/// when one was created.
pub fn ripple_delete_source_range(
    p: &mut Project,
    clip_id: ClipId,
    src_start: TimeUs,
    src_end: TimeUs,
) -> Result<(Option<ClipId>, String), EditError> {
    let (kind, idx) = p.timeline.find_clip(clip_id).ok_or(EditError::NoSuchClip)?;
    // Silences are only cut from the a-roll (§1) — V1 clips, never Gaps.
    if kind != TrackKind::V1 {
        return Err(EditError::WrongTrack);
    }
    let clip = p.timeline.v1[idx].clone();
    // Gaps and graphics have no source to cut silence out of (§17.2).
    if clip.is_gap() || clip.is_graphic() {
        return Err(EditError::WrongTrack);
    }
    let name = clip_name(p, &clip);
    let source_in = clip.source_in_us;
    let source_out = clip.source_out_us;
    let speed = clip.speed;

    // Clamp the requested cut to the clip's source window (edge rules below).
    let a = src_start.clamp(source_in, source_out);
    let b = src_end.clamp(source_in, source_out);
    if b <= a {
        return Err(EditError::Nothing);
    }

    // One project frame in *source* time under this clip's speed — the sliver
    // floor. A remaining piece thinner than this folds into the adjacent trim
    // case instead of becoming a sliver clip. Silence edges are arbitrary µs,
    // so `a`/`b` themselves are NOT snapped to frames (§1) — only this rule.
    let min_src = ((frame_us(&p.meta) as f64) * speed).round().max(1.0) as TimeUs;
    let drop_head = (a - source_in) < min_src; // left piece is a sliver → start-trim
    let drop_tail = (source_out - b) < min_src; // right piece is a sliver → end-trim

    let label = format!("Cut silence {name}");

    // Whole clip removed → ripple_delete's re-anchoring policy (§6.3).
    if drop_head && drop_tail {
        ripple_delete(p, clip_id)?;
        return Ok((None, label));
    }

    // Source→timeline offset of the cut within the original clip.
    let off_a = (((a - source_in) as f64) / speed).round() as TimeUs;

    // START-TRIM to `b` (§6.3 start-trim rule: original id and anchor offsets
    // are kept; the clip start does not move, later clips ripple earlier by
    // construction).
    if drop_head {
        p.timeline.v1[idx].source_in_us = b;
        return Ok((None, label));
    }

    // END-TRIM to `a`: the removed tail carries the clip's fade-out and
    // trailing crossfade away, and its dependents ripple onto the successor
    // exactly as split-out-then-ripple-delete would (§5, §6.3).
    if drop_tail {
        let successor = p.timeline.v1.get(idx + 1).map(|c| c.id);
        {
            let c = &mut p.timeline.v1[idx];
            c.source_out_us = a;
            c.fade_out_us = 0;
            c.vfade_out_us = 0;
            c.xfade_us = 0;
        }
        // No successor: ripple_delete's predecessor re-anchor math collapses to
        // "keep offset", so only the has-successor case moves anything.
        if let Some(sid) = successor {
            remap_anchors(&mut p.timeline, clip_id, move |off| {
                if off < off_a {
                    (clip_id, off)
                } else {
                    (sid, off - off_a)
                }
            });
        }
        return Ok((None, label));
    }

    // INTERIOR cut: the first part keeps [source_in, a) with its head fades and
    // trailing edge cleared (new interior cut); a NEW second part carries
    // (b, source_out] with the original tail fade-out and crossfade — same
    // distribution as split_at (§5).
    let off_b = (((b - source_in) as f64) / speed).round() as TimeUs;
    let second_id = ClipId(p.alloc_id());
    let mut second = clip.clone();
    second.id = second_id;
    second.source_in_us = b;
    // Second part owns the original trailing edge; its leading edge is the new
    // interior cut (§5).
    second.fade_in_us = 0;
    second.vfade_in_us = 0;
    {
        let first = &mut p.timeline.v1[idx];
        first.source_out_us = a;
        // First part keeps its head; trailing edge is the new interior cut.
        first.fade_out_us = 0;
        first.vfade_out_us = 0;
        first.xfade_us = 0;
    }
    p.timeline.v1.insert(idx + 1, second);
    // Anchors re-base exactly as split_at(a) + ripple_delete(middle) composed:
    // before the cut stay put; in the removed middle land at the second part's
    // start region; past the removed span shift onto the second part.
    remap_anchors(&mut p.timeline, clip_id, move |off| {
        if off < off_a {
            (clip_id, off)
        } else if off < off_b {
            (second_id, off - off_a)
        } else {
            (second_id, off - off_b)
        }
    });
    Ok((Some(second_id), label))
}

/// Ripple delete (§6.4 `d`): V1 → remove and close the gap (later clips shift
/// by construction); free tracks → plain remove. Anchored clips re-anchor to
/// the V1 successor (same offset — they stay over the material that slides
/// in); deleting the last clip re-anchors to the predecessor with the offset
/// shifted so the resolved position is unchanged; empty V1 → timeline anchor.
pub fn ripple_delete(p: &mut Project, clip_id: ClipId) -> Result<String, EditError> {
    let (kind, idx) = p.timeline.find_clip(clip_id).ok_or(EditError::NoSuchClip)?;
    let name = clip_name(
        p,
        p.timeline
            .clip(clip_id)
            .expect("validated by find_clip/lookup above"),
    );
    match kind {
        TrackKind::V1 => {
            // The cut on each side of the deleted clip is destroyed (§5).
            if idx > 0 {
                p.timeline.v1[idx - 1].xfade_us = 0;
            }
            // Resolve anchored positions while the clip is still in place —
            // the timeline-anchor fallback needs pre-delete positions.
            let doomed = p.timeline.v1[idx].id;
            if p.timeline.v1.len() == 1 {
                anchors_to_timeline(&mut p.timeline, doomed);
            }
            let removed = p.timeline.v1.remove(idx);
            if let Some(successor) = p.timeline.v1.get(idx) {
                let sid = successor.id;
                remap_anchors(&mut p.timeline, removed.id, |off| (sid, off));
            } else if idx > 0 {
                let pred = &p.timeline.v1[idx - 1];
                let (pid, pdur) = (pred.id, pred.duration_us());
                remap_anchors(&mut p.timeline, removed.id, |off| (pid, pdur + off));
            }
        }
        _ => {
            p.timeline
                .free_track_mut(kind)
                .expect("validated by find_clip/lookup above")
                .remove(idx);
        }
    }
    // A removed clip may have held the last reference to a shared strip (§5)
    // or grade (§15), and it owned its graphic outright (§17).
    strip_gc(p);
    grade_gc(p);
    graphic_gc(p);
    Ok(format!("Delete {name}"))
}

/// Lift delete (§6.4 `D`, V1 only): replace with a Gap of equal length.
/// Anchors move to the gap (same offset), so cutaways hold their position.
pub fn lift_delete(p: &mut Project, clip_id: ClipId) -> Result<String, EditError> {
    let (kind, idx) = p.timeline.find_clip(clip_id).ok_or(EditError::NoSuchClip)?;
    if kind != TrackKind::V1 {
        return Err(EditError::WrongTrack);
    }
    let name = clip_name(p, &p.timeline.v1[idx]);
    let dur = p.timeline.v1[idx].duration_us();
    let gap_id = ClipId(p.alloc_id());
    let gap = Clip::new(gap_id, None, 0, dur);
    let old_id = p.timeline.v1[idx].id;
    p.timeline.v1[idx] = gap;
    if idx > 0 {
        // A Gap at the cut clears the crossfade (§5).
        p.timeline.v1[idx - 1].xfade_us = 0;
    }
    remap_anchors(&mut p.timeline, old_id, |off| (gap_id, off));
    // The replaced clip may have held the last reference to a shared strip (§5)
    // or grade (§15), and it owned its graphic outright (§17).
    strip_gc(p);
    grade_gc(p);
    graphic_gc(p);
    Ok(format!("Lift {name}"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edge {
    In,
    Out,
}

/// Ripple trim by delta (§6.4 Ctrl+arrows). `delta_us` is the change to the
/// clip's timeline duration at that edge: positive = longer. Clamped to the
/// source material and a 1-frame minimum; V1 neighbors shift by construction.
pub fn trim(
    p: &mut Project,
    clip_id: ClipId,
    edge: Edge,
    delta_us: TimeUs,
) -> Result<String, EditError> {
    let fu = frame_us(&p.meta);
    let (kind, idx) = p.timeline.find_clip(clip_id).ok_or(EditError::NoSuchClip)?;
    let cap = source_cap(
        p,
        p.timeline
            .clip(clip_id)
            .expect("validated by find_clip/lookup above"),
    );
    let name = clip_name(
        p,
        p.timeline
            .clip(clip_id)
            .expect("validated by find_clip/lookup above"),
    );
    let (applied, grew_at_start) = {
        let clip = p
            .timeline
            .clip_mut(clip_id)
            .expect("validated by find_clip/lookup above");
        let min_src = to_source(clip, fu).max(1);
        let src_delta = to_source(clip, delta_us);
        match edge {
            Edge::In => {
                // Longer = pull source_in earlier; clamp to source start and
                // to keeping ≥1 frame.
                let hi = (clip.source_out_us - min_src).max(0);
                let new_in = (clip.source_in_us - src_delta).clamp(0, hi);
                let applied = clip.source_in_us - new_in;
                clip.source_in_us = new_in;
                (applied, true)
            }
            Edge::Out => {
                let mut new_out = clip.source_out_us + src_delta;
                if let Some(cap) = cap {
                    new_out = new_out.min(cap);
                }
                new_out = new_out.max(clip.source_in_us + min_src);
                let applied = new_out - clip.source_out_us;
                clip.source_out_us = new_out;
                (applied, false)
            }
        }
    };
    if applied == 0 {
        return Err(EditError::Nothing);
    }
    // A start-edge trim on a free clip moves its leading edge in time so the
    // rest of the clip stays put over the timeline.
    if kind != TrackKind::V1 && grew_at_start {
        let clip_speed = p
            .timeline
            .clip(clip_id)
            .expect("validated by find_clip/lookup above")
            .speed;
        let tl_delta = ((applied as f64) / clip_speed).round() as TimeUs;
        let fc = &mut p
            .timeline
            .free_track_mut(kind)
            .expect("validated by find_clip/lookup above")[idx];
        match &mut fc.anchor {
            Some((_, off)) => *off -= tl_delta,
            None => fc.timeline_start_us -= tl_delta,
        }
    }
    let dir = if edge == Edge::In { "in" } else { "out" };
    Ok(format!("Trim {dir} {name}"))
}

/// Trim-to-playhead (§6.4 `;` / `'`). Ripple: the removed span closes up.
/// Non-ripple on V1: a Gap of the removed length holds the timeline still;
/// free tracks just shorten (non-ripple is the only behavior there).
pub fn trim_to_playhead(
    p: &mut Project,
    clip_id: ClipId,
    edge: Edge,
    playhead_us: TimeUs,
    ripple: bool,
) -> Result<String, EditError> {
    let fu = frame_us(&p.meta);
    let (kind, idx) = p.timeline.find_clip(clip_id).ok_or(EditError::NoSuchClip)?;
    let start = match kind {
        TrackKind::V1 => p.timeline.v1_start_us(idx),
        _ => {
            let fc = &p
                .timeline
                .free_track(kind)
                .expect("validated by find_clip/lookup above")[idx];
            p.timeline.free_start_us(fc)
        }
    };
    let clip = p
        .timeline
        .clip(clip_id)
        .expect("validated by find_clip/lookup above")
        .clone();
    let end = start + clip.duration_us();
    let name = clip_name(p, &clip);
    let (removed, at_start) = match edge {
        Edge::In => (playhead_us - start, true),
        Edge::Out => (end - playhead_us, false),
    };
    // Playhead must sit inside the clip, leaving at least one frame behind.
    if playhead_us < start || playhead_us > end {
        return Err(EditError::OutsideClip);
    }
    if (at_start && playhead_us > end - fu) || (!at_start && playhead_us < start + fu) {
        return Err(EditError::OutsideClip);
    }
    if removed <= 0 {
        return Err(EditError::Nothing);
    }
    {
        let c = p
            .timeline
            .clip_mut(clip_id)
            .expect("validated by find_clip/lookup above");
        let src = to_source(c, removed);
        match edge {
            Edge::In => c.source_in_us += src,
            Edge::Out => c.source_out_us -= src,
        }
    }
    match kind {
        TrackKind::V1 => {
            if !ripple {
                // Leave a Gap of the removed length (§6.4 Alt variants).
                let gap = Clip::new(ClipId(p.alloc_id()), None, 0, removed);
                let at = if at_start { idx } else { idx + 1 };
                p.timeline.v1.insert(at, gap);
                if !at_start {
                    // Gap inserted at the trailing cut kills its crossfade.
                    p.timeline
                        .clip_mut(clip_id)
                        .expect("validated by find_clip/lookup above")
                        .xfade_us = 0;
                } else if idx > 0 {
                    p.timeline.v1[idx - 1].xfade_us = 0;
                }
            }
        }
        _ => {
            if at_start {
                let fc = &mut p
                    .timeline
                    .free_track_mut(kind)
                    .expect("validated by find_clip/lookup above")[idx];
                match &mut fc.anchor {
                    Some((_, off)) => *off += removed,
                    None => fc.timeline_start_us += removed,
                }
            }
        }
    }
    let which = if at_start { "start" } else { "end" };
    Ok(format!("Trim {name} {which} to playhead"))
}

/// Reorder the active V1 clip earlier/later in the sequence (§6.4
/// Ctrl+Shift+↑/↓). Anchored b-roll travels along by construction. Every cut
/// the move destroys or creates starts with no crossfade (§5).
pub fn reorder_v1(p: &mut Project, clip_id: ClipId, later: bool) -> Result<String, EditError> {
    let idx = p
        .timeline
        .v1_index_of(clip_id)
        .ok_or(EditError::NoSuchClip)?;
    let new_idx = if later {
        if idx + 1 >= p.timeline.v1.len() {
            return Err(EditError::Nothing);
        }
        idx + 1
    } else {
        if idx == 0 {
            return Err(EditError::Nothing);
        }
        idx - 1
    };
    let name = clip_name(p, &p.timeline.v1[idx]);
    // Clear crossfades on every boundary that changes: around the old
    // position and around the new one (§5 lifecycle).
    for i in [
        idx.saturating_sub(1),
        idx,
        new_idx.saturating_sub(1),
        new_idx,
    ] {
        if let Some(c) = p.timeline.v1.get_mut(i) {
            c.xfade_us = 0;
        }
    }
    p.timeline.v1.swap(idx, new_idx);
    Ok(format!(
        "Move {name} {}",
        if later { "later" } else { "earlier" }
    ))
}

/// Slide a free clip in time (§6.4 Ctrl+Shift+←/→), clamped against its
/// track neighbors and the timeline start. V1 clips can't slide (§6.3).
pub fn slide_free(p: &mut Project, clip_id: ClipId, delta_us: TimeUs) -> Result<String, EditError> {
    let (kind, idx) = p.timeline.find_clip(clip_id).ok_or(EditError::NoSuchClip)?;
    if kind == TrackKind::V1 {
        return Err(EditError::WrongTrack);
    }
    let name = clip_name(
        p,
        p.timeline
            .clip(clip_id)
            .expect("validated by find_clip/lookup above"),
    );
    let spans = free_spans(&p.timeline, kind);
    let me = spans
        .iter()
        .position(|&(i, _, _)| i == idx)
        .expect("validated by find_clip/lookup above");
    let (_, start, end) = spans[me];
    let lo = if me > 0 { spans[me - 1].2 } else { 0 };
    let hi = spans.get(me + 1).map(|&(_, s, _)| s).unwrap_or(TimeUs::MAX);
    let new_start = (start + delta_us).clamp(lo, hi.saturating_sub(end - start).max(lo));
    let applied = new_start - start;
    if applied == 0 {
        return Err(delta_us_is_collision(delta_us));
    }
    let fc = &mut p
        .timeline
        .free_track_mut(kind)
        .expect("validated by find_clip/lookup above")[idx];
    match &mut fc.anchor {
        Some((_, off)) => *off += applied,
        None => fc.timeline_start_us += applied,
    }
    Ok(format!("Slide {name}"))
}

fn delta_us_is_collision(delta: TimeUs) -> EditError {
    if delta == 0 {
        EditError::Nothing
    } else {
        EditError::Collision
    }
}

/// Duplicate the active clip adjacent to it (§6.4 Ctrl+Alt+↑/↓). Returns
/// (earlier, later) ids — the caller picks which to select. On free tracks
/// the copy lands immediately after; no room = `Collision`.
pub fn duplicate(p: &mut Project, clip_id: ClipId) -> Result<(ClipId, ClipId), EditError> {
    let (kind, idx) = p.timeline.find_clip(clip_id).ok_or(EditError::NoSuchClip)?;
    let copy_id = ClipId(p.alloc_id());
    match kind {
        TrackKind::V1 => {
            let mut copy = p.timeline.v1[idx].clone();
            copy.id = copy_id;
            // A graphic is never shared: the copy gets its own row (§17.2).
            deep_copy_graphic(p, &mut copy, None);
            // Both cuts the insertion creates start clean (§5).
            copy.xfade_us = 0;
            p.timeline.v1[idx].xfade_us = 0;
            p.timeline.v1.insert(idx + 1, copy);
        }
        _ => {
            let fc = p
                .timeline
                .free_track(kind)
                .expect("validated by find_clip/lookup above")[idx]
                .clone();
            let start = p.timeline.free_start_us(&fc);
            let dur = fc.clip.duration_us();
            let spans = free_spans(&p.timeline, kind);
            let me = spans
                .iter()
                .position(|&(i, _, _)| i == idx)
                .expect("validated by find_clip/lookup above");
            let gap_end = spans.get(me + 1).map(|&(_, s, _)| s).unwrap_or(TimeUs::MAX);
            if gap_end.saturating_sub(start + dur) < dur {
                return Err(EditError::Collision);
            }
            let mut copy = fc.clone();
            copy.clip.id = copy_id;
            deep_copy_graphic(p, &mut copy.clip, None);
            copy.timeline_start_us = start + dur;
            copy.anchor = fc.anchor.map(|(id, off)| (id, off + dur));
            p.timeline
                .free_track_mut(kind)
                .expect("validated by find_clip/lookup above")
                .insert(idx + 1, copy);
        }
    }
    Ok((clip_id, copy_id))
}

/// Slip (§6.4 `,`/`.`): shift source in/out together; timeline length is
/// untouched. Clamped to the available source material.
pub fn slip(p: &mut Project, clip_id: ClipId, delta_us: TimeUs) -> Result<String, EditError> {
    let cap = source_cap(p, p.timeline.clip(clip_id).ok_or(EditError::NoSuchClip)?);
    let name = clip_name(
        p,
        p.timeline
            .clip(clip_id)
            .expect("validated by find_clip/lookup above"),
    );
    let clip = p
        .timeline
        .clip_mut(clip_id)
        .expect("validated by find_clip/lookup above");
    // No source behind a gap or a graphic — nothing to slip (§17.2).
    if clip.is_gap() || clip.is_graphic() {
        return Err(EditError::WrongTrack);
    }
    let src_delta = to_source(clip, delta_us);
    let lo = -clip.source_in_us;
    let hi = cap
        .map(|c| c - clip.source_out_us)
        .unwrap_or(TimeUs::MAX / 2);
    let applied = src_delta.clamp(lo, hi.max(lo));
    if applied == 0 {
        return Err(EditError::Nothing);
    }
    clip.source_in_us += applied;
    clip.source_out_us += applied;
    Ok(format!("Slip {name}"))
}

/// Append media to the end of V1 (§7.1 `e` in the Media view).
pub fn append_to_v1(
    p: &mut Project,
    media_id: MediaId,
    range: Option<(TimeUs, TimeUs)>,
) -> Result<ClipId, EditError> {
    let media = p.media_by_id(media_id).ok_or(EditError::NoSuchMedia)?;
    let (src_in, src_out) = match range {
        Some(r) => r,
        None => (0, media.duration_us.unwrap_or(5 * US_PER_SEC)),
    };
    if src_out <= src_in {
        return Err(EditError::Nothing);
    }
    let id = ClipId(p.alloc_id());
    p.timeline
        .v1
        .push(Clip::new(id, Some(media_id), src_in, src_out));
    Ok(id)
}

/// One clipboard entry (§6.4 `y`). `rel_start_us` is the clip's timeline
/// start relative to the earliest yanked clip, so multi-clip pastes keep
/// their spacing.
#[derive(Debug, Clone)]
pub struct YankedClip {
    pub track: TrackKind,
    pub clip: Clip,
    pub rel_start_us: TimeUs,
    /// The clip's graphic document, snapshotted at yank time (§17.2): the
    /// source row may be GC'd before the paste lands, and every paste needs a
    /// fresh private copy anyway.
    pub graphic: Option<GraphicDoc>,
}

/// Yank a set of clips (no model mutation — the clipboard lives in the app).
pub fn yank(p: &Project, ids: &[ClipId]) -> Vec<YankedClip> {
    let mut items: Vec<(TrackKind, Clip, TimeUs)> = Vec::new();
    for &id in ids {
        if let Some((kind, idx)) = p.timeline.find_clip(id) {
            let (clip, start) = match kind {
                TrackKind::V1 => (p.timeline.v1[idx].clone(), p.timeline.v1_start_us(idx)),
                _ => {
                    let fc = &p
                        .timeline
                        .free_track(kind)
                        .expect("validated by find_clip/lookup above")[idx];
                    (fc.clip.clone(), p.timeline.free_start_us(fc))
                }
            };
            items.push((kind, clip, start));
        }
    }
    let origin = items.iter().map(|&(_, _, s)| s).min().unwrap_or(0);
    items.sort_by_key(|&(_, _, s)| s);
    items
        .into_iter()
        .map(|(track, clip, s)| YankedClip {
            graphic: graphic_of(p, &clip),
            track,
            clip,
            rel_start_us: s - origin,
        })
        .collect()
}

/// Paste (§6.4 `p`): V1 clips ripple-insert as a block at the playhead
/// (splitting the clip under it if mid-clip); free clips land at
/// playhead + their yanked spacing, anchored to the V1 clip under them, and
/// shift right to the first fitting gap on collision. Returns pasted ids.
pub fn paste(
    p: &mut Project,
    clipboard: &[YankedClip],
    playhead_us: TimeUs,
) -> Result<Vec<ClipId>, EditError> {
    if clipboard.is_empty() {
        return Err(EditError::Nothing);
    }
    let mut new_ids = Vec::new();

    // --- V1 block insert ---
    let v1_items: Vec<&YankedClip> = clipboard
        .iter()
        .filter(|y| y.track == TrackKind::V1)
        .collect();
    if !v1_items.is_empty() {
        let insert_at = v1_insertion_index(p, playhead_us)?;
        if insert_at > 0 {
            // Inserting at a cut clears its crossfade (§5).
            p.timeline.v1[insert_at - 1].xfade_us = 0;
        }
        for (n, y) in v1_items.iter().enumerate() {
            let mut clip = y.clip.clone();
            clip.id = ClipId(p.alloc_id());
            // Every pasted graphic gets its own row (§17.2), recreated from the
            // yank-time snapshot when the source row is gone.
            deep_copy_graphic(p, &mut clip, y.graphic.as_ref());
            if n + 1 == v1_items.len() {
                clip.xfade_us = 0; // trailing cut of the block is new
            }
            new_ids.push(clip.id);
            p.timeline.v1.insert(insert_at + n, clip);
        }
    }

    // --- Free-track pastes ---
    for y in clipboard.iter().filter(|y| y.track != TrackKind::V1) {
        let dur = y.clip.duration_us();
        let want = playhead_us + y.rel_start_us;
        let spans = free_spans(&p.timeline, y.track);
        // First gap at or after `want` that fits (§6.3 shift-right).
        let start = first_free_start(&spans, want, dur);
        let mut clip = y.clip.clone();
        clip.id = ClipId(p.alloc_id());
        deep_copy_graphic(p, &mut clip, y.graphic.as_ref());
        new_ids.push(clip.id);
        // Anchor to the V1 clip under the landing spot (§6.3 default).
        let anchor = anchor_under(&p.timeline, start);
        p.timeline
            .free_track_mut(y.track)
            .expect("validated by find_clip/lookup above")
            .push(FreeClip {
                clip,
                timeline_start_us: start,
                anchor,
            });
    }
    Ok(new_ids)
}

/// Where a ripple insert at `playhead_us` lands in the V1 sequence, splitting
/// the clip under the playhead if it falls mid-clip.
fn v1_insertion_index(p: &mut Project, playhead_us: TimeUs) -> Result<usize, EditError> {
    if p.timeline.v1.is_empty() || playhead_us >= p.timeline.duration_us() {
        return Ok(p.timeline.v1.len());
    }
    let idx = p
        .timeline
        .v1_index_at(playhead_us)
        .expect("validated by find_clip/lookup above");
    let start = p.timeline.v1_start_us(idx);
    if playhead_us <= start {
        return Ok(idx);
    }
    let fu = frame_us(&p.meta);
    if playhead_us - start < fu {
        return Ok(idx);
    }
    let end = start + p.timeline.v1[idx].duration_us();
    if end - playhead_us < fu {
        return Ok(idx + 1);
    }
    let id = p.timeline.v1[idx].id;
    split_at(p, id, playhead_us)?;
    Ok(idx + 1)
}

/// Toggle a marker at the playhead (§7.3 `m`): sets one, or removes an
/// existing marker within half a frame.
pub fn toggle_marker(p: &mut Project, time_us: TimeUs) -> Result<String, EditError> {
    let tol = frame_us(&p.meta) / 2;
    if let Some(i) = p
        .markers
        .iter()
        .position(|m| (m.time_us - time_us).abs() <= tol)
    {
        let m = p.markers.remove(i);
        return Ok(if m.name.is_empty() {
            "Remove marker".to_string()
        } else {
            format!("Remove marker {}", m.name)
        });
    }
    let id = MarkerId(p.alloc_id());
    p.markers.push(Marker {
        id,
        time_us,
        name: String::new(),
        color: None,
    });
    p.markers.sort_by_key(|m| m.time_us);
    Ok("Set marker".to_string())
}

/// Change the project format (§8.1): a normal undoable command; fit/fill
/// clips re-frame at render time by construction (§6.4).
pub fn set_project_format(
    p: &mut Project,
    width: u32,
    height: u32,
    fps_num: u32,
    fps_den: u32,
) -> Result<String, EditError> {
    let m = &mut p.meta;
    if (m.width, m.height, m.fps_num, m.fps_den) == (width, height, fps_num, fps_den) {
        return Err(EditError::Nothing);
    }
    m.width = width;
    m.height = height;
    m.fps_num = fps_num;
    m.fps_den = fps_den.max(1);
    Ok(format!(
        "Project format {width}×{height} @ {:.6}",
        fps_num as f64 / fps_den.max(1) as f64
    ))
}

// ---------------------------------------------------------------------------
// Edge fades & crossfades (§5)
// ---------------------------------------------------------------------------

/// Adjust the audio edge fade at one end of a clip (§5 `(` / `)`). What the
/// edge *means* depends on what it touches: a butted V1 cut (adjacent
/// non-Gap V1 neighbor on that side) adjusts the equal-power **crossfade**
/// stored on the earlier clip of the cut — one symmetric `xfade_us`, clamped
/// to the handle material on each side and to both clips' durations. Every
/// other edge — a Gap, the timeline start/end, or a free track — adjusts the
/// plain fade to/from silence (`fade_in_us` / `fade_out_us`), clamped so the
/// two fades never cross. `(`/`)` on a Gap is refused (§5).
pub fn edge_fade(
    p: &mut Project,
    clip_id: ClipId,
    edge: Edge,
    delta_us: TimeUs,
) -> Result<String, EditError> {
    let (kind, idx) = p.timeline.find_clip(clip_id).ok_or(EditError::NoSuchClip)?;
    let me = p
        .timeline
        .clip(clip_id)
        .expect("validated by find_clip/lookup above");
    // Gaps have no audio; graphics have no whole-clip opacity either — the
    // in/out animations cover clip-level fading (§17.3).
    if me.is_gap() || me.is_graphic() {
        return Err(EditError::WrongTrack);
    }
    // Does this edge sit on a butted V1 cut? If so, resolve the (earlier,
    // later) sequence indices whose crossfade it drives.
    let xfade_target = if kind == TrackKind::V1 {
        match edge {
            // A graphic on either side of the cut has no audio handles to
            // crossfade with, so that boundary takes the plain-fade path.
            Edge::Out => match p.timeline.v1.get(idx + 1) {
                Some(succ) if !succ.is_gap() && !succ.is_graphic() => Some((idx, idx + 1)),
                _ => None,
            },
            Edge::In => {
                let prev = idx.checked_sub(1).map(|i| &p.timeline.v1[i]);
                matches!(prev, Some(c) if !c.is_gap() && !c.is_graphic()).then(|| (idx - 1, idx))
            }
        }
    } else {
        None
    };

    if let Some((ei, li)) = xfade_target {
        let earlier = p.timeline.v1[ei].clone();
        let later = p.timeline.v1[li].clone();
        let max = max_xfade(p, &earlier, &later);
        let cur = earlier.xfade_us;
        let new = (cur + delta_us).clamp(0, max);
        if new == cur {
            return Err(EditError::Nothing);
        }
        p.timeline.v1[ei].xfade_us = new;
        Ok(format!("crossfade {:.2} s", new as f64 / US_PER_SEC as f64))
    } else {
        let c = p
            .timeline
            .clip_mut(clip_id)
            .expect("validated by find_clip/lookup above");
        let dur = c.duration_us();
        let (cur, other) = match edge {
            Edge::In => (c.fade_in_us, c.fade_out_us),
            Edge::Out => (c.fade_out_us, c.fade_in_us),
        };
        // Fades never cross: leave at least the other fade's room (§5).
        let new = (cur + delta_us).clamp(0, (dur - other).max(0));
        if new == cur {
            return Err(EditError::Nothing);
        }
        match edge {
            Edge::In => c.fade_in_us = new,
            Edge::Out => c.fade_out_us = new,
        }
        let dir = if edge == Edge::In { "in" } else { "out" };
        Ok(format!(
            "fade {dir} {:.2} s",
            new as f64 / US_PER_SEC as f64
        ))
    }
}

// ---------------------------------------------------------------------------
// B-roll & free-track audio inserts (§6.4, §9)
// ---------------------------------------------------------------------------

/// Insert `media` as b-roll on V2 at `at_us` (§6.4 marquee flow `b`): a
/// FreeClip anchored to the V1 clip it starts over (timeline-anchored when V1
/// is empty or `at_us` is past the end), audio muted. On collision (§6.3) the
/// duration is clamped to the free space before the next V2 clip; if there is
/// no room for even one frame it is refused. Returns the new clip id (the app
/// moves the playhead to its end) and the label.
pub fn insert_broll(
    p: &mut Project,
    media_id: MediaId,
    at_us: TimeUs,
    source_in_us: TimeUs,
    source_out_us: TimeUs,
) -> Result<(ClipId, String), EditError> {
    if p.media_by_id(media_id).is_none() {
        return Err(EditError::NoSuchMedia);
    }
    if source_out_us <= source_in_us {
        return Err(EditError::Nothing);
    }
    let fu = frame_us(&p.meta);
    let spans = free_spans(&p.timeline, TrackKind::V2);
    // `at_us` inside an existing clip → no room at all (§6.3).
    if spans.iter().any(|&(_, s, e)| s <= at_us && at_us < e) {
        return Err(EditError::Collision);
    }
    // Clamp the duration to the free space before the next V2 clip.
    let next_start = spans
        .iter()
        .filter(|&&(_, s, _)| s >= at_us)
        .map(|&(_, s, _)| s)
        .min()
        .unwrap_or(TimeUs::MAX);
    let room = next_start - at_us;
    if room < fu {
        return Err(EditError::Collision);
    }
    let dur = (source_out_us - source_in_us).min(room);
    let id = ClipId(p.alloc_id());
    let mut clip = Clip::new(id, Some(media_id), source_in_us, source_in_us + dur);
    clip.muted = true; // b-roll audio defaults off (§6.4).
    let name = clip_name(p, &clip);
    let anchor = anchor_under(&p.timeline, at_us);
    p.timeline.v2.push(FreeClip {
        clip,
        timeline_start_us: at_us,
        anchor,
    });
    Ok((id, format!("Insert b-roll {name}")))
}

/// Insert an audio file on a free audio track (§9: audio lands on A1/A2).
/// `kind` must be A1 or A2. Anchored to the covering V1 clip (same rule as
/// b-roll); on collision it shifts right to the first gap that fits (§6.3, the
/// paste rule). Not muted. Returns the new clip id and the label.
pub fn insert_free_audio(
    p: &mut Project,
    kind: TrackKind,
    media_id: MediaId,
    at_us: TimeUs,
    source_in_us: TimeUs,
    source_out_us: TimeUs,
) -> Result<(ClipId, String), EditError> {
    if !matches!(kind, TrackKind::A1 | TrackKind::A2) {
        return Err(EditError::WrongTrack);
    }
    if p.media_by_id(media_id).is_none() {
        return Err(EditError::NoSuchMedia);
    }
    if source_out_us <= source_in_us {
        return Err(EditError::Nothing);
    }
    let dur = source_out_us - source_in_us;
    let spans = free_spans(&p.timeline, kind);
    let start = first_free_start(&spans, at_us.max(0), dur);
    let id = ClipId(p.alloc_id());
    let clip = Clip::new(id, Some(media_id), source_in_us, source_out_us);
    let name = clip_name(p, &clip);
    let anchor = anchor_under(&p.timeline, start);
    p.timeline
        .free_track_mut(kind)
        .expect("A1/A2 validated above")
        .push(FreeClip {
            clip,
            timeline_start_us: start,
            anchor,
        });
    Ok((id, format!("Insert audio {name}")))
}

// ---------------------------------------------------------------------------
// Strip linking (§5) — the shareable-treatment mechanism
// ---------------------------------------------------------------------------

/// Count of clips referencing `strip_id` — drives the "● linked with N clips"
/// UI (§5).
pub fn strip_refs(p: &Project, strip_id: StripId) -> usize {
    let tl = &p.timeline;
    let free = [&tl.v2, &tl.g, &tl.a1, &tl.a2]
        .into_iter()
        .flat_map(|t| t.iter().map(|fc| &fc.clip));
    tl.v1
        .iter()
        .chain(free)
        .filter(|c| c.strip_id == Some(strip_id))
        .count()
}

/// Drop Strip rows with zero referents (§5). Called from every op that can
/// orphan one — cheap: a single retain over a referenced-id set.
fn strip_gc(p: &mut Project) {
    let mut used: std::collections::HashSet<StripId> = std::collections::HashSet::new();
    for c in &p.timeline.v1 {
        if let Some(s) = c.strip_id {
            used.insert(s);
        }
    }
    for track in [
        &p.timeline.v2,
        &p.timeline.g,
        &p.timeline.a1,
        &p.timeline.a2,
    ] {
        for fc in track {
            if let Some(s) = fc.clip.strip_id {
                used.insert(s);
            }
        }
    }
    p.strips.retain(|s| used.contains(&s.id));
}

/// Ensure `clip_id` points at a real Strip row, allocating a flat one if it is
/// currently flat/`None` (or its row went missing). Returns that strip id.
fn ensure_strip(p: &mut Project, clip_id: ClipId) -> Result<StripId, EditError> {
    let sid = p
        .timeline
        .clip(clip_id)
        .ok_or(EditError::NoSuchClip)?
        .strip_id;
    match sid {
        Some(sid) if p.strips.iter().any(|s| s.id == sid) => Ok(sid),
        Some(sid) => {
            // Dangling reference: materialize a flat row at that id.
            p.strips.push(Strip::new(sid, &StripParams::default()));
            Ok(sid)
        }
        None => {
            let new_id = StripId(p.alloc_id());
            p.strips.push(Strip::new(new_id, &StripParams::default()));
            p.timeline
                .clip_mut(clip_id)
                .expect("validated above")
                .strip_id = Some(new_id);
            Ok(new_id)
        }
    }
}

/// Write a clip's Strip treatment (§5). The row is shared — linked clips all
/// change. A clip with no strip gets a fresh row pointed at it; a sole-referent
/// clip whose params go flat drops back to `strip_id = None` (the row is GC'd).
pub fn set_strip_params(
    p: &mut Project,
    clip_id: ClipId,
    params: &StripParams,
) -> Result<String, EditError> {
    let sid = p
        .timeline
        .clip(clip_id)
        .ok_or(EditError::NoSuchClip)?
        .strip_id;
    match sid {
        Some(sid) if params.is_flat() && strip_refs(p, sid) == 1 => {
            // Sole referent going flat: drop to None; GC removes the row.
            p.timeline
                .clip_mut(clip_id)
                .expect("validated above")
                .strip_id = None;
        }
        Some(sid) => match p.strips.iter_mut().find(|s| s.id == sid) {
            Some(strip) => strip.set_params(params),
            None => p.strips.push(Strip::new(sid, params)),
        },
        None => {
            if params.is_flat() {
                return Err(EditError::Nothing);
            }
            let new_id = StripId(p.alloc_id());
            p.strips.push(Strip::new(new_id, params));
            p.timeline
                .clip_mut(clip_id)
                .expect("validated above")
                .strip_id = Some(new_id);
        }
    }
    strip_gc(p);
    Ok("Set strip treatment".to_string())
}

/// Link each of `to_clips` to `from_clip`'s Strip (§5): they share one row, so
/// editing any of them edits all. `from_clip` is given a strip row first if it
/// is flat/`None`.
pub fn link_strip(
    p: &mut Project,
    from_clip: ClipId,
    to_clips: &[ClipId],
) -> Result<String, EditError> {
    if to_clips.is_empty() {
        return Err(EditError::Nothing);
    }
    let sid = ensure_strip(p, from_clip)?;
    let mut n = 0;
    for &t in to_clips {
        if let Some(c) = p.timeline.clip_mut(t) {
            c.strip_id = Some(sid);
            n += 1;
        }
    }
    strip_gc(p);
    Ok(format!("Link strip to {n} clips"))
}

/// Break a clip out of a shared Strip (§5): clone the shared row into a private
/// one for this clip. `Nothing` if the strip was not shared or the clip has no
/// strip.
pub fn unlink_strip(p: &mut Project, clip_id: ClipId) -> Result<String, EditError> {
    let sid = p
        .timeline
        .clip(clip_id)
        .ok_or(EditError::NoSuchClip)?
        .strip_id;
    let Some(sid) = sid else {
        return Err(EditError::Nothing);
    };
    if strip_refs(p, sid) <= 1 {
        return Err(EditError::Nothing);
    }
    let params = p
        .strips
        .iter()
        .find(|s| s.id == sid)
        .map(Strip::params)
        .unwrap_or_default();
    let new_id = StripId(p.alloc_id());
    p.strips.push(Strip::new(new_id, &params));
    p.timeline
        .clip_mut(clip_id)
        .expect("validated above")
        .strip_id = Some(new_id);
    strip_gc(p);
    Ok("Unlink strip".to_string())
}

// ---------------------------------------------------------------------------
// Grade linking (§15) — the shareable-color mechanism
// ---------------------------------------------------------------------------

/// Count of clips referencing `grade_id` — drives the "● linked with N clips"
/// UI (§15).
pub fn grade_refs(p: &Project, grade_id: GradeId) -> usize {
    let tl = &p.timeline;
    let free = [&tl.v2, &tl.g, &tl.a1, &tl.a2]
        .into_iter()
        .flat_map(|t| t.iter().map(|fc| &fc.clip));
    tl.v1
        .iter()
        .chain(free)
        .filter(|c| c.grade_id == Some(grade_id))
        .count()
}

/// Drop Grade rows with zero referents (§15). Called from every op that can
/// orphan one — cheap: a single retain over a referenced-id set.
fn grade_gc(p: &mut Project) {
    let mut used: std::collections::HashSet<GradeId> = std::collections::HashSet::new();
    for c in &p.timeline.v1 {
        if let Some(g) = c.grade_id {
            used.insert(g);
        }
    }
    for track in [
        &p.timeline.v2,
        &p.timeline.g,
        &p.timeline.a1,
        &p.timeline.a2,
    ] {
        for fc in track {
            if let Some(g) = fc.clip.grade_id {
                used.insert(g);
            }
        }
    }
    p.grades.retain(|g| used.contains(&g.id));
}

/// Ensure `clip_id` points at a real Grade row, allocating an identity one if it
/// is currently ungraded/`None` (or its row went missing). Returns that grade id.
fn ensure_grade(p: &mut Project, clip_id: ClipId) -> Result<GradeId, EditError> {
    let gid = p
        .timeline
        .clip(clip_id)
        .ok_or(EditError::NoSuchClip)?
        .grade_id;
    match gid {
        Some(gid) if p.grades.iter().any(|g| g.id == gid) => Ok(gid),
        Some(gid) => {
            // Dangling reference: materialize an identity row at that id.
            p.grades.push(Grade::new(gid, &GradeParams::default()));
            Ok(gid)
        }
        None => {
            let new_id = GradeId(p.alloc_id());
            p.grades.push(Grade::new(new_id, &GradeParams::default()));
            p.timeline
                .clip_mut(clip_id)
                .expect("validated above")
                .grade_id = Some(new_id);
            Ok(new_id)
        }
    }
}

/// Write a clip's color grade (§15). The row is shared — linked clips all
/// change. A clip with no grade gets a fresh row pointed at it; a sole-referent
/// clip whose params go identity drops back to `grade_id = None` (row GC'd).
pub fn set_grade_params(
    p: &mut Project,
    clip_id: ClipId,
    params: &GradeParams,
) -> Result<String, EditError> {
    let gid = p
        .timeline
        .clip(clip_id)
        .ok_or(EditError::NoSuchClip)?
        .grade_id;
    match gid {
        Some(gid) if params.is_identity() && grade_refs(p, gid) == 1 => {
            // Sole referent going identity: drop to None; GC removes the row.
            p.timeline
                .clip_mut(clip_id)
                .expect("validated above")
                .grade_id = None;
        }
        Some(gid) => match p.grades.iter_mut().find(|g| g.id == gid) {
            Some(grade) => grade.set_params(params),
            None => p.grades.push(Grade::new(gid, params)),
        },
        None => {
            if params.is_identity() {
                return Err(EditError::Nothing);
            }
            let new_id = GradeId(p.alloc_id());
            p.grades.push(Grade::new(new_id, params));
            p.timeline
                .clip_mut(clip_id)
                .expect("validated above")
                .grade_id = Some(new_id);
        }
    }
    grade_gc(p);
    Ok("Set color grade".to_string())
}

/// Link each of `to_clips` to `from_clip`'s Grade (§15): they share one row, so
/// editing any of them edits all. `from_clip` is given a grade row first if it
/// is ungraded/`None`.
pub fn link_grade(
    p: &mut Project,
    from_clip: ClipId,
    to_clips: &[ClipId],
) -> Result<String, EditError> {
    if to_clips.is_empty() {
        return Err(EditError::Nothing);
    }
    let gid = ensure_grade(p, from_clip)?;
    let mut n = 0;
    for &t in to_clips {
        if let Some(c) = p.timeline.clip_mut(t) {
            c.grade_id = Some(gid);
            n += 1;
        }
    }
    grade_gc(p);
    Ok(format!("Link color to {n} clips"))
}

/// Break a clip out of a shared Grade (§15): clone the shared row into a private
/// one for this clip. `Nothing` if the grade was not shared or the clip has no
/// grade.
pub fn unlink_grade(p: &mut Project, clip_id: ClipId) -> Result<String, EditError> {
    let gid = p
        .timeline
        .clip(clip_id)
        .ok_or(EditError::NoSuchClip)?
        .grade_id;
    let Some(gid) = gid else {
        return Err(EditError::Nothing);
    };
    if grade_refs(p, gid) <= 1 {
        return Err(EditError::Nothing);
    }
    let params = p
        .grades
        .iter()
        .find(|g| g.id == gid)
        .map(Grade::params)
        .unwrap_or_default();
    let new_id = GradeId(p.alloc_id());
    p.grades.push(Grade::new(new_id, &params));
    p.timeline
        .clip_mut(clip_id)
        .expect("validated above")
        .grade_id = Some(new_id);
    grade_gc(p);
    Ok("Unlink color grade".to_string())
}

// ---------------------------------------------------------------------------
// Graphics (§17) — one document per clip, never shared
// ---------------------------------------------------------------------------

/// Default graphic clip length when the caller doesn't say (§17.2: infinite
/// source like a still, so this follows the still convention).
pub const DEFAULT_GRAPHIC_DUR_US: TimeUs = 5 * US_PER_SEC;

/// The parsed document a clip renders, if it is a graphic and its row is
/// present (a dangling id reads as `None`).
pub fn graphic_of(p: &Project, clip: &Clip) -> Option<GraphicDoc> {
    let gid = clip.graphic_id?;
    p.graphics.iter().find(|g| g.id == gid).map(Graphic::doc)
}

/// Count of clips referencing `graphic_id`. Always 0 or 1 — §17.2 makes a
/// graphic a single clip — which is exactly why it is worth asserting.
pub fn graphic_refs(p: &Project, graphic_id: GraphicId) -> usize {
    let tl = &p.timeline;
    let free = [&tl.v2, &tl.g, &tl.a1, &tl.a2]
        .into_iter()
        .flat_map(|t| t.iter().map(|fc| &fc.clip));
    tl.v1
        .iter()
        .chain(free)
        .filter(|c| c.graphic_id == Some(graphic_id))
        .count()
}

/// Drop Graphic rows with zero referents (§17). Called from every op that can
/// orphan one, exactly like `strip_gc`/`grade_gc`.
fn graphic_gc(p: &mut Project) {
    let mut used: std::collections::HashSet<GraphicId> = std::collections::HashSet::new();
    for c in &p.timeline.v1 {
        if let Some(g) = c.graphic_id {
            used.insert(g);
        }
    }
    for track in [
        &p.timeline.v2,
        &p.timeline.g,
        &p.timeline.a1,
        &p.timeline.a2,
    ] {
        for fc in track {
            if let Some(g) = fc.clip.graphic_id {
                used.insert(g);
            }
        }
    }
    p.graphics.retain(|g| used.contains(&g.id));
}

/// Allocate a fresh Graphic row holding `doc`.
fn new_graphic_row(p: &mut Project, doc: &GraphicDoc) -> GraphicId {
    let id = GraphicId(p.alloc_id());
    p.graphics.push(Graphic::new(id, doc));
    id
}

/// Enforce the single-referent invariant (§17.2) on a clip that was *copied*
/// from another: give it its own row holding a clone of the document, so two
/// clips never share one graphic. `fallback` is the document to use when the
/// original row is gone (a paste after the source was deleted).
fn deep_copy_graphic(p: &mut Project, clip: &mut Clip, fallback: Option<&GraphicDoc>) {
    let Some(gid) = clip.graphic_id else { return };
    let doc = p
        .graphics
        .iter()
        .find(|g| g.id == gid)
        .map(Graphic::doc)
        .or_else(|| fallback.cloned())
        .unwrap_or_default();
    clip.graphic_id = Some(new_graphic_row(p, &doc));
}

/// Insert a graphic as a full-frame title card in V1 at the playhead (§17.2),
/// splitting the clip under the playhead exactly like a paste. `dur_us`
/// defaults to [`DEFAULT_GRAPHIC_DUR_US`]. Returns the new clip id.
pub fn insert_graphic_v1(
    p: &mut Project,
    doc: &GraphicDoc,
    playhead_us: TimeUs,
    dur_us: Option<TimeUs>,
) -> Result<ClipId, EditError> {
    let dur = dur_us.unwrap_or(DEFAULT_GRAPHIC_DUR_US);
    if dur < frame_us(&p.meta) {
        return Err(EditError::Nothing);
    }
    let at = v1_insertion_index(p, playhead_us)?;
    if at > 0 {
        // Inserting at a cut clears its crossfade (§5).
        p.timeline.v1[at - 1].xfade_us = 0;
    }
    let gid = new_graphic_row(p, doc);
    let id = ClipId(p.alloc_id());
    let mut clip = Clip::new(id, None, 0, dur);
    clip.graphic_id = Some(gid);
    p.timeline.v1.insert(at, clip);
    Ok(id)
}

/// Insert a graphic as an overlay on the G track at `at_us` (§17.2): an
/// anchored FreeClip, same collision handling as b-roll — the duration clamps
/// to the free space before the next G clip, and a fully covered `at_us` is
/// refused. Returns the new clip id and the label.
pub fn insert_graphic_g(
    p: &mut Project,
    doc: &GraphicDoc,
    at_us: TimeUs,
    dur_us: Option<TimeUs>,
) -> Result<(ClipId, String), EditError> {
    let want = dur_us.unwrap_or(DEFAULT_GRAPHIC_DUR_US);
    let fu = frame_us(&p.meta);
    if want < fu {
        return Err(EditError::Nothing);
    }
    let spans = free_spans(&p.timeline, TrackKind::G);
    // `at_us` inside an existing graphic → no room at all (§6.3).
    if spans.iter().any(|&(_, s, e)| s <= at_us && at_us < e) {
        return Err(EditError::Collision);
    }
    let next_start = spans
        .iter()
        .filter(|&&(_, s, _)| s >= at_us)
        .map(|&(_, s, _)| s)
        .min()
        .unwrap_or(TimeUs::MAX);
    let room = next_start - at_us;
    if room < fu {
        return Err(EditError::Collision);
    }
    let dur = want.min(room);
    let gid = new_graphic_row(p, doc);
    let id = ClipId(p.alloc_id());
    let mut clip = Clip::new(id, None, 0, dur);
    clip.graphic_id = Some(gid);
    let name = clip_name(p, &clip);
    let anchor = anchor_under(&p.timeline, at_us);
    p.timeline.g.push(FreeClip {
        clip,
        timeline_start_us: at_us,
        anchor,
    });
    Ok((id, format!("Insert graphic {name}")))
}

/// Write a clip's graphic document (§17.6) — the editor's only mutation path.
/// The row is private to this clip, so this is always an in-place edit; a
/// vanished row is rematerialized at the same id. Coalesces under
/// [`CoalesceKey::GraphicEdit`] so a whole editor session is one undo step.
pub fn set_graphic_doc(
    p: &mut Project,
    clip: ClipId,
    doc: &GraphicDoc,
) -> Result<String, EditError> {
    let gid = p
        .timeline
        .clip(clip)
        .ok_or(EditError::NoSuchClip)?
        .graphic_id
        .ok_or(EditError::WrongTrack)?;
    match p.graphics.iter_mut().find(|g| g.id == gid) {
        Some(row) => {
            if row.doc() == *doc {
                return Err(EditError::Nothing);
            }
            row.set_doc(doc);
        }
        None => p.graphics.push(Graphic::new(gid, doc)),
    }
    Ok("Edit graphic".to_string())
}

// ---------------------------------------------------------------------------
// Paste attributes (§6.4 `Y` / `P`)
// ---------------------------------------------------------------------------

/// A clip's copyable attributes (§6.4 `Y`). Shareable objects (Strip
/// treatment, grade) are *linked* on paste — the `strip_params`/`grade_params`
/// snapshots let a deleted row be recreated; per-clip values (framing, gain,
/// pan, fades, channel mode, mute) are *copied*.
#[derive(Debug, Clone, PartialEq)]
pub struct YankedAttrs {
    // Shareable objects — linked on paste (§5, §15).
    pub strip_id: Option<StripId>,
    pub strip_params: Option<StripParams>,
    pub grade_id: Option<GradeId>,
    pub grade_params: Option<GradeParams>,
    // Per-clip framing — copied.
    pub crop_l: i32,
    pub crop_r: i32,
    pub crop_t: i32,
    pub crop_b: i32,
    pub nudge_x: i32,
    pub nudge_y: i32,
    pub rotate: f64,
    pub fit_mode: FitMode,
    pub scale: Option<f64>,
    pub tx: Option<f64>,
    pub ty: Option<f64>,
    // Per-clip mixing — copied.
    pub gain_db: f64,
    pub pan: f64,
    pub fade_in_us: TimeUs,
    pub fade_out_us: TimeUs,
    pub channel_mode: ChannelMode,
    pub muted: bool,
}

/// Snapshot a clip's shareable + per-clip attributes (§6.4 `Y`). No mutation —
/// the yanked attributes live in the app.
pub fn yank_attributes(p: &Project, clip_id: ClipId) -> Option<YankedAttrs> {
    let c = p.timeline.clip(clip_id)?;
    let strip_params = c
        .strip_id
        .and_then(|sid| p.strips.iter().find(|s| s.id == sid))
        .map(Strip::params);
    let grade_params = c
        .grade_id
        .and_then(|gid| p.grades.iter().find(|g| g.id == gid))
        .map(Grade::params);
    Some(YankedAttrs {
        strip_id: c.strip_id,
        strip_params,
        grade_id: c.grade_id,
        grade_params,
        crop_l: c.crop_l,
        crop_r: c.crop_r,
        crop_t: c.crop_t,
        crop_b: c.crop_b,
        nudge_x: c.nudge_x,
        nudge_y: c.nudge_y,
        rotate: c.rotate,
        fit_mode: c.fit_mode,
        scale: c.scale,
        tx: c.tx,
        ty: c.ty,
        gain_db: c.gain_db,
        pan: c.pan,
        fade_in_us: c.fade_in_us,
        fade_out_us: c.fade_out_us,
        channel_mode: c.channel_mode,
        muted: c.muted,
    })
}

/// Apply yanked attributes to every target (§6.4 `P`). Shareable objects are
/// linked (targets point at the same Strip/Grade row — a vanished row is
/// recreated from the snapshot); per-clip values are copied. One undo step.
pub fn paste_attributes(
    p: &mut Project,
    attrs: &YankedAttrs,
    targets: &[ClipId],
) -> Result<String, EditError> {
    if targets.is_empty() {
        return Err(EditError::Nothing);
    }
    // Resolve the strip row to link: recreate it if the source's row is gone.
    let link_sid = match attrs.strip_id {
        Some(sid) if p.strips.iter().any(|s| s.id == sid) => Some(sid),
        Some(_) => {
            let params = attrs.strip_params.clone().unwrap_or_default();
            let new_id = StripId(p.alloc_id());
            p.strips.push(Strip::new(new_id, &params));
            Some(new_id)
        }
        None => None,
    };
    // Resolve the grade row to link: recreate it if the source's row is gone.
    let link_gid = match attrs.grade_id {
        Some(gid) if p.grades.iter().any(|g| g.id == gid) => Some(gid),
        Some(_) => {
            let params = attrs.grade_params.unwrap_or_default();
            let new_id = GradeId(p.alloc_id());
            p.grades.push(Grade::new(new_id, &params));
            Some(new_id)
        }
        None => None,
    };
    let mut n = 0;
    for &t in targets {
        if let Some(c) = p.timeline.clip_mut(t) {
            c.strip_id = link_sid;
            c.grade_id = link_gid;
            c.crop_l = attrs.crop_l;
            c.crop_r = attrs.crop_r;
            c.crop_t = attrs.crop_t;
            c.crop_b = attrs.crop_b;
            c.nudge_x = attrs.nudge_x;
            c.nudge_y = attrs.nudge_y;
            c.rotate = attrs.rotate;
            c.fit_mode = attrs.fit_mode;
            c.scale = attrs.scale;
            c.tx = attrs.tx;
            c.ty = attrs.ty;
            c.gain_db = attrs.gain_db;
            c.pan = attrs.pan;
            c.fade_in_us = attrs.fade_in_us;
            c.fade_out_us = attrs.fade_out_us;
            c.channel_mode = attrs.channel_mode;
            c.muted = attrs.muted;
            n += 1;
        }
    }
    if n == 0 {
        return Err(EditError::NoSuchClip);
    }
    strip_gc(p);
    grade_gc(p);
    Ok(format!("Paste attributes to {n} clips"))
}

// ---------------------------------------------------------------------------
// Cross-project yank/paste (§16 "Cross-project yank")
// ---------------------------------------------------------------------------

/// An id no project can ever hold: ids are alloc'd incrementally from 0, so
/// `i64::MAX` is unreachable. `YankedAttrs::detached` uses it to force
/// `paste_attributes` down its recreate-from-snapshot path (§16).
const FOREIGN_ID: i64 = i64::MAX;

/// Everything a cross-project paste needs, captured at yank time (§16).
#[derive(Debug, Clone)]
pub struct YankPayload {
    pub clips: Vec<YankedClip>,
    /// Media rows referenced by the yanked clips, cloned at yank time.
    pub media: Vec<Media>,
    /// Shared-object params referenced by the yanked clips, keyed by their
    /// id in the SOURCE project.
    pub strips: Vec<(StripId, StripParams)>,
    pub grades: Vec<(GradeId, GradeParams)>,
    /// Graphic documents referenced by the yanked clips (§17), keyed by their
    /// id in the SOURCE project. Image elements inside a document keep their
    /// absolute paths as-is: a cross-project paste is same-machine by
    /// construction today (one app, one clipboard), so relinking images the
    /// way §9 relinks media is deferred until it can actually break.
    pub graphics: Vec<(GraphicId, GraphicDoc)>,
}

/// Yank `ids` plus everything needed to paste them into another project (§16).
/// No mutation — the clipboard is app-global, so this snapshot has to outlive
/// the project it came from.
pub fn yank_payload(p: &Project, ids: &[ClipId]) -> YankPayload {
    let clips = yank(p, ids);
    let mut media: Vec<Media> = Vec::new();
    let mut strips: Vec<(StripId, StripParams)> = Vec::new();
    let mut grades: Vec<(GradeId, GradeParams)> = Vec::new();
    let mut graphics: Vec<(GraphicId, GraphicDoc)> = Vec::new();
    for y in &clips {
        if let Some(mid) = y.clip.media_id {
            if !media.iter().any(|m| m.id == mid) {
                if let Some(m) = p.media_by_id(mid) {
                    media.push(m.clone());
                }
            }
        }
        if let Some(sid) = y.clip.strip_id {
            if !strips.iter().any(|&(id, _)| id == sid) {
                if let Some(s) = p.strips.iter().find(|s| s.id == sid) {
                    strips.push((sid, s.params()));
                }
            }
        }
        if let Some(gid) = y.clip.grade_id {
            if !grades.iter().any(|&(id, _)| id == gid) {
                if let Some(g) = p.grades.iter().find(|g| g.id == gid) {
                    grades.push((gid, g.params()));
                }
            }
        }
        if let Some(gid) = y.clip.graphic_id {
            if !graphics.iter().any(|(id, _)| *id == gid) {
                if let Some(g) = p.graphics.iter().find(|g| g.id == gid) {
                    graphics.push((gid, g.doc()));
                }
            }
        }
    }
    YankPayload {
        clips,
        media,
        strips,
        grades,
        graphics,
    }
}

/// Paste a payload yanked from ANOTHER project (§16). Media auto-imports
/// (deduped by content hash — the §8.3 cache is keyed by hash, so proxies and
/// waveforms are instantly warm in the target); shared objects are *copied*
/// into new rows, because Strip treatments and grades cannot link across `.dv`
/// files. The pasted clips stay linked among themselves. Placement is exactly
/// `paste`'s — this only remaps references and delegates.
pub fn paste_foreign(
    p: &mut Project,
    payload: &YankPayload,
    playhead_us: TimeUs,
) -> Result<Vec<ClipId>, EditError> {
    let mut media_map: std::collections::HashMap<MediaId, MediaId> =
        std::collections::HashMap::new();
    let mut strip_map: std::collections::HashMap<StripId, Option<StripId>> =
        std::collections::HashMap::new();
    let mut grade_map: std::collections::HashMap<GradeId, Option<GradeId>> =
        std::collections::HashMap::new();
    let mut graphic_map: std::collections::HashMap<GraphicId, Option<GraphicId>> =
        std::collections::HashMap::new();

    // One import / one new shared row per distinct source id actually
    // referenced by the pasted clips — unreferenced payload entries create
    // nothing.
    for y in &payload.clips {
        if let Some(src_mid) = y.clip.media_id.filter(|m| !media_map.contains_key(m)) {
            let src = payload
                .media
                .iter()
                .find(|m| m.id == src_mid)
                .ok_or(EditError::NoSuchMedia)?;
            let new_mid = match p.media.iter().find(|m| m.hash == src.hash) {
                // Already imported here (same content hash) — reuse the row.
                Some(existing) => existing.id,
                None => {
                    let mut row = src.clone();
                    row.id = MediaId(p.alloc_id());
                    let id = row.id;
                    p.media.push(row);
                    id
                }
            };
            media_map.insert(src_mid, new_mid);
        }
        if let Some(src_sid) = y.clip.strip_id.filter(|s| !strip_map.contains_key(s)) {
            let new_sid =
                payload
                    .strips
                    .iter()
                    .find(|&&(id, _)| id == src_sid)
                    .map(|(_, params)| {
                        let id = StripId(p.alloc_id());
                        p.strips.push(Strip::new(id, params));
                        id
                    });
            strip_map.insert(src_sid, new_sid);
        }
        if let Some(src_gid) = y.clip.grade_id.filter(|g| !grade_map.contains_key(g)) {
            let new_gid =
                payload
                    .grades
                    .iter()
                    .find(|&&(id, _)| id == src_gid)
                    .map(|(_, params)| {
                        let id = GradeId(p.alloc_id());
                        p.grades.push(Grade::new(id, params));
                        id
                    });
            grade_map.insert(src_gid, new_gid);
        }
        if let Some(src_id) = y.clip.graphic_id.filter(|g| !graphic_map.contains_key(g)) {
            let new_id = payload
                .graphics
                .iter()
                .find(|(id, _)| *id == src_id)
                .map(|(_, doc)| new_graphic_row(p, doc));
            graphic_map.insert(src_id, new_id);
        }
    }

    let remapped: Vec<YankedClip> = payload
        .clips
        .iter()
        .map(|y| {
            let mut y = y.clone();
            // Gap clips (media_id None) pass through untouched.
            y.clip.media_id = y.clip.media_id.map(|m| {
                *media_map
                    .get(&m)
                    .expect("every referenced media was mapped above")
            });
            y.clip.strip_id = y
                .clip
                .strip_id
                .and_then(|s| strip_map.get(&s).copied().flatten());
            y.clip.grade_id = y
                .clip
                .grade_id
                .and_then(|g| grade_map.get(&g).copied().flatten());
            y.clip.graphic_id = y
                .clip
                .graphic_id
                .and_then(|g| graphic_map.get(&g).copied().flatten());
            y
        })
        .collect();

    let ids = paste(p, &remapped, playhead_us)?;
    // `paste` deep-copies every graphic into a per-clip row (§17.2), so the
    // rows imported above were only carriers — sweep them up.
    graphic_gc(p);
    Ok(ids)
}

impl YankedAttrs {
    /// Cross-project variant of `P` (§16): shared objects cannot link across
    /// `.dv` files, so the strip/grade ids are replaced with a sentinel id no
    /// project can hold — which sends `paste_attributes` down its
    /// recreate-from-snapshot path, creating ONE new shared object in the
    /// target that every paste target links to. Ids whose params are `None`
    /// are dropped entirely (nothing to copy).
    pub fn detached(&self) -> YankedAttrs {
        let mut out = self.clone();
        match (self.strip_id, &self.strip_params) {
            (Some(_), Some(_)) => out.strip_id = Some(StripId(FOREIGN_ID)),
            _ => {
                out.strip_id = None;
                out.strip_params = None;
            }
        }
        match (self.grade_id, self.grade_params) {
            (Some(_), Some(_)) => out.grade_id = Some(GradeId(FOREIGN_ID)),
            _ => {
                out.grade_id = None;
                out.grade_params = None;
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Small clip & track setters (§5)
// ---------------------------------------------------------------------------

/// Nudge a clip's gain (§5 `Alt+↑/↓`), clamped to [−60, +12] dB. Per-clip, so
/// never shared. Coalesces under `CoalesceKey::Gain`.
pub fn nudge_gain(p: &mut Project, clip_id: ClipId, delta_db: f64) -> Result<String, EditError> {
    let c = p.timeline.clip_mut(clip_id).ok_or(EditError::NoSuchClip)?;
    let new = (c.gain_db + delta_db).clamp(-60.0, 12.0);
    if new == c.gain_db {
        return Err(EditError::Nothing);
    }
    c.gain_db = new;
    Ok(format!("Gain {new:+.1} dB"))
}

/// Set a clip's pan (§5), clamped to [−1, 1].
pub fn set_clip_pan(p: &mut Project, clip_id: ClipId, pan: f64) -> Result<String, EditError> {
    let c = p.timeline.clip_mut(clip_id).ok_or(EditError::NoSuchClip)?;
    let new = pan.clamp(-1.0, 1.0);
    if new == c.pan {
        return Err(EditError::Nothing);
    }
    c.pan = new;
    Ok(format!("Pan {new:+.2}"))
}

/// Set a clip's channel mode (§5: stereo / left / right / sum).
pub fn set_channel_mode(
    p: &mut Project,
    clip_id: ClipId,
    mode: ChannelMode,
) -> Result<String, EditError> {
    let c = p.timeline.clip_mut(clip_id).ok_or(EditError::NoSuchClip)?;
    if c.channel_mode == mode {
        return Err(EditError::Nothing);
    }
    c.channel_mode = mode;
    let name = match mode {
        ChannelMode::Stereo => "stereo",
        ChannelMode::Left => "left",
        ChannelMode::Right => "right",
        ChannelMode::Sum => "sum",
    };
    Ok(format!("Channel {name}"))
}

/// Nudge a track's gain (§5 per-track), clamped to [−60, +12] dB.
pub fn set_track_gain(
    p: &mut Project,
    kind: TrackKind,
    delta_db: f64,
) -> Result<String, EditError> {
    let t = track_mut(&mut p.tracks, kind);
    let new = (t.gain_db + delta_db).clamp(-60.0, 12.0);
    if new == t.gain_db {
        return Err(EditError::Nothing);
    }
    t.gain_db = new;
    Ok(format!(
        "{} gain {new:+.1} dB",
        kind.as_str().to_uppercase()
    ))
}

/// Toggle a track's mute (§5 per-track).
pub fn toggle_track_mute(p: &mut Project, kind: TrackKind) -> Result<String, EditError> {
    let t = track_mut(&mut p.tracks, kind);
    t.muted = !t.muted;
    let state = if t.muted { "muted" } else { "unmuted" };
    Ok(format!("{} {state}", kind.as_str().to_uppercase()))
}

/// Toggle a track's solo (§5 per-track).
pub fn toggle_track_solo(p: &mut Project, kind: TrackKind) -> Result<String, EditError> {
    let t = track_mut(&mut p.tracks, kind);
    t.solo = !t.solo;
    let state = if t.solo { "solo on" } else { "solo off" };
    Ok(format!("{} {state}", kind.as_str().to_uppercase()))
}

/// Toggle a track's ducking (§5; meaningful on A2 music but allowed anywhere).
pub fn toggle_track_duck(p: &mut Project, kind: TrackKind) -> Result<String, EditError> {
    let t = track_mut(&mut p.tracks, kind);
    t.duck = !t.duck;
    let state = if t.duck { "ducking on" } else { "ducking off" };
    Ok(format!("{} {state}", kind.as_str().to_uppercase()))
}

/// Set a track's delay-bus send level (§5), clamped to [0, 1].
pub fn set_track_send(p: &mut Project, kind: TrackKind, send: f64) -> Result<String, EditError> {
    let t = track_mut(&mut p.tracks, kind);
    let new = send.clamp(0.0, 1.0);
    if new == t.send {
        return Err(EditError::Nothing);
    }
    t.send = new;
    Ok(format!("{} send {new:.2}", kind.as_str().to_uppercase()))
}

/// Set a track's pan (§5 per-track), clamped to [−1, 1].
pub fn set_track_pan(p: &mut Project, kind: TrackKind, pan: f64) -> Result<String, EditError> {
    let t = track_mut(&mut p.tracks, kind);
    let new = pan.clamp(-1.0, 1.0);
    if new == t.pan {
        return Err(EditError::Nothing);
    }
    t.pan = new;
    Ok(format!("{} pan {new:+.2}", kind.as_str().to_uppercase()))
}

// ---------------------------------------------------------------------------
// Speed, video fades & framing (§6.4)
// ---------------------------------------------------------------------------

/// §6.4 Speed (`<`/`>`): set the active clip's playback speed, clamped
/// 0.25×–4×. V1 duration is derived from source ÷ speed, so the change ripples
/// through the sequence by construction. Refused on a Gap (no source to speed
/// up) with the same variant `slip` uses for gaps.
pub fn set_speed(p: &mut Project, clip_id: ClipId, speed: f64) -> Result<String, EditError> {
    let c = p.timeline.clip_mut(clip_id).ok_or(EditError::NoSuchClip)?;
    // No source to speed up on a gap or a graphic (§17.2).
    if c.is_gap() || c.is_graphic() {
        return Err(EditError::WrongTrack);
    }
    let new = speed.clamp(0.25, 4.0);
    if new == c.speed {
        return Err(EditError::Nothing);
    }
    c.speed = new;
    Ok(format!("Speed {new}×"))
}

/// §6.4 video fade from black (`fade_in = true`) / to black (`fade_in =
/// false`), clamped to `[0, clip timeline duration]`. A brightness multiply in
/// the display shader — the one-visible-frame invariant is untouched (§6.2).
pub fn set_video_fade(
    p: &mut Project,
    clip_id: ClipId,
    fade_in: bool,
    dur_us: TimeUs,
) -> Result<String, EditError> {
    let c = p.timeline.clip_mut(clip_id).ok_or(EditError::NoSuchClip)?;
    let new = dur_us.clamp(0, c.duration_us());
    let cur = if fade_in {
        c.vfade_in_us
    } else {
        c.vfade_out_us
    };
    if new == cur {
        return Err(EditError::Nothing);
    }
    if fade_in {
        c.vfade_in_us = new;
    } else {
        c.vfade_out_us = new;
    }
    let dir = if fade_in { "in" } else { "out" };
    Ok(format!(
        "Video fade {dir} {:.2} s",
        new as f64 / US_PER_SEC as f64
    ))
}

/// The four crop insets a Frame-mode edge operation can drive (§6.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CropEdge {
    Left,
    Right,
    Top,
    Bottom,
}

/// Frame mode (§6.4): cut an edge inward (positive `delta_px`) or grow it back
/// (negative). Crops clamp to ≥ 0 and to leaving a rect of at least 16 px on
/// that axis; the nudge is then re-clamped to the new overflow (`nudge_x` in
/// `[-crop_l, crop_r]`, `nudge_y` in `[-crop_t, crop_b]`). `src_w`/`src_h` are
/// the clip's media pixel dimensions.
pub fn crop_edge(
    p: &mut Project,
    clip_id: ClipId,
    edge: CropEdge,
    delta_px: i32,
    src_w: u32,
    src_h: u32,
) -> Result<String, EditError> {
    let c = p.timeline.clip_mut(clip_id).ok_or(EditError::NoSuchClip)?;
    let before = (c.crop_l, c.crop_r, c.crop_t, c.crop_b, c.nudge_x, c.nudge_y);
    let (w, h) = (src_w as i32, src_h as i32);
    match edge {
        // Upper bound keeps ≥ 16 px of rect on the axis (the opposite inset is
        // fixed within this op).
        CropEdge::Left => c.crop_l = (c.crop_l + delta_px).clamp(0, (w - c.crop_r - 16).max(0)),
        CropEdge::Right => c.crop_r = (c.crop_r + delta_px).clamp(0, (w - c.crop_l - 16).max(0)),
        CropEdge::Top => c.crop_t = (c.crop_t + delta_px).clamp(0, (h - c.crop_b - 16).max(0)),
        CropEdge::Bottom => c.crop_b = (c.crop_b + delta_px).clamp(0, (h - c.crop_t - 16).max(0)),
    }
    // The overflow the nudge can spend just changed — re-clamp it.
    c.nudge_x = c.nudge_x.clamp(-c.crop_l, c.crop_r);
    c.nudge_y = c.nudge_y.clamp(-c.crop_t, c.crop_b);
    if (c.crop_l, c.crop_r, c.crop_t, c.crop_b, c.nudge_x, c.nudge_y) == before {
        return Err(EditError::Nothing);
    }
    let dir = match edge {
        CropEdge::Left => "left",
        CropEdge::Right => "right",
        CropEdge::Top => "top",
        CropEdge::Bottom => "bottom",
    };
    Ok(format!("Crop {dir}"))
}

/// Frame mode (§6.4 `wasd`): slide the source rect within the source, clamped
/// to the available overflow (`nudge_x` in `[-crop_l, crop_r]`, `nudge_y` in
/// `[-crop_t, crop_b]`).
pub fn nudge_rect(p: &mut Project, clip_id: ClipId, dx: i32, dy: i32) -> Result<String, EditError> {
    let c = p.timeline.clip_mut(clip_id).ok_or(EditError::NoSuchClip)?;
    let new_x = (c.nudge_x + dx).clamp(-c.crop_l, c.crop_r);
    let new_y = (c.nudge_y + dy).clamp(-c.crop_t, c.crop_b);
    if (new_x, new_y) == (c.nudge_x, c.nudge_y) {
        return Err(EditError::Nothing);
    }
    c.nudge_x = new_x;
    c.nudge_y = new_y;
    Ok("Nudge frame".to_string())
}

/// §6.4 rotation (`r`/`R`, `Alt+r`): add `delta_deg`, normalized to `[0, 360)`.
/// `rem_euclid` keeps 90° / 0.25° steps exact multiples (no float dust when the
/// current angle and the delta are both multiples of 0.25).
pub fn rotate_clip(p: &mut Project, clip_id: ClipId, delta_deg: f64) -> Result<String, EditError> {
    let c = p.timeline.clip_mut(clip_id).ok_or(EditError::NoSuchClip)?;
    let new = (c.rotate + delta_deg).rem_euclid(360.0);
    if new == c.rotate {
        return Err(EditError::Nothing);
    }
    c.rotate = new;
    Ok(format!("Rotate {new}°"))
}

/// `F` toggle (§6.4): Fill ⇄ Fit; Custom returns to Fill. Clears the custom
/// scale/tx/ty when leaving Custom.
pub fn toggle_fit(p: &mut Project, clip_id: ClipId) -> Result<String, EditError> {
    let c = p.timeline.clip_mut(clip_id).ok_or(EditError::NoSuchClip)?;
    let (new_mode, label) = match c.fit_mode {
        FitMode::Fill => (FitMode::Fit, "Fit frame"),
        FitMode::Fit => (FitMode::Fill, "Fill frame"),
        FitMode::Custom => (FitMode::Fill, "Fill frame"),
    };
    if c.fit_mode == FitMode::Custom {
        c.scale = None;
        c.tx = None;
        c.ty = None;
    }
    c.fit_mode = new_mode;
    Ok(label.to_string())
}

/// Switch to custom framing with explicit values (§6.4: touching a manual field
/// switches to custom seeded from the current derived values — the caller
/// computes the seed via `dv_core::framing::derived`). `scale` is clamped to a
/// sane positive range (0.01–100).
pub fn set_custom_framing(
    p: &mut Project,
    clip_id: ClipId,
    scale: f64,
    tx: f64,
    ty: f64,
) -> Result<String, EditError> {
    let c = p.timeline.clip_mut(clip_id).ok_or(EditError::NoSuchClip)?;
    let scale = scale.clamp(0.01, 100.0);
    let (ns, ntx, nty) = (Some(scale), Some(tx), Some(ty));
    if c.fit_mode == FitMode::Custom && (c.scale, c.tx, c.ty) == (ns, ntx, nty) {
        return Err(EditError::Nothing);
    }
    c.fit_mode = FitMode::Custom;
    c.scale = ns;
    c.tx = ntx;
    c.ty = nty;
    Ok("Custom framing".to_string())
}

/// Frame mode `0` (§6.4/§7.3): reset all framing — crops, nudge, rotation, back
/// to Fill, custom values cleared.
pub fn reset_framing(p: &mut Project, clip_id: ClipId) -> Result<String, EditError> {
    let c = p.timeline.clip_mut(clip_id).ok_or(EditError::NoSuchClip)?;
    let is_default = (c.crop_l, c.crop_r, c.crop_t, c.crop_b) == (0, 0, 0, 0)
        && (c.nudge_x, c.nudge_y) == (0, 0)
        && c.rotate == 0.0
        && c.fit_mode == FitMode::Fill
        && (c.scale, c.tx, c.ty) == (None, None, None);
    if is_default {
        return Err(EditError::Nothing);
    }
    c.crop_l = 0;
    c.crop_r = 0;
    c.crop_t = 0;
    c.crop_b = 0;
    c.nudge_x = 0;
    c.nudge_y = 0;
    c.rotate = 0.0;
    c.fit_mode = FitMode::Fill;
    c.scale = None;
    c.tx = None;
    c.ty = None;
    Ok("Reset framing".to_string())
}

// ---------------------------------------------------------------------------
// Markers (§7.1 carry-over)
// ---------------------------------------------------------------------------

/// Rename a marker (§7.1). Unknown id → `NoSuchClip` (the shared not-found
/// variant — there is no marker-specific one).
pub fn rename_marker(
    p: &mut Project,
    marker_id: MarkerId,
    name: &str,
) -> Result<String, EditError> {
    let m = p
        .markers
        .iter_mut()
        .find(|m| m.id == marker_id)
        .ok_or(EditError::NoSuchClip)?;
    if m.name == name {
        return Err(EditError::Nothing);
    }
    m.name = name.to_string();
    Ok(if name.is_empty() {
        "Clear marker name".to_string()
    } else {
        format!("Rename marker {name}")
    })
}

/// Recolor a marker (§7.1); `None` clears the color. Unknown id → `NoSuchClip`.
pub fn set_marker_color(
    p: &mut Project,
    marker_id: MarkerId,
    color: Option<u8>,
) -> Result<String, EditError> {
    let m = p
        .markers
        .iter_mut()
        .find(|m| m.id == marker_id)
        .ok_or(EditError::NoSuchClip)?;
    if m.color == color {
        return Err(EditError::Nothing);
    }
    m.color = color;
    Ok(if color.is_some() {
        "Marker color".to_string()
    } else {
        "Clear marker color".to_string()
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)] // tests: panicking on setup failure is the point

    use super::*;
    use crate::model::{Media, MediaKind};

    const SEC: TimeUs = US_PER_SEC;
    /// 25 fps → 40 ms frames, clean numbers in µs.
    const FRAME: TimeUs = 40_000;

    /// Project: 25 fps, one 60 s media, V1 = three 10 s clips (ids 1,2,3),
    /// one V2 clip (id 10) anchored to clip 2 at +2 s, one timeline-anchored
    /// A2 clip (id 11) at 5 s.
    fn proj() -> Project {
        let mut p = Project::new("t", 0);
        p.meta.fps_num = 25;
        p.meta.fps_den = 1;
        p.media.push(Media {
            id: MediaId(100),
            path: "/tmp/src.mp4".into(),
            hash: "h".into(),
            kind: MediaKind::Video,
            duration_us: Some(60 * SEC),
            video_codec: Some("h264".into()),
            audio_codec: Some("aac".into()),
            width: Some(1920),
            height: Some(1080),
            fps_num: Some(25),
            fps_den: Some(1),
            added_at: 0,
            offline: false,
        });
        for id in 1..=3 {
            p.timeline.v1.push(Clip::new(
                ClipId(id),
                Some(MediaId(100)),
                (id - 1) * 10 * SEC,
                id * 10 * SEC,
            ));
        }
        p.timeline.v2.push(FreeClip {
            clip: Clip::new(ClipId(10), Some(MediaId(100)), 0, 3 * SEC),
            timeline_start_us: 0, // stale on purpose — anchor wins
            anchor: Some((ClipId(2), 2 * SEC)),
        });
        p.timeline.a2.push(FreeClip {
            clip: Clip::new(ClipId(11), Some(MediaId(100)), 0, 4 * SEC),
            timeline_start_us: 5 * SEC,
            anchor: None,
        });
        p.bump_id_counter(11);
        p
    }

    fn broll_start(p: &Project) -> TimeUs {
        p.timeline.free_start_us(&p.timeline.v2[0])
    }

    // -- split --------------------------------------------------------------

    #[test]
    fn split_divides_source_and_keeps_outer_fades() {
        let mut p = proj();
        {
            let c = p.timeline.clip_mut(ClipId(1)).unwrap();
            c.fade_in_us = SEC;
            c.fade_out_us = SEC;
            c.xfade_us = 500_000;
        }
        let (a, b) = split_at(&mut p, ClipId(1), 4 * SEC).unwrap();
        assert_eq!(a, ClipId(1));
        assert_eq!(p.timeline.v1.len(), 4);
        let first = p.timeline.clip(a).unwrap();
        let second = p.timeline.clip(b).unwrap();
        assert_eq!(first.source_out_us, 4 * SEC);
        assert_eq!(second.source_in_us, 4 * SEC);
        assert_eq!(second.source_out_us, 10 * SEC);
        // §5: outer edges untouched, interior cut at zero.
        assert_eq!(first.fade_in_us, SEC);
        assert_eq!(first.fade_out_us, 0);
        assert_eq!(first.xfade_us, 0);
        assert_eq!(second.fade_in_us, 0);
        assert_eq!(second.fade_out_us, SEC);
        assert_eq!(second.xfade_us, 500_000);
        assert_eq!(p.timeline.duration_us(), 30 * SEC);
    }

    #[test]
    fn split_rejects_at_edges() {
        let mut p = proj();
        assert_eq!(split_at(&mut p, ClipId(1), 0), Err(EditError::OutsideClip));
        assert_eq!(
            split_at(&mut p, ClipId(1), 10 * SEC),
            Err(EditError::OutsideClip)
        );
    }

    #[test]
    fn split_reanchors_broll_past_the_cut() {
        let mut p = proj();
        // b-roll on clip 2 at +2 s (absolute 12 s). Split clip 2 at 11 s:
        // the split is before the b-roll → it re-anchors to the second part.
        let (_, second) = split_at(&mut p, ClipId(2), 11 * SEC).unwrap();
        let fc = &p.timeline.v2[0];
        assert_eq!(fc.anchor, Some((second, SEC)));
        assert_eq!(broll_start(&p), 12 * SEC, "resolved position unchanged");

        // Split again after the b-roll start → anchor stays on that part.
        let mut p2 = proj();
        let (first, _) = split_at(&mut p2, ClipId(2), 13 * SEC).unwrap();
        assert_eq!(p2.timeline.v2[0].anchor, Some((first, 2 * SEC)));
        assert_eq!(broll_start(&p2), 12 * SEC);
    }

    #[test]
    fn split_free_clip() {
        let mut p = proj();
        // A2 clip at 5 s, 4 s long; split at 7 s.
        let (a, b) = split_at(&mut p, ClipId(11), 7 * SEC).unwrap();
        assert_eq!(p.timeline.a2.len(), 2);
        assert_eq!(p.timeline.a2[0].clip.id, a);
        assert_eq!(p.timeline.a2[1].clip.id, b);
        assert_eq!(p.timeline.free_start_us(&p.timeline.a2[1]), 7 * SEC);
        assert_eq!(p.timeline.a2[1].clip.source_in_us, 2 * SEC);
    }

    // -- ripple delete ------------------------------------------------------

    #[test]
    fn ripple_delete_closes_gap_and_travels_anchor() {
        let mut p = proj();
        assert_eq!(broll_start(&p), 12 * SEC);
        ripple_delete(&mut p, ClipId(1)).unwrap();
        assert_eq!(p.timeline.duration_us(), 20 * SEC);
        // Anchored to clip 2, which slid 10 s earlier — b-roll travels (§6.3).
        assert_eq!(broll_start(&p), 2 * SEC);
        // Timeline-anchored A2 clip stays put (§6.3 opt-out).
        assert_eq!(p.timeline.free_start_us(&p.timeline.a2[0]), 5 * SEC);
    }

    #[test]
    fn ripple_delete_of_anchor_reanchors_to_successor() {
        let mut p = proj();
        ripple_delete(&mut p, ClipId(2)).unwrap();
        // Successor (clip 3) slides into clip 2's place; b-roll keeps offset.
        assert_eq!(p.timeline.v2[0].anchor, Some((ClipId(3), 2 * SEC)));
        assert_eq!(broll_start(&p), 12 * SEC);
    }

    #[test]
    fn ripple_delete_last_reanchors_to_predecessor() {
        let mut p = proj();
        // Move the b-roll onto clip 3 first.
        p.timeline.v2[0].anchor = Some((ClipId(3), SEC));
        ripple_delete(&mut p, ClipId(3)).unwrap();
        // Predecessor anchor, offset shifted by its duration → resolved
        // position unchanged (21 s).
        assert_eq!(p.timeline.v2[0].anchor, Some((ClipId(2), 11 * SEC)));
        assert_eq!(broll_start(&p), 21 * SEC);
    }

    #[test]
    fn ripple_delete_only_clip_falls_back_to_timeline_anchor() {
        let mut p = proj();
        p.timeline.v1.truncate(1);
        p.timeline.v2[0].anchor = Some((ClipId(1), 2 * SEC));
        ripple_delete(&mut p, ClipId(1)).unwrap();
        let fc = &p.timeline.v2[0];
        assert_eq!(fc.anchor, None);
        assert_eq!(fc.timeline_start_us, 2 * SEC);
    }

    #[test]
    fn ripple_delete_clears_neighbor_crossfade() {
        let mut p = proj();
        p.timeline.v1[0].xfade_us = 300_000; // cut 1|2
        p.timeline.v1[1].xfade_us = 400_000; // cut 2|3
        ripple_delete(&mut p, ClipId(2)).unwrap();
        // Both cuts died with the clip (§5): 1's trailing cut is new.
        assert_eq!(p.timeline.v1[0].xfade_us, 0);
    }

    #[test]
    fn delete_free_clip_is_plain_remove() {
        let mut p = proj();
        ripple_delete(&mut p, ClipId(11)).unwrap();
        assert!(p.timeline.a2.is_empty());
        assert_eq!(p.timeline.duration_us(), 30 * SEC);
    }

    // -- ripple_delete_source_range (§1 silence removal) --------------------

    #[test]
    fn cut_source_range_interior_splits_and_ripples() {
        let mut p = proj();
        // Clip 2 source [10,20). Cut source [12,15) → keep [10,12) + new [15,20).
        let (second, label) =
            ripple_delete_source_range(&mut p, ClipId(2), 12 * SEC, 15 * SEC).unwrap();
        let second = second.expect("interior cut makes a second part");
        assert_eq!(label, "Cut silence src.mp4");
        assert_eq!(p.timeline.v1.len(), 4);
        let first = p.timeline.clip(ClipId(2)).unwrap();
        assert_eq!(
            (first.source_in_us, first.source_out_us),
            (10 * SEC, 12 * SEC)
        );
        let sc = p.timeline.clip(second).unwrap();
        assert_eq!((sc.source_in_us, sc.source_out_us), (15 * SEC, 20 * SEC));
        // 3 s of source removed → whole sequence shrinks by 3 s.
        assert_eq!(p.timeline.duration_us(), 27 * SEC);
    }

    #[test]
    fn cut_source_range_xfade_lifecycle() {
        let mut p = proj();
        {
            let c = p.timeline.clip_mut(ClipId(2)).unwrap();
            c.fade_in_us = SEC;
            c.fade_out_us = SEC;
            c.vfade_in_us = SEC;
            c.vfade_out_us = SEC;
            c.xfade_us = 500_000;
        }
        let (second, _) =
            ripple_delete_source_range(&mut p, ClipId(2), 12 * SEC, 15 * SEC).unwrap();
        let second = second.unwrap();
        let first = p.timeline.clip(ClipId(2)).unwrap();
        // Head stays on part one; the interior cut is clean (§5).
        assert_eq!(first.fade_in_us, SEC);
        assert_eq!(first.vfade_in_us, SEC);
        assert_eq!(first.fade_out_us, 0);
        assert_eq!(first.vfade_out_us, 0);
        assert_eq!(first.xfade_us, 0);
        let sc = p.timeline.clip(second).unwrap();
        // Tail fade-out + trailing crossfade move to part two; its head is clean.
        assert_eq!(sc.fade_in_us, 0);
        assert_eq!(sc.vfade_in_us, 0);
        assert_eq!(sc.fade_out_us, SEC);
        assert_eq!(sc.vfade_out_us, SEC);
        assert_eq!(sc.xfade_us, 500_000);
    }

    #[test]
    fn cut_source_range_start_edge_is_start_trim() {
        let mut p = proj();
        // src_start below source_in clamps to a start-trim to src_end (13 s).
        let (second, _) = ripple_delete_source_range(&mut p, ClipId(2), 8 * SEC, 13 * SEC).unwrap();
        assert_eq!(second, None, "no new clip on a start-trim");
        assert_eq!(p.timeline.v1.len(), 3);
        let c = p.timeline.clip(ClipId(2)).unwrap();
        assert_eq!((c.source_in_us, c.source_out_us), (13 * SEC, 20 * SEC));
        assert_eq!(p.timeline.duration_us(), 27 * SEC);
        // §6.3 start-trim rule: anchor offset is kept.
        assert_eq!(p.timeline.v2[0].anchor, Some((ClipId(2), 2 * SEC)));
    }

    #[test]
    fn cut_source_range_end_edge_is_end_trim() {
        let mut p = proj();
        // Anchor a b-roll clip past the cut so we can watch it re-parent.
        p.timeline.a1.push(FreeClip {
            clip: Clip::new(ClipId(20), Some(MediaId(100)), 0, SEC),
            timeline_start_us: 0,
            anchor: Some((ClipId(2), 7 * SEC)),
        });
        p.bump_id_counter(20);
        // src_end past source_out clamps to an end-trim to src_start (15 s).
        let (second, _) =
            ripple_delete_source_range(&mut p, ClipId(2), 15 * SEC, 25 * SEC).unwrap();
        assert_eq!(second, None, "no new clip on an end-trim");
        let c = p.timeline.clip(ClipId(2)).unwrap();
        assert_eq!((c.source_in_us, c.source_out_us), (10 * SEC, 15 * SEC));
        assert_eq!(p.timeline.duration_us(), 25 * SEC);
        // Anchor before the cut (off 2 s < off_a 5 s) stays on clip 2.
        assert_eq!(p.timeline.v2[0].anchor, Some((ClipId(2), 2 * SEC)));
        // Anchor past the cut (off 7 s) re-parents to the successor (clip 3),
        // offset rebased, resolved position preserved.
        let broll = &p.timeline.a1[0];
        assert_eq!(broll.anchor, Some((ClipId(3), 2 * SEC)));
        assert_eq!(p.timeline.free_start_us(broll), 17 * SEC);
    }

    #[test]
    fn cut_source_range_whole_clip_delegates_to_ripple_delete() {
        let mut p = proj();
        let (second, label) =
            ripple_delete_source_range(&mut p, ClipId(2), 8 * SEC, 25 * SEC).unwrap();
        assert_eq!(second, None);
        assert_eq!(label, "Cut silence src.mp4");
        assert_eq!(p.timeline.v1.len(), 2);
        assert_eq!(p.timeline.duration_us(), 20 * SEC);
        // ripple_delete's policy: dependents re-anchor to the successor.
        assert_eq!(p.timeline.v2[0].anchor, Some((ClipId(3), 2 * SEC)));
    }

    #[test]
    fn cut_source_range_sliver_left_folds_to_start_trim() {
        let mut p = proj();
        // Left piece < 1 frame of source → start-trim, no sliver clip.
        let (second, _) =
            ripple_delete_source_range(&mut p, ClipId(2), 10 * SEC + 20_000, 14 * SEC).unwrap();
        assert_eq!(second, None);
        assert_eq!(p.timeline.v1.len(), 3);
        assert_eq!(p.timeline.clip(ClipId(2)).unwrap().source_in_us, 14 * SEC);
    }

    #[test]
    fn cut_source_range_sliver_right_folds_to_end_trim() {
        let mut p = proj();
        // Right piece < 1 frame of source → end-trim, no sliver clip.
        let (second, _) =
            ripple_delete_source_range(&mut p, ClipId(2), 14 * SEC, 20 * SEC - 20_000).unwrap();
        assert_eq!(second, None);
        assert_eq!(p.timeline.v1.len(), 3);
        assert_eq!(p.timeline.clip(ClipId(2)).unwrap().source_out_us, 14 * SEC);
    }

    #[test]
    fn cut_source_range_reanchors_before_at_and_after() {
        let mut p = proj();
        // Three anchors on clip 2: before the cut, at the cut start, past it.
        for (id, off) in [(20, SEC), (21, 2 * SEC), (22, 6 * SEC)] {
            p.timeline.a1.push(FreeClip {
                clip: Clip::new(ClipId(id), Some(MediaId(100)), 0, SEC),
                timeline_start_us: 0,
                anchor: Some((ClipId(2), off)),
            });
        }
        p.bump_id_counter(22);
        // Cut source [12,15): off_a = 2 s, off_b = 5 s.
        let (second, _) =
            ripple_delete_source_range(&mut p, ClipId(2), 12 * SEC, 15 * SEC).unwrap();
        let second = second.unwrap();
        let anchor = |p: &Project, id: i64| {
            p.timeline
                .a1
                .iter()
                .find(|f| f.clip.id == ClipId(id))
                .unwrap()
                .anchor
        };
        // Before the cut: unchanged.
        assert_eq!(anchor(&p, 20), Some((ClipId(2), SEC)));
        // In the removed middle: lands at the second part's start.
        assert_eq!(anchor(&p, 21), Some((second, 0)));
        // Past the removed span: on the second part, offset shifted by off_b.
        assert_eq!(anchor(&p, 22), Some((second, SEC)));
    }

    #[test]
    fn cut_source_range_speed_2x_shift() {
        let mut p = proj();
        p.timeline.v1[1].speed = 2.0; // clip 2 now 5 s long (10 s source ÷ 2)
        assert_eq!(p.timeline.duration_us(), 25 * SEC);
        // Cut 4 s of source [12,16) → timeline shrinks by 4 s ÷ 2 = 2 s.
        let (second, _) =
            ripple_delete_source_range(&mut p, ClipId(2), 12 * SEC, 16 * SEC).unwrap();
        let second = second.unwrap();
        assert_eq!(p.timeline.clip(ClipId(2)).unwrap().source_out_us, 12 * SEC);
        assert_eq!(p.timeline.clip(second).unwrap().source_in_us, 16 * SEC);
        assert_eq!(p.timeline.duration_us(), 23 * SEC);
    }

    #[test]
    fn cut_source_range_rejections() {
        let mut p = proj();
        // Unknown id.
        assert_eq!(
            ripple_delete_source_range(&mut p, ClipId(999), 0, SEC),
            Err(EditError::NoSuchClip)
        );
        // Non-V1 (free-track clip).
        assert_eq!(
            ripple_delete_source_range(&mut p, ClipId(11), 0, SEC),
            Err(EditError::WrongTrack)
        );
        // Gap on V1.
        p.timeline.v1.push(Clip::new(ClipId(30), None, 0, 5 * SEC));
        assert_eq!(
            ripple_delete_source_range(&mut p, ClipId(30), 0, SEC),
            Err(EditError::WrongTrack)
        );
        // Empty range after clamping.
        assert_eq!(
            ripple_delete_source_range(&mut p, ClipId(2), 15 * SEC, 15 * SEC),
            Err(EditError::Nothing)
        );
    }

    #[test]
    fn cut_source_range_round_trips_through_perform() {
        let mut p = proj();
        let mut undo = UndoStack::new();
        perform(&mut p, &mut undo, 0, CoalesceKey::None, |p| {
            ripple_delete_source_range(p, ClipId(2), 12 * SEC, 15 * SEC).map(|(_, l)| l)
        })
        .unwrap();
        let after = p.clone();
        undo.undo(&mut p).unwrap();
        assert_eq!(p.timeline.v1.len(), 3);
        assert_eq!(p.timeline.duration_us(), 30 * SEC);
        assert!(p.peek_id_counter() >= after.peek_id_counter());
        undo.redo(&mut p).unwrap();
        assert_eq!(p.timeline, after.timeline, "redo restores identical ids");
    }

    // -- lift ---------------------------------------------------------------

    #[test]
    fn lift_leaves_equal_gap_and_keeps_anchor_position() {
        let mut p = proj();
        p.timeline.v1[0].xfade_us = 300_000;
        lift_delete(&mut p, ClipId(2)).unwrap();
        assert_eq!(p.timeline.duration_us(), 30 * SEC);
        let gap = &p.timeline.v1[1];
        assert!(gap.is_gap());
        assert_eq!(gap.duration_us(), 10 * SEC);
        assert_eq!(p.timeline.v1[0].xfade_us, 0, "gap at the cut kills xfade");
        // Anchor moved to the gap: resolved position unchanged.
        assert_eq!(p.timeline.v2[0].anchor, Some((gap.id, 2 * SEC)));
        assert_eq!(broll_start(&p), 12 * SEC);
        assert_eq!(
            lift_delete(&mut p, ClipId(11)),
            Err(EditError::WrongTrack),
            "lift is V1-only (§6.4)"
        );
    }

    // -- trim ---------------------------------------------------------------

    #[test]
    fn trim_out_clamps_to_media_end() {
        let mut p = proj();
        // Clip 3 runs 20–30 s of a 60 s source: +40 s hits the cap.
        trim(&mut p, ClipId(3), Edge::Out, 40 * SEC).unwrap();
        assert_eq!(p.timeline.clip(ClipId(3)).unwrap().source_out_us, 60 * SEC);
        assert_eq!(
            trim(&mut p, ClipId(3), Edge::Out, SEC),
            Err(EditError::Nothing)
        );
    }

    #[test]
    fn trim_in_clamps_to_source_start_and_min_length() {
        let mut p = proj();
        // Growing when already at the source start applies nothing.
        assert_eq!(
            trim(&mut p, ClipId(1), Edge::In, 5 * SEC),
            Err(EditError::Nothing)
        );
        assert_eq!(p.timeline.clip(ClipId(1)).unwrap().source_in_us, 0);
        // Shrink almost everything: stops at one frame.
        trim(&mut p, ClipId(1), Edge::In, -20 * SEC).unwrap();
        let c = p.timeline.clip(ClipId(1)).unwrap();
        assert_eq!(c.duration_us(), FRAME);
    }

    #[test]
    fn trim_free_clip_start_keeps_far_edge_planted() {
        let mut p = proj();
        // A2 clip [5,9): shrink 1 s off the start → [6,9).
        trim(&mut p, ClipId(11), Edge::In, -SEC).unwrap();
        let fc = &p.timeline.a2[0];
        assert_eq!(p.timeline.free_start_us(fc), 6 * SEC);
        assert_eq!(fc.clip.source_in_us, SEC);
        assert_eq!(fc.clip.duration_us(), 3 * SEC);
        // Same for an anchored clip: offset moves instead.
        trim(&mut p, ClipId(10), Edge::In, -SEC).unwrap();
        assert_eq!(p.timeline.v2[0].anchor, Some((ClipId(2), 3 * SEC)));
    }

    #[test]
    fn trim_to_playhead_ripple_and_gap_variants() {
        let mut p = proj();
        // `;` on clip 2 (spans 10–20 s) at playhead 13 s.
        trim_to_playhead(&mut p, ClipId(2), Edge::In, 13 * SEC, true).unwrap();
        let c = p.timeline.clip(ClipId(2)).unwrap();
        assert_eq!(c.source_in_us, 13 * SEC);
        assert_eq!(p.timeline.duration_us(), 27 * SEC);

        // Alt+' non-ripple end trim: gap of the removed length appears.
        let mut p2 = proj();
        trim_to_playhead(&mut p2, ClipId(2), Edge::Out, 18 * SEC, false).unwrap();
        assert_eq!(p2.timeline.duration_us(), 30 * SEC, "timeline length held");
        assert!(p2.timeline.v1[2].is_gap());
        assert_eq!(p2.timeline.v1[2].duration_us(), 2 * SEC);
        // Playhead outside the clip refuses.
        assert_eq!(
            trim_to_playhead(&mut p2, ClipId(1), Edge::Out, 25 * SEC, true),
            Err(EditError::OutsideClip)
        );
    }

    // -- reorder / slide ----------------------------------------------------

    #[test]
    fn reorder_travels_anchored_broll_and_clears_xfades() {
        let mut p = proj();
        p.timeline.v1[0].xfade_us = 100;
        p.timeline.v1[1].xfade_us = 200;
        reorder_v1(&mut p, ClipId(2), false).unwrap(); // 2 before 1
        assert_eq!(
            p.timeline.v1.iter().map(|c| c.id.0).collect::<Vec<_>>(),
            vec![2, 1, 3]
        );
        // b-roll anchored to clip 2 travels to the front (§6.3).
        assert_eq!(broll_start(&p), 2 * SEC);
        assert!(p.timeline.v1.iter().all(|c| c.xfade_us == 0));
        assert_eq!(
            reorder_v1(&mut p, ClipId(2), false),
            Err(EditError::Nothing),
            "already first"
        );
    }

    #[test]
    fn slide_clamps_against_neighbors_and_zero() {
        let mut p = proj();
        // Add a second A2 clip right after the first: [9, 11).
        p.timeline.a2.push(FreeClip {
            clip: Clip::new(ClipId(12), Some(MediaId(100)), 0, 2 * SEC),
            timeline_start_us: 9 * SEC,
            anchor: None,
        });
        // Slide first A2 clip [5,9) right by 5 s → clamps to butt at 9 s… it
        // already ends at 9 s, so nothing moves → Collision.
        assert_eq!(
            slide_free(&mut p, ClipId(11), 5 * SEC),
            Err(EditError::Collision)
        );
        // Left by 10 s clamps at 0.
        slide_free(&mut p, ClipId(11), -10 * SEC).unwrap();
        assert_eq!(p.timeline.free_start_us(&p.timeline.a2[0]), 0);
        // V1 refuses.
        assert_eq!(
            slide_free(&mut p, ClipId(1), SEC),
            Err(EditError::WrongTrack)
        );
    }

    // -- duplicate / slip ---------------------------------------------------

    #[test]
    fn duplicate_v1_inserts_adjacent_pair() {
        let mut p = proj();
        p.timeline.v1[0].xfade_us = 100;
        let (earlier, later) = duplicate(&mut p, ClipId(1)).unwrap();
        assert_eq!(earlier, ClipId(1));
        assert_eq!(p.timeline.v1[1].id, later);
        assert_eq!(p.timeline.v1[1].source_in_us, 0);
        assert_eq!(p.timeline.duration_us(), 40 * SEC);
        assert_eq!(p.timeline.v1[0].xfade_us, 0, "new cuts start clean");
    }

    #[test]
    fn duplicate_free_needs_room() {
        let mut p = proj();
        let (_, copy) = duplicate(&mut p, ClipId(11)).unwrap();
        assert_eq!(p.timeline.free_start_us(&p.timeline.a2[1]), 9 * SEC);
        assert_eq!(p.timeline.a2[1].clip.id, copy);
        // Duplicate the b-roll (anchored): copy anchors right after it.
        let (_, bcopy) = duplicate(&mut p, ClipId(10)).unwrap();
        assert_eq!(p.timeline.v2[1].clip.id, bcopy);
        assert_eq!(p.timeline.v2[1].anchor, Some((ClipId(2), 5 * SEC)));
        // No room: a clip butted right after blocks duplication.
        let mut p2 = proj();
        p2.timeline.a2.push(FreeClip {
            clip: Clip::new(ClipId(12), Some(MediaId(100)), 0, SEC),
            timeline_start_us: 9 * SEC,
            anchor: None,
        });
        assert_eq!(duplicate(&mut p2, ClipId(11)), Err(EditError::Collision));
    }

    #[test]
    fn slip_shifts_source_window_only() {
        let mut p = proj();
        slip(&mut p, ClipId(2), 5 * SEC).unwrap();
        let c = p.timeline.clip(ClipId(2)).unwrap();
        assert_eq!((c.source_in_us, c.source_out_us), (15 * SEC, 25 * SEC));
        assert_eq!(p.timeline.duration_us(), 30 * SEC);
        // Clamped at the source end: clip 2 can slip at most +35 s more.
        slip(&mut p, ClipId(2), 60 * SEC).unwrap();
        let c = p.timeline.clip(ClipId(2)).unwrap();
        assert_eq!((c.source_in_us, c.source_out_us), (50 * SEC, 60 * SEC));
        assert_eq!(slip(&mut p, ClipId(2), SEC), Err(EditError::Nothing));
    }

    // -- yank / paste -------------------------------------------------------

    #[test]
    fn paste_v1_mid_clip_splits_and_ripples() {
        let mut p = proj();
        let clipboard = yank(&p, &[ClipId(1)]);
        assert_eq!(clipboard.len(), 1);
        // Paste at 15 s: splits clip 2, inserts the copy between the halves.
        let ids = paste(&mut p, &clipboard, 15 * SEC).unwrap();
        assert_eq!(ids.len(), 1);
        assert_eq!(p.timeline.duration_us(), 40 * SEC);
        assert_eq!(p.timeline.v1.len(), 5);
        assert_eq!(p.timeline.v1[2].id, ids[0]);
        assert_eq!(p.timeline.v1_start_us(2), 15 * SEC);
        // The b-roll (anchor offset +2 s on clip 2) stays on the first half.
        assert_eq!(broll_start(&p), 12 * SEC);
    }

    #[test]
    fn paste_at_clip_boundary_does_not_split() {
        let mut p = proj();
        let clipboard = yank(&p, &[ClipId(3)]);
        paste(&mut p, &clipboard, 10 * SEC).unwrap();
        assert_eq!(p.timeline.v1.len(), 4);
        assert_eq!(p.timeline.v1[1].source_in_us, 20 * SEC);
        // Past the end appends.
        let mut p2 = proj();
        let cb2 = yank(&p2, &[ClipId(1)]);
        paste(&mut p2, &cb2, 99 * SEC).unwrap();
        assert_eq!(p2.timeline.v1.len(), 4);
        assert_eq!(p2.timeline.v1[3].source_in_us, 0);
    }

    #[test]
    fn paste_free_clip_anchors_under_landing_spot() {
        let mut p = proj();
        let clipboard = yank(&p, &[ClipId(11)]);
        // Paste at 12 s: lands over clip 2 (starts 10 s) → anchored (+2 s).
        let ids = paste(&mut p, &clipboard, 12 * SEC).unwrap();
        let fc = p.timeline.a2.iter().find(|f| f.clip.id == ids[0]).unwrap();
        assert_eq!(fc.anchor, Some((ClipId(2), 2 * SEC)));
        // Colliding paste shifts right to the first gap that fits: [5,9) is
        // taken and the 9–12 s gap is too short for 4 s, so it lands after
        // the clip pasted above at [12,16).
        let ids2 = paste(&mut p, &clipboard, 6 * SEC).unwrap();
        let fc2 = p.timeline.a2.iter().find(|f| f.clip.id == ids2[0]).unwrap();
        assert_eq!(p.timeline.free_start_us(fc2), 16 * SEC);
    }

    #[test]
    fn yank_multi_keeps_spacing() {
        let p = proj();
        let cb = yank(&p, &[ClipId(2), ClipId(11)]);
        // Origin is the earliest (A2 at 5 s); V1 clip 2 starts at 10 s.
        let v1 = cb.iter().find(|y| y.track == TrackKind::V1).unwrap();
        let a2 = cb.iter().find(|y| y.track == TrackKind::A2).unwrap();
        assert_eq!(a2.rel_start_us, 0);
        assert_eq!(v1.rel_start_us, 5 * SEC);
    }

    // -- markers / format ---------------------------------------------------

    #[test]
    fn marker_toggles_within_half_frame() {
        let mut p = proj();
        toggle_marker(&mut p, 5 * SEC).unwrap();
        assert_eq!(p.markers.len(), 1);
        toggle_marker(&mut p, 5 * SEC + FRAME / 4).unwrap();
        assert!(p.markers.is_empty(), "second press removes");
    }

    #[test]
    fn project_format_change() {
        let mut p = proj();
        set_project_format(&mut p, 1080, 1920, 25, 1).unwrap();
        assert_eq!((p.meta.width, p.meta.height), (1080, 1920));
        assert_eq!(
            set_project_format(&mut p, 1080, 1920, 25, 1),
            Err(EditError::Nothing)
        );
    }

    // -- perform / undo -----------------------------------------------------

    #[test]
    fn perform_round_trips_and_preserves_ids() {
        let mut p = proj();
        let mut undo = UndoStack::new();
        perform(&mut p, &mut undo, 0, CoalesceKey::None, |p| {
            split_at(p, ClipId(2), 15 * SEC).map(|_| "Split".to_string())
        })
        .unwrap();
        let after = p.clone();
        undo.undo(&mut p).unwrap();
        assert_eq!(p.timeline.v1.len(), 3);
        assert!(
            p.peek_id_counter() >= after.peek_id_counter(),
            "undo never rewinds the id allocator"
        );
        undo.redo(&mut p).unwrap();
        assert_eq!(p.timeline, after.timeline, "redo restores identical ids");
    }

    #[test]
    fn perform_rolls_back_on_error() {
        let mut p = proj();
        let before = p.timeline.clone();
        let mut undo = UndoStack::new();
        let r = perform(&mut p, &mut undo, 0, CoalesceKey::None, |p| {
            // Mutate, then fail: the mutation must not stick.
            p.timeline.v1.remove(0);
            Err(EditError::Collision)
        });
        assert_eq!(r, Err(EditError::Collision));
        assert_eq!(p.timeline, before);
        assert!(!undo.can_undo());
    }

    #[test]
    fn trim_taps_coalesce_into_one_undo_step() {
        let mut p = proj();
        let mut undo = UndoStack::new();
        for i in 0..5 {
            perform(
                &mut p,
                &mut undo,
                i * 100,
                CoalesceKey::TrimOut(ClipId(1)),
                |p| trim(p, ClipId(1), Edge::Out, -FRAME),
            )
            .unwrap();
        }
        assert_eq!(
            p.timeline.clip(ClipId(1)).unwrap().duration_us(),
            10 * SEC - 5 * FRAME
        );
        undo.undo(&mut p).unwrap();
        assert_eq!(
            p.timeline.clip(ClipId(1)).unwrap().duration_us(),
            10 * SEC,
            "five taps revert as one step"
        );
        assert!(!undo.can_undo());
    }

    #[test]
    fn different_coalesce_keys_stay_separate() {
        let mut p = proj();
        let mut undo = UndoStack::new();
        perform(&mut p, &mut undo, 0, CoalesceKey::TrimOut(ClipId(1)), |p| {
            trim(p, ClipId(1), Edge::Out, -FRAME)
        })
        .unwrap();
        perform(
            &mut p,
            &mut undo,
            100,
            CoalesceKey::TrimIn(ClipId(1)),
            |p| trim(p, ClipId(1), Edge::In, -FRAME),
        )
        .unwrap();
        undo.undo(&mut p).unwrap();
        assert!(undo.can_undo(), "two distinct steps");
    }

    #[test]
    fn no_op_edit_is_not_recorded() {
        let mut p = proj();
        let mut undo = UndoStack::new();
        let r = perform(&mut p, &mut undo, 0, CoalesceKey::None, |_| {
            Ok("nothing".to_string())
        });
        assert_eq!(r, Err(EditError::Nothing));
        assert!(!undo.can_undo());
    }

    // -- edge fades & crossfades (§5) ---------------------------------------

    /// One media of `dur` and two butted V1 clips over the given source
    /// ranges (ids 1, 2). `dur = None` = image (infinite handle).
    fn two_clip_proj(dur: Option<TimeUs>, a: (TimeUs, TimeUs), b: (TimeUs, TimeUs)) -> Project {
        let mut p = Project::new("t", 0);
        p.meta.fps_num = 25;
        p.meta.fps_den = 1;
        p.media.push(Media {
            id: MediaId(100),
            path: "/tmp/src.mp4".into(),
            hash: "h".into(),
            kind: if dur.is_some() {
                MediaKind::Video
            } else {
                MediaKind::Image
            },
            duration_us: dur,
            video_codec: Some("h264".into()),
            audio_codec: Some("aac".into()),
            width: Some(1920),
            height: Some(1080),
            fps_num: Some(25),
            fps_den: Some(1),
            added_at: 0,
            offline: false,
        });
        p.timeline
            .v1
            .push(Clip::new(ClipId(1), Some(MediaId(100)), a.0, a.1));
        p.timeline
            .v1
            .push(Clip::new(ClipId(2), Some(MediaId(100)), b.0, b.1));
        p.bump_id_counter(2);
        p
    }

    #[test]
    fn crossfade_grows_shrinks_and_clamps_to_duration() {
        // Both handles ≥ 5 s (A right 10 s, B left 3 s → xfade ≤ 6 s); the 5 s
        // clip durations bind first.
        let mut p = two_clip_proj(Some(20 * SEC), (5 * SEC, 10 * SEC), (3 * SEC, 8 * SEC));
        // `)` on clip 1 (edge Out) drives the 1|2 crossfade, stored on clip 1.
        edge_fade(&mut p, ClipId(1), Edge::Out, 2 * SEC).unwrap();
        assert_eq!(p.timeline.v1[0].xfade_us, 2 * SEC);
        // Same cut via `(` on clip 2 (edge In) grows the same value.
        edge_fade(&mut p, ClipId(2), Edge::In, SEC).unwrap();
        assert_eq!(p.timeline.v1[0].xfade_us, 3 * SEC);
        // Grow past the limit clamps to the shorter clip duration (5 s).
        edge_fade(&mut p, ClipId(1), Edge::Out, 20 * SEC).unwrap();
        assert_eq!(p.timeline.v1[0].xfade_us, 5 * SEC);
        // Already at max → Nothing.
        assert_eq!(
            edge_fade(&mut p, ClipId(1), Edge::Out, SEC),
            Err(EditError::Nothing)
        );
        // Shrink.
        edge_fade(&mut p, ClipId(1), Edge::Out, -2 * SEC).unwrap();
        assert_eq!(p.timeline.v1[0].xfade_us, 3 * SEC);
        // Cannot go below zero.
        edge_fade(&mut p, ClipId(1), Edge::Out, -20 * SEC).unwrap();
        assert_eq!(p.timeline.v1[0].xfade_us, 0);
    }

    #[test]
    fn crossfade_clamps_to_later_side_handle() {
        // A right handle 10 s (source_out 20 of 30 s cap), B left handle 2 s
        // (source_in 2 s) → xfade ≤ 4 s; durations 10 s each don't bind.
        let mut p = two_clip_proj(Some(30 * SEC), (10 * SEC, 20 * SEC), (2 * SEC, 12 * SEC));
        edge_fade(&mut p, ClipId(1), Edge::Out, 20 * SEC).unwrap();
        assert_eq!(p.timeline.v1[0].xfade_us, 4 * SEC, "later handle binds");
    }

    #[test]
    fn crossfade_clamps_to_earlier_side_handle() {
        // A right handle 2 s (source_out 28 of 30 s cap) → xfade ≤ 4 s; B left
        // handle 10 s doesn't bind; durations 18 s / 10 s don't bind.
        let mut p = two_clip_proj(Some(30 * SEC), (10 * SEC, 28 * SEC), (10 * SEC, 20 * SEC));
        edge_fade(&mut p, ClipId(1), Edge::Out, 20 * SEC).unwrap();
        assert_eq!(p.timeline.v1[0].xfade_us, 4 * SEC, "earlier handle binds");
    }

    #[test]
    fn crossfade_earlier_image_has_infinite_handle() {
        // Earlier clip is an image (no media duration) → earlier handle
        // infinite; later handle (source_in 6 s) bounds xfade to 12 s, but the
        // 5 s / 8 s durations bind first at 5 s.
        let mut p = two_clip_proj(None, (0, 5 * SEC), (6 * SEC, 14 * SEC));
        edge_fade(&mut p, ClipId(1), Edge::Out, 30 * SEC).unwrap();
        assert_eq!(p.timeline.v1[0].xfade_us, 5 * SEC);
    }

    #[test]
    fn edge_fade_on_free_clip_sets_plain_fade() {
        let mut p = proj();
        // A2 clip 11 is on a free track → plain fade.
        edge_fade(&mut p, ClipId(11), Edge::In, 500_000).unwrap();
        assert_eq!(p.timeline.clip(ClipId(11)).unwrap().fade_in_us, 500_000);
        assert_eq!(p.timeline.clip(ClipId(11)).unwrap().xfade_us, 0);
        edge_fade(&mut p, ClipId(11), Edge::Out, 300_000).unwrap();
        assert_eq!(p.timeline.clip(ClipId(11)).unwrap().fade_out_us, 300_000);
    }

    #[test]
    fn edge_fade_at_timeline_edges_is_plain_fade() {
        let mut p = proj();
        // Clip 1 leading edge touches the timeline start (no predecessor).
        edge_fade(&mut p, ClipId(1), Edge::In, SEC).unwrap();
        assert_eq!(p.timeline.v1[0].fade_in_us, SEC);
        assert_eq!(p.timeline.v1[0].xfade_us, 0);
        // Clip 3 trailing edge touches the timeline end (no successor).
        edge_fade(&mut p, ClipId(3), Edge::Out, SEC).unwrap();
        assert_eq!(p.timeline.v1[2].fade_out_us, SEC);
        assert_eq!(p.timeline.v1[2].xfade_us, 0);
    }

    #[test]
    fn edge_fade_beside_a_gap_is_plain_fade() {
        let mut p = proj();
        lift_delete(&mut p, ClipId(2)).unwrap(); // clip 2 → Gap at index 1
                                                 // Clip 3's leading edge now touches a Gap → plain fade, not xfade.
        edge_fade(&mut p, ClipId(3), Edge::In, SEC).unwrap();
        assert_eq!(p.timeline.v1[2].fade_in_us, SEC);
        assert_eq!(p.timeline.v1[2].xfade_us, 0);
    }

    #[test]
    fn plain_fades_never_cross() {
        let mut p = proj();
        // Free A2 clip 11 is 4 s. Set fade_out to 3 s, then grow fade_in past 1 s.
        edge_fade(&mut p, ClipId(11), Edge::Out, 3 * SEC).unwrap();
        edge_fade(&mut p, ClipId(11), Edge::In, 9 * SEC).unwrap();
        let c = p.timeline.clip(ClipId(11)).unwrap();
        assert_eq!(c.fade_in_us, SEC, "clamped to duration − fade_out");
        assert_eq!(c.fade_out_us, 3 * SEC);
    }

    #[test]
    fn edge_fade_rejects_gaps() {
        let mut p = proj();
        lift_delete(&mut p, ClipId(2)).unwrap();
        let gap_id = p.timeline.v1[1].id;
        assert!(p.timeline.v1[1].is_gap());
        assert_eq!(
            edge_fade(&mut p, gap_id, Edge::In, SEC),
            Err(EditError::WrongTrack)
        );
    }

    // -- b-roll insert (§6.4) -----------------------------------------------

    #[test]
    fn insert_broll_anchors_and_mutes() {
        let mut p = proj();
        p.timeline.v2.clear(); // start with an empty V2 lane
        let (id, label) = insert_broll(&mut p, MediaId(100), 12 * SEC, 0, 3 * SEC).unwrap();
        assert!(label.starts_with("Insert b-roll"));
        let fc = p.timeline.v2.iter().find(|f| f.clip.id == id).unwrap();
        // 12 s lands over clip 2 (starts 10 s) → anchored (+2 s).
        assert_eq!(fc.anchor, Some((ClipId(2), 2 * SEC)));
        assert!(fc.clip.muted, "b-roll audio defaults off");
        assert_eq!(fc.clip.duration_us(), 3 * SEC);
    }

    #[test]
    fn insert_broll_clamps_duration_to_next_clip() {
        let mut p = proj();
        // Existing V2 clip 10 resolves to [12, 15). Insert at 8 s wanting 10 s
        // → room is 12 − 8 = 4 s.
        let (id, _) = insert_broll(&mut p, MediaId(100), 8 * SEC, 0, 10 * SEC).unwrap();
        let fc = p.timeline.v2.iter().find(|f| f.clip.id == id).unwrap();
        assert_eq!(fc.clip.duration_us(), 4 * SEC);
    }

    #[test]
    fn insert_broll_collides_inside_existing_clip() {
        let mut p = proj();
        // 13 s is inside the existing [12, 15) V2 clip → no room.
        assert_eq!(
            insert_broll(&mut p, MediaId(100), 13 * SEC, 0, 2 * SEC),
            Err(EditError::Collision)
        );
    }

    #[test]
    fn insert_broll_past_end_is_timeline_anchored() {
        let mut p = proj();
        p.timeline.v2.clear();
        let (id, _) = insert_broll(&mut p, MediaId(100), 99 * SEC, 0, 2 * SEC).unwrap();
        let fc = p.timeline.v2.iter().find(|f| f.clip.id == id).unwrap();
        assert_eq!(fc.anchor, None);
        assert_eq!(fc.timeline_start_us, 99 * SEC);
    }

    #[test]
    fn insert_broll_leaves_v1_crossfades_alone() {
        let mut p = proj();
        p.timeline.v1[0].xfade_us = 300_000; // cut 1|2
        p.timeline.v1[1].xfade_us = 400_000; // cut 2|3
        p.timeline.v2.clear();
        insert_broll(&mut p, MediaId(100), 11 * SEC, 0, 3 * SEC).unwrap();
        assert_eq!(p.timeline.v1[0].xfade_us, 300_000);
        assert_eq!(p.timeline.v1[1].xfade_us, 400_000);
    }

    // -- free-track audio insert (§9) ---------------------------------------

    #[test]
    fn insert_free_audio_shifts_right_on_collision() {
        let mut p = proj();
        // A2 clip 11 occupies [5, 9). Insert 4 s at 6 s → shifts to 9 s.
        let (id, label) =
            insert_free_audio(&mut p, TrackKind::A2, MediaId(100), 6 * SEC, 0, 4 * SEC).unwrap();
        assert!(label.starts_with("Insert audio"));
        let fc = p.timeline.a2.iter().find(|f| f.clip.id == id).unwrap();
        assert_eq!(p.timeline.free_start_us(fc), 9 * SEC);
        assert!(!fc.clip.muted, "audio inserts are not muted");
    }

    #[test]
    fn insert_free_audio_rejects_v1_and_v2() {
        let mut p = proj();
        assert_eq!(
            insert_free_audio(&mut p, TrackKind::V1, MediaId(100), 0, 0, SEC),
            Err(EditError::WrongTrack)
        );
        assert_eq!(
            insert_free_audio(&mut p, TrackKind::V2, MediaId(100), 0, 0, SEC),
            Err(EditError::WrongTrack)
        );
    }

    // -- strip linking (§5) -------------------------------------------------

    fn interview() -> StripParams {
        StripParams::builtin_presets()
            .into_iter()
            .find(|(n, _)| *n == "Interview")
            .unwrap()
            .1
    }

    #[test]
    fn set_strip_params_creates_one_row_then_edits_in_place() {
        let mut p = proj();
        set_strip_params(&mut p, ClipId(1), &interview()).unwrap();
        assert_eq!(p.strips.len(), 1);
        let sid = p.timeline.clip(ClipId(1)).unwrap().strip_id.unwrap();
        // A second edit reuses the same row (no new allocation).
        let voiceover = StripParams::builtin_presets()
            .into_iter()
            .find(|(n, _)| *n == "Voiceover")
            .unwrap()
            .1;
        set_strip_params(&mut p, ClipId(1), &voiceover).unwrap();
        assert_eq!(p.strips.len(), 1);
        assert_eq!(p.timeline.clip(ClipId(1)).unwrap().strip_id, Some(sid));
        assert_eq!(p.strips[0].params(), voiceover);
    }

    #[test]
    fn set_strip_params_flat_on_sole_referent_drops_row() {
        let mut p = proj();
        set_strip_params(&mut p, ClipId(1), &interview()).unwrap();
        assert_eq!(p.strips.len(), 1);
        set_strip_params(&mut p, ClipId(1), &StripParams::default()).unwrap();
        assert!(p.strips.is_empty(), "flat sole referent drops to None + GC");
        assert_eq!(p.timeline.clip(ClipId(1)).unwrap().strip_id, None);
    }

    #[test]
    fn link_and_unlink_share_and_split_rows() {
        let mut p = proj();
        set_strip_params(&mut p, ClipId(1), &interview()).unwrap();
        let sid = p.timeline.clip(ClipId(1)).unwrap().strip_id.unwrap();
        link_strip(&mut p, ClipId(1), &[ClipId(2), ClipId(3)]).unwrap();
        assert_eq!(strip_refs(&p, sid), 3);
        assert_eq!(p.timeline.clip(ClipId(2)).unwrap().strip_id, Some(sid));
        // Editing through any linked clip changes the shared row.
        let voiceover = StripParams::builtin_presets()
            .into_iter()
            .find(|(n, _)| *n == "Voiceover")
            .unwrap()
            .1;
        set_strip_params(&mut p, ClipId(2), &voiceover).unwrap();
        assert_eq!(p.strips.len(), 1);
        assert_eq!(p.timeline.clip(ClipId(1)).unwrap().strip_id, Some(sid));
        // Unlink clip 2 → a private clone; the shared row loses one referent.
        unlink_strip(&mut p, ClipId(2)).unwrap();
        assert_eq!(p.strips.len(), 2);
        assert_eq!(strip_refs(&p, sid), 2);
        let sid2 = p.timeline.clip(ClipId(2)).unwrap().strip_id.unwrap();
        assert_ne!(sid2, sid);
        // Unlinking an unshared clip does nothing.
        assert_eq!(unlink_strip(&mut p, ClipId(2)), Err(EditError::Nothing));
    }

    #[test]
    fn link_strip_from_flat_clip_allocates_a_row() {
        let mut p = proj();
        // Clip 1 has no strip; linking still gives everyone a shared (flat) row.
        link_strip(&mut p, ClipId(1), &[ClipId(2)]).unwrap();
        assert_eq!(p.strips.len(), 1);
        let sid = p.timeline.clip(ClipId(1)).unwrap().strip_id.unwrap();
        assert_eq!(p.timeline.clip(ClipId(2)).unwrap().strip_id, Some(sid));
    }

    #[test]
    fn ripple_delete_gcs_last_strip_referent() {
        let mut p = proj();
        // A strip only on the A2 clip 11.
        set_strip_params(&mut p, ClipId(11), &interview()).unwrap();
        assert_eq!(p.strips.len(), 1);
        ripple_delete(&mut p, ClipId(11)).unwrap();
        assert!(
            p.strips.is_empty(),
            "deleting the last referent GCs the row"
        );
    }

    // -- grade linking (§15) ------------------------------------------------

    fn warm() -> GradeParams {
        GradeParams {
            temperature: 0.3,
            exposure: 0.25,
            ..GradeParams::default()
        }
    }

    fn cool() -> GradeParams {
        GradeParams {
            temperature: -0.4,
            saturation: 1.2,
            ..GradeParams::default()
        }
    }

    #[test]
    fn set_grade_params_creates_one_row_then_edits_in_place() {
        let mut p = proj();
        set_grade_params(&mut p, ClipId(1), &warm()).unwrap();
        assert_eq!(p.grades.len(), 1);
        let gid = p.timeline.clip(ClipId(1)).unwrap().grade_id.unwrap();
        // A second edit reuses the same row (no new allocation).
        set_grade_params(&mut p, ClipId(1), &cool()).unwrap();
        assert_eq!(p.grades.len(), 1);
        assert_eq!(p.timeline.clip(ClipId(1)).unwrap().grade_id, Some(gid));
        assert_eq!(p.grades[0].params(), cool());
    }

    #[test]
    fn set_grade_params_identity_on_sole_referent_drops_row() {
        let mut p = proj();
        set_grade_params(&mut p, ClipId(1), &warm()).unwrap();
        assert_eq!(p.grades.len(), 1);
        set_grade_params(&mut p, ClipId(1), &GradeParams::default()).unwrap();
        assert!(
            p.grades.is_empty(),
            "identity sole referent drops to None + GC"
        );
        assert_eq!(p.timeline.clip(ClipId(1)).unwrap().grade_id, None);
        // An identity grade on an ungraded clip is a no-op.
        assert_eq!(
            set_grade_params(&mut p, ClipId(1), &GradeParams::default()),
            Err(EditError::Nothing)
        );
    }

    #[test]
    fn link_and_unlink_share_and_split_grade_rows() {
        let mut p = proj();
        set_grade_params(&mut p, ClipId(1), &warm()).unwrap();
        let gid = p.timeline.clip(ClipId(1)).unwrap().grade_id.unwrap();
        link_grade(&mut p, ClipId(1), &[ClipId(2), ClipId(3)]).unwrap();
        assert_eq!(grade_refs(&p, gid), 3);
        assert_eq!(p.timeline.clip(ClipId(2)).unwrap().grade_id, Some(gid));
        // Editing through any linked clip changes the shared row.
        set_grade_params(&mut p, ClipId(2), &cool()).unwrap();
        assert_eq!(p.grades.len(), 1);
        assert_eq!(p.timeline.clip(ClipId(1)).unwrap().grade_id, Some(gid));
        assert_eq!(p.grades[0].params(), cool());
        // Unlink clip 2 → a private clone; the shared row loses one referent.
        unlink_grade(&mut p, ClipId(2)).unwrap();
        assert_eq!(p.grades.len(), 2);
        assert_eq!(grade_refs(&p, gid), 2);
        let gid2 = p.timeline.clip(ClipId(2)).unwrap().grade_id.unwrap();
        assert_ne!(gid2, gid);
        // Unlinking an unshared clip does nothing.
        assert_eq!(unlink_grade(&mut p, ClipId(2)), Err(EditError::Nothing));
    }

    #[test]
    fn link_grade_from_ungraded_clip_allocates_a_row() {
        let mut p = proj();
        // Clip 1 has no grade; linking still gives everyone a shared (identity) row.
        link_grade(&mut p, ClipId(1), &[ClipId(2)]).unwrap();
        assert_eq!(p.grades.len(), 1);
        let gid = p.timeline.clip(ClipId(1)).unwrap().grade_id.unwrap();
        assert_eq!(p.timeline.clip(ClipId(2)).unwrap().grade_id, Some(gid));
    }

    #[test]
    fn ripple_delete_gcs_last_grade_referent() {
        let mut p = proj();
        // A grade only on the A2 clip 11.
        set_grade_params(&mut p, ClipId(11), &warm()).unwrap();
        assert_eq!(p.grades.len(), 1);
        ripple_delete(&mut p, ClipId(11)).unwrap();
        assert!(
            p.grades.is_empty(),
            "deleting the last referent GCs the row"
        );
    }

    // -- paste attributes (§6.4) --------------------------------------------

    #[test]
    fn paste_attributes_links_strips_but_copies_per_clip_values() {
        let mut p = proj();
        set_strip_params(&mut p, ClipId(1), &interview()).unwrap();
        {
            let c = p.timeline.clip_mut(ClipId(1)).unwrap();
            c.gain_db = 5.0;
            c.pan = -0.5;
            c.fade_in_us = SEC;
            c.crop_l = 10;
            c.muted = true;
        }
        let attrs = yank_attributes(&p, ClipId(1)).unwrap();
        paste_attributes(&mut p, &attrs, &[ClipId(2), ClipId(3)]).unwrap();
        let sid = p.timeline.clip(ClipId(1)).unwrap().strip_id;
        for id in [ClipId(2), ClipId(3)] {
            let c = p.timeline.clip(id).unwrap();
            assert_eq!(c.strip_id, sid, "strip is linked (same row)");
            assert_eq!(c.gain_db, 5.0, "gain is copied");
            assert_eq!(c.pan, -0.5);
            assert_eq!(c.fade_in_us, SEC);
            assert_eq!(c.crop_l, 10);
            assert!(c.muted);
        }
        // Only one shared row for all three clips.
        assert_eq!(p.strips.len(), 1);
        assert_eq!(strip_refs(&p, sid.unwrap()), 3);
    }

    #[test]
    fn paste_attributes_recreates_a_vanished_strip_row() {
        let mut p = proj();
        set_strip_params(&mut p, ClipId(1), &interview()).unwrap();
        let attrs = yank_attributes(&p, ClipId(1)).unwrap();
        // Deleting clip 1 GCs its (sole) strip row.
        ripple_delete(&mut p, ClipId(1)).unwrap();
        assert!(p.strips.is_empty());
        paste_attributes(&mut p, &attrs, &[ClipId(2)]).unwrap();
        assert_eq!(p.strips.len(), 1, "row recreated from the snapshot");
        assert_eq!(p.strips[0].params(), interview());
        assert!(p.timeline.clip(ClipId(2)).unwrap().strip_id.is_some());
    }

    #[test]
    fn paste_attributes_links_grades_and_recreates_vanished_row() {
        let mut p = proj();
        set_grade_params(&mut p, ClipId(1), &warm()).unwrap();
        let attrs = yank_attributes(&p, ClipId(1)).unwrap();
        let gid = p.timeline.clip(ClipId(1)).unwrap().grade_id;
        // Live row: targets link to the very same grade.
        paste_attributes(&mut p, &attrs, &[ClipId(2), ClipId(3)]).unwrap();
        assert_eq!(p.grades.len(), 1);
        assert_eq!(grade_refs(&p, gid.unwrap()), 3);
        assert_eq!(p.timeline.clip(ClipId(2)).unwrap().grade_id, gid);
        // Vanished row: recreated at a NEW id from the snapshot.
        let mut p = proj();
        set_grade_params(&mut p, ClipId(1), &warm()).unwrap();
        let attrs = yank_attributes(&p, ClipId(1)).unwrap();
        ripple_delete(&mut p, ClipId(1)).unwrap();
        assert!(p.grades.is_empty());
        paste_attributes(&mut p, &attrs, &[ClipId(2)]).unwrap();
        assert_eq!(p.grades.len(), 1, "row recreated from the snapshot");
        assert_eq!(p.grades[0].params(), warm());
        let new_gid = p.timeline.clip(ClipId(2)).unwrap().grade_id.unwrap();
        assert_ne!(Some(new_gid), attrs.grade_id, "recreated at a new id");
    }

    // -- cross-project paste (§16) ------------------------------------------

    /// An empty 25 fps project with no media and no clips — the "other tab".
    fn other_proj() -> Project {
        let mut p = Project::new("other", 0);
        p.meta.fps_num = 25;
        p.meta.fps_den = 1;
        p
    }

    #[test]
    fn paste_foreign_imports_media_with_a_fresh_id() {
        let src = proj();
        let payload = yank_payload(&src, &[ClipId(2)]);
        assert_eq!(payload.media.len(), 1);
        let mut dst = other_proj();
        let ids = paste_foreign(&mut dst, &payload, 0).unwrap();
        assert_eq!(ids.len(), 1);
        assert_eq!(dst.media.len(), 1, "media auto-imported (§16)");
        let m = &dst.media[0];
        assert_ne!(m.id, MediaId(100), "fresh id in the target project");
        assert_eq!(m.hash, "h");
        assert_eq!(m.path, std::path::PathBuf::from("/tmp/src.mp4"));
        assert_eq!(m.duration_us, Some(60 * SEC));
        // Same source range plays back, now pointing at the imported row.
        let c = dst.timeline.clip(ids[0]).unwrap();
        assert_eq!(c.media_id, Some(m.id));
        assert_eq!((c.source_in_us, c.source_out_us), (10 * SEC, 20 * SEC));
    }

    #[test]
    fn paste_foreign_reuses_media_with_the_same_hash() {
        let src = proj();
        let payload = yank_payload(&src, &[ClipId(1)]);
        // Target already holds the same content under a different id.
        let mut dst = proj();
        let before = dst.media.len();
        let ids = paste_foreign(&mut dst, &payload, 0).unwrap();
        assert_eq!(dst.media.len(), before, "deduped by content hash (§16)");
        assert_eq!(
            dst.timeline.clip(ids[0]).unwrap().media_id,
            Some(MediaId(100))
        );
    }

    #[test]
    fn paste_foreign_copies_one_shared_strip_for_all_pasted_clips() {
        let mut src = proj();
        set_strip_params(&mut src, ClipId(1), &interview()).unwrap();
        let sid = src.timeline.clip(ClipId(1)).unwrap().strip_id.unwrap();
        src.timeline.clip_mut(ClipId(2)).unwrap().strip_id = Some(sid);
        let payload = yank_payload(&src, &[ClipId(1), ClipId(2)]);
        assert_eq!(payload.strips.len(), 1, "one entry per distinct source id");

        let mut dst = other_proj();
        let ids = paste_foreign(&mut dst, &payload, 0).unwrap();
        assert_eq!(dst.strips.len(), 1, "ONE new shared strip in the target");
        let new_sid = dst.strips[0].id;
        assert_ne!(new_sid, sid, "never links to the source project's row");
        assert_eq!(dst.strips[0].params(), interview());
        for id in ids {
            assert_eq!(dst.timeline.clip(id).unwrap().strip_id, Some(new_sid));
        }
    }

    #[test]
    fn paste_foreign_copies_one_shared_grade_for_all_pasted_clips() {
        let mut src = proj();
        set_grade_params(&mut src, ClipId(1), &warm()).unwrap();
        let gid = src.timeline.clip(ClipId(1)).unwrap().grade_id.unwrap();
        src.timeline.clip_mut(ClipId(2)).unwrap().grade_id = Some(gid);
        let payload = yank_payload(&src, &[ClipId(1), ClipId(2)]);
        assert_eq!(payload.grades.len(), 1);

        let mut dst = other_proj();
        let ids = paste_foreign(&mut dst, &payload, 0).unwrap();
        assert_eq!(dst.grades.len(), 1, "ONE new shared grade in the target");
        let new_gid = dst.grades[0].id;
        assert_ne!(new_gid, gid);
        assert_eq!(dst.grades[0].params(), warm());
        for id in ids {
            assert_eq!(dst.timeline.clip(id).unwrap().grade_id, Some(new_gid));
        }
    }

    #[test]
    fn paste_foreign_keeps_multi_clip_spacing() {
        let src = proj();
        // V1 clip 2 (10 s) + A2 clip 11 (5 s) → rel spacing 5 s (see
        // `yank_multi_keeps_spacing`).
        let payload = yank_payload(&src, &[ClipId(2), ClipId(11)]);
        let mut dst = other_proj();
        paste_foreign(&mut dst, &payload, 3 * SEC).unwrap();
        assert_eq!(dst.timeline.a2.len(), 1);
        assert_eq!(dst.timeline.free_start_us(&dst.timeline.a2[0]), 3 * SEC);
        assert_eq!(dst.timeline.v1.len(), 1, "empty target: V1 appends");
        // Both tracks reference the one imported media row.
        assert_eq!(dst.media.len(), 1);
        let mid = Some(dst.media[0].id);
        assert_eq!(dst.timeline.v1[0].media_id, mid);
        assert_eq!(dst.timeline.a2[0].clip.media_id, mid);
    }

    #[test]
    fn detached_attrs_recreate_shared_objects_in_a_foreign_project() {
        let mut src = proj();
        set_strip_params(&mut src, ClipId(1), &interview()).unwrap();
        set_grade_params(&mut src, ClipId(1), &warm()).unwrap();
        src.timeline.clip_mut(ClipId(1)).unwrap().gain_db = 5.0;
        let attrs = yank_attributes(&src, ClipId(1)).unwrap().detached();

        let mut dst = proj();
        paste_attributes(&mut dst, &attrs, &[ClipId(2), ClipId(3)]).unwrap();
        assert_eq!(dst.strips.len(), 1, "one new strip row in the target");
        assert_eq!(dst.grades.len(), 1, "one new grade row in the target");
        let new_sid = dst.strips[0].id;
        let new_gid = dst.grades[0].id;
        assert_eq!(dst.strips[0].params(), interview());
        assert_eq!(dst.grades[0].params(), warm());
        for id in [ClipId(2), ClipId(3)] {
            let c = dst.timeline.clip(id).unwrap();
            assert_eq!(c.strip_id, Some(new_sid), "all targets share one row");
            assert_eq!(c.grade_id, Some(new_gid));
            assert_eq!(c.gain_db, 5.0, "per-clip values still copied");
        }
        // Nothing to copy → dropped entirely.
        let plain = yank_attributes(&dst, ClipId(1)).unwrap().detached();
        assert!(plain.strip_id.is_none() && plain.strip_params.is_none());
        assert!(plain.grade_id.is_none() && plain.grade_params.is_none());
    }

    #[test]
    fn paste_foreign_needs_the_media_row_but_gaps_pass_through() {
        let src = proj();
        let mut payload = yank_payload(&src, &[ClipId(1)]);
        payload.media.clear();
        let mut dst = other_proj();
        assert_eq!(
            paste_foreign(&mut dst, &payload, 0),
            Err(EditError::NoSuchMedia)
        );
        // A gap clip carries no media_id — it pastes without any import.
        let mut src2 = proj();
        lift_delete(&mut src2, ClipId(1)).unwrap();
        assert!(src2.timeline.v1[0].is_gap());
        let payload2 = yank_payload(&src2, &[src2.timeline.v1[0].id]);
        assert!(payload2.media.is_empty());
        let ids = paste_foreign(&mut dst, &payload2, 0).unwrap();
        assert!(dst.media.is_empty());
        assert!(dst.timeline.clip(ids[0]).unwrap().is_gap());
    }

    // -- small setters (§5) -------------------------------------------------

    #[test]
    fn clip_gain_pan_channel_clamp_and_set() {
        let mut p = proj();
        nudge_gain(&mut p, ClipId(1), 100.0).unwrap();
        assert_eq!(p.timeline.clip(ClipId(1)).unwrap().gain_db, 12.0);
        nudge_gain(&mut p, ClipId(1), -1000.0).unwrap();
        assert_eq!(p.timeline.clip(ClipId(1)).unwrap().gain_db, -60.0);
        set_clip_pan(&mut p, ClipId(1), -5.0).unwrap();
        assert_eq!(p.timeline.clip(ClipId(1)).unwrap().pan, -1.0);
        set_channel_mode(&mut p, ClipId(1), ChannelMode::Sum).unwrap();
        assert_eq!(
            p.timeline.clip(ClipId(1)).unwrap().channel_mode,
            ChannelMode::Sum
        );
        assert_eq!(
            set_channel_mode(&mut p, ClipId(1), ChannelMode::Sum),
            Err(EditError::Nothing)
        );
    }

    #[test]
    fn track_setters_clamp_and_toggle() {
        let mut p = proj();
        set_track_gain(&mut p, TrackKind::A2, 100.0).unwrap();
        assert_eq!(p.tracks.a2.gain_db, 12.0);
        toggle_track_mute(&mut p, TrackKind::A2).unwrap();
        assert!(p.tracks.a2.muted);
        toggle_track_solo(&mut p, TrackKind::A1).unwrap();
        assert!(p.tracks.a1.solo);
        toggle_track_duck(&mut p, TrackKind::A2).unwrap();
        assert!(p.tracks.a2.duck);
        set_track_send(&mut p, TrackKind::A2, 5.0).unwrap();
        assert_eq!(p.tracks.a2.send, 1.0);
        assert_eq!(
            set_track_send(&mut p, TrackKind::A2, 5.0),
            Err(EditError::Nothing)
        );
    }

    // -- undo restores strips + tracks --------------------------------------

    #[test]
    fn perform_reverts_strips_and_tracks() {
        let mut p = proj();
        let mut undo = UndoStack::new();
        perform(&mut p, &mut undo, 0, CoalesceKey::None, |p| {
            set_strip_params(p, ClipId(1), &interview())
        })
        .unwrap();
        perform(&mut p, &mut undo, 1000, CoalesceKey::None, |p| {
            set_track_gain(p, TrackKind::A2, 3.0)
        })
        .unwrap();
        assert_eq!(p.strips.len(), 1);
        assert_eq!(p.tracks.a2.gain_db, 3.0);
        // Undo the track gain: tracks revert, strip stays.
        undo.undo(&mut p).unwrap();
        assert_eq!(p.tracks.a2.gain_db, 0.0);
        assert_eq!(p.strips.len(), 1);
        // Undo the strip edit: strips revert.
        undo.undo(&mut p).unwrap();
        assert!(p.strips.is_empty());
        assert_eq!(p.timeline.clip(ClipId(1)).unwrap().strip_id, None);
        // Redo re-applies both.
        undo.redo(&mut p).unwrap();
        undo.redo(&mut p).unwrap();
        assert_eq!(p.strips.len(), 1);
        assert_eq!(p.tracks.a2.gain_db, 3.0);
    }

    #[test]
    fn perform_reverts_grades() {
        let mut p = proj();
        let mut undo = UndoStack::new();
        perform(&mut p, &mut undo, 0, CoalesceKey::None, |p| {
            set_grade_params(p, ClipId(1), &warm())
        })
        .unwrap();
        perform(&mut p, &mut undo, 1000, CoalesceKey::None, |p| {
            set_grade_params(p, ClipId(1), &cool())
        })
        .unwrap();
        assert_eq!(p.grades.len(), 1);
        assert_eq!(p.grades[0].params(), cool());
        // Undo the second edit: the row reverts to the earlier params.
        undo.undo(&mut p).unwrap();
        assert_eq!(p.grades[0].params(), warm());
        // Undo the first: the row is gone again.
        undo.undo(&mut p).unwrap();
        assert!(p.grades.is_empty());
        assert_eq!(p.timeline.clip(ClipId(1)).unwrap().grade_id, None);
        // Redo re-applies both.
        undo.redo(&mut p).unwrap();
        undo.redo(&mut p).unwrap();
        assert_eq!(p.grades.len(), 1);
        assert_eq!(p.grades[0].params(), cool());
    }

    #[test]
    fn gain_nudges_coalesce_into_one_step() {
        let mut p = proj();
        let mut undo = UndoStack::new();
        for i in 0..3 {
            perform(
                &mut p,
                &mut undo,
                i * 100,
                CoalesceKey::Gain(ClipId(1)),
                |p| nudge_gain(p, ClipId(1), 1.0),
            )
            .unwrap();
        }
        assert_eq!(p.timeline.clip(ClipId(1)).unwrap().gain_db, 3.0);
        undo.undo(&mut p).unwrap();
        assert_eq!(p.timeline.clip(ClipId(1)).unwrap().gain_db, 0.0);
        assert!(!undo.can_undo(), "three nudges revert as one step");
    }

    // -- speed (§6.4) -------------------------------------------------------

    #[test]
    fn speed_clamps_and_reflows_v1() {
        let mut p = proj();
        // Clamp high: 100× → 4×; clip 1 (10 s src) is now 2.5 s.
        set_speed(&mut p, ClipId(1), 100.0).unwrap();
        assert_eq!(p.timeline.clip(ClipId(1)).unwrap().speed, 4.0);
        assert_eq!(
            p.timeline.clip(ClipId(1)).unwrap().duration_us(),
            10 * SEC / 4
        );
        // Duration is derived → clip 2 slides earlier by construction (V1 layout).
        assert_eq!(p.timeline.v1_start_us(1), 10 * SEC / 4);
        // Clamp low: 0× → 0.25×; clip 1 is now 40 s and clip 2 starts at 40 s.
        set_speed(&mut p, ClipId(1), 0.0).unwrap();
        assert_eq!(p.timeline.clip(ClipId(1)).unwrap().speed, 0.25);
        assert_eq!(p.timeline.v1_start_us(1), 40 * SEC);
        // No change → Nothing; unknown clip → NoSuchClip.
        assert_eq!(set_speed(&mut p, ClipId(1), 0.25), Err(EditError::Nothing));
        assert_eq!(
            set_speed(&mut p, ClipId(999), 2.0),
            Err(EditError::NoSuchClip)
        );
    }

    #[test]
    fn speed_rejects_gap() {
        let mut p = proj();
        lift_delete(&mut p, ClipId(2)).unwrap();
        let gap_id = p.timeline.v1[1].id;
        assert!(p.timeline.v1[1].is_gap());
        assert_eq!(set_speed(&mut p, gap_id, 2.0), Err(EditError::WrongTrack));
    }

    // -- video fades (§6.4) -------------------------------------------------

    #[test]
    fn video_fade_clamps_to_clip_duration() {
        let mut p = proj();
        // Clip 1 is 10 s.
        set_video_fade(&mut p, ClipId(1), true, 3 * SEC).unwrap();
        assert_eq!(p.timeline.clip(ClipId(1)).unwrap().vfade_in_us, 3 * SEC);
        // Over the duration clamps to 10 s.
        set_video_fade(&mut p, ClipId(1), false, 99 * SEC).unwrap();
        assert_eq!(p.timeline.clip(ClipId(1)).unwrap().vfade_out_us, 10 * SEC);
        // Negative clamps to 0.
        set_video_fade(&mut p, ClipId(1), true, -SEC).unwrap();
        assert_eq!(p.timeline.clip(ClipId(1)).unwrap().vfade_in_us, 0);
        // No change → Nothing.
        assert_eq!(
            set_video_fade(&mut p, ClipId(1), true, 0),
            Err(EditError::Nothing)
        );
    }

    // -- framing: crop & nudge (§6.4) ---------------------------------------

    #[test]
    fn crop_clamps_zero_and_16px_floor() {
        let mut p = proj();
        // Media is 1920×1080.
        crop_edge(&mut p, ClipId(1), CropEdge::Left, 100, 1920, 1080).unwrap();
        assert_eq!(p.timeline.clip(ClipId(1)).unwrap().crop_l, 100);
        // Grow past the width: clamps to leave 16 px (crop_r = 0) → 1904.
        crop_edge(&mut p, ClipId(1), CropEdge::Left, 5000, 1920, 1080).unwrap();
        assert_eq!(p.timeline.clip(ClipId(1)).unwrap().crop_l, 1920 - 16);
        // Grow past below zero: clamps to 0.
        crop_edge(&mut p, ClipId(1), CropEdge::Left, -5000, 1920, 1080).unwrap();
        assert_eq!(p.timeline.clip(ClipId(1)).unwrap().crop_l, 0);
        // No change → Nothing.
        assert_eq!(
            crop_edge(&mut p, ClipId(1), CropEdge::Left, -10, 1920, 1080),
            Err(EditError::Nothing)
        );
        // Unknown clip.
        assert_eq!(
            crop_edge(&mut p, ClipId(999), CropEdge::Top, 5, 1920, 1080),
            Err(EditError::NoSuchClip)
        );
    }

    #[test]
    fn crop_reclamps_nudge_to_new_overflow() {
        let mut p = proj();
        // crop_r = 200, then push nudge_x to its +200 max.
        crop_edge(&mut p, ClipId(1), CropEdge::Right, 200, 1920, 1080).unwrap();
        nudge_rect(&mut p, ClipId(1), 500, 0).unwrap();
        assert_eq!(p.timeline.clip(ClipId(1)).unwrap().nudge_x, 200);
        // Shrink crop_r to 50 → nudge_x re-clamps down to 50.
        crop_edge(&mut p, ClipId(1), CropEdge::Right, -150, 1920, 1080).unwrap();
        let c = p.timeline.clip(ClipId(1)).unwrap();
        assert_eq!(c.crop_r, 50);
        assert_eq!(c.nudge_x, 50);
    }

    #[test]
    fn nudge_clamps_to_crop_overflow() {
        let mut p = proj();
        crop_edge(&mut p, ClipId(1), CropEdge::Left, 30, 1920, 1080).unwrap();
        crop_edge(&mut p, ClipId(1), CropEdge::Right, 40, 1920, 1080).unwrap();
        // nudge_x lives in [-30, 40].
        nudge_rect(&mut p, ClipId(1), -100, 0).unwrap();
        assert_eq!(p.timeline.clip(ClipId(1)).unwrap().nudge_x, -30);
        nudge_rect(&mut p, ClipId(1), 1000, 0).unwrap();
        assert_eq!(p.timeline.clip(ClipId(1)).unwrap().nudge_x, 40);
        // No vertical overflow (crops 0) and no horizontal move → Nothing.
        assert_eq!(
            nudge_rect(&mut p, ClipId(1), 0, 50),
            Err(EditError::Nothing)
        );
    }

    // -- framing: rotation (§6.4) -------------------------------------------

    #[test]
    fn rotation_normalizes_and_stays_exact() {
        let mut p = proj();
        rotate_clip(&mut p, ClipId(1), 90.0).unwrap();
        assert_eq!(p.timeline.clip(ClipId(1)).unwrap().rotate, 90.0);
        // 90 + 270 = 360 wraps to exactly 0.
        rotate_clip(&mut p, ClipId(1), 270.0).unwrap();
        assert_eq!(p.timeline.clip(ClipId(1)).unwrap().rotate, 0.0);
        // Negative delta wraps to exactly 270.
        rotate_clip(&mut p, ClipId(1), -90.0).unwrap();
        assert_eq!(p.timeline.clip(ClipId(1)).unwrap().rotate, 270.0);
        // 0.25° step stays exact.
        rotate_clip(&mut p, ClipId(1), 0.25).unwrap();
        assert_eq!(p.timeline.clip(ClipId(1)).unwrap().rotate, 270.25);
        // A full turn is a no-op.
        assert_eq!(
            rotate_clip(&mut p, ClipId(1), 360.0),
            Err(EditError::Nothing)
        );
        // Negative from zero wraps exactly.
        rotate_clip(&mut p, ClipId(2), -0.25).unwrap();
        assert_eq!(p.timeline.clip(ClipId(2)).unwrap().rotate, 359.75);
    }

    // -- framing: fit / custom / reset (§6.4) -------------------------------

    #[test]
    fn toggle_fit_cycles_and_clears_custom() {
        let mut p = proj();
        // Default Fill → Fit → Fill.
        toggle_fit(&mut p, ClipId(1)).unwrap();
        assert_eq!(p.timeline.clip(ClipId(1)).unwrap().fit_mode, FitMode::Fit);
        toggle_fit(&mut p, ClipId(1)).unwrap();
        assert_eq!(p.timeline.clip(ClipId(1)).unwrap().fit_mode, FitMode::Fill);
        // From Custom → Fill, clearing the custom values.
        set_custom_framing(&mut p, ClipId(1), 1.5, 10.0, -5.0).unwrap();
        assert_eq!(
            p.timeline.clip(ClipId(1)).unwrap().fit_mode,
            FitMode::Custom
        );
        toggle_fit(&mut p, ClipId(1)).unwrap();
        let c = p.timeline.clip(ClipId(1)).unwrap();
        assert_eq!(c.fit_mode, FitMode::Fill);
        assert_eq!((c.scale, c.tx, c.ty), (None, None, None));
    }

    #[test]
    fn set_custom_framing_clamps_scale() {
        let mut p = proj();
        set_custom_framing(&mut p, ClipId(1), 1000.0, 5.0, 6.0).unwrap();
        let c = p.timeline.clip(ClipId(1)).unwrap();
        assert_eq!(c.fit_mode, FitMode::Custom);
        assert_eq!(c.scale, Some(100.0));
        assert_eq!((c.tx, c.ty), (Some(5.0), Some(6.0)));
        // Clamp low end.
        set_custom_framing(&mut p, ClipId(1), 0.0, 5.0, 6.0).unwrap();
        assert_eq!(p.timeline.clip(ClipId(1)).unwrap().scale, Some(0.01));
        // Same values → Nothing.
        assert_eq!(
            set_custom_framing(&mut p, ClipId(1), 0.01, 5.0, 6.0),
            Err(EditError::Nothing)
        );
    }

    #[test]
    fn reset_framing_clears_everything() {
        let mut p = proj();
        {
            let c = p.timeline.clip_mut(ClipId(1)).unwrap();
            c.crop_l = 5;
            c.crop_r = 6;
            c.crop_t = 7;
            c.crop_b = 8;
            c.nudge_x = 2;
            c.nudge_y = -3;
            c.rotate = 90.0;
            c.fit_mode = FitMode::Custom;
            c.scale = Some(2.0);
            c.tx = Some(1.0);
            c.ty = Some(1.0);
        }
        reset_framing(&mut p, ClipId(1)).unwrap();
        let c = p.timeline.clip(ClipId(1)).unwrap();
        assert_eq!((c.crop_l, c.crop_r, c.crop_t, c.crop_b), (0, 0, 0, 0));
        assert_eq!((c.nudge_x, c.nudge_y), (0, 0));
        assert_eq!(c.rotate, 0.0);
        assert_eq!(c.fit_mode, FitMode::Fill);
        assert_eq!((c.scale, c.tx, c.ty), (None, None, None));
        // Already default → Nothing.
        assert_eq!(reset_framing(&mut p, ClipId(1)), Err(EditError::Nothing));
    }

    // -- markers (§7.1) -----------------------------------------------------

    #[test]
    fn marker_rename_and_recolor() {
        let mut p = proj();
        toggle_marker(&mut p, 5 * SEC).unwrap();
        let mid = p.markers[0].id;
        rename_marker(&mut p, mid, "Intro").unwrap();
        assert_eq!(p.markers[0].name, "Intro");
        set_marker_color(&mut p, mid, Some(3)).unwrap();
        assert_eq!(p.markers[0].color, Some(3));
        set_marker_color(&mut p, mid, None).unwrap();
        assert_eq!(p.markers[0].color, None);
        // No-ops.
        assert_eq!(rename_marker(&mut p, mid, "Intro"), Err(EditError::Nothing));
        assert_eq!(set_marker_color(&mut p, mid, None), Err(EditError::Nothing));
        // Unknown marker id → the shared not-found variant.
        assert_eq!(
            rename_marker(&mut p, MarkerId(999), "x"),
            Err(EditError::NoSuchClip)
        );
        assert_eq!(
            set_marker_color(&mut p, MarkerId(999), Some(1)),
            Err(EditError::NoSuchClip)
        );
    }

    // -- graphics (§17) -----------------------------------------------------

    fn doc(text: &str) -> GraphicDoc {
        use crate::graphic::{ElementKind, GraphicElement, TextBlock};
        let mut d = GraphicDoc::default();
        d.elements.push(GraphicElement {
            kind: ElementKind::Text(TextBlock {
                content: text.into(),
                ..TextBlock::default()
            }),
            ..GraphicElement::default()
        });
        d
    }

    fn doc_of(p: &Project, clip: ClipId) -> GraphicDoc {
        graphic_of(p, p.timeline.clip(clip).unwrap()).unwrap()
    }

    #[test]
    fn insert_graphic_v1_ripples_at_the_playhead() {
        let mut p = proj();
        // Mid-clip: the clip under the playhead splits, the card lands between.
        let id = insert_graphic_v1(&mut p, &doc("Chapter one"), 4 * SEC, None).unwrap();
        assert_eq!(p.timeline.v1.len(), 5);
        assert_eq!(p.timeline.v1[1].id, id);
        assert_eq!(p.timeline.v1_start_us(1), 4 * SEC);
        let c = p.timeline.clip(id).unwrap();
        assert!(c.is_graphic() && !c.is_gap());
        assert_eq!(c.media_id, None);
        assert_eq!((c.source_in_us, c.source_out_us), (0, 5 * SEC));
        // Default duration is the still convention; V1 grew by exactly that.
        assert_eq!(p.timeline.duration_us(), 35 * SEC);
        assert_eq!(p.graphics.len(), 1);
        assert_eq!(graphic_refs(&p, p.graphics[0].id), 1);
        assert_eq!(doc_of(&p, id).title(), "Chapter one");
        // An explicit duration is honored; past the end it appends.
        let tail = insert_graphic_v1(&mut p, &doc("End"), 999 * SEC, Some(2 * SEC)).unwrap();
        assert_eq!(p.timeline.v1.last().unwrap().id, tail);
        assert_eq!(p.timeline.duration_us(), 37 * SEC);
        // Sub-frame durations are refused.
        assert_eq!(
            insert_graphic_v1(&mut p, &doc("x"), 0, Some(1)),
            Err(EditError::Nothing)
        );
    }

    #[test]
    fn insert_graphic_g_anchors_clamps_and_refuses() {
        let mut p = proj();
        let (a, label) = insert_graphic_g(&mut p, &doc("Lower third"), 12 * SEC, None).unwrap();
        assert_eq!(label, "Insert graphic Lower third");
        assert_eq!(p.timeline.g.len(), 1);
        // Anchored under the V1 clip it starts over (§6.3 default).
        assert_eq!(p.timeline.g[0].anchor, Some((ClipId(2), 2 * SEC)));
        assert_eq!(p.timeline.free_start_us(&p.timeline.g[0]), 12 * SEC);
        assert_eq!(p.timeline.clip(a).unwrap().duration_us(), 5 * SEC);

        // Landing inside the existing graphic → no room at all.
        assert_eq!(
            insert_graphic_g(&mut p, &doc("x"), 13 * SEC, None),
            Err(EditError::Collision)
        );
        // Landing before it clamps the duration to the free gap.
        let (b, _) = insert_graphic_g(&mut p, &doc("Squeezed"), 10 * SEC, None).unwrap();
        assert_eq!(p.timeline.clip(b).unwrap().duration_us(), 2 * SEC);
        assert_eq!(p.graphics.len(), 2);
        // Ripple-deleting the V1 anchor carries the graphic with it (§6.3).
        ripple_delete(&mut p, ClipId(1)).unwrap();
        assert_eq!(p.timeline.free_start_us(&p.timeline.g[0]), 2 * SEC);
    }

    #[test]
    fn set_graphic_doc_edits_in_place_and_refuses_non_graphics() {
        let mut p = proj();
        let id = insert_graphic_v1(&mut p, &doc("First"), 0, None).unwrap();
        let gid = p.timeline.clip(id).unwrap().graphic_id.unwrap();
        set_graphic_doc(&mut p, id, &doc("Second")).unwrap();
        assert_eq!(p.graphics.len(), 1, "edited in place — no new row");
        assert_eq!(p.timeline.clip(id).unwrap().graphic_id, Some(gid));
        assert_eq!(doc_of(&p, id).title(), "Second");
        // Rewriting the same document changes nothing.
        assert_eq!(
            set_graphic_doc(&mut p, id, &doc("Second")),
            Err(EditError::Nothing)
        );
        // A media clip has no document to write.
        assert_eq!(
            set_graphic_doc(&mut p, ClipId(1), &doc("x")),
            Err(EditError::WrongTrack)
        );
        assert_eq!(
            set_graphic_doc(&mut p, ClipId(999), &doc("x")),
            Err(EditError::NoSuchClip)
        );
        // A vanished row is rematerialized at the same id.
        p.graphics.clear();
        set_graphic_doc(&mut p, id, &doc("Third")).unwrap();
        assert_eq!(p.graphics.len(), 1);
        assert_eq!(p.graphics[0].id, gid);
    }

    #[test]
    fn split_duplicate_and_paste_deep_copy_the_document() {
        let mut p = proj();
        let id = insert_graphic_v1(&mut p, &doc("Shared?"), 0, Some(6 * SEC)).unwrap();

        let (a, b) = split_at(&mut p, id, 3 * SEC).unwrap();
        assert_eq!(p.graphics.len(), 2, "split gives the halves separate rows");
        let (ga, gb) = (
            p.timeline.clip(a).unwrap().graphic_id.unwrap(),
            p.timeline.clip(b).unwrap().graphic_id.unwrap(),
        );
        assert_ne!(ga, gb);
        // Editing one half leaves the other untouched.
        set_graphic_doc(&mut p, b, &doc("Only b")).unwrap();
        assert_eq!(doc_of(&p, a).title(), "Shared?");
        assert_eq!(doc_of(&p, b).title(), "Only b");

        let (_, copy) = duplicate(&mut p, a).unwrap();
        assert_eq!(p.graphics.len(), 3);
        assert_ne!(
            p.timeline.clip(copy).unwrap().graphic_id,
            p.timeline.clip(a).unwrap().graphic_id
        );
        set_graphic_doc(&mut p, copy, &doc("Only the copy")).unwrap();
        assert_eq!(doc_of(&p, a).title(), "Shared?");

        // Paste: another private row, even after the source row is gone.
        let yanked = yank(&p, &[a]);
        assert_eq!(yanked[0].graphic.as_ref().unwrap().title(), "Shared?");
        ripple_delete(&mut p, a).unwrap();
        assert_eq!(p.graphics.len(), 2, "delete GC'd the source row");
        let pasted = paste(&mut p, &yanked, 0).unwrap();
        assert_eq!(p.graphics.len(), 3);
        assert_eq!(doc_of(&p, pasted[0]).title(), "Shared?");
        assert_eq!(graphic_refs(&p, p.graphics[2].id), 1);
    }

    #[test]
    fn deleting_a_graphic_clip_gcs_its_row() {
        let mut p = proj();
        let v1 = insert_graphic_v1(&mut p, &doc("Card"), 0, None).unwrap();
        let (g, _) = insert_graphic_g(&mut p, &doc("Overlay"), 25 * SEC, None).unwrap();
        assert_eq!(p.graphics.len(), 2);
        ripple_delete(&mut p, g).unwrap();
        assert_eq!(p.graphics.len(), 1);
        // Lift-delete replaces the card with a Gap — the row goes with it.
        lift_delete(&mut p, v1).unwrap();
        assert!(p.graphics.is_empty());
        assert!(p.timeline.v1[0].is_gap());
    }

    #[test]
    fn graphic_clips_refuse_source_and_opacity_operations() {
        let mut p = proj();
        let id = insert_graphic_v1(&mut p, &doc("Card"), 0, None).unwrap();
        assert_eq!(slip(&mut p, id, SEC), Err(EditError::WrongTrack));
        assert_eq!(set_speed(&mut p, id, 2.0), Err(EditError::WrongTrack));
        assert_eq!(
            edge_fade(&mut p, id, Edge::In, 100_000),
            Err(EditError::WrongTrack)
        );
        assert_eq!(
            ripple_delete_source_range(&mut p, id, 0, SEC).err(),
            Some(EditError::WrongTrack)
        );
        // The neighbor's cut against a graphic takes the plain-fade path, not
        // the crossfade one (§17.3: graphics have no audio handles).
        edge_fade(&mut p, ClipId(1), Edge::In, 200_000).unwrap();
        assert_eq!(p.timeline.clip(ClipId(1)).unwrap().fade_in_us, 200_000);
        assert_eq!(p.timeline.clip(id).unwrap().xfade_us, 0);
        // Trim still works: a graphic's source is unbounded like a still.
        trim(&mut p, id, Edge::Out, 3 * SEC).unwrap();
        assert_eq!(p.timeline.clip(id).unwrap().duration_us(), 8 * SEC);
    }

    #[test]
    fn graphic_edits_undo_redo_and_coalesce_as_one_session() {
        let mut p = proj();
        let mut undo = UndoStack::new();
        let id = perform(&mut p, &mut undo, 0, CoalesceKey::None, |p| {
            insert_graphic_v1(p, &doc("Take 1"), 0, None).map(|_| "Insert graphic".to_string())
        })
        .map(|_| p.timeline.v1[0].id)
        .unwrap();

        // A whole editor session coalesces however long it lasts (§17.6).
        for (i, text) in ["Take 2", "Take 3"].iter().enumerate() {
            perform(
                &mut p,
                &mut undo,
                60_000 * (i as u64 + 1),
                CoalesceKey::GraphicEdit(id),
                |p| set_graphic_doc(p, id, &doc(text)),
            )
            .unwrap();
        }
        assert_eq!(doc_of(&p, id).title(), "Take 3");
        undo.undo(&mut p).unwrap();
        assert_eq!(doc_of(&p, id).title(), "Take 1", "one step for the session");
        undo.redo(&mut p).unwrap();
        assert_eq!(doc_of(&p, id).title(), "Take 3");
        // Undoing the insert takes the row with it.
        undo.undo(&mut p).unwrap();
        undo.undo(&mut p).unwrap();
        assert!(p.graphics.is_empty());
        undo.redo(&mut p).unwrap();
        assert_eq!(p.graphics.len(), 1);
        // A sealed session does not absorb the next edit.
        undo.seal();
        perform(
            &mut p,
            &mut undo,
            60_001,
            CoalesceKey::GraphicEdit(id),
            |p| set_graphic_doc(p, id, &doc("Later")),
        )
        .unwrap();
        undo.undo(&mut p).unwrap();
        assert_eq!(doc_of(&p, id).title(), "Take 1");
    }

    #[test]
    fn yank_payload_carries_documents_across_projects() {
        let mut src = proj();
        let a = insert_graphic_v1(&mut src, &doc("Card A"), 0, None).unwrap();
        let (b, _) = insert_graphic_g(&mut src, &doc("Card B"), 30 * SEC, None).unwrap();
        let payload = yank_payload(&src, &[a, b]);
        assert_eq!(payload.graphics.len(), 2);

        let mut dst = other_proj();
        let ids = paste_foreign(&mut dst, &payload, 0).unwrap();
        assert_eq!(ids.len(), 2);
        assert_eq!(dst.graphics.len(), 2, "one private row per pasted clip");
        let titles: Vec<String> = ids.iter().map(|&id| doc_of(&dst, id).title()).collect();
        assert!(titles.contains(&"Card A".to_string()));
        assert!(titles.contains(&"Card B".to_string()));
        for &id in &ids {
            let gid = dst.timeline.clip(id).unwrap().graphic_id.unwrap();
            assert_eq!(graphic_refs(&dst, gid), 1, "never shared (§17.2)");
            assert!(dst.graphics.iter().any(|g| g.id == gid), "id remapped");
        }
        assert_eq!(dst.timeline.g.len(), 1, "the G-track clip stays on G");
        // Editing one pasted document leaves the source project alone.
        set_graphic_doc(&mut dst, ids[0], &doc("Changed")).unwrap();
        assert_eq!(doc_of(&src, a).title(), "Card A");
    }
}
