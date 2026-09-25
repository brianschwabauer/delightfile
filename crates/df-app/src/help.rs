//! The `~` / `F1` help browser: every binding that is live right now, read
//! straight out of the registry.
//!
//! This is the surface PLAN §4 is really about — "one registry of
//! `(context, chord, when) → Command` feeds four surfaces", and this is the one
//! where the keymap documents itself. There is no hand-written list of shortcuts
//! anywhere in delightfile, and there must never be one: a binding that exists
//! appears here because [`Registry::active_bindings`] returned it, and a binding
//! that has been rebound in `keymap.toml` appears here under its new keys for
//! free.
//!
//! What this module does with those rows is two things, both pure:
//!
//! 1. flatten them into printable lines, keeping df-core's ordering —
//!    most-specific context first, **declaration order** inside a context, which
//!    PLAN §4 asks for explicitly ("`[which]` ordering preserves declaration
//!    order, not alphabetical"); and
//! 2. narrow them by the `f` filter field, using the same smart-case matcher the
//!    file listing uses, so `f` means the same thing in both places.
//!
//! ## …and one thing that is not a binding
//!
//! [`LEGEND`] — what the *marks* on a row mean. A dimmed row, a coloured bar
//! down an edge, a dot before the size: the window says a dozen things without
//! words, and until now there was nowhere to look them up. It lives here rather
//! than in a second overlay because "what does that mean" and "what do I press"
//! are the same reflex, reached by the same key, and a second sheet would be a
//! second thing to remember the existence of.
//!
//! It is hand-written, and that is the one exception to this file's rule that
//! nothing is. A binding can be read out of the registry because the registry
//! *is* the binding; a colour's meaning lives in a painter's argument list and
//! cannot be read out of anything. The unit test below is the substitute: it
//! pins the wording, so a legend that drifts from the painter drifts loudly.

use df_core::fs::match_name;
use df_core::keymap::{Context, ContextStack, Registry, WhenFlags};

/// One binding, flattened for printing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelpRow {
    pub context: Context,
    /// `g g`, `Ctrl+p`, `, m` — what [`Binding::label`] renders.
    ///
    /// [`Binding::label`]: df_core::keymap::Binding::label
    pub keys: String,
    pub description: String,
    /// The command id, which is also the vocabulary of `keymap.toml` — shown
    /// dimmed at the end of the row so that reading the help sheet tells you
    /// what to write in the file (PLAN §3).
    pub id: String,
}

/// The mark itself, so the legend can *draw* what it is naming.
///
/// A sheet that says "yellow bar" in the same grey as every other line asks the
/// reader to take its word for the colour, which is the one thing they came
/// here unsure about. Every variant is painted by [`crate::chrome::help`] out
/// of the palette and the constants the row painter itself uses, so a swatch
/// cannot drift from the mark it stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Swatch {
    /// Nothing to draw: the row is about text, and the words are the mark.
    None,
    /// A git dot, in that status's own colour.
    Dot(df_core::git::FileStatus),
    /// One of the three row bars.
    Bar(Mark),
}

/// Which row bar a [`Swatch::Bar`] stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mark {
    Selected,
    Yanked,
    Cut,
}

/// One entry in the legend: a mark, and what it means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LegendRow {
    /// What you see on a row — `dimmed`, `yellow bar`, `~4.2 MB`.
    pub mark: &'static str,
    /// The mark itself, drawn beside the word.
    pub swatch: Swatch,
    pub meaning: &'static str,
}

/// Every wordless thing the list pane says, in one place.
///
/// Ordered by how often a person meets it rather than by colour: the dim is
/// what sends people looking, the bars are what an operation is about to act
/// on, and the dots are git. The size column's two marks come last because they
/// explain themselves the moment they change.
///
/// The wording is shared with the rest of the window on purpose — `ignored` is
/// the word on the tag in [`crate::ui::IGNORED_TAG`] and in the spot card's
/// "Visibility" row, so the three surfaces teach one vocabulary.
pub const LEGEND: &[LegendRow] = &[
    LegendRow {
        mark: "dimmed row",
        swatch: Swatch::None,
        meaning: "hidden, git is ignoring it, it has been cut, or it is in the parent column",
    },
    LegendRow {
        mark: "yellow bar, left",
        swatch: Swatch::Bar(Mark::Selected),
        meaning: "selected — what the next operation acts on",
    },
    LegendRow {
        mark: "teal bar, right",
        swatch: Swatch::Bar(Mark::Yanked),
        meaning: "yanked: copied to the clipboard, still where it was",
    },
    LegendRow {
        mark: "peach bar, right",
        swatch: Swatch::Bar(Mark::Cut),
        meaning: "cut: it moves when you paste",
    },
    LegendRow {
        mark: "green dot",
        swatch: Swatch::Dot(df_core::git::FileStatus::Added),
        meaning: "added to the index",
    },
    LegendRow {
        mark: "faint green dot",
        swatch: Swatch::Dot(df_core::git::FileStatus::Untracked),
        meaning: "untracked: git has never seen it",
    },
    LegendRow {
        mark: "peach dot",
        swatch: Swatch::Dot(df_core::git::FileStatus::Modified),
        meaning: "modified since the last commit",
    },
    LegendRow {
        mark: "maroon dot",
        swatch: Swatch::Dot(df_core::git::FileStatus::Deleted),
        meaning: "deleted",
    },
    LegendRow {
        mark: "blue dot",
        swatch: Swatch::Dot(df_core::git::FileStatus::Renamed),
        meaning: "renamed",
    },
    LegendRow {
        mark: "yellow dot",
        swatch: Swatch::Dot(df_core::git::FileStatus::Typechange),
        meaning: "type changed — a file became a link, or the other way round",
    },
    LegendRow {
        mark: "red dot",
        swatch: Swatch::Dot(df_core::git::FileStatus::Conflict),
        meaning: "conflicted: a merge left it for you",
    },
    LegendRow {
        mark: "green name",
        swatch: Swatch::None,
        meaning: "you can run it",
    },
    LegendRow {
        mark: "grey name",
        swatch: Swatch::None,
        meaning: "configuration or a lockfile — there, and rarely what you want",
    },
    // Spelled exactly as the column spells it (`crate::format::human_size` with
    // `folder_size_text`'s tilde), so the sheet and the row are quotable
    // against each other. The test below pins the pair.
    LegendRow {
        mark: "~4.2 MB",
        swatch: Swatch::None,
        meaning: "a folder still being measured; the number only goes up",
    },
    LegendRow {
        mark: "12 items",
        swatch: Swatch::None,
        meaning: "a folder counted but not yet measured",
    },
];

/// A printable line: a context heading, one binding, or one legend entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelpLine {
    Group(&'static str),
    Row(HelpRow),
    /// A mark and its meaning. Not selectable: there is nothing to press.
    Legend(LegendRow),
}

impl HelpLine {
    /// Whether the cursor can land here. Headings and legend entries are read,
    /// not selected.
    pub fn selectable(&self) -> bool {
        matches!(self, HelpLine::Row(_))
    }
}

/// Every binding reachable in this context stack, in the order the registry
/// returns them.
pub fn all_rows(registry: &Registry, stack: &ContextStack, flags: WhenFlags) -> Vec<HelpRow> {
    registry
        .active_bindings(stack, flags)
        .into_iter()
        .map(|b| HelpRow {
            context: b.context,
            keys: b.label(),
            description: b.description.clone(),
            id: b.command.id(),
        })
        .collect()
}

/// The lines to draw: `rows` narrowed by `query` and split into groups.
///
/// A group whose every row was filtered out disappears with them — a heading
/// over nothing is a claim that there is something there.
pub fn lines(rows: &[HelpRow], query: &str) -> Vec<HelpLine> {
    let mut out: Vec<HelpLine> = Vec::new();
    let mut current: Option<Context> = None;
    for row in rows.iter().filter(|r| matches(r, query)) {
        if current != Some(row.context) {
            current = Some(row.context);
            out.push(HelpLine::Group(context_title(row.context)));
        }
        out.push(HelpLine::Row(row.clone()));
    }

    // The legend goes last, under its own heading, and narrows to the same
    // query — so typing `ignored` finds the explanation as readily as typing
    // `sort` finds the chord.
    let legend: Vec<&LegendRow> = LEGEND
        .iter()
        .filter(|entry| legend_matches(entry, query))
        .collect();
    if !legend.is_empty() {
        out.push(HelpLine::Group("Marks"));
        out.extend(legend.into_iter().map(|entry| HelpLine::Legend(*entry)));
    }
    out
}

/// Does this row survive the filter?
///
/// The query is matched against the keys, the description **and** the command
/// id, because all three are things a person types into this box: `ctrl` to see
/// what the modifier does, `sort` to find the sort chord, `linemode` because
/// they read the id in `keymap.toml` and want to know what it is bound to.
fn matches(row: &HelpRow, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    [&row.keys, &row.description, &row.id]
        .into_iter()
        .any(|field| match_name(field, query).is_some())
}

fn legend_matches(entry: &LegendRow, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    [entry.mark, entry.meaning]
        .into_iter()
        .any(|field| match_name(field, query).is_some())
}

/// A context's heading. Capitalised rather than [`Context::name`]'s lowercase
/// config spelling — the file format and the prose are two different registers.
pub fn context_title(context: Context) -> &'static str {
    match context {
        Context::Global => "Global",
        Context::Files => "Files",
        Context::Input => "Input",
        Context::Confirm => "Confirm",
        Context::Pick => "Pick",
        Context::Tasks => "Tasks",
        Context::Spot => "Spot",
        Context::Help => "Help",
        Context::Palette => "Palette",
    }
}

/// What the sheet's heading says about the filter: the query it is narrowed by,
/// and where the field's caret is in it while the field is open.
///
/// One struct rather than two arguments because the two are one thing — the
/// query without the caret is a *committed* filter, the query with it is a
/// field being typed into, and the heading draws them differently.
#[derive(Debug, Clone, Copy, Default)]
pub struct Filter<'a> {
    pub query: &'a str,
    /// The caret's byte offset into `query`, or `None` when the field is shut.
    pub caret: Option<usize>,
}

/// The help overlay's own view state: where the cursor is and where the list
/// has scrolled to. The scrolling itself is [`crate::viewport`]'s rule, the
/// same one the file panes use.
#[derive(Debug, Clone, Copy, Default)]
pub struct Help {
    pub cursor: usize,
    pub first: usize,
    /// The wheel's roll that has not come to a whole line yet
    /// ([`crate::mouse::roll`]).
    carry: f32,
    /// The wheel has scrolled the sheet off the cursor, and it stays where
    /// the wheel left it until a key moves the cursor: the panes' rule
    /// ([`crate::tab::Listing::attach`]).
    detached: bool,
}

impl Help {
    /// Move the cursor by `delta` *selectable* lines, skipping the headings and
    /// clamping at both ends.
    ///
    /// The step is **saturating**, so `isize::MIN` and `isize::MAX` are the two
    /// ends of the sheet — which is what `Home` and `End` pass, rather than a
    /// count of lines the caller would have to ask for first.
    pub fn move_cursor(&mut self, lines: &[HelpLine], delta: isize) {
        self.detached = false;
        let selectable: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| l.selectable())
            .map(|(i, _)| i)
            .collect();
        if selectable.is_empty() {
            self.cursor = 0;
            return;
        }
        // Where the cursor is among the selectable lines — or the nearest one
        // below it, which is where it lands after a filter change dropped the
        // line it was on.
        let at = selectable
            .iter()
            .position(|i| *i >= self.cursor)
            .unwrap_or(selectable.len() - 1) as isize;
        let next = at
            .saturating_add(delta)
            .clamp(0, selectable.len() as isize - 1) as usize;
        self.cursor = selectable[next];
    }

    /// Put the cursor on the first binding — where it goes when the overlay
    /// opens and every time the filter changes what is in it.
    pub fn reset(&mut self, lines: &[HelpLine]) {
        self.cursor = lines.iter().position(HelpLine::selectable).unwrap_or(0);
        self.first = 0;
        self.carry = 0.0;
        self.detached = false;
    }

    /// Where the sheet starts for `lines` lines in a `page` of them: by the
    /// panes' scrolloff rule around the cursor, or — while the wheel has
    /// taken it off the cursor — where the wheel left it, inside the lines,
    /// which a filter may have made fewer.
    pub fn settle(&mut self, lines: usize, page: usize, scrolloff: usize) {
        self.first = if self.detached {
            self.first.min(lines.saturating_sub(page))
        } else {
            crate::viewport::first_visible(self.first, self.cursor, lines, page, scrolloff)
        };
    }

    /// The wheel over the sheet, in points, with `lines` lines in a `page` of
    /// them: whole lines at a time ([`crate::mouse::roll`]), the sheet
    /// leaving the cursor where it was. Returns whether the lines moved.
    pub fn wheel(&mut self, points: f32, lines: usize, page: usize) -> bool {
        let rows = crate::mouse::wheel_rows(points, crate::chrome::HELP_ROW);
        let last = lines.saturating_sub(page);
        let first = crate::mouse::roll(self.first, last, &mut self.carry, rows);
        if first == self.first {
            return false;
        }
        self.first = first;
        self.detached = true;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(context: Context, keys: &str, description: &str, id: &str) -> HelpRow {
        HelpRow {
            context,
            keys: keys.to_string(),
            description: description.to_string(),
            id: id.to_string(),
        }
    }

    fn sample() -> Vec<HelpRow> {
        vec![
            row(Context::Files, "q", "Quit", "quit"),
            row(Context::Files, ", m", "Sort by modified", "sort-mtime"),
            row(
                Context::Files,
                "f",
                "Filter this folder, hide the rest",
                "filter",
            ),
            row(
                Context::Global,
                "Ctrl+p",
                "Command palette",
                "command-palette",
            ),
        ]
    }

    /// An unfiltered sheet keeps the registry's order and heads each group.
    #[test]
    fn every_group_gets_one_heading_in_order() {
        let lines = lines(&sample(), "");
        assert_eq!(
            lines.len(),
            6 + 1 + LEGEND.len(),
            "four rows, two headings, and the legend under its own"
        );
        assert_eq!(lines[0], HelpLine::Group("Files"));
        assert!(matches!(&lines[1], HelpLine::Row(r) if r.keys == "q"));
        assert!(matches!(&lines[2], HelpLine::Row(r) if r.keys == ", m"));
        assert_eq!(lines[4], HelpLine::Group("Global"));
        assert_eq!(lines[6], HelpLine::Group("Marks"));
        assert!(matches!(lines[7], HelpLine::Legend(_)));
    }

    /// The legend answers the question people actually arrive with — "why is
    /// that row grey" — and it answers it in the same word the row itself uses.
    #[test]
    fn the_legend_explains_the_marks_the_pane_draws() {
        let marks: Vec<&str> = LEGEND.iter().map(|e| e.mark).collect();
        assert!(marks.contains(&"dimmed row"), "{marks:?}");
        assert!(
            marks.iter().any(|m| m.contains("yellow bar")),
            "the selection bar: {marks:?}"
        );
        assert!(
            marks.iter().any(|m| m.contains("teal bar"))
                && marks.iter().any(|m| m.contains("peach bar")),
            "both yank bars: {marks:?}"
        );
        assert!(
            marks.iter().filter(|m| m.ends_with("dot")).count() >= 7,
            "every git status has a dot: {marks:?}"
        );
        // Every entry says something, and says it once.
        let mut seen = std::collections::HashSet::new();
        for entry in LEGEND {
            assert!(seen.insert(entry.mark), "{} appears twice", entry.mark);
            assert!(!entry.meaning.is_empty(), "{} explains nothing", entry.mark);
        }
        // …and the size column's two marks are quoted from the formatter
        // rather than paraphrased: a legend that spells `~ 4.2 MB` while the
        // column draws `~4.2 MB` is teaching a mark that does not exist.
        let bytes = 4 * 1024 * 1024 + 205 * 1024;
        let running = crate::format::folder_size_text(
            Some(crate::folders::Size {
                bytes,
                settled: false,
            }),
            None,
        )
        .expect("a size formats");
        assert!(marks.contains(&running.as_str()), "{running}: {marks:?}");
        let counted = crate::format::folder_size_text(
            None,
            Some(df_core::du::ChildCount {
                entries: 12,
                capped: false,
            }),
        )
        .expect("a count formats");
        assert!(marks.contains(&counted.as_str()), "{counted}: {marks:?}");

        // Every mark that names a drawn thing carries the thing itself, so the
        // sheet is readable in a monochrome screenshot of it.
        for entry in LEGEND {
            if entry.mark.ends_with("dot") {
                assert!(
                    matches!(entry.swatch, Swatch::Dot(_)),
                    "{} has no dot to draw",
                    entry.mark
                );
            }
            if entry.mark.contains("bar") {
                assert!(
                    matches!(entry.swatch, Swatch::Bar(_)),
                    "{} has no bar to draw",
                    entry.mark
                );
            }
        }
    }

    /// …and it narrows with everything else, so the sheet does not turn into a
    /// wall of legend the moment you filter the bindings away.
    #[test]
    fn the_legend_answers_the_filter_too() {
        let found = lines(&sample(), "ignoring");
        assert_eq!(found[0], HelpLine::Group("Marks"), "{found:?}");
        assert!(found.iter().all(|l| !l.selectable()), "{found:?}");

        // A query that matches neither takes the heading with it.
        assert!(lines(&sample(), "nothing at all").is_empty());
    }

    /// The filter reads the keys, the description and the command id.
    #[test]
    fn the_filter_matches_all_three_columns() {
        let rows = sample();
        let only = |query: &str| -> Vec<String> {
            lines(&rows, query)
                .into_iter()
                .filter_map(|l| match l {
                    HelpLine::Row(r) => Some(r.id),
                    HelpLine::Group(_) | HelpLine::Legend(_) => None,
                })
                .collect()
        };
        assert_eq!(only("sort"), vec!["sort-mtime"], "by id");
        assert_eq!(only("modified"), vec!["sort-mtime"], "by description");
        assert_eq!(only("ctrl"), vec!["command-palette"], "by keys");
        assert!(only("nothing at all").is_empty());
    }

    /// Smart case, exactly as in the file listing: a lowercase query matches
    /// anything, a capital means the capital.
    #[test]
    fn the_filter_is_smart_case() {
        let rows = sample();
        assert_eq!(lines(&rows, "quit").len(), 2, "heading + row");
        assert_eq!(lines(&rows, "Quit").len(), 2, "the description shouts too");
        assert!(lines(&rows, "QUIT").is_empty());
    }

    /// A heading over nothing is a claim there is something there.
    #[test]
    fn a_group_with_no_matches_takes_its_heading_with_it() {
        let lines = lines(&sample(), "palette");
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0], HelpLine::Group("Global"));
    }

    /// The cursor lands on bindings and steps over the headings — and over the
    /// legend, which has nothing to press.
    #[test]
    fn the_cursor_skips_the_headings() {
        let lines = lines(&sample(), "");
        let mut help = Help::default();
        help.reset(&lines);
        assert_eq!(help.cursor, 1, "the first binding, not the heading");
        help.move_cursor(&lines, 1);
        assert_eq!(help.cursor, 2);
        help.move_cursor(&lines, 1);
        assert_eq!(help.cursor, 3);
        // Over the "Global" heading at 4, onto the row at 5.
        help.move_cursor(&lines, 1);
        assert_eq!(help.cursor, 5);
        help.move_cursor(&lines, 1);
        assert_eq!(help.cursor, 5, "and it stops at the bottom");
        help.move_cursor(&lines, -10);
        assert_eq!(help.cursor, 1, "…and at the top");
    }

    /// `Home` and `End` are a saturating step, not a counted one: the caller
    /// passes the two ends of `isize` and lands on the two ends of the sheet.
    /// The step used to be a plain `+`, which overflowed on exactly that.
    #[test]
    fn the_ends_of_the_sheet_are_one_step_away() {
        let lines = lines(&sample(), "");
        let last = lines
            .iter()
            .rposition(HelpLine::selectable)
            .expect("the sample has rows");
        let mut help = Help::default();
        help.reset(&lines);
        help.move_cursor(&lines, isize::MAX);
        assert_eq!(help.cursor, last, "End is the last binding");
        help.move_cursor(&lines, isize::MIN);
        assert_eq!(help.cursor, 1, "…and Home is the first, over its heading");
        // A page is an ordinary step, and it stops at the ends like any other.
        help.move_cursor(&lines, 2);
        assert_eq!(help.cursor, 3);
        help.move_cursor(&lines, 40);
        assert_eq!(help.cursor, last);
    }

    /// An empty sheet — every binding filtered away — must not leave the cursor
    /// pointing at a line that is not there.
    #[test]
    fn an_empty_sheet_parks_the_cursor() {
        let mut help = Help::default();
        help.reset(&[]);
        help.move_cursor(&[], 1);
        assert_eq!(help.cursor, 0);
    }

    /// The rows really do come from the registry, keys and ids and all.
    #[test]
    fn the_sheet_is_read_out_of_the_registry() {
        let registry = Registry::defaults();
        let rows = all_rows(&registry, &ContextStack::browser(), WhenFlags::NONE);
        assert!(rows.iter().any(|r| r.id == "quit" && r.keys == "q"));
        assert!(rows.iter().any(|r| r.id == "sort-mtime" && r.keys == ", m"));
        // Most-specific context first: every Files row precedes every Global one.
        let last_files = rows.iter().rposition(|r| r.context == Context::Files);
        let first_global = rows.iter().position(|r| r.context == Context::Global);
        assert!(
            matches!((last_files, first_global), (Some(a), Some(b)) if a < b),
            "{last_files:?} / {first_global:?}"
        );
    }
}
