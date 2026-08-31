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

/// A printable line: either a context heading or one binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelpLine {
    Group(&'static str),
    Row(HelpRow),
}

impl HelpLine {
    /// Whether the cursor can land here. Headings are read, not selected.
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

/// The help overlay's own view state: where the cursor is and where the list
/// has scrolled to. The scrolling itself is [`crate::viewport`]'s rule, the
/// same one the file panes use.
#[derive(Debug, Clone, Copy, Default)]
pub struct Help {
    pub cursor: usize,
    pub first: usize,
}

impl Help {
    /// Move the cursor by `delta` *selectable* lines, skipping the headings and
    /// clamping at both ends.
    pub fn move_cursor(&mut self, lines: &[HelpLine], delta: isize) {
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
        let next = (at + delta).clamp(0, selectable.len() as isize - 1) as usize;
        self.cursor = selectable[next];
    }

    /// Put the cursor on the first binding — where it goes when the overlay
    /// opens and every time the filter changes what is in it.
    pub fn reset(&mut self, lines: &[HelpLine]) {
        self.cursor = lines.iter().position(HelpLine::selectable).unwrap_or(0);
        self.first = 0;
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
            row(Context::Files, "f", "Filter files", "filter"),
            row(Context::Global, "Ctrl+p", "Command palette", "command-palette"),
        ]
    }

    /// An unfiltered sheet keeps the registry's order and heads each group.
    #[test]
    fn every_group_gets_one_heading_in_order() {
        let lines = lines(&sample(), "");
        assert_eq!(lines.len(), 6, "four rows plus two headings");
        assert_eq!(lines[0], HelpLine::Group("Files"));
        assert!(matches!(&lines[1], HelpLine::Row(r) if r.keys == "q"));
        assert!(matches!(&lines[2], HelpLine::Row(r) if r.keys == ", m"));
        assert_eq!(lines[4], HelpLine::Group("Global"));
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
                    HelpLine::Group(_) => None,
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

    /// The cursor lands on bindings and steps over the headings between them.
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
        let rows = all_rows(&registry, &ContextStack::browser(), WhenFlags::LIST);
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
