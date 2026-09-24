//! The two menus: the right-click context menu (PLAN §7.5), "mirroring opener
//! rules + operations, with shortcuts rendered inline", and the app menu that
//! drops out of the button at the top row's leading end.
//!
//! Both are the same floating card the which-key hint and the opener picker
//! are ([`crate::chrome::card`]) at a third size, for the reason that file's
//! header gives: one card style is what makes six surfaces read as one
//! program. And both are one model — a list of [`Item`]s, any of which may
//! fly a list of its own out — so a submenu, a check mark or a separator
//! behaves the same in either, and there is one keyboard, one hit test and one
//! painter to keep right.
//!
//! ## What it is *not*
//!
//! It is not a second command system. Every row here is a key that already
//! exists, and the key is drawn on the row — right-aligned and dim, the way a
//! menu has taught its own shortcuts since 1984. A menu item with no keyboard
//! equivalent would be a feature only the mouse could reach, which is the
//! opposite of what this program is. The app menu reads its keys out of the
//! registry ([`Registry::binding_label`]) rather than spelling them, so a key
//! somebody moved in `keymap.toml` is taught where they moved it.
//!
//! ## Motion
//!
//! Instant in, faded out (`delightful-ui` §3, PLAN §8). A menu that grew or
//! slid on the way in would put an animation between the click and the answer;
//! on the way out there is nothing left to wait for, so it fades over
//! [`FADE`] and the app drops it when the fade is spent.

use std::time::{Duration, Instant};

use df_core::config::{LineMode, SortBy, ViewScale};
use df_core::keymap::{Command, Registry};

use crate::chrome::{
    card, fade as fade_color, key_font, CARD_PAD, CARD_ROW_RADIUS, FONT, ICON_GAP, PAD_X,
};
use crate::hover::{pressed_rect, Hovers};
use crate::ripple::Ripples;
use crate::theme::mix;
use crate::ui::{Control, Painting};

/// One menu row's height, in logical points.
///
/// Two points taller than a file row: a menu is read once and aimed at, not
/// scanned in bulk, and 24 is also `delightful-ui` §1's hit-target floor — a
/// row that is exactly big enough to click without care.
pub const ROW: f32 = 24.0;

/// The space a separator opens between two groups, in logical points.
///
/// A gap, plus a hairline drawn in the middle of it. Ten points is enough that
/// the groups read as groups at a glance; a rule alone (with no space) reads as
/// a row with a line through it.
const SEPARATOR: f32 = 9.0;

/// The narrowest the card gets. Wide enough that "Copy" plus its `y` do not sit
/// on top of each other, so a menu of short verbs still looks like a menu.
const MIN_WIDTH: f32 = 168.0;

/// The gap between a label and its key column. Generous: they are two different
/// kinds of information and the eye has to be able to ignore one of them.
const KEY_GAP: f32 = 28.0;

/// How far the card stays off the window's edge, in logical points. The same
/// margin every other floating surface keeps.
const MARGIN: f32 = 6.0;

/// How far under the control it hangs from a [`Anchor::Below`] card sits, in
/// logical points: enough air that the card reads as having come *out of* the
/// button rather than as a second plate glued to it.
const BELOW_GAP: f32 = 4.0;

/// How long the menu takes to fade once it is dismissed.
///
/// 120 ms — PLAN §8's state-fade duration. The menu is *gone* the instant it is
/// dismissed (the click that dismissed it has already been acted on); this is
/// only the pixels catching up, so it must be shorter than the eye's patience
/// and it must never be waited on.
pub const FADE: Duration = Duration::from_millis(120);

/// How far a submenu overlaps its parent row's card, in logical points.
///
/// A small overlap rather than a gap: the pointer travels diagonally from the
/// parent row to the submenu, and a gap between the two cards is a corridor the
/// pointer falls out of — the classic menu bug.
const SUBMENU_OVERLAP: f32 = 4.0;

/// The room a check mark gets in front of the label: the glyph and a word
/// space ([`ICON_GAP`]) after it.
///
/// Fixed rather than measured per row, and reserved on an unticked row of the
/// group too ([`Item::checked`]'s `Some(false)`), so the labels of a radio
/// group start in one column whichever of them is ticked — a label that
/// stepped sideways as the tick moved would be the menu moving under the eye.
const CHECK_COLUMN: f32 = 12.0 + ICON_GAP;

/// The tick, with the patched font (nf-fa-check) and without it.
const CHECK_ICON: char = '\u{f00c}';
const CHECK_GLYPH: &str = "✓";

/// What a menu row does. The identity only — the doing is [`crate::app`]'s,
/// exactly as with [`df_core::keymap::Command`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Open,
    /// The `n`th opener rule that matched the hovered file.
    OpenWith(usize),
    /// The parent row of the opener submenu; activating it flies the submenu
    /// out rather than doing anything itself.
    OpenWithMenu,
    Yank,
    Cut,
    Paste,
    Rename,
    Trash,
    /// Unpack the hovered archive into this directory (PLAN §7.3).
    ExtractHere,
    /// …or into a new folder named after it.
    ExtractSubfolder,
    /// …or, with several archives selected, all of them into one new folder.
    ExtractMerged,
    CopyPath,
    CopyName,
    Properties,
    /// Put a trashed item back where it came from (PLAN §7.4).
    Restore,
    /// Destroy trashed items for good.
    Purge,
    /// …and destroy all of them.
    EmptyTrash,
    /// An app-menu row: the command it *is*, run through the one door its key
    /// goes through, so the row cannot behave differently from the key.
    Run(Command),
    /// A file dialog's type filter, by its index in the dialog's own list:
    /// show only the files it admits.
    FileType(usize),
    /// …or every file, whatever the dialog's filters say.
    AllFiles,
    /// A row that does nothing itself: the app menu's "View", "Sort" and "File
    /// type", which are only the lists they fly out, and "Reverse" while the
    /// sort has no direction to reverse.
    Nothing,
}

/// One row.
#[derive(Debug, Clone)]
pub struct Item {
    pub label: String,
    /// The keyboard equivalent, drawn right-aligned and dim. Empty only for a
    /// row the keyboard reaches some other way — the opener submenu's rows are
    /// all `O`, which their parent already says, and "Empty trash" is the
    /// palette's — or for a command nobody has bound.
    pub keys: String,
    pub action: Action,
    /// Dimmed and inert. A disabled row is *shown*, not hidden: a menu whose
    /// rows move about depending on what is selected is a menu you cannot aim
    /// at from memory (`delightful-ui` §8).
    pub enabled: bool,
    /// Draw a separator above this row.
    pub gap_before: bool,
    /// A check or radio row: `Some(true)` draws the tick in its column in front
    /// of the label, `Some(false)` keeps the column empty so the labels of a
    /// group still start in one place, and `None` is an ordinary row with no
    /// column at all.
    pub checked: Option<bool>,
    /// The rows this one flies out, when it is a parent. One level: a submenu's
    /// own rows are leaves.
    pub submenu: Option<Vec<Item>>,
}

impl Item {
    fn new(label: &str, keys: &str, action: Action, enabled: bool) -> Item {
        Item {
            label: label.to_string(),
            keys: keys.to_string(),
            action,
            enabled,
            gap_before: false,
            checked: None,
            submenu: None,
        }
    }

    fn after_gap(mut self) -> Item {
        self.gap_before = true;
        self
    }

    fn check(mut self, on: bool) -> Item {
        self.checked = Some(on);
        self
    }

    fn with_submenu(mut self, rows: Vec<Item>) -> Item {
        self.submenu = Some(rows);
        self
    }

    /// Does this row fly a submenu out?
    pub fn has_submenu(&self) -> bool {
        self.submenu.is_some()
    }
}

// ── The context menu ────────────────────────────────────────────────────────

/// What the menu needs to know about the world to decide its rows.
///
/// A plain struct of answers rather than a borrow of the app, so the enablement
/// rules are testable without a window — which matters, because "why is Paste
/// grey" is a question that has to have one answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Facts {
    /// Is there a row under the cursor at all? An empty directory has none.
    pub has_row: bool,
    /// What the cursor row is, when there is one.
    pub is_dir: bool,
    /// How many files an operation would act on: the selection, or the cursor
    /// row.
    pub targets: usize,
    /// Is there anything on the internal clipboard?
    ///
    /// **Only the internal one.** `p` also falls back to the *system*
    /// clipboard (PLAN §7.4), but finding out what that is holding means
    /// shelling out to `wl-paste`, and doing that on every right-click would
    /// put a process spawn in the path of opening a menu. So the row reports
    /// what this program is carrying, and the fallback is a thing `p` does.
    pub clipboard: bool,
    /// Whether the cursor row is an archive something on this machine can
    /// extract (PLAN §7.3) — by the reader here, or by 7-Zip or `bsdtar`.
    ///
    /// The extract rows are **hidden**, not disabled, when it is not — the
    /// menu's general rule is that a row stays put and greys out, so the shape
    /// is aimable from memory, and that rule is about rows that *sometimes*
    /// apply to the thing under the pointer. "Extract" never applies to a text
    /// file, and two permanently grey rows on every right-click in a source
    /// directory would be two rows of noise to read past.
    pub archive: bool,
    /// How many archives the targets come to, a multi-part set counting once.
    /// Two or more adds "Extract all into one folder"; one would make it
    /// "Extract to folder" under a longer name.
    pub archives: usize,
    /// Whether the list pane is showing the trash (PLAN §7.4).
    ///
    /// A *different menu*, not the ordinary one with rows greyed out. The
    /// menu's usual rule — rows stay put so the shape is aimable from memory —
    /// is about one directory's rows differing from another's; the trash is a
    /// different place with three verbs of its own, and eight permanently grey
    /// rows above them would be eight rows to read past every time.
    pub trash: bool,
    /// How many items the trash holds, for the "Empty trash" row.
    pub trashed: usize,
}

/// The rows, in order, with their enablement.
///
/// `openers` are the names of the opener rules that match the hovered file, in
/// the order the `O` picker offers them: they are the "Open with" submenu.
pub fn items(facts: Facts, openers: &[String]) -> Vec<Item> {
    let acts = facts.targets > 0;
    if facts.trash {
        return vec![
            Item::new("Restore", "Enter", Action::Restore, acts),
            Item::new("Destroy permanently", "D", Action::Purge, acts),
            Item::new("Copy original path", "c c", Action::CopyPath, facts.has_row).after_gap(),
            Item::new("Properties", "Tab", Action::Properties, facts.has_row),
            // Last, after a gap, and the only row that acts on things the
            // pointer is not on: it is the one gesture in this menu that cannot
            // be taken back, so it is the hardest one to hit by accident.
            Item::new("Empty trash", "", Action::EmptyTrash, facts.trashed > 0).after_gap(),
        ];
    }
    // The submenu's rows carry no key of their own: every one of them is `O`,
    // and the parent row already says so.
    let open_with = openers
        .iter()
        .enumerate()
        .map(|(index, name)| Item::new(name, "", Action::OpenWith(index), true))
        .collect();
    let mut items = vec![
        Item::new(
            if facts.is_dir { "Open folder" } else { "Open" },
            "Enter",
            Action::Open,
            facts.has_row,
        ),
        Item::new(
            "Open with",
            "O",
            Action::OpenWithMenu,
            facts.has_row && !openers.is_empty(),
        )
        .with_submenu(open_with),
        Item::new("Copy", "y", Action::Yank, acts).after_gap(),
        Item::new("Cut", "x", Action::Cut, acts),
        Item::new("Paste", "p", Action::Paste, facts.clipboard),
        Item::new("Rename", "r", Action::Rename, facts.has_row),
        Item::new("Move to trash", "d", Action::Trash, acts),
        Item::new("Copy path", "c c", Action::CopyPath, facts.has_row).after_gap(),
        Item::new("Copy name", "c f", Action::CopyName, facts.has_row),
        Item::new("Properties", "Tab", Action::Properties, facts.has_row).after_gap(),
    ];
    if facts.archive {
        // Directly under "Open with", where the eye already is when the
        // question is "what else can I do with this file".
        let at = items
            .iter()
            .position(|item| item.action == Action::OpenWithMenu)
            .map(|i| i + 1)
            .unwrap_or(1);
        // In the opener rule's order: the folder first, because it is what `o`
        // does and the one that cannot make a mess of the directory.
        let mut extract = vec![
            Item::new("Extract to folder", "E", Action::ExtractSubfolder, true),
            Item::new("Extract here", "e", Action::ExtractHere, true),
        ];
        if facts.archives > 1 {
            // No key of its own: the keyboard reaches it through `O`, where
            // the opener picker offers it under the same name.
            extract.push(Item::new(
                "Extract all into one folder",
                "O",
                Action::ExtractMerged,
                true,
            ));
        }
        items.splice(at..at, extract);
    }
    // A menu that opened on empty pane space with everything grey would be a
    // menu about nothing; Paste is the one row that still makes sense there,
    // and `items` already says so.
    items.retain(|item| item.action != Action::OpenWithMenu || !openers.is_empty());
    items
}

// ── A file dialog's type filters ────────────────────────────────────────────

/// The last radio of the type-filter list, and what the chip reads while it
/// is the one ticked.
pub const ALL_FILES: &str = "All files";

/// A file dialog's type filters as radio rows: one per filter, in the order
/// the dialog gave them, the active one ticked, and [`ALL_FILES`] after a gap,
/// ticked when no filter is narrowing the listing (`active` is `None`).
///
/// The chip's popover and the app menu's "File type" list are both exactly
/// this, so the two cannot disagree about which row is which. No keys: the
/// one key near this, `.`, steps a ladder rather than choosing a row, and
/// drawing it beside "All files" would teach it as something it is not.
pub fn type_items<'a>(
    names: impl IntoIterator<Item = &'a str>,
    active: Option<usize>,
) -> Vec<Item> {
    let mut rows: Vec<Item> = names
        .into_iter()
        .enumerate()
        .map(|(index, name)| {
            Item::new(name, "", Action::FileType(index), true).check(active == Some(index))
        })
        .collect();
    rows.push(
        Item::new(ALL_FILES, "", Action::AllFiles, true)
            .check(active.is_none())
            .after_gap(),
    );
    rows
}

// ── The app menu ────────────────────────────────────────────────────────────

/// What the app menu needs to know to decide its rows: [`Facts`]' counterpart,
/// and a plain struct for the same reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppFacts {
    /// A `--chooser-file` session: this window is some other program's file
    /// dialog, where `q` cancels the dialog rather than quitting a file
    /// manager — so the last row says what it will actually do.
    pub picker: bool,
    /// Whether the list pane is a directory on this machine, rather than an
    /// archive's interior, a remote service or the trash. "Go to path…" types
    /// a path this disk has to resolve, so it refuses anywhere else — with a
    /// check of its own rather than the gate's lists, which is why it needs a
    /// fact of its own here.
    pub local: bool,
    /// How many files an operation would act on: the selection, or the cursor
    /// row.
    pub targets: usize,
    /// Is there anything on the internal clipboard? The same question, and the
    /// same deliberately narrow answer, as [`Facts::clipboard`].
    pub clipboard: bool,
    /// Where this directory is on the view-scale ladder (PLAN §4.1).
    pub scale: ViewScale,
    pub hidden: bool,
    pub linemode: LineMode,
    pub sort: SortBy,
    pub reverse: bool,
}

/// The sort command that orders by `by`, in the direction `reverse` says — or
/// `None` for the one order that has no command (`none`, which only the config
/// can ask for).
///
/// A shuffle has no direction, so both directions of `Random` are `, r`.
fn sort_command(by: SortBy, reverse: bool) -> Option<Command> {
    use Command as C;
    Some(match (by, reverse) {
        (SortBy::Alphabetical, false) => C::SortAlphabetical,
        (SortBy::Alphabetical, true) => C::SortAlphabeticalReverse,
        (SortBy::Natural, false) => C::SortNatural,
        (SortBy::Natural, true) => C::SortNaturalReverse,
        (SortBy::Mtime, false) => C::SortMtime,
        (SortBy::Mtime, true) => C::SortMtimeReverse,
        (SortBy::Btime, false) => C::SortBtime,
        (SortBy::Btime, true) => C::SortBtimeReverse,
        (SortBy::Extension, false) => C::SortExtension,
        (SortBy::Extension, true) => C::SortExtensionReverse,
        (SortBy::Size, false) => C::SortSize,
        (SortBy::Size, true) => C::SortSizeReverse,
        (SortBy::Random, _) => C::SortRandom,
        (SortBy::None, _) => return None,
    })
}

/// The app menu's rows, in order, with their enablement, ticks and submenus.
///
/// Every leaf is a [`Command`] ([`Action::Run`]) and its key is whatever the
/// registry advertises for it, or nothing when it is unbound. `refused` is the
/// question [`crate::app`]'s gate asks before a verb runs where the list pane
/// is showing an archive, a remote service or the trash: a row it would refuse
/// is greyed here rather than left live to toast "not here" when clicked.
///
/// `types` is a file dialog's [`type_items`], flown out of a "File type" row
/// beside View and Sort; empty — every session that is not a dialog with
/// filters — and there is no such row. Absent rather than grey, for the
/// reason the context menu's extract rows are: a file manager has no dialog
/// whose filters the row could ever list, and a row that is grey every time
/// is a row to read past every time.
pub fn app_items(
    facts: AppFacts,
    types: Vec<Item>,
    keymap: &Registry,
    refused: impl Fn(Command) -> bool,
) -> Vec<Item> {
    use Command as C;
    let acts = facts.targets > 0;
    let run = |label: &str, command: Command, enabled: bool| {
        Item::new(
            label,
            &keymap.binding_label(command).unwrap_or_default(),
            Action::Run(command),
            enabled && !refused(command),
        )
    };

    let scales = [
        ("Compact", ViewScale::Compact, C::ViewScaleCompact),
        (
            "Comfortable",
            ViewScale::Comfortable,
            C::ViewScaleComfortable,
        ),
        ("Roomy", ViewScale::Roomy, C::ViewScaleRoomy),
        ("Grid", ViewScale::Grid, C::ViewScaleGrid),
    ];
    let linemodes = [
        ("Size", LineMode::Size, C::LinemodeSize),
        ("Permissions", LineMode::Permissions, C::LinemodePermissions),
        ("Created", LineMode::Btime, C::LinemodeBtime),
        ("Modified", LineMode::Mtime, C::LinemodeMtime),
        ("Owner", LineMode::Owner, C::LinemodeOwner),
        ("None", LineMode::None, C::LinemodeNone),
    ];
    let mut view: Vec<Item> = scales
        .iter()
        .map(|(label, step, command)| run(label, *command, true).check(facts.scale == *step))
        .collect();
    view.push(
        run("Show hidden files", C::ToggleHidden, true)
            .check(facts.hidden)
            .after_gap(),
    );
    for (index, (label, mode, command)) in linemodes.iter().enumerate() {
        let item = run(label, *command, true).check(facts.linemode == *mode);
        view.push(if index == 0 { item.after_gap() } else { item });
    }

    // Each key keeps the direction the listing is in now, so picking a key is
    // "order by this" and never also "and flip it" — the direction is the
    // Reverse row's business. Each row is the command for exactly that, so
    // the key it teaches is the key that does what the click does.
    let sorts = [
        ("Alphabetical", SortBy::Alphabetical),
        ("Natural", SortBy::Natural),
        ("Modified", SortBy::Mtime),
        ("Created", SortBy::Btime),
        ("Extension", SortBy::Extension),
        ("Size", SortBy::Size),
        ("Random", SortBy::Random),
    ];
    let mut sort: Vec<Item> = sorts
        .iter()
        .filter_map(|(label, by)| {
            let command = sort_command(*by, facts.reverse)?;
            Some(run(label, command, true).check(facts.sort == *by))
        })
        .collect();
    // Reverse is the current key again, the other way round. A shuffle has no
    // other way round, so the row greys rather than reshuffling under a name
    // that promised something else.
    let flip = sort_command(facts.sort, !facts.reverse).filter(|_| facts.sort != SortBy::Random);
    sort.push(
        match flip {
            Some(command) => run("Reverse", command, true),
            None => Item::new("Reverse", "", Action::Nothing, false),
        }
        .check(facts.reverse)
        .after_gap(),
    );

    let mut rows = vec![
        run("New tab", C::TabCreate, true),
        run("New window", C::NewWindow, true),
        run("Go to path…", C::GotoPath, facts.local).after_gap(),
        run("Jump to…", C::FuzzyJump, true),
        run("Search everywhere by name…", C::SearchName, true),
        run("Search everywhere inside files…", C::SearchContent, true),
        run("Filter this folder…", C::Filter, true),
        run("Select all", C::SelectAll, true).after_gap(),
        run("Invert selection", C::InvertSelection, true),
        run("Copy", C::Yank, acts).after_gap(),
        run("Cut", C::YankCut, acts),
        run("Paste", C::Paste, facts.clipboard),
        run("Rename", C::Rename, acts),
        run("New file or folder…", C::Create, true),
        run("Move to trash", C::Trash, acts),
        run("Undo", C::Undo, true),
        // Always live: a parent row only opens a list, and a grey one would
        // hide the rows under it that *can* act.
        Item::new("View", "", Action::Nothing, true)
            .with_submenu(view)
            .after_gap(),
        Item::new("Sort", "", Action::Nothing, true).with_submenu(sort),
        run("Mounts…", C::MountManager, true).after_gap(),
        run("Trash", C::OpenTrash, true),
        run("Tasks", C::TasksShow, true),
        run("Selection basket", C::BasketShow, true),
        run("Disk usage", C::DiskUsage, true),
        run("Command palette…", C::CommandPalette, true).after_gap(),
        run("Keyboard shortcuts", C::Help, true),
        run(if facts.picker { "Cancel" } else { "Quit" }, C::Quit, true).after_gap(),
    ];
    if !types.is_empty() {
        // After Sort, in the group of lists about what the pane shows.
        let at = rows
            .iter()
            .position(|item| item.label == "Sort")
            .map_or(rows.len(), |sort| sort + 1);
        rows.insert(
            at,
            Item::new("File type", "", Action::Nothing, true).with_submenu(types),
        );
    }
    rows
}

// ── The menu, while it is up ────────────────────────────────────────────────

/// Which of the two menus this is. They share everything but where they come
/// from and what closes them: the app menu's own key toggles it, and its
/// button draws pressed while it is out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Context,
    App,
    /// A file dialog's type filters, dropped out of their chip on the top
    /// row. The chip stays pressed while it is out, as the app menu's button
    /// does.
    Types,
}

/// What the card is placed from.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Anchor {
    /// Where the pointer was. The card is placed *from* this, never centred on
    /// it: a menu whose first row is under the pointer is a menu you can
    /// activate by twitching.
    Point(egui::Pos2),
    /// Under a control — the app menu's button — dropping out of its
    /// bottom-left corner, [`BELOW_GAP`] down.
    Below(egui::Rect),
}

/// The menu, while it is up.
pub struct Menu {
    pub kind: Kind,
    pub anchor: Anchor,
    pub items: Vec<Item>,
    /// The keyboard's row, once `↑`/`↓` has been pressed. `None` until then —
    /// a menu opened by the pointer must not pre-select anything, or `Enter`
    /// would do something nobody aimed at.
    pub cursor: Option<usize>,
    /// Which row's submenu is out, by its index in `items`, and where the
    /// submenu's own cursor is.
    pub submenu: Option<usize>,
    pub sub_cursor: Option<usize>,
    /// Set when the menu is dismissed; it is drawn fading until [`FADE`] is up.
    pub closing: Option<Instant>,
}

impl Menu {
    /// The right-click menu, placed from where the pointer was.
    pub fn context(at: egui::Pos2, items: Vec<Item>) -> Menu {
        Menu::new(Kind::Context, Anchor::Point(at), items)
    }

    /// The app menu, hanging from its button.
    pub fn app(button: egui::Rect, items: Vec<Item>) -> Menu {
        Menu::new(Kind::App, Anchor::Below(button), items)
    }

    /// The type-filter popover ([`type_items`]), hanging from its chip.
    pub fn types(chip: egui::Rect, items: Vec<Item>) -> Menu {
        Menu::new(Kind::Types, Anchor::Below(chip), items)
    }

    fn new(kind: Kind, anchor: Anchor, items: Vec<Item>) -> Menu {
        Menu {
            kind,
            anchor,
            items,
            cursor: None,
            submenu: None,
            sub_cursor: None,
            closing: None,
        }
    }

    /// Is this menu still taking input? A closing one is pixels, not a surface.
    pub fn live(&self) -> bool {
        self.closing.is_none()
    }

    pub fn alpha(&self, now: Instant) -> f32 {
        match self.closing {
            None => 1.0,
            Some(at) => {
                let elapsed = now.saturating_duration_since(at).as_secs_f32();
                (1.0 - elapsed / FADE.as_secs_f32()).clamp(0.0, 1.0)
            }
        }
    }

    /// Has the fade finished, so the app can drop it?
    pub fn spent(&self, now: Instant) -> bool {
        self.closing
            .is_some_and(|at| now.saturating_duration_since(at) >= FADE)
    }

    /// The rows of the submenu that is out, if one is.
    pub fn sub_items(&self) -> Option<&[Item]> {
        self.items.get(self.submenu?)?.submenu.as_deref()
    }

    /// `↑`/`↓`, in whichever list is live. Skips disabled rows: a keyboard that
    /// stopped on a grey row would be offering something it cannot do.
    pub fn move_cursor(&mut self, delta: isize) {
        if let Some(rows) = self.sub_items() {
            if let Some(next) = step(rows, self.sub_cursor, delta) {
                self.sub_cursor = Some(next);
            }
            return;
        }
        if let Some(next) = step(&self.items, self.cursor, delta) {
            self.cursor = Some(next);
        }
    }

    /// What `Enter` would do, or `None` when nothing is picked.
    pub fn activate(&self) -> Option<Action> {
        let item = match self.sub_items() {
            Some(rows) => rows.get(self.sub_cursor?)?,
            None => self.items.get(self.cursor?)?,
        };
        item.enabled.then_some(item.action)
    }

    /// `→`, or hovering a parent row: fly its submenu out.
    ///
    /// The cursor's row — or, before the keyboard has picked a row at all, the
    /// first row that has one, so `→` straight after a right click still opens
    /// "Open with" as it always has. A disabled parent or an empty list opens
    /// nothing: a card that flew out with nothing in it would be a dead end.
    pub fn open_submenu(&mut self) -> bool {
        let index = match self.cursor {
            Some(index) => index,
            None => match self.items.iter().position(Item::has_submenu) {
                Some(index) => index,
                None => return false,
            },
        };
        let Some(item) = self.items.get(index) else {
            return false;
        };
        if !item.enabled || item.submenu.as_ref().is_none_or(Vec::is_empty) {
            return false;
        }
        self.cursor = Some(index);
        // A different parent is a different list, and a cursor carried across
        // from the last one would be pointing at a row it never chose.
        if self.submenu != Some(index) {
            self.submenu = Some(index);
            self.sub_cursor = None;
        }
        true
    }

    /// `←`: back to the parent list, keeping the row the submenu belongs to.
    pub fn close_submenu(&mut self) -> bool {
        if self.submenu.is_none() {
            return false;
        }
        self.submenu = None;
        self.sub_cursor = None;
        true
    }
}

/// One `↑`/`↓` through `items` from `from`, wrapping and stepping over the
/// disabled rows. From nothing, `↓` lands on the first live row and `↑` on
/// the last. `None` when no row is live at all.
fn step(items: &[Item], from: Option<usize>, delta: isize) -> Option<usize> {
    let len = items.len();
    if len == 0 {
        return None;
    }
    let mut at = match from {
        Some(at) => at as isize,
        None if delta < 0 => len as isize,
        None => -1,
    };
    for _ in 0..len {
        at = wrap(at + delta, len) as isize;
        if items[at as usize].enabled {
            return Some(at as usize);
        }
    }
    None
}

fn wrap(at: isize, len: usize) -> usize {
    let len = len as isize;
    (((at % len) + len) % len) as usize
}

// ── Geometry ────────────────────────────────────────────────────────────────

/// Where the card and its rows are.
pub struct Geometry {
    pub card: egui::Rect,
    /// One rect per item, in `items` order.
    pub rows: Vec<egui::Rect>,
    /// The submenu's card and rows, when it is out.
    pub sub: Option<(egui::Rect, Vec<egui::Rect>)>,
}

impl Geometry {
    /// What the pointer is over.
    pub fn hit(&self, pos: egui::Pos2) -> Option<Control> {
        if let Some((_, rows)) = &self.sub {
            if let Some(index) = rows.iter().position(|r| r.contains(pos)) {
                return Some(Control::SubmenuItem(index));
            }
        }
        self.rows
            .iter()
            .position(|r| r.contains(pos))
            .map(Control::MenuItem)
    }

    /// Is the pointer anywhere on the menu at all — including the card's
    /// padding and its separators? A press there must dismiss nothing.
    pub fn contains(&self, pos: egui::Pos2) -> bool {
        self.card.contains(pos) || self.sub.as_ref().is_some_and(|(c, _)| c.contains(pos))
    }

    pub fn rect_of(&self, control: Control) -> Option<egui::Rect> {
        match control {
            Control::MenuItem(index) => self.rows.get(index).copied(),
            Control::SubmenuItem(index) => self
                .sub
                .as_ref()
                .and_then(|(_, rows)| rows.get(index).copied()),
            _ => None,
        }
    }
}

/// A card of `size` placed from `anchor`, kept inside `area`.
///
/// The rule every menu on every platform uses, and the reason it is a named
/// function with a test: the card grows away from its anchor, **flips** to the
/// other side when there is not room, and only slides as a last resort.
///
/// From a point, it grows down and to the right of the pointer, and flipping is
/// what keeps the pointer on a corner of the card rather than in the middle of
/// it, which is what makes the first row still be one flick away near an edge.
///
/// From under a control, it drops out of the control's bottom-left corner, and
/// flips to hang its right edge from the control's right edge, or to stand on
/// the control's top edge, when that side has no room. It stays below if
/// neither side can hold it whole and slides up only as far as it must: a menu
/// that jumped over its own button to be cut off at the top instead would have
/// travelled for nothing.
pub fn place(area: egui::Rect, anchor: Anchor, size: egui::Vec2) -> egui::Rect {
    let (left, top) = match anchor {
        Anchor::Point(at) => {
            let fits_right = at.x + size.x <= area.right() - MARGIN;
            let fits_below = at.y + size.y <= area.bottom() - MARGIN;
            (
                if fits_right { at.x } else { at.x - size.x },
                if fits_below { at.y } else { at.y - size.y },
            )
        }
        Anchor::Below(control) => {
            let below = control.bottom() + BELOW_GAP;
            let above = control.top() - BELOW_GAP - size.y;
            let fits_right = control.left() + size.x <= area.right() - MARGIN;
            let fits_below = below + size.y <= area.bottom() - MARGIN;
            let fits_above = above >= area.top() + MARGIN;
            (
                if fits_right {
                    control.left()
                } else {
                    control.right() - size.x
                },
                if fits_below || !fits_above {
                    below
                } else {
                    above
                },
            )
        }
    };
    // The slide: a window too small for the card either way. Clamped rather
    // than allowed off screen, because a menu with rows past the edge is a menu
    // with unreachable rows.
    let left = left.clamp(
        area.left() + MARGIN,
        (area.right() - MARGIN - size.x).max(area.left() + MARGIN),
    );
    let top = top.clamp(
        area.top() + MARGIN,
        (area.bottom() - MARGIN - size.y).max(area.top() + MARGIN),
    );
    egui::Rect::from_min_size(egui::pos2(left, top), size)
}

/// How tall a list of items is, separators included.
pub fn height(items: &[Item]) -> f32 {
    let gaps = items.iter().filter(|i| i.gap_before).count() as f32;
    items.len() as f32 * ROW + gaps * SEPARATOR + CARD_PAD * 2.0
}

/// How wide a card for `items` has to be, measured from the text.
///
/// A top-level list reserves the key column and the chevron's on every row, so
/// the key column does not step sideways on the one row that has a chevron. A
/// submenu has no chevrons of its own, and gives a key column only to the rows
/// that have a key — the opener list is names alone, and a card padded out for
/// keys it does not have would be a card with a hole down its right side.
fn width(items: &[Item], painter: &egui::Painter, top_level: bool) -> f32 {
    let label_font = egui::FontId::proportional(FONT);
    items
        .iter()
        .map(|item| {
            let check = if item.checked.is_some() {
                CHECK_COLUMN
            } else {
                0.0
            };
            let label = crate::chrome::text_width(painter, &item.label, label_font.clone());
            let keys = if top_level || !item.keys.is_empty() {
                KEY_GAP + crate::chrome::text_width(painter, &item.keys, key_font(FONT - 1.0))
            } else {
                0.0
            };
            let chevron = if top_level { CHEVRON_COLUMN } else { 0.0 };
            check + label + keys + chevron
        })
        .fold(MIN_WIDTH, f32::max)
        + CARD_PAD * 2.0
}

/// The rows of `items` stacked down `card`, separators opening their gaps.
fn stack(card: egui::Rect, items: &[Item]) -> Vec<egui::Rect> {
    let mut rows = Vec::with_capacity(items.len());
    let mut y = card.top() + CARD_PAD;
    for item in items {
        if item.gap_before {
            y += SEPARATOR;
        }
        rows.push(egui::Rect::from_min_size(
            egui::pos2(card.left() + CARD_PAD, y),
            egui::vec2(card.width() - CARD_PAD * 2.0, ROW),
        ));
        y += ROW;
    }
    rows
}

/// Lay the menu out. Needs a painter because the width is measured from the
/// text — a menu sized by a guess is a menu with a ragged key column.
pub fn geometry(area: egui::Rect, menu: &Menu, painter: &egui::Painter) -> Geometry {
    let size = egui::vec2(
        width(&menu.items, painter, true).min((area.width() - MARGIN * 2.0).max(MIN_WIDTH)),
        height(&menu.items),
    );
    let card = place(area, menu.anchor, size);
    let rows = stack(card, &menu.items);

    let sub = menu.submenu.and_then(|parent| {
        let items = menu.sub_items()?;
        let parent = rows.get(parent).copied().unwrap_or(card);
        let sub_size = egui::vec2(
            width(items, painter, false).min((area.width() - MARGIN * 2.0).max(MIN_WIDTH)),
            height(items),
        );
        // Anchored at the parent row's outer corner, so `place` flips it to the
        // *left* of the card near the right edge of the window — which is where
        // every submenu on every platform goes.
        let anchor = egui::pos2(card.right() - SUBMENU_OVERLAP, parent.top() - CARD_PAD);
        let sub_card = if anchor.x + sub_size.x <= area.right() - MARGIN {
            place(area, Anchor::Point(anchor), sub_size)
        } else {
            place(
                area,
                Anchor::Point(egui::pos2(card.left() + SUBMENU_OVERLAP, anchor.y)),
                sub_size,
            )
        };
        Some((sub_card, stack(sub_card, items)))
    });

    Geometry { card, rows, sub }
}

/// The room the `▸` gets at a row's right-hand end: the glyph, seven and a
/// half points at [`FONT`], and the gap that keeps a key beside it from
/// touching it.
const CHEVRON_COLUMN: f32 = 12.0;

/// Where a row's key column ends.
///
/// In a list with a parent row anywhere in it, the whole list keeps
/// [`CHEVRON_COLUMN`] clear at its right-hand end and every key stands to the
/// left of it: "Open with" reads `O ▸` rather than the two drawn on top of
/// each other, and the other rows' keys stay in one column with its `O`. A
/// list with no parent row has no `▸` to make room for, and its keys keep the
/// edge.
fn keys_right(row: egui::Rect, chevrons: bool) -> f32 {
    row.right() - PAD_X - if chevrons { CHEVRON_COLUMN } else { 0.0 }
}

// ── Paint ───────────────────────────────────────────────────────────────────

/// Draw the menu and its submenu.
pub fn paint(
    paint: &Painting<'_>,
    menu: &Menu,
    geometry: &Geometry,
    hovers: &Hovers<Control>,
    ripples: &Ripples<Control>,
    now: Instant,
) {
    let alpha = menu.alpha(now);
    if alpha <= 0.0 {
        return;
    }
    card(paint, geometry.card, alpha);
    let selected = menu.cursor.filter(|_| menu.submenu.is_none());
    list(
        paint,
        &menu.items,
        &geometry.rows,
        Control::MenuItem,
        selected,
        (hovers, ripples),
        alpha,
        now,
    );

    let (Some((sub_card, sub_rows)), Some(items)) = (&geometry.sub, menu.sub_items()) else {
        return;
    };
    card(paint, *sub_card, alpha);
    list(
        paint,
        items,
        sub_rows,
        Control::SubmenuItem,
        menu.sub_cursor,
        (hovers, ripples),
        alpha,
        now,
    );
}

/// One card's rows and the separators between them.
#[allow(clippy::too_many_arguments)] // a painter's arguments are its inputs
fn list(
    paint: &Painting<'_>,
    items: &[Item],
    rects: &[egui::Rect],
    control: fn(usize) -> Control,
    selected: Option<usize>,
    (hovers, ripples): (&Hovers<Control>, &Ripples<Control>),
    alpha: f32,
    now: Instant,
) {
    let chevrons = items.iter().any(Item::has_submenu);
    for (index, (item, rect)) in items.iter().zip(rects).enumerate() {
        if item.gap_before {
            // The hairline in the middle of the gap it opened. Inset to the
            // card's text column so it reads as separating rows rather than as
            // cutting the card in half.
            let y = rect.top() - SEPARATOR / 2.0;
            paint.painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(rect.left() + PAD_X, y),
                    egui::pos2(rect.right() - PAD_X, y + 1.0),
                ),
                0,
                fade_color(paint.palette.surface1, alpha),
            );
        }
        row(
            paint,
            *rect,
            item,
            chevrons,
            selected == Some(index),
            control(index),
            hovers,
            ripples,
            alpha,
            now,
        );
    }
}

/// One row of either list. `chevrons` is whether the list it is in keeps a
/// chevron column ([`keys_right`]).
#[allow(clippy::too_many_arguments)]
fn row(
    paint: &Painting<'_>,
    rect: egui::Rect,
    item: &Item,
    chevrons: bool,
    selected: bool,
    key: Control,
    hovers: &Hovers<Control>,
    ripples: &Ripples<Control>,
    alpha: f32,
    now: Instant,
) {
    let palette = paint.palette;
    let enabled = item.enabled;
    // A disabled row takes no hover and no press: the pointer must not be able
    // to make something inert look live.
    let hover = if enabled { hovers.hover(key) } else { 0.0 };
    let press = if enabled { hovers.press(key) } else { 0.0 };
    let rect = pressed_rect(rect, press);
    let lit = hover.max(f32::from(selected));
    if lit > 0.0 {
        paint.painter.rect_filled(
            rect,
            CARD_ROW_RADIUS,
            fade_color(mix(palette.crust, palette.surface1, lit), alpha),
        );
    }
    let inside = paint.painter.with_clip_rect(rect);
    for splash in ripples.splashes(key, now) {
        inside.circle_filled(
            splash.center,
            splash.radius,
            egui::Color32::from_white_alpha((splash.alpha * alpha * 255.0).round() as u8),
        );
    }

    let text_color = if enabled {
        fade_color(palette.text, alpha)
    } else {
        // Dim, not hidden — see [`Item::enabled`].
        fade_color(palette.overlay0, alpha)
    };
    let key_color = fade_color(palette.overlay0, alpha);
    let keys_width = if item.keys.is_empty() {
        0.0
    } else {
        let galley = inside.layout_no_wrap(item.keys.clone(), key_font(FONT - 1.0), key_color);
        let width = galley.size().x;
        inside.galley(
            egui::pos2(
                keys_right(rect, chevrons) - width,
                rect.center().y - galley.size().y / 2.0,
            ),
            galley,
            key_color,
        );
        width + PAD_X
    };
    if item.has_submenu() {
        // The submenu's promise, in the place every menu puts it.
        inside.text(
            egui::pos2(rect.right() - PAD_X, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            "▸",
            egui::FontId::proportional(FONT),
            text_color,
        );
    }
    // The tick, in the column [`CHECK_COLUMN`] keeps for it — and that column
    // kept empty on the unticked rows of the group.
    let check = match item.checked {
        Some(on) => {
            if on {
                inside.text(
                    egui::pos2(rect.left() + PAD_X, rect.center().y),
                    egui::Align2::LEFT_CENTER,
                    crate::icons::glyph(paint.nerd, CHECK_ICON, CHECK_GLYPH),
                    egui::FontId::proportional(FONT),
                    text_color,
                );
            }
            CHECK_COLUMN
        }
        None => 0.0,
    };
    crate::chrome::truncated(
        &inside,
        egui::pos2(rect.left() + PAD_X + check, rect.center().y),
        &item.label,
        text_color,
        (rect.width()
            - PAD_X * 2.0
            - check
            - keys_width
            - if chevrons { CHEVRON_COLUMN } else { 0.0 })
        .max(0.0),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> Facts {
        Facts {
            has_row: true,
            is_dir: false,
            targets: 1,
            clipboard: true,
            archive: false,
            archives: 0,
            trash: false,
            trashed: 0,
        }
    }

    /// The two opener rules the tests' hovered file matches.
    fn openers() -> Vec<String> {
        vec!["Zed".to_string(), "Firefox".to_string()]
    }

    /// Every row carries a key, because every row *is* one.
    #[test]
    fn every_menu_row_teaches_its_own_shortcut() {
        for item in items(facts(), &openers()) {
            assert!(!item.keys.is_empty(), "{} has no key", item.label);
        }
    }

    /// The context menu, row for row, as it was before the app menu shared its
    /// model: the same labels, keys, verbs, gaps and enablement, no ticks, and
    /// "Open with" flying out the opener rules by name with no keys of their
    /// own.
    #[test]
    fn the_context_menu_rows_are_unchanged() {
        let rows = items(facts(), &openers());
        let got: Vec<(&str, &str, Action, bool)> = rows
            .iter()
            .map(|i| (i.label.as_str(), i.keys.as_str(), i.action, i.gap_before))
            .collect();
        assert_eq!(
            got,
            vec![
                ("Open", "Enter", Action::Open, false),
                ("Open with", "O", Action::OpenWithMenu, false),
                ("Copy", "y", Action::Yank, true),
                ("Cut", "x", Action::Cut, false),
                ("Paste", "p", Action::Paste, false),
                ("Rename", "r", Action::Rename, false),
                ("Move to trash", "d", Action::Trash, false),
                ("Copy path", "c c", Action::CopyPath, true),
                ("Copy name", "c f", Action::CopyName, false),
                ("Properties", "Tab", Action::Properties, true),
            ]
        );
        assert!(rows.iter().all(|i| i.enabled && i.checked.is_none()));
        // Only "Open with" is a parent, and its list is the openers, in order.
        let parents: Vec<&str> = rows
            .iter()
            .filter(|i| i.has_submenu())
            .map(|i| i.label.as_str())
            .collect();
        assert_eq!(parents, vec!["Open with"]);
        let sub = rows[1].submenu.as_ref().expect("the opener list");
        let sub: Vec<(&str, &str, Action, bool)> = sub
            .iter()
            .map(|i| (i.label.as_str(), i.keys.as_str(), i.action, i.enabled))
            .collect();
        assert_eq!(
            sub,
            vec![
                ("Zed", "", Action::OpenWith(0), true),
                ("Firefox", "", Action::OpenWith(1), true),
            ]
        );
    }

    /// Enablement, one clause at a time.
    #[test]
    fn the_rows_are_enabled_by_what_is_actually_there() {
        let enabled = |facts: Facts, openers: &[String], action: Action| {
            items(facts, openers)
                .into_iter()
                .find(|i| i.action == action)
                .map(|i| i.enabled)
        };
        // An empty directory: nothing to open, nothing to act on — but a
        // clipboard is still pastable, which is the whole reason the menu
        // opens on empty pane space at all.
        let empty = Facts {
            has_row: false,
            is_dir: false,
            targets: 0,
            clipboard: true,
            archive: false,
            archives: 0,
            trash: false,
            trashed: 0,
        };
        assert_eq!(enabled(empty, &[], Action::Open), Some(false));
        assert_eq!(enabled(empty, &[], Action::Yank), Some(false));
        assert_eq!(enabled(empty, &[], Action::Trash), Some(false));
        assert_eq!(enabled(empty, &[], Action::Paste), Some(true));
        assert_eq!(enabled(empty, &[], Action::Properties), Some(false));
        // An empty clipboard greys exactly one row.
        let nothing_yanked = Facts {
            clipboard: false,
            ..facts()
        };
        assert_eq!(
            enabled(nothing_yanked, &openers(), Action::Paste),
            Some(false)
        );
        assert_eq!(
            enabled(nothing_yanked, &openers(), Action::Yank),
            Some(true)
        );
        // A file no opener rule matches loses the submenu row entirely: a
        // "Open with ▸" that flew out an empty card would be a dead end.
        assert_eq!(enabled(facts(), &[], Action::OpenWithMenu), None);
        assert_eq!(
            enabled(facts(), &openers(), Action::OpenWithMenu),
            Some(true)
        );
    }

    /// A folder's first row says so — the same distinction `Enter` makes.
    #[test]
    fn a_directory_row_is_labelled_as_one() {
        let dir = Facts {
            is_dir: true,
            ..facts()
        };
        assert_eq!(items(dir, &openers())[0].label, "Open folder");
        assert_eq!(items(facts(), &openers())[0].label, "Open");
    }

    fn area() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, 900.0))
    }

    /// The anchor flip: the card grows away from whichever edge it is near, and
    /// never leaves the window.
    #[test]
    fn the_card_flips_rather_than_hanging_off_the_edge() {
        let size = egui::vec2(200.0, 300.0);
        let at = |x: f32, y: f32| Anchor::Point(egui::pos2(x, y));
        // Room both ways: down and to the right of the pointer.
        let a = place(area(), at(100.0, 100.0), size);
        assert_eq!(a.min, egui::pos2(100.0, 100.0));
        // Near the right edge: flipped left, so the pointer is on its right
        // corner rather than past its edge.
        let b = place(area(), at(1380.0, 100.0), size);
        assert!((b.right() - 1380.0).abs() < 1e-3, "{b:?}");
        // Near the bottom: flipped up.
        let c = place(area(), at(100.0, 880.0), size);
        assert!((c.bottom() - 880.0).abs() < 1e-3, "{c:?}");
        // Both at once.
        let d = place(area(), at(1380.0, 880.0), size);
        assert!((d.right() - 1380.0).abs() < 1e-3);
        assert!((d.bottom() - 880.0).abs() < 1e-3);
        for rect in [a, b, c, d] {
            assert!(rect.left() >= area().left() && rect.right() <= area().right() + 1e-3);
            assert!(rect.top() >= area().top() && rect.bottom() <= area().bottom() + 1e-3);
        }
    }

    /// A window too small for the card at all slides it in rather than letting
    /// rows fall off the edge.
    #[test]
    fn a_card_too_big_for_the_window_is_slid_in() {
        let tiny = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(120.0, 100.0));
        let rect = place(
            tiny,
            Anchor::Point(egui::pos2(110.0, 90.0)),
            egui::vec2(200.0, 300.0),
        );
        assert!((rect.left() - (tiny.left() + MARGIN)).abs() < 1e-3);
        assert!((rect.top() - (tiny.top() + MARGIN)).abs() < 1e-3);
    }

    /// The app menu drops out of its button: top-left at the button's
    /// bottom-left, a gap down. Out of room to the right it hangs from the
    /// button's right edge instead; out of room below it stands on the
    /// button's top edge; with room on neither side it stays below and slides
    /// up only as far as the window makes it.
    #[test]
    fn a_card_below_a_control_drops_out_of_its_corner() {
        let size = egui::vec2(200.0, 300.0);
        let button = egui::Rect::from_min_size(egui::pos2(10.0, 40.0), egui::vec2(28.0, 28.0));
        let a = place(area(), Anchor::Below(button), size);
        assert_eq!(a.min, egui::pos2(10.0, 68.0 + BELOW_GAP));
        assert_eq!(a.size(), size);

        let right = egui::Rect::from_min_size(egui::pos2(1350.0, 40.0), egui::vec2(28.0, 28.0));
        let b = place(area(), Anchor::Below(right), size);
        assert!((b.right() - right.right()).abs() < 1e-3, "{b:?}");
        assert!((b.top() - (right.bottom() + BELOW_GAP)).abs() < 1e-3);

        let low = egui::Rect::from_min_size(egui::pos2(10.0, 800.0), egui::vec2(28.0, 28.0));
        let c = place(area(), Anchor::Below(low), size);
        assert!((c.bottom() - (low.top() - BELOW_GAP)).abs() < 1e-3, "{c:?}");
        assert_eq!(c.left(), low.left());

        // Taller than the room on either side: below, slid up to fit.
        let short = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, 400.0));
        let mid = egui::Rect::from_min_size(egui::pos2(10.0, 150.0), egui::vec2(28.0, 28.0));
        let d = place(short, Anchor::Below(mid), size);
        assert!(
            (d.bottom() - (short.bottom() - MARGIN)).abs() < 1e-3,
            "{d:?}"
        );
        for (rect, within) in [(a, area()), (b, area()), (c, area()), (d, short)] {
            assert!(rect.left() >= within.left() && rect.right() <= within.right() + 1e-3);
            assert!(rect.top() >= within.top() && rect.bottom() <= within.bottom() + 1e-3);
        }
    }

    /// The extract rows appear only for an archive, and they land where the
    /// eye already is — next to "Open with", not at the bottom — in the
    /// opener rule's order: the folder, here, and all-into-one only when there
    /// are several archives to put into one.
    #[test]
    fn extract_rows_appear_only_on_an_archive() {
        let plain = items(facts(), &openers());
        assert!(plain.iter().all(|i| i.action != Action::ExtractHere));

        let rows = items(
            Facts {
                archive: true,
                archives: 1,
                ..facts()
            },
            &openers(),
        );
        let at = |action: Action| rows.iter().position(|i| i.action == action);
        let sub = at(Action::ExtractSubfolder).expect("extract to folder");
        let here = at(Action::ExtractHere).expect("extract here");
        assert_eq!(here, sub + 1, "the extract rows are adjacent");
        assert!(sub > at(Action::Open).expect("open"));
        assert!(here < at(Action::Yank).expect("copy"));
        assert!(rows[here].enabled && rows[sub].enabled);
        assert_eq!(rows[sub].label, "Extract to folder");
        assert_eq!(rows[sub].keys, "E");
        assert_eq!(rows[here].label, "Extract here");
        assert_eq!(rows[here].keys, "e");
        assert_eq!(
            at(Action::ExtractMerged),
            None,
            "one archive has nothing to merge"
        );

        let rows = items(
            Facts {
                archive: true,
                archives: 3,
                targets: 3,
                ..facts()
            },
            &openers(),
        );
        let at = |action: Action| rows.iter().position(|i| i.action == action);
        let merged = at(Action::ExtractMerged).expect("extract all into one folder");
        assert_eq!(merged, at(Action::ExtractHere).expect("here") + 1);
        assert_eq!(rows[merged].label, "Extract all into one folder");
    }

    /// The trash gets a menu of its own — three verbs, not the ordinary ten
    /// with seven of them grey (PLAN §7.4).
    #[test]
    fn the_trash_gets_its_own_menu() {
        let in_trash = Facts {
            trash: true,
            trashed: 4,
            ..facts()
        };
        let rows = items(in_trash, &openers());
        let actions: Vec<Action> = rows.iter().map(|item| item.action).collect();
        assert_eq!(
            actions,
            vec![
                Action::Restore,
                Action::Purge,
                Action::CopyPath,
                Action::Properties,
                Action::EmptyTrash,
            ]
        );
        // Nothing that would act on a trashed file where it lies.
        assert!(!actions.contains(&Action::Trash));
        assert!(!actions.contains(&Action::Paste));
        assert!(!actions.contains(&Action::Rename));
        // …and the irreversible row is last, after a gap, so it is the hardest
        // one in the menu to hit by accident.
        let last = rows.last().expect("a row");
        assert_eq!(last.action, Action::EmptyTrash);
        assert!(last.gap_before);

        // An empty trash still shows the row, greyed: a menu whose shape
        // changes with its contents is a menu you cannot aim at from memory.
        let empty = Facts {
            trash: true,
            trashed: 0,
            targets: 0,
            has_row: false,
            ..facts()
        };
        let enabled = |action: Action| {
            items(empty, &[])
                .into_iter()
                .find(|i| i.action == action)
                .map(|i| i.enabled)
        };
        assert_eq!(enabled(Action::EmptyTrash), Some(false));
        assert_eq!(enabled(Action::Restore), Some(false));
    }

    /// The keyboard skips grey rows and wraps at both ends.
    #[test]
    fn the_keyboard_walks_only_the_rows_it_can_use() {
        let sparse = Facts {
            has_row: false,
            is_dir: false,
            targets: 0,
            clipboard: true,
            archive: false,
            archives: 0,
            trash: false,
            trashed: 0,
        };
        let mut menu = Menu::context(egui::pos2(0.0, 0.0), items(sparse, &[]));
        assert_eq!(menu.cursor, None, "an unaimed menu picks nothing");
        assert_eq!(menu.activate(), None);
        menu.move_cursor(1);
        assert_eq!(menu.activate(), Some(Action::Paste));
        // Only one row is live, so every further move stays on it.
        menu.move_cursor(1);
        assert_eq!(menu.activate(), Some(Action::Paste));
        menu.move_cursor(-1);
        assert_eq!(menu.activate(), Some(Action::Paste));

        // …and with everything live, `↑` from nothing lands on the last row.
        let mut menu = Menu::context(egui::pos2(0.0, 0.0), items(facts(), &openers()));
        menu.move_cursor(-1);
        assert_eq!(menu.activate(), Some(Action::Properties));
        menu.move_cursor(1);
        assert_eq!(menu.activate(), Some(Action::Open));
    }

    /// The submenu takes the arrows while it is out, and gives them back.
    #[test]
    fn the_submenu_owns_the_keyboard_while_it_is_out() {
        let mut menu = Menu::context(egui::pos2(0.0, 0.0), items(facts(), &openers()));
        assert!(menu.open_submenu());
        menu.move_cursor(1);
        assert_eq!(menu.activate(), Some(Action::OpenWith(0)));
        menu.move_cursor(1);
        assert_eq!(menu.activate(), Some(Action::OpenWith(1)));
        assert!(menu.close_submenu());
        assert!(!menu.close_submenu());
        // The parent row kept the cursor, so `←` does not lose your place.
        assert_eq!(menu.activate(), Some(Action::OpenWithMenu));
    }

    /// The fade is a fade, and it ends.
    #[test]
    fn a_dismissed_menu_fades_and_then_is_spent() {
        let t0 = Instant::now();
        let mut menu = Menu::context(egui::pos2(0.0, 0.0), items(facts(), &openers()));
        assert_eq!(menu.alpha(t0), 1.0);
        assert!(menu.live() && !menu.spent(t0));
        menu.closing = Some(t0);
        assert!(!menu.live());
        let mid = menu.alpha(t0 + FADE / 2);
        assert!(mid > 0.2 && mid < 0.8, "got {mid}");
        assert_eq!(menu.alpha(t0 + FADE), 0.0);
        assert!(menu.spent(t0 + FADE));
    }

    /// The layout: rows stack, separators open the gaps, and the hit test finds
    /// what was drawn.
    #[test]
    fn the_rows_are_laid_out_and_hit_tested_the_same_way() {
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(Default::default(), |ui| {
            let menu = Menu::context(egui::pos2(200.0, 200.0), items(facts(), &openers()));
            let g = geometry(area(), &menu, ui.painter());
            assert_eq!(g.rows.len(), menu.items.len());
            assert!((g.card.height() - height(&menu.items)).abs() < 1e-3);
            for (index, rect) in g.rows.iter().enumerate() {
                assert_eq!(g.hit(rect.center()), Some(Control::MenuItem(index)));
                assert!(g.contains(rect.center()));
                assert!(g.card.contains(rect.center()));
            }
            assert_eq!(g.hit(egui::pos2(0.0, 0.0)), None);
            assert!(!g.contains(egui::pos2(0.0, 0.0)));

            // The submenu is beside the card, not on top of it.
            let mut menu = Menu::context(
                egui::pos2(200.0, 200.0),
                items(facts(), &["Zed".to_string(), "mpv".to_string()]),
            );
            menu.open_submenu();
            let g = geometry(area(), &menu, ui.painter());
            let (sub, rows) = g.sub.as_ref().expect("the submenu is out");
            assert_eq!(rows.len(), 2);
            assert!(sub.left() > g.card.left());
            assert_eq!(g.hit(rows[1].center()), Some(Control::SubmenuItem(1)));
        });
    }

    // ── The app menu ────────────────────────────────────────────────────────

    fn app_facts() -> AppFacts {
        AppFacts {
            picker: false,
            local: true,
            targets: 1,
            clipboard: true,
            scale: ViewScale::Compact,
            hidden: false,
            linemode: LineMode::Size,
            sort: SortBy::Alphabetical,
            reverse: false,
        }
    }

    fn app(facts: AppFacts) -> Vec<Item> {
        app_items(facts, Vec::new(), &Registry::defaults(), |_| false)
    }

    fn row<'a>(rows: &'a [Item], label: &str) -> &'a Item {
        rows.iter()
            .find(|i| i.label == label)
            .unwrap_or_else(|| panic!("no row {label:?}"))
    }

    fn ticked(rows: &[Item]) -> Vec<&str> {
        rows.iter()
            .filter(|i| i.checked == Some(true))
            .map(|i| i.label.as_str())
            .collect()
    }

    /// The whole menu, top to bottom: every row, where the separators fall,
    /// and the key each one teaches — read out of the default registry, not
    /// spelled here a second time.
    #[test]
    fn the_app_menu_rows_are_in_their_groups() {
        let rows = app(app_facts());
        let got: Vec<(&str, &str, bool)> = rows
            .iter()
            .map(|i| (i.label.as_str(), i.keys.as_str(), i.gap_before))
            .collect();
        assert_eq!(
            got,
            vec![
                ("New tab", "t", false),
                ("New window", "Ctrl+n", false),
                ("Go to path…", "Ctrl+l", true),
                ("Jump to…", "z", false),
                ("Search everywhere by name…", "s", false),
                ("Search everywhere inside files…", "S", false),
                ("Filter this folder…", "f", false),
                ("Select all", "Ctrl+a", true),
                ("Invert selection", "Ctrl+r", false),
                ("Copy", "y", true),
                ("Cut", "x", false),
                ("Paste", "p", false),
                ("Rename", "r", false),
                ("New file or folder…", "a", false),
                ("Move to trash", "d", false),
                ("Undo", "u", false),
                ("View", "", true),
                ("Sort", "", false),
                ("Mounts…", "M", true),
                ("Trash", "g t", false),
                ("Tasks", "w", false),
                ("Selection basket", "B", false),
                ("Disk usage", "m u", false),
                ("Command palette…", "Ctrl+p", true),
                ("Keyboard shortcuts", "F1", false),
                ("Quit", "q", true),
            ]
        );
        // Every leaf is the command its key runs, so it cannot drift from it.
        use Command as C;
        let command = |label: &str| match row(&rows, label).action {
            Action::Run(command) => command,
            other => panic!("{label} is {other:?}"),
        };
        assert_eq!(command("New tab"), C::TabCreate);
        assert_eq!(command("Go to path…"), C::GotoPath);
        assert_eq!(command("Copy"), C::Yank);
        assert_eq!(command("Cut"), C::YankCut);
        assert_eq!(command("New file or folder…"), C::Create);
        assert_eq!(command("Trash"), C::OpenTrash);
        assert_eq!(command("Keyboard shortcuts"), C::Help);
        assert_eq!(command("Quit"), C::Quit);
        // Only the two parents fly anything out, and nothing on the top level
        // is a tick.
        let parents: Vec<&str> = rows
            .iter()
            .filter(|i| i.has_submenu())
            .map(|i| i.label.as_str())
            .collect();
        assert_eq!(parents, vec!["View", "Sort"]);
        assert!(rows.iter().all(|i| i.checked.is_none()));
        assert!(rows.iter().all(|i| i.enabled), "everything can act here");
    }

    /// A rebound key is taught where it was moved to, and an unbound command
    /// teaches nothing rather than a key that does not run it.
    #[test]
    fn the_app_menu_teaches_the_keys_the_registry_has() {
        let mut keymap = Registry::defaults();
        let t = df_core::keymap::parse_sequence("t").expect("parses");
        keymap.unbind(df_core::keymap::Context::Files, &t);
        let rows = app_items(app_facts(), Vec::new(), &keymap, |_| false);
        assert_eq!(row(&rows, "New tab").keys, "");
        keymap
            .register(
                df_core::keymap::Context::Files,
                df_core::keymap::parse_sequence("alt+t").expect("parses"),
                Command::TabCreate,
                "New tab",
                df_core::keymap::When::Always,
            )
            .expect("free");
        let rows = app_items(app_facts(), Vec::new(), &keymap, |_| false);
        assert_eq!(row(&rows, "New tab").keys, "Alt+t");
    }

    /// Greyed by what is there: nothing yanked greys Paste, nothing under the
    /// cursor greys the four verbs that need a file — and the parents never
    /// grey, because a grey "View" would hide rows that can still act.
    #[test]
    fn the_app_menu_greys_what_cannot_act_here() {
        let enabled = |facts: AppFacts, label: &str| row(&app(facts), label).enabled;
        let empty = AppFacts {
            targets: 0,
            clipboard: false,
            ..app_facts()
        };
        for label in ["Copy", "Cut", "Rename", "Move to trash", "Paste"] {
            assert!(
                !enabled(empty, label),
                "{label} is live with nothing to act on"
            );
            assert!(enabled(app_facts(), label), "{label} is grey with a file");
        }
        for label in [
            "New tab",
            "New file or folder…",
            "Select all",
            "Undo",
            "View",
            "Sort",
            "Quit",
        ] {
            assert!(enabled(empty, label), "{label} greyed for no reason");
        }

        // An archive, a server or the trash: "Go to path…" has nothing it
        // could resolve there, and it is the only row that fact greys.
        let away = AppFacts {
            local: false,
            ..app_facts()
        };
        assert!(!enabled(away, "Go to path…"));
        assert!(enabled(app_facts(), "Go to path…"));
        let greyed: Vec<String> = app(away)
            .into_iter()
            .filter(|i| !i.enabled)
            .map(|i| i.label)
            .collect();
        assert_eq!(greyed, vec!["Go to path…"]);
    }

    /// "Open with" draws its `O` beside its ▸, not on top of it: a list with a
    /// parent row ends every key a chevron column in from the edge, and the
    /// gap that leaves beside the drawn ▸ is a real one. A list with no
    /// parent row keeps its keys at the edge.
    #[test]
    fn a_key_stands_clear_of_the_chevron() {
        assert!(items(facts(), &openers()).iter().any(Item::has_submenu));
        let in_trash = Facts {
            trash: true,
            ..facts()
        };
        assert!(!items(in_trash, &openers()).iter().any(Item::has_submenu));

        let row = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(200.0, ROW));
        assert_eq!(keys_right(row, false), row.right() - PAD_X);
        assert_eq!(keys_right(row, true), row.right() - PAD_X - CHEVRON_COLUMN);
        let ctx = egui::Context::default();
        let _ = crate::icons::install(&ctx);
        let _ = ctx.run_ui(Default::default(), |ui| {
            // The ▸ is drawn right-aligned at the row's padding.
            let chevron =
                crate::chrome::text_width(ui.painter(), "▸", egui::FontId::proportional(FONT));
            let gap = (row.right() - PAD_X - chevron) - keys_right(row, true);
            assert!(gap >= 4.0, "the key is {gap} pt from the ▸");
        });
    }

    /// The gate's own answer greys the row: whatever `refused` would turn away
    /// with a toast is grey instead — and a parent stays live even when every
    /// row it could be refused for is.
    #[test]
    fn the_app_menu_greys_what_the_gate_would_refuse() {
        use Command as C;
        let trash_like = |command: Command| {
            matches!(
                command,
                C::Yank | C::YankCut | C::Paste | C::Trash | C::Create | C::OpenTrash
            )
        };
        let rows = app_items(app_facts(), Vec::new(), &Registry::defaults(), trash_like);
        for label in [
            "Copy",
            "Cut",
            "Paste",
            "Move to trash",
            "New file or folder…",
            "Trash",
        ] {
            assert!(!row(&rows, label).enabled, "{label} would be refused");
        }
        for label in ["Rename", "Undo", "Select all", "View", "Sort", "Quit"] {
            assert!(row(&rows, label).enabled, "{label} is not refused");
        }
        let everything = app_items(app_facts(), Vec::new(), &Registry::defaults(), |_| true);
        assert!(row(&everything, "View").enabled && row(&everything, "Sort").enabled);
    }

    /// A picker session's last row is the dialog's Cancel, not a file
    /// manager's Quit — the same `q`, named for what it does here.
    #[test]
    fn a_picker_session_cancels_rather_than_quits() {
        let rows = app(AppFacts {
            picker: true,
            ..app_facts()
        });
        let last = rows.last().expect("a row");
        assert_eq!(last.label, "Cancel");
        assert_eq!(last.action, Action::Run(Command::Quit));
        assert_eq!(last.keys, "q");
    }

    /// The View list: the ladder's four steps as one radio group, hidden files
    /// as a check, and the linemodes as a second radio group — separated, every
    /// row in the check column, and the tick on whatever is true now.
    #[test]
    fn the_view_submenu_ticks_what_is_on() {
        let rows = app(AppFacts {
            scale: ViewScale::Roomy,
            hidden: true,
            linemode: LineMode::Mtime,
            ..app_facts()
        });
        let view = row(&rows, "View").submenu.as_ref().expect("a list");
        let labels: Vec<(&str, bool)> = view
            .iter()
            .map(|i| (i.label.as_str(), i.gap_before))
            .collect();
        assert_eq!(
            labels,
            vec![
                ("Compact", false),
                ("Comfortable", false),
                ("Roomy", false),
                ("Grid", false),
                ("Show hidden files", true),
                ("Size", true),
                ("Permissions", false),
                ("Created", false),
                ("Modified", false),
                ("Owner", false),
                ("None", false),
            ]
        );
        assert!(view.iter().all(|i| i.checked.is_some() && i.enabled));
        assert_eq!(ticked(view), vec!["Roomy", "Show hidden files", "Modified"]);
        use Command as C;
        assert_eq!(row(view, "Grid").action, Action::Run(C::ViewScaleGrid));
        assert_eq!(
            row(view, "Grid").keys,
            "",
            "the steps are unbound by default"
        );
        assert_eq!(row(view, "Show hidden files").keys, ".");
        assert_eq!(row(view, "Created").action, Action::Run(C::LinemodeBtime));
        assert_eq!(row(view, "Created").keys, "m b");

        let rows = app(app_facts());
        let view = row(&rows, "View").submenu.as_ref().expect("a list");
        assert_eq!(ticked(view), vec!["Compact", "Size"]);
    }

    /// The Sort list: the keys as a radio group that keeps the direction the
    /// listing is in, and Reverse as the current key the other way round —
    /// grey for a shuffle, which has no other way round.
    #[test]
    fn the_sort_submenu_keeps_the_direction_and_reverses_the_key() {
        use Command as C;
        let sort = |facts: AppFacts| -> Vec<Item> {
            let rows = app(facts);
            row(&rows, "Sort").submenu.clone().expect("a list")
        };
        let forward = sort(AppFacts {
            sort: SortBy::Size,
            ..app_facts()
        });
        let labels: Vec<&str> = forward.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "Alphabetical",
                "Natural",
                "Modified",
                "Created",
                "Extension",
                "Size",
                "Random",
                "Reverse",
            ]
        );
        assert!(row(&forward, "Reverse").gap_before);
        assert!(forward.iter().all(|i| i.checked.is_some()));
        assert_eq!(ticked(&forward), vec!["Size"]);
        assert_eq!(row(&forward, "Modified").action, Action::Run(C::SortMtime));
        assert_eq!(row(&forward, "Modified").keys, ", m");
        assert_eq!(
            row(&forward, "Reverse").action,
            Action::Run(C::SortSizeReverse)
        );
        assert_eq!(row(&forward, "Reverse").keys, ", S");
        assert!(row(&forward, "Reverse").enabled);

        let reversed = sort(AppFacts {
            sort: SortBy::Size,
            reverse: true,
            ..app_facts()
        });
        assert_eq!(ticked(&reversed), vec!["Size", "Reverse"]);
        assert_eq!(
            row(&reversed, "Modified").action,
            Action::Run(C::SortMtimeReverse)
        );
        assert_eq!(row(&reversed, "Reverse").action, Action::Run(C::SortSize));
        // A shuffle is a shuffle both ways round.
        assert_eq!(row(&reversed, "Random").action, Action::Run(C::SortRandom));

        let shuffled = sort(AppFacts {
            sort: SortBy::Random,
            ..app_facts()
        });
        assert_eq!(ticked(&shuffled), vec!["Random"]);
        let reverse = row(&shuffled, "Reverse");
        assert!(!reverse.enabled, "a shuffle has no direction to reverse");
        assert_eq!(reverse.checked, Some(false));
    }

    /// A submenu with separators and ticks is laid out and hit-tested like the
    /// main list, and the keyboard walks it past its grey rows.
    #[test]
    fn a_submenu_with_groups_is_laid_out_like_the_main_list() {
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(Default::default(), |ui| {
            let button = egui::Rect::from_min_size(egui::pos2(10.0, 40.0), egui::vec2(28.0, 28.0));
            let mut menu = Menu::app(button, app(app_facts()));
            assert_eq!(menu.kind, Kind::App);
            let view = menu
                .items
                .iter()
                .position(|i| i.label == "View")
                .expect("a View row");
            menu.cursor = Some(view);
            assert!(menu.open_submenu());
            let g = geometry(area(), &menu, ui.painter());
            assert_eq!(g.card.min, egui::pos2(10.0, 68.0 + BELOW_GAP));
            let (sub, rows) = g.sub.as_ref().expect("the View list is out");
            let items = menu.sub_items().expect("the View list");
            assert_eq!(rows.len(), items.len());
            assert!((sub.height() - height(items)).abs() < 1e-3);
            assert!(sub.left() > g.card.left());
            for (index, rect) in rows.iter().enumerate() {
                assert_eq!(g.hit(rect.center()), Some(Control::SubmenuItem(index)));
            }
            // Two separators: the gaps are real space, not overlapping rows.
            assert!(rows[4].top() - rows[3].bottom() > 1.0);

            // `↓` walks it, and Enter runs the row it is on.
            menu.move_cursor(1);
            assert_eq!(
                menu.activate(),
                Some(Action::Run(Command::ViewScaleCompact))
            );
            // Moving to the other parent is a fresh list with no cursor in it.
            let sort = view + 1;
            menu.cursor = Some(sort);
            assert!(menu.open_submenu());
            assert_eq!(menu.sub_cursor, None);
            assert_eq!(menu.activate(), None);
        });

        // Reverse is grey under a shuffle, so the keyboard steps over it.
        let mut menu = Menu::app(
            egui::Rect::NOTHING,
            app(AppFacts {
                sort: SortBy::Random,
                ..app_facts()
            }),
        );
        menu.cursor = menu.items.iter().position(|i| i.label == "Sort");
        assert!(menu.open_submenu());
        menu.move_cursor(-1);
        assert_eq!(menu.activate(), Some(Action::Run(Command::SortRandom)));
    }

    // ── A file dialog's type filters ────────────────────────────────────────

    /// One radio per filter in the dialog's order, then "All files" after a
    /// gap; the tick is on the active filter, or on "All files" when none is
    /// narrowing. Every row is live and has no key.
    #[test]
    fn the_type_list_is_the_filters_then_all_files() {
        let rows = type_items(["Images", "PDF"], Some(1));
        let labels: Vec<&str> = rows.iter().map(|i| i.label.as_str()).collect();
        assert_eq!(labels, ["Images", "PDF", ALL_FILES]);
        assert_eq!(ticked(&rows), ["PDF"]);
        assert_eq!(
            rows.iter().map(|i| i.action).collect::<Vec<_>>(),
            [Action::FileType(0), Action::FileType(1), Action::AllFiles]
        );
        assert!(rows.iter().all(|i| i.enabled && i.keys.is_empty()));
        assert!(rows.iter().all(|i| i.checked.is_some()), "one radio group");
        assert_eq!(
            rows.iter().map(|i| i.gap_before).collect::<Vec<_>>(),
            [false, false, true]
        );

        let all = type_items(["Images", "PDF"], None);
        assert_eq!(ticked(&all), [ALL_FILES]);
        // The popover hangs from the chip it came out of.
        let chip = egui::Rect::from_min_size(egui::pos2(400.0, 8.0), egui::vec2(90.0, 30.0));
        let menu = Menu::types(chip, all);
        assert_eq!(menu.kind, Kind::Types);
        assert_eq!(menu.anchor, Anchor::Below(chip));
    }

    /// The app menu grows a "File type" list after Sort only when it is given
    /// one — a dialog with filters — and the list is exactly the rows given.
    #[test]
    fn the_app_menu_lists_file_types_only_when_there_are_some() {
        assert!(app(app_facts()).iter().all(|i| i.label != "File type"));

        let rows = app_items(
            app_facts(),
            type_items(["Images"], Some(0)),
            &Registry::defaults(),
            |_| false,
        );
        let at = rows
            .iter()
            .position(|i| i.label == "File type")
            .expect("a File type row");
        assert_eq!(rows[at - 1].label, "Sort");
        assert!(rows[at].enabled);
        let list = rows[at].submenu.as_deref().expect("a list");
        assert_eq!(ticked(list), ["Images"]);
        assert_eq!(list.last().map(|i| i.action), Some(Action::AllFiles));
    }
}
