//! The `w` task panel (PLAN §5): what the worker pool is doing, and the three
//! things you can do about it — pause, cancel, look at the error.
//!
//! The engine's [`snapshot`](df_core::tasks::TaskEngine::snapshot) is the render
//! source, as its docs insist: the event stream is for *reacting* (recording a
//! journal entry, raising a toast), and a panel that rebuilt itself from events
//! would drift the first time one was published while the panel was shut.
//!
//! Row text is a pure function of a snapshot, so "what does a paused copy of an
//! unmeasured directory say" is a unit test rather than something you have to
//! catch a task mid-flight to see.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use df_core::tasks::{TaskId, TaskSnapshot, TaskState};

use crate::chrome::{self, CARD_PAD, FONT, PAD_X};
use crate::format::human_size;
use crate::hover::{pressed_rect, Hovers};
use crate::motion::{Easing, Tween};
use crate::ripple::Ripples;
use crate::theme::mix;
use crate::ui::{Control, Painting};

/// One task's row. Two lines: the name, and the bar with its counts under it —
/// a task's name is the thing you look for, and its numbers are what you look
/// at once you have found it.
const ROW: f32 = 40.0;

/// The progress bar's height. Thin: it is a gauge, not a widget.
const BAR: f32 = 4.0;

/// How many rows the panel shows before it scrolls. The engine keeps 128
/// finished tasks (`MAX_FINISHED_TASKS`), which is a history rather than a
/// screenful; eight is about as many as can be taken in at once. At most: a
/// window too short for eight gets as many as fit ([`geometry`]).
const VISIBLE: usize = 8;

/// The panel's width, in points. Wide enough for "Copy 12 items → /very/long…"
/// plus a state chip without the name being reduced to an ellipsis.
const WIDTH: f32 = 460.0;

/// How long the bar takes to travel to a new value.
///
/// Progress arrives in jumps — a chunk, a file — and a bar that teleported
/// would read as a series of unrelated states rather than as one thing filling.
/// A quarter of a second on the plan's slide curve is long enough to see the
/// movement and short enough that the bar is never behind the numbers beside
/// it.
const FILL_TIME: Duration = Duration::from_millis(260);

/// A target this close to where the bar already is is not worth retargeting
/// for: sub-half-percent moves are invisible, and restarting the tween on each
/// one would mean the bar never finishes and the panel never stops asking for
/// frames.
const FILL_EPSILON: f32 = 0.005;

/// How a state reads, in a word and a colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    Running,
    Waiting,
    Paused,
    Good,
    Bad,
    Quiet,
}

/// One panel row, already worded.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskRow {
    pub id: TaskId,
    pub name: String,
    /// The state chip's text.
    pub state: String,
    pub tone: Tone,
    /// `None` for a task whose total is not known yet — an indeterminate bar,
    /// never a zero-length one pretending to be progress.
    pub fraction: Option<f32>,
    /// The counts under the bar: files, and bytes.
    pub counts: String,
    /// The failure's text, shown when the row is inspected (`Enter`).
    pub error: Option<String>,
}

/// Word one snapshot.
pub fn row_for(task: &TaskSnapshot) -> TaskRow {
    let (state, tone, error) = match &task.state {
        TaskState::Pending => ("queued".to_string(), Tone::Waiting, None),
        TaskState::Running(_) => ("running".to_string(), Tone::Running, None),
        TaskState::Paused(_) => ("paused".to_string(), Tone::Paused, None),
        TaskState::Cancelled => ("cancelled".to_string(), Tone::Quiet, None),
        TaskState::Done => ("done".to_string(), Tone::Good, None),
        TaskState::Failed { error, retries } => (
            if *retries > 0 {
                format!("failed, retry {retries}")
            } else {
                "failed".to_string()
            },
            Tone::Bad,
            Some(error.clone()),
        ),
    };
    let progress = task.state.progress();
    let counts = match progress {
        // A job that has not measured its work yet says so: "0 / 0 files" would
        // be a number the user could act on, and it is not one.
        Some(p) if p.bytes_total == 0 && p.files_total == 0 => "sizing up the work…".to_string(),
        Some(p) => {
            let mut parts = Vec::new();
            if p.files_total > 0 {
                parts.push(format!(
                    "{} / {} files",
                    df_core::text::grouped(p.files_done),
                    df_core::text::grouped(p.files_total)
                ));
            }
            if p.bytes_total > 0 {
                parts.push(format!(
                    "{} / {}",
                    human_size(p.bytes_done),
                    human_size(p.bytes_total)
                ));
            }
            parts.join(" · ")
        }
        None => String::new(),
    };
    TaskRow {
        id: task.id,
        name: task.name.clone(),
        state,
        tone,
        fraction: progress.and_then(|p| p.fraction()),
        counts,
        error,
    }
}

/// The `w` panel's own state.
#[derive(Debug)]
pub struct TaskPanel {
    pub cursor: usize,
    /// `Enter` expands the selected row's error text.
    pub inspect: bool,
    /// First visible row.
    pub first: usize,
    /// Each bar's animated fill.
    fills: HashMap<TaskId, Tween>,
    /// How many rows the panel showed when it was last drawn: what the cursor
    /// scrolls by. Fewer than [`VISIBLE`] in a window too short for eight
    /// ([`TaskPanel::fit`]).
    shown: usize,
    /// When the rows last scrolled, for their bar.
    bar: crate::scrollbar::Linger,
    /// The wheel's roll that has not come to a whole row yet
    /// ([`crate::mouse::roll`]).
    carry: f32,
    /// The wheel has scrolled the rows off the cursor, and they stay where it
    /// left them until a key or a click moves the cursor: the panes' rule
    /// ([`crate::tab::Listing::attach`]).
    detached: bool,
}

impl Default for TaskPanel {
    fn default() -> TaskPanel {
        TaskPanel {
            cursor: 0,
            inspect: false,
            first: 0,
            fills: HashMap::new(),
            shown: VISIBLE,
            bar: crate::scrollbar::Linger::default(),
            carry: 0.0,
            detached: false,
        }
    }
}

impl TaskPanel {
    pub fn new() -> TaskPanel {
        TaskPanel::default()
    }

    pub fn move_cursor(&mut self, delta: isize, rows: usize) {
        if rows == 0 {
            self.cursor = 0;
            return;
        }
        let last = rows as isize - 1;
        self.cursor = (self.cursor as isize + delta).clamp(0, last) as usize;
        self.inspect = false;
        self.detached = false;
        self.scroll_into_view(rows);
    }

    pub fn select(&mut self, index: usize, rows: usize) {
        self.cursor = index.min(rows.saturating_sub(1));
        self.detached = false;
        self.scroll_into_view(rows);
    }

    fn scroll_into_view(&mut self, rows: usize) {
        let shown = self.shown.max(1);
        let last_visible = self.first + shown - 1;
        if self.cursor < self.first {
            self.first = self.cursor;
        } else if self.cursor > last_visible {
            self.first = self.cursor + 1 - shown;
        }
        // …and never past the last screenful, so a window made taller shows
        // more rows rather than a gap under the last one.
        self.first = self.first.min(rows.saturating_sub(shown));
    }

    /// The panel was laid out showing `shown` of its `rows`: the cursor
    /// scrolls by that from now on, and is brought back into view now, since
    /// a window made shorter can have left it below the last row drawn —
    /// unless the wheel has taken the view off it, when the rows are only
    /// kept inside the list.
    pub fn fit(&mut self, shown: usize, rows: usize, now: Instant) {
        self.shown = shown;
        if self.detached {
            self.first = self.first.min(rows.saturating_sub(shown));
        } else {
            self.scroll_into_view(rows);
        }
        self.bar.saw(self.first as f32, now);
    }

    /// The wheel over the panel, in points, with `rows` tasks listed: whole
    /// rows at a time ([`crate::mouse::roll`]), the view leaving the cursor
    /// where it was. Returns whether the rows moved.
    pub fn wheel(&mut self, points: f32, rows: usize, now: Instant) -> bool {
        let step = crate::mouse::wheel_rows(points, ROW);
        let last = rows.saturating_sub(self.shown);
        let first = crate::mouse::roll(self.first, last, &mut self.carry, step);
        self.scroll_to(first, rows, now)
    }

    /// Start the rows at `first`, kept inside the `rows` listed, off the
    /// cursor until a key or a click moves it. Returns whether they moved.
    pub fn scroll_to(&mut self, first: usize, rows: usize, now: Instant) -> bool {
        let first = first.min(rows.saturating_sub(self.shown));
        if first == self.first {
            return false;
        }
        self.first = first;
        self.detached = true;
        self.bar.saw(first as f32, now);
        true
    }

    /// When the rows last scrolled, for their bar's linger.
    pub fn scrolled_at(&self) -> Option<Instant> {
        self.bar.scrolled_at()
    }

    /// A hand let go of the bar: it lingers from now
    /// ([`crate::scrollbar::Linger::let_go`]).
    pub fn let_go(&mut self, now: Instant) {
        self.bar.let_go(now);
    }

    /// The task the keys act on.
    pub fn selected(&self, rows: &[TaskRow]) -> Option<TaskId> {
        rows.get(self.cursor).map(|r| r.id)
    }

    /// Retarget the bars, and forget the ones whose task is gone.
    pub fn tick(&mut self, rows: &[TaskRow], now: Instant) {
        let known = self.fills.len();
        self.fills.retain(|id, _| rows.iter().any(|r| r.id == *id));
        // A task gone from the list, or a new one in it, is the list rebuilt
        // under the panel: a view that moves for it has not been scrolled.
        let mut rebuilt = self.fills.len() != known;
        for row in rows {
            let target = row.fraction.unwrap_or(0.0);
            match self.fills.get(&row.id) {
                Some(tween) if (tween.to - target).abs() <= FILL_EPSILON => {}
                Some(tween) => {
                    let from = tween.value(now);
                    self.fills.insert(
                        row.id,
                        Tween::new(from, target, FILL_TIME, Easing::OutQuint, now),
                    );
                }
                None => {
                    rebuilt = true;
                    // The first sight of a task starts its bar where it already
                    // is rather than sweeping up from zero — a panel opened
                    // mid-copy should show the truth immediately.
                    self.fills.insert(
                        row.id,
                        Tween::new(target, target, FILL_TIME, Easing::OutQuint, now),
                    );
                }
            }
        }
        if self.cursor >= rows.len() {
            self.cursor = rows.len().saturating_sub(1);
        }
        if self.first > self.cursor && !self.detached {
            self.first = self.cursor;
        }
        if rebuilt {
            self.bar = crate::scrollbar::Linger::default();
        }
    }

    pub fn fill(&self, id: TaskId, now: Instant) -> f32 {
        self.fills.get(&id).map(|t| t.value(now)).unwrap_or(0.0)
    }

    /// Any bar still travelling? (PLAN §1: the panel must go still.)
    ///
    /// A tween whose ends are the same value is not travelling however young it
    /// is — that is the shape a task's *first* frame takes, and treating it as
    /// motion would mean opening the panel always cost a fifth of a second of
    /// frames for a bar that never moved.
    pub fn animating(&self, now: Instant) -> bool {
        self.fills
            .values()
            .any(|t| (t.to - t.from).abs() > f32::EPSILON && !t.finished(now))
    }
}

/// Where the panel's rows are. Shared by paint and hit test.
///
/// Up to [`VISIBLE`] rows, and as many as the window has room for above
/// `bar_top` ([`crate::dialog::fit_rows`]): a short window scrolls the
/// tasks rather than hanging the panel's heading off its top.
pub fn geometry(area: egui::Rect, bar_top: f32, rows: usize) -> (egui::Rect, Vec<egui::Rect>) {
    let fixed = CARD_PAD * 2.0 + chrome::CARD_ROW + chrome::HINT_ROW;
    let above = egui::Rect::from_min_max(area.min, egui::pos2(area.right(), bar_top));
    let (listed, fitted) =
        crate::dialog::fit_rows(above, WIDTH, fixed, ROW, rows.clamp(1, VISIBLE));
    // Hung above `bar_top`, where the panel has always sat: the fitted card
    // lends it a size, not a place.
    let card = egui::Rect::from_min_size(
        egui::pos2(
            fitted.left(),
            (bar_top - chrome::CARD_MARGIN - fitted.height()).max(area.top() + chrome::CARD_MARGIN),
        ),
        fitted.size(),
    );
    let top = card.top() + CARD_PAD + chrome::CARD_ROW;
    let rects = (0..rows.min(listed))
        .map(|i| {
            egui::Rect::from_min_size(
                egui::pos2(card.left() + CARD_PAD, top + i as f32 * ROW),
                egui::vec2(card.width() - CARD_PAD * 2.0, ROW),
            )
        })
        .collect();
    (card, rects)
}

/// The panel's bar, beside its rows, while it shows fewer tasks than there
/// are.
pub fn bar(
    card: egui::Rect,
    rects: &[egui::Rect],
    panel: &TaskPanel,
    rows: usize,
) -> Option<crate::scrollbar::Geometry> {
    crate::scrollbar::card(
        card,
        body(rects)?,
        panel.first as f32,
        rects.len() as f32,
        rows as f32,
    )
}

/// Where the panel's bar is pointed at by, while there are more tasks than
/// rows ([`crate::scrollbar::band`]).
pub fn band(card: egui::Rect, rects: &[egui::Rect], rows: usize) -> Option<egui::Rect> {
    crate::scrollbar::band(card, body(rects)?, rects.len() as f32, rows as f32)
}

/// The rows, as one rect.
fn body(rects: &[egui::Rect]) -> Option<egui::Rect> {
    let (first, last) = (rects.first()?, rects.last()?);
    Some(egui::Rect::from_min_max(first.min, last.max))
}

/// Draw the panel.
#[allow(clippy::too_many_arguments)] // a painter's arguments are its inputs
pub fn paint(
    paint: &Painting<'_>,
    card: egui::Rect,
    rects: &[egui::Rect],
    rows: &[TaskRow],
    panel: &TaskPanel,
    hovers: &Hovers<Control>,
    ripples: &Ripples<Control>,
    now: Instant,
) {
    let painter = paint.painter;
    let palette = paint.palette;
    chrome::card(paint, card, 1.0);

    painter.text(
        egui::pos2(
            card.left() + CARD_PAD,
            card.top() + CARD_PAD + chrome::CARD_ROW / 2.0,
        ),
        egui::Align2::LEFT_CENTER,
        "Tasks",
        egui::FontId::proportional(FONT + 2.0),
        palette.text,
    );
    // The `×` in the heading's far corner, and the counts left of it.
    let close = chrome::close_button_rect(card);
    chrome::close_button(paint, close, hovers, ripples);
    let active = rows
        .iter()
        .filter(|r| matches!(r.tone, Tone::Running | Tone::Waiting | Tone::Paused))
        .count();
    painter.text(
        egui::pos2(
            close.left() - crate::ui::GAP,
            card.top() + CARD_PAD + chrome::CARD_ROW / 2.0,
        ),
        egui::Align2::RIGHT_CENTER,
        if rows.is_empty() {
            String::new()
        } else {
            format!("{active} running · {} listed", rows.len())
        },
        egui::FontId::proportional(FONT),
        palette.faint,
    );

    if rows.is_empty() {
        // An empty state that says what would put something here
        // (`delightful-ui` §11), not "no tasks".
        painter.text(
            egui::pos2(card.center().x, card.center().y + 6.0),
            egui::Align2::CENTER_CENTER,
            "Nothing running. Copies and deletes show up here.",
            egui::FontId::proportional(FONT),
            palette.faint,
        );
        return;
    }

    for (i, rect) in rects.iter().enumerate() {
        let Some(row) = rows.get(panel.first + i) else {
            break;
        };
        let index = panel.first + i;
        let on_cursor = index == panel.cursor;
        // The row's place among the drawn ones, which is what the hit test
        // reports: a scrolled panel keyed by its tasks would light, and ripple,
        // a row the pointer is not on.
        let key = Control::PanelRow(i);
        let hover = hovers.hover(key);
        let rect = pressed_rect(*rect, hovers.press(key));
        let fill = mix(
            if on_cursor {
                palette.surface1
            } else {
                palette.crust
            },
            palette.surface0,
            hover,
        );
        if on_cursor || hover > 0.0 {
            painter.rect_filled(rect, chrome::CARD_ROW_RADIUS, fill);
        }
        let inside = painter.with_clip_rect(rect);
        for splash in ripples.splashes(key, now) {
            inside.circle_filled(
                splash.center,
                splash.radius,
                crate::theme::splash(palette, splash.alpha),
            );
        }

        let tone = tone_color(row.tone, palette);
        // The chip's words in the tone as ink; its plate and the bar are
        // marks, in the tone itself. The two grey tones are text already.
        let words = match row.tone {
            Tone::Waiting | Tone::Quiet => tone,
            _ => crate::theme::ink(palette, tone),
        };
        // The state chip, right-aligned on the name's line.
        let chip = inside.layout_no_wrap(
            row.state.clone(),
            egui::FontId::proportional(FONT - 1.0),
            words,
        );
        let chip_rect = egui::Rect::from_min_size(
            egui::pos2(
                rect.right() - PAD_X - chip.size().x - 12.0,
                rect.top() + 6.0,
            ),
            egui::vec2(chip.size().x + 12.0, chip.size().y + 4.0),
        );
        inside.rect_filled(chip_rect, 4, mix(palette.crust, tone, 0.18));
        inside.galley(
            egui::pos2(chip_rect.left() + 6.0, chip_rect.top() + 2.0),
            chip,
            words,
        );
        chrome::truncated(
            &inside,
            egui::pos2(rect.left() + PAD_X, rect.top() + 13.0),
            &row.name,
            if on_cursor {
                palette.text
            } else {
                palette.subtext0
            },
            (chip_rect.left() - 8.0 - rect.left() - PAD_X).max(0.0),
        );

        // The bar. An unmeasured task gets a quiet track and no fill, which is
        // the honest picture of "running, size unknown".
        let track = egui::Rect::from_min_size(
            egui::pos2(rect.left() + PAD_X, rect.top() + 24.0),
            egui::vec2((rect.width() - PAD_X * 2.0).max(0.0), BAR),
        );
        inside.rect_filled(track, 2, mix(palette.crust, palette.surface1, 0.9));
        if row.fraction.is_some() {
            let width = track.width() * panel.fill(row.id, now).clamp(0.0, 1.0);
            if width > 0.0 {
                inside.rect_filled(
                    egui::Rect::from_min_size(track.min, egui::vec2(width, BAR)),
                    2,
                    tone,
                );
            }
        }

        let detail = if panel.inspect && on_cursor {
            row.error.clone().unwrap_or_else(|| row.counts.clone())
        } else {
            row.counts.clone()
        };
        chrome::truncated(
            &inside,
            egui::pos2(rect.left() + PAD_X, rect.bottom() - 8.0),
            &detail,
            if panel.inspect && on_cursor && row.error.is_some() {
                crate::theme::ink(palette, palette.red)
            } else {
                palette.quiet
            },
            (rect.width() - PAD_X * 2.0).max(0.0),
        );
    }
    if let Some(bar) = bar(card, rects, panel, rows.len()) {
        crate::scrollbar::paint_card(
            paint,
            &bar,
            crate::scrollbar::Surface::Tasks,
            hovers,
            panel.scrolled_at(),
            1.0,
        );
    }
}

fn tone_color(tone: Tone, palette: &crate::theme::Palette) -> egui::Color32 {
    match tone {
        Tone::Running => palette.blue,
        Tone::Waiting => palette.quiet,
        Tone::Paused => palette.peach,
        Tone::Good => palette.green,
        Tone::Bad => palette.red,
        Tone::Quiet => palette.faint,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The task panel's rows against its plate (`delightful-ui` §15). It was
    /// already right — this pins it, because the two surfaces that were wrong
    /// were wrong precisely because nothing asserted their own geometry.
    #[test]
    fn the_card_radii_are_concentric() {
        assert_eq!(
            chrome::CARD_ROW_RADIUS as f32 + CARD_PAD,
            chrome::CARD_RADIUS as f32
        );
    }
    use df_core::tasks::{Lane, Progress};

    fn snapshot(id: TaskId, state: TaskState) -> TaskSnapshot {
        TaskSnapshot {
            id,
            name: "Copy 3 items → /home/brian/Work".to_string(),
            lane: Lane::Macro,
            state,
            terminal: false,
        }
    }

    #[test]
    fn a_running_task_shows_both_counts_and_a_fraction() {
        let row = row_for(&snapshot(
            1,
            TaskState::Running(Progress {
                bytes_done: 512 * 1024,
                bytes_total: 1024 * 1024,
                files_done: 3,
                files_total: 12,
            }),
        ));
        assert_eq!(row.state, "running");
        assert_eq!(row.tone, Tone::Running);
        assert_eq!(row.counts, "3 / 12 files · 512.0 KB / 1.0 MB");
        assert_eq!(row.fraction, Some(0.5));
        assert!(row.error.is_none());
    }

    /// A job that has not measured its work yet must not draw a bar at 0 % —
    /// that is a claim about progress, and there is not one to make.
    #[test]
    fn an_unmeasured_task_says_so_instead_of_showing_zero() {
        let row = row_for(&snapshot(2, TaskState::Running(Progress::default())));
        assert_eq!(row.fraction, None);
        assert_eq!(row.counts, "sizing up the work…");
    }

    #[test]
    fn every_terminal_state_reads_as_itself() {
        let cases = [
            (TaskState::Pending, "queued", Tone::Waiting),
            (TaskState::Done, "done", Tone::Good),
            (TaskState::Cancelled, "cancelled", Tone::Quiet),
            (
                TaskState::Paused(Progress {
                    bytes_done: 1,
                    bytes_total: 4,
                    ..Progress::default()
                }),
                "paused",
                Tone::Paused,
            ),
        ];
        for (state, text, tone) in cases {
            let row = row_for(&snapshot(3, state));
            assert_eq!(row.state, text);
            assert_eq!(row.tone, tone);
        }
        let failed = row_for(&snapshot(
            4,
            TaskState::Failed {
                error: "/srv/x: permission denied".to_string(),
                retries: 2,
            },
        ));
        assert_eq!(failed.state, "failed, retry 2");
        assert_eq!(failed.tone, Tone::Bad);
        assert_eq!(
            failed.error.as_deref(),
            Some("/srv/x: permission denied"),
            "Enter has something to expand"
        );
    }

    #[test]
    fn the_cursor_stays_inside_the_list_and_the_view_follows_it() {
        let rows: Vec<TaskRow> = (0..12)
            .map(|i| row_for(&snapshot(i, TaskState::Done)))
            .collect();
        let mut panel = TaskPanel::new();
        panel.move_cursor(-1, rows.len());
        assert_eq!(panel.cursor, 0);
        panel.move_cursor(100, rows.len());
        assert_eq!(panel.cursor, rows.len() - 1);
        assert_eq!(panel.first, rows.len() - VISIBLE, "the view followed");
        panel.move_cursor(-100, rows.len());
        assert_eq!((panel.cursor, panel.first), (0, 0));
        assert_eq!(panel.selected(&rows), Some(0));
        // …and an empty panel has nothing selected rather than row 0 of nothing.
        assert_eq!(panel.selected(&[]), None);
    }

    /// A short window gets the tasks it has room for above `bar_top`, and the
    /// cursor scrolls by those; a tall one gets the eight it always did. The
    /// bar is there only while tasks are left out.
    #[test]
    fn a_short_window_fits_the_panels_rows() {
        let rows: Vec<TaskRow> = (0..12)
            .map(|i| row_for(&snapshot(i, TaskState::Done)))
            .collect();
        let window = |height: f32| {
            egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, height))
        };
        let panel = TaskPanel::new();

        let tall = window(900.0);
        let (card, rects) = geometry(tall, tall.bottom() - 8.0, rows.len());
        assert_eq!(rects.len(), VISIBLE, "the old count");
        assert!(bar(card, &rects, &panel, rows.len()).is_some());
        let (card, rects) = geometry(tall, tall.bottom() - 8.0, 3);
        assert_eq!(bar(card, &rects, &panel, 3), None, "three tasks fit");
        assert_eq!(band(card, &rects, 3), None);

        let short = window(300.0);
        let (card, rects) = geometry(short, short.bottom() - 8.0, rows.len());
        assert!(short.contains_rect(card), "{card:?}");
        assert!(!rects.is_empty() && rects.len() < VISIBLE);
        assert!(rects.iter().all(|rect| card.contains_rect(*rect)));
        assert!(bar(card, &rects, &panel, rows.len()).is_some());
        assert!(band(card, &rects, rows.len()).is_some_and(|band| card.contains_rect(band)));

        let mut panel = TaskPanel::new();
        panel.fit(rects.len(), rows.len(), Instant::now());
        panel.move_cursor(100, rows.len());
        assert_eq!(
            panel.first,
            rows.len() - rects.len(),
            "followed by the rows drawn"
        );
        // A window made taller shows more rows, not a gap under the last one.
        panel.fit(VISIBLE, rows.len(), Instant::now());
        assert_eq!(panel.first, rows.len() - VISIBLE);

        // An empty panel keeps its line of guidance, inside the window.
        let (card, rects) = geometry(short, short.bottom() - 8.0, 0);
        assert!(rects.is_empty() && short.contains_rect(card));
    }

    /// A task dropping off the list while the panel is scrolled to its end
    /// moves the view, and that is the list rebuilt, not a scroll: the bar
    /// has nothing to linger for. A key that scrolls is one.
    #[test]
    fn a_task_leaving_the_list_is_not_a_scroll() {
        let t0 = Instant::now();
        let rows = |n: u64| -> Vec<TaskRow> {
            (0..n)
                .map(|i| row_for(&snapshot(i, TaskState::Done)))
                .collect()
        };
        let (twelve, eleven) = (rows(12), rows(11));
        let mut panel = TaskPanel::new();
        // The frame's order: told the tasks, then how many rows it shows.
        panel.tick(&twelve, t0);
        panel.fit(4, twelve.len(), t0);
        assert_eq!(panel.scrolled_at(), None, "opening is not a scroll");

        let t1 = t0 + Duration::from_millis(16);
        panel.move_cursor(100, twelve.len());
        panel.tick(&twelve, t1);
        panel.fit(4, twelve.len(), t1);
        assert_eq!(panel.scrolled_at(), Some(t1), "a key scrolled it");

        let t2 = t1 + Duration::from_secs(5);
        panel.tick(&eleven, t2);
        panel.fit(4, eleven.len(), t2);
        assert_eq!(panel.first, eleven.len() - 4, "the view moved up a row");
        assert_eq!(panel.scrolled_at(), None, "and nobody scrolled it");
        let t3 = t2 + Duration::from_millis(16);
        panel.tick(&eleven, t3);
        panel.fit(4, eleven.len(), t3);
        assert_eq!(panel.scrolled_at(), None);
    }

    /// The bar animates to a new value and then stops asking for frames.
    #[test]
    fn the_progress_bar_settles() {
        let t0 = Instant::now();
        let mut panel = TaskPanel::new();
        let half = vec![row_for(&snapshot(
            1,
            TaskState::Running(Progress {
                bytes_done: 1,
                bytes_total: 2,
                ..Progress::default()
            }),
        ))];
        panel.tick(&half, t0);
        assert!(
            !panel.animating(t0),
            "the first sight of a task is not a sweep"
        );
        assert!((panel.fill(1, t0) - 0.5).abs() < 1e-3);

        let full = vec![row_for(&snapshot(
            1,
            TaskState::Running(Progress {
                bytes_done: 2,
                bytes_total: 2,
                ..Progress::default()
            }),
        ))];
        panel.tick(&full, t0);
        assert!(panel.animating(t0), "and a change travels");
        assert!(!panel.animating(t0 + FILL_TIME), "…then arrives");
        assert!((panel.fill(1, t0 + FILL_TIME) - 1.0).abs() < 1e-3);

        // A task that is gone takes its bar with it.
        panel.tick(&[], t0 + FILL_TIME);
        assert!(!panel.animating(t0 + FILL_TIME));
        assert_eq!(panel.fill(1, t0 + FILL_TIME), 0.0);
    }

    #[test]
    fn the_panel_paints_without_panicking() {
        let now = Instant::now();
        let rows: Vec<TaskRow> = [
            TaskState::Running(Progress {
                bytes_done: 10,
                bytes_total: 100,
                files_done: 1,
                files_total: 9,
            }),
            TaskState::Paused(Progress::default()),
            TaskState::Failed {
                error: "nope".to_string(),
                retries: 0,
            },
        ]
        .into_iter()
        .enumerate()
        .map(|(i, s)| row_for(&snapshot(i as TaskId, s)))
        .collect();
        let mut panel = TaskPanel::new();
        panel.tick(&rows, now);
        panel.inspect = true;
        panel.cursor = 2;
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(Default::default(), |ui| {
            let palette = crate::theme::Palette::default();
            let theme = df_core::config::Theme::default();
            let painting = Painting {
                tips: None,
                held: None,
                painter: ui.painter(),
                palette: &palette,
                theme: &theme,
                tags: &crate::tags::BUILT_IN,
                nerd: false,
                show_symlink: true,
                now,
            };
            let area = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, 900.0));
            let (card, rects) = geometry(area, 860.0, rows.len());
            paint(
                &painting,
                card,
                &rects,
                &rows,
                &panel,
                &Hovers::new(),
                &Ripples::new(),
                now,
            );
            let (card, rects) = geometry(area, 860.0, 0);
            paint(
                &painting,
                card,
                &rects,
                &[],
                &panel,
                &Hovers::new(),
                &Ripples::new(),
                now,
            );
        });
    }
}
