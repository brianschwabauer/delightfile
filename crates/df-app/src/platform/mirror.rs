//! The app menu as a menu bar (`plans/other-platforms/02-macos.md` M2.37):
//! which of the ☰ menu's rows goes under which of the menus a Mac's menu bar
//! has, and a chord as the key equivalent a menu item shows.
//!
//! On macOS the app menu lives in the system's menu bar, not behind a button
//! on the top row. It is the same menu: the rows are the ones
//! [`crate::menu::app_items`] builds for the ☰ button, with their labels,
//! their ticks and their greys, and choosing one does what clicking it does.
//! What this module decides is only where each row goes, since a Mac's bar
//! has fixed names — the application's own menu, then File, Edit, View, Go,
//! Window, Help — and the ☰ menu's groups are named for what they are about.
//!
//! **Groups move whole.** A ☰ list that is a menu's own subject is that
//! menu's rows (Go, Edit, View); a list of verbs that lands in another menu
//! is its rows after a separator (Find, in File, where Finder has it); a list
//! of one setting's choices stays a list flown out of a row (Sort, File type,
//! Appearance, in View, as Finder's View has Sort By). The rows left on the
//! ☰ menu's top level go where a Mac keeps their kind: New tab and New
//! window in File, Clipboard in Edit (Finder's Show Clipboard), Places… and
//! Trash in Go, the panels and the command palette in View, Keyboard
//! shortcuts in Help, and Quit is the application menu's own Quit. A row
//! this table does not know yet — one added to the ☰ menu later — goes at
//! the end of File, so nothing the ☰ menu offers is ever missing from the
//! bar.
//!
//! The application menu and Window are AppKit's own items ([`System`]):
//! About, Services, Hide, Show All and Quit; Minimize, Zoom and Bring All to
//! Front. Their key equivalents are the system's and answer as the system's
//! do. Every other menu's key equivalents are the keymap's, shown and not
//! answered: the key goes to the window, as it does today, and the keymap is
//! the one door a key goes through (see `platform/macos/menubar.rs`).
//!
//! **A key equivalent is one chord.** A row whose key is a single chord —
//! `⌘n`, `t`, `F1`, `⇧u` — shows it in AppKit's notation (the ☰ menu's `⌘`
//! is the `ctrl` role, M2.20); a row whose key is a sequence (`g h`) shows
//! none, since a menu item cannot say "then".
//!
//! Pure: no AppKit here, so the mapping is tested on every target.

use df_core::keymap::{Chord, Command, Key, Registry};

use crate::menu::{Action, Item};

/// The menus of the bar, left to right.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Title {
    /// The application's own menu, which the bar shows under the program's
    /// name.
    App,
    File,
    Edit,
    View,
    Go,
    Window,
    Help,
}

impl Title {
    pub const ALL: [Title; 7] = [
        Title::App,
        Title::File,
        Title::Edit,
        Title::View,
        Title::Go,
        Title::Window,
        Title::Help,
    ];

    /// The menus built from the ☰ menu's rows, in the bar's order: every one
    /// but the two whose items are AppKit's own.
    pub const MIRRORED: [Title; 5] = [
        Title::File,
        Title::Edit,
        Title::View,
        Title::Go,
        Title::Help,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Title::App => NAME,
            Title::File => "File",
            Title::Edit => "Edit",
            Title::View => "View",
            Title::Go => "Go",
            Title::Window => "Window",
            Title::Help => "Help",
        }
    }
}

/// The program's name, as the application menu's items say it.
pub const NAME: &str = "delightfile";

/// One row of a menu in the bar.
#[derive(Debug, Clone, PartialEq)]
pub enum Row {
    Separator,
    /// A row of the ☰ menu's.
    Item(Entry),
    /// One of AppKit's own.
    System(System),
}

/// A ☰ row, as the bar shows it.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub title: String,
    pub key: Option<KeyEquivalent>,
    /// What choosing it does: the ☰ row's own action. A row with a
    /// `submenu` does nothing itself.
    pub action: Action,
    pub enabled: bool,
    /// A tick row: `Some(true)` ticked, `Some(false)` one of a group left
    /// unticked.
    pub checked: Option<bool>,
    /// The list it flies out, when it is a parent.
    pub submenu: Option<Vec<Row>>,
}

/// AppKit's own items, sent to the application or the window as the system's
/// menus send them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum System {
    About,
    Services,
    Hide,
    HideOthers,
    ShowAll,
    /// The ☰ menu's Quit (or, in a file dialog, Cancel): `terminate:`, the
    /// path Cmd+Q has always taken on a Mac (M2.26).
    Quit,
    Minimize,
    Zoom,
    BringAllToFront,
}

impl System {
    pub fn title(self) -> String {
        match self {
            System::About => format!("About {NAME}"),
            System::Services => "Services".to_string(),
            System::Hide => format!("Hide {NAME}"),
            System::HideOthers => "Hide Others".to_string(),
            System::ShowAll => "Show All".to_string(),
            System::Quit => format!("Quit {NAME}"),
            System::Minimize => "Minimize".to_string(),
            System::Zoom => "Zoom".to_string(),
            System::BringAllToFront => "Bring All to Front".to_string(),
        }
    }

    /// The system's own key equivalents: ⌘H, ⌥⌘H, ⌘Q and ⌘M.
    pub fn key(self) -> Option<KeyEquivalent> {
        let command = |key: char, option: bool| KeyEquivalent {
            key,
            command: true,
            option,
            shift: false,
            control: false,
        };
        match self {
            System::Hide => Some(command('h', false)),
            System::HideOthers => Some(command('h', true)),
            System::Quit => Some(command('q', false)),
            System::Minimize => Some(command('m', false)),
            _ => None,
        }
    }
}

/// A key equivalent as `NSMenuItem` takes one: a character and the
/// modifiers held with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEquivalent {
    /// `keyEquivalent`: a letter always lower case (Shift is `shift`), a
    /// punctuation mark as it is typed, or one of AppKit's characters for a
    /// key that types none (`NSF1FunctionKey`, `NSUpArrowFunctionKey`, …).
    pub key: char,
    /// ⌘: the keymap's `ctrl` role on a Mac (M2.20).
    pub command: bool,
    /// ⌥
    pub option: bool,
    /// ⇧
    pub shift: bool,
    /// ⌃: the keymap's `super` role, which nothing makes on a Mac.
    pub control: bool,
}

/// The application menu's items.
pub fn app_menu() -> Vec<Row> {
    vec![
        Row::System(System::About),
        Row::Separator,
        Row::System(System::Services),
        Row::Separator,
        Row::System(System::Hide),
        Row::System(System::HideOthers),
        Row::System(System::ShowAll),
        Row::Separator,
        Row::System(System::Quit),
    ]
}

/// The Window menu's items, above the list of windows AppKit keeps under
/// them.
pub fn window_menu() -> Vec<Row> {
    vec![
        Row::System(System::Minimize),
        Row::System(System::Zoom),
        Row::Separator,
        Row::System(System::BringAllToFront),
    ]
}

/// Where a row of the ☰ menu's top level goes: a menu, the section of it
/// (sections are kept apart by separators, in this order), and whether the
/// row is its list's rows laid in the menu or the row itself.
fn home(item: &Item) -> Option<(Title, u8, Laid)> {
    use Command as C;
    Some(
        match (&item.action, item.label.as_str(), item.has_submenu()) {
            (_, "Go", true) => (Title::Go, 0, Laid::Rows),
            (_, "Edit", true) => (Title::Edit, 0, Laid::Rows),
            (_, "View", true) => (Title::View, 0, Laid::Rows),
            (_, "Find", true) => (Title::File, 1, Laid::Rows),
            (_, "Sort" | "File type" | "Appearance", true) => (Title::View, 1, Laid::Row),
            (Action::Run(C::TabCreate | C::NewWindow), _, _) => (Title::File, 0, Laid::Row),
            (Action::Run(C::YankShow), _, _) => (Title::Edit, 1, Laid::Row),
            (Action::Run(C::TasksShow | C::DiskUsage), _, _) => (Title::View, 2, Laid::Row),
            (Action::Run(C::CommandPalette), _, _) => (Title::View, 3, Laid::Row),
            (Action::Run(C::MountManager | C::OpenTrash), _, _) => (Title::Go, 1, Laid::Row),
            (Action::Run(C::Help), _, _) => (Title::Help, 0, Laid::Row),
            // The application menu's own Quit stands for it (`System::Quit`).
            (Action::Run(C::Quit), _, _) => return None,
            // A row this table does not know: still in the bar.
            _ => (Title::File, u8::MAX, Laid::Row),
        },
    )
}

/// How a ☰ row goes into its menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Laid {
    /// The row's list, row by row: the group is the menu's, or a section of
    /// it.
    Rows,
    /// The row itself, flying its list out if it has one.
    Row,
}

/// The ☰ menu's rows — [`crate::menu::app_items`] with the places put in —
/// as the bar's mirrored menus ([`Title::MIRRORED`]), in order.
pub fn bar(items: &[Item], keymap: &Registry) -> Vec<(Title, Vec<Row>)> {
    // (menu, section, rows), in the ☰ menu's order within a section.
    let mut sections: Vec<(Title, u8, Vec<Row>)> = Vec::new();
    for item in items {
        let Some((title, section, laid)) = home(item) else {
            continue;
        };
        let rows = match (laid, &item.submenu) {
            (Laid::Rows, Some(list)) => rows(list, keymap),
            _ => vec![Row::Item(entry(item, keymap))],
        };
        match sections
            .iter_mut()
            .find(|(t, s, _)| *t == title && *s == section)
        {
            Some((_, _, into)) => into.extend(rows),
            None => sections.push((title, section, rows)),
        }
    }
    sections.sort_by_key(|(title, section, _)| {
        (Title::MIRRORED.iter().position(|t| t == title), *section)
    });
    Title::MIRRORED
        .iter()
        .map(|&title| {
            let mut menu = Vec::new();
            for (_, _, rows) in sections
                .iter()
                .filter(|(t, _, rows)| *t == title && !rows.is_empty())
            {
                if !menu.is_empty() {
                    menu.push(Row::Separator);
                }
                menu.extend(rows.iter().cloned());
            }
            (title, menu)
        })
        .collect()
}

/// A ☰ list as menu rows: a separator where the list has a gap, never
/// first.
fn rows(items: &[Item], keymap: &Registry) -> Vec<Row> {
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        if item.gap_before && !out.is_empty() {
            out.push(Row::Separator);
        }
        out.push(Row::Item(entry(item, keymap)));
    }
    out
}

fn entry(item: &Item, keymap: &Registry) -> Entry {
    Entry {
        title: item.label.clone(),
        key: chord_of(item, keymap).and_then(key_equivalent),
        action: item.action,
        enabled: item.enabled,
        checked: item.checked,
        submenu: item.submenu.as_ref().map(|list| rows(list, keymap)),
    }
}

/// The chord a command row's key is, when it is one chord: the binding of
/// the row's command that the ☰ menu drew (its label is the row's `keys`).
/// Rows that are not a command — a place, a file type — carry keys the
/// keymap spells as sequences, or none.
fn chord_of(item: &Item, keymap: &Registry) -> Option<Chord> {
    let Action::Run(command) = item.action else {
        return None;
    };
    if item.keys.is_empty() {
        return None;
    }
    keymap
        .bindings()
        .iter()
        .filter(|binding| binding.command == command && binding.seq.len() == 1)
        .find(|binding| binding.label() == item.keys)
        .map(|binding| binding.seq[0])
}

/// `chord` as AppKit's key equivalent. `None` for a function key past
/// AppKit's F35.
pub fn key_equivalent(chord: Chord) -> Option<KeyEquivalent> {
    let mods = chord.mods;
    let function = |offset: u32| char::from_u32(0xF700 + offset);
    let (key, shift) = match chord.key {
        Key::Char(c) if c.is_ascii_alphabetic() => (c.to_ascii_lowercase(), mods.shift),
        // Shifted punctuation is its shifted glyph, as the label writes it:
        // `<`, not ⇧`,`.
        Key::Char(c) if mods.shift => match chord.key.shifted_glyph() {
            Some(glyph) => (glyph, false),
            None => (c, true),
        },
        Key::Char(c) => (c, false),
        Key::F(n @ 1..=35) => (function(0x04 + u32::from(n) - 1)?, mods.shift),
        Key::F(_) => return None,
        Key::Escape => ('\u{1b}', mods.shift),
        Key::Enter => ('\r', mods.shift),
        Key::Tab => ('\t', mods.shift),
        Key::Backspace => ('\u{8}', mods.shift),
        Key::Delete => (function(0x28)?, mods.shift),
        Key::Insert => (function(0x27)?, mods.shift),
        Key::Space => (' ', mods.shift),
        Key::ArrowUp => (function(0x00)?, mods.shift),
        Key::ArrowDown => (function(0x01)?, mods.shift),
        Key::ArrowLeft => (function(0x02)?, mods.shift),
        Key::ArrowRight => (function(0x03)?, mods.shift),
        Key::Home => (function(0x29)?, mods.shift),
        Key::End => (function(0x2B)?, mods.shift),
        Key::PageUp => (function(0x2C)?, mods.shift),
        Key::PageDown => (function(0x2D)?, mods.shift),
    };
    Some(KeyEquivalent {
        key,
        command: mods.ctrl,
        option: mods.alt,
        shift,
        control: mods.super_key,
    })
}

#[cfg(test)]
mod tests {
    use df_core::config::{LineMode, SortBy, ThemeMode, ViewScale};
    use df_core::keymap::{parse_chord, Mods};

    use super::*;
    use crate::menu::{self, AppFacts, GoRow};

    fn facts() -> AppFacts {
        AppFacts {
            picker: false,
            local: true,
            targets: 1,
            clipboard: true,
            scale: ViewScale::Comfortable,
            hidden: false,
            linemode: LineMode::Size,
            sort: SortBy::Natural,
            reverse: false,
            parent_open: true,
            preview_open: true,
            appearance: ThemeMode::Auto,
        }
    }

    /// The ☰ menu as the window opens it: the app menu's rows with the
    /// places put into Go, and a file dialog's type list when `types`.
    fn hamburger(keymap: &Registry, types: bool, refused: impl Fn(Command) -> bool) -> Vec<Item> {
        let types = if types {
            menu::type_items(["Images", "Documents"], Some(0))
        } else {
            Vec::new()
        };
        let mut items = menu::app_items(facts(), types, keymap, &refused);
        let places = [
            GoRow {
                label: "~".to_string(),
                keys: "g h".to_string(),
            },
            GoRow {
                label: "~/Work".to_string(),
                keys: "g w".to_string(),
            },
        ];
        menu::insert_go(
            &mut items,
            menu::place_rows(&places, false, keymap, &refused),
        );
        items
    }

    /// Every leaf of `items`, depth first: what choosing something can do.
    fn leaves(items: &[Item]) -> Vec<(String, Action)> {
        let mut out = Vec::new();
        for item in items {
            match &item.submenu {
                Some(list) => out.extend(leaves(list)),
                None => out.push((item.label.clone(), item.action)),
            }
        }
        out
    }

    fn bar_leaves(rows: &[Row]) -> Vec<(String, Action)> {
        let mut out = Vec::new();
        for row in rows {
            if let Row::Item(entry) = row {
                match &entry.submenu {
                    Some(list) => out.extend(bar_leaves(list)),
                    None => out.push((entry.title.clone(), entry.action)),
                }
            }
        }
        out
    }

    fn find<'a>(rows: &'a [Row], title: &str) -> Option<&'a Entry> {
        rows.iter().find_map(|row| match row {
            Row::Item(entry) if entry.title == title => Some(entry),
            Row::Item(entry) => entry.submenu.as_deref().and_then(|list| find(list, title)),
            _ => None,
        })
    }

    fn menu_of(bar: &[(Title, Vec<Row>)], title: Title) -> &[Row] {
        bar.iter()
            .find(|(t, _)| *t == title)
            .map(|(_, rows)| rows.as_slice())
            .unwrap_or_default()
    }

    /// The bar's menus, left to right, under the names a Mac gives them;
    /// the two that are AppKit's own are not built from the ☰ menu.
    #[test]
    fn the_bar_is_a_macs_seven_menus() {
        assert_eq!(
            Title::ALL.map(Title::label),
            [
                "delightfile",
                "File",
                "Edit",
                "View",
                "Go",
                "Window",
                "Help"
            ]
        );
        assert!(Title::MIRRORED
            .iter()
            .all(|title| ![Title::App, Title::Window].contains(title)));
        assert!(Title::MIRRORED
            .iter()
            .all(|title| Title::ALL.contains(title)));
    }

    /// Every row the ☰ menu offers is somewhere in the bar, once, with its
    /// action — Quit being the application menu's own — with and without a
    /// file dialog's type list.
    #[test]
    fn every_row_of_the_app_menu_has_a_home_in_the_bar() {
        let keymap = Registry::defaults();
        for types in [false, true] {
            let items = hamburger(&keymap, types, |_| false);
            let bar = bar(&items, &keymap);
            let mut placed: Vec<(String, Action)> =
                bar.iter().flat_map(|(_, rows)| bar_leaves(rows)).collect();
            let mut wanted: Vec<(String, Action)> = leaves(&items)
                .into_iter()
                .filter(|(_, action)| *action != Action::Run(Command::Quit))
                .collect();
            let key = |(label, action): &(String, Action)| format!("{label} {action:?}");
            placed.sort_by_key(key);
            wanted.sort_by_key(key);
            assert_eq!(placed, wanted, "types {types}");
            assert!(app_menu().contains(&Row::System(System::Quit)));
            assert_eq!(
                bar.iter().map(|(title, _)| *title).collect::<Vec<_>>(),
                Title::MIRRORED
            );
        }
    }

    /// The groups land where the module essay says, whole and in the ☰
    /// menu's order, and the settings' lists stay lists.
    #[test]
    fn the_groups_go_where_a_mac_keeps_them() {
        let keymap = Registry::defaults();
        let items = hamburger(&keymap, true, |_| false);
        let bar = bar(&items, &keymap);
        let titles = |title: Title| -> Vec<String> {
            menu_of(&bar, title)
                .iter()
                .map(|row| match row {
                    Row::Separator => "—".to_string(),
                    Row::Item(entry) => entry.title.clone(),
                    Row::System(system) => system.title(),
                })
                .collect()
        };
        assert_eq!(
            titles(Title::File),
            [
                "New tab",
                "New window",
                "—",
                "Search everywhere by name…",
                "Search everywhere inside files…",
                "Filter this folder…",
            ]
        );
        assert_eq!(
            titles(Title::Go),
            [
                "Go to path…",
                "Jump to…",
                "Open terminal here",
                "—",
                "~",
                "~/Work",
                "Pin this folder",
                "—",
                "Places…",
                "Trash",
            ]
        );
        assert_eq!(titles(Title::Help), ["Keyboard shortcuts"]);
        let edit = titles(Title::Edit);
        assert_eq!(edit.first().map(String::as_str), Some("Undo"));
        assert_eq!(&edit[edit.len() - 2..], ["—", "Clipboard"]);
        let view = titles(Title::View);
        assert_eq!(view.first().map(String::as_str), Some("Compact"));
        assert_eq!(
            &view[view.len() - 8..],
            [
                "Sort",
                "File type",
                "Appearance",
                "—",
                "Tasks",
                "Disk usage",
                "—",
                "Command palette…",
            ]
        );
        for list in ["Sort", "File type", "Appearance"] {
            let entry = find(menu_of(&bar, Title::View), list).expect(list);
            assert!(
                entry.submenu.as_ref().is_some_and(|rows| !rows.is_empty()),
                "{list}"
            );
        }
        // No menu starts or ends on a separator, or has two together.
        for (title, rows) in &bar {
            assert!(!matches!(rows.first(), Some(Row::Separator)), "{title:?}");
            assert!(!matches!(rows.last(), Some(Row::Separator)), "{title:?}");
            assert!(
                !rows
                    .windows(2)
                    .any(|pair| matches!(pair, [Row::Separator, Row::Separator])),
                "{title:?}"
            );
        }
    }

    /// A row the ☰ menu greys is grey in the bar, and a ticked one ticked:
    /// the bar's rows are the ☰ menu's, predicate and all.
    #[test]
    fn greys_and_ticks_are_the_app_menus() {
        let keymap = Registry::defaults();
        let items = hamburger(&keymap, false, |command| command == Command::GotoPath);
        let bar = bar(&items, &keymap);
        let go = menu_of(&bar, Title::Go);
        assert!(!find(go, "Go to path…").expect("Go to path").enabled);
        assert!(find(go, "Jump to…").expect("Jump to").enabled);
        let view = menu_of(&bar, Title::View);
        assert_eq!(
            find(view, "Comfortable").and_then(|e| e.checked),
            Some(true)
        );
        assert_eq!(find(view, "Compact").and_then(|e| e.checked), Some(false));
        assert_eq!(find(view, "Natural").and_then(|e| e.checked), Some(true));
        assert_eq!(
            find(view, "Follow the desktop").and_then(|e| e.checked),
            Some(true)
        );
        // And the actions are the rows' own.
        assert_eq!(
            find(menu_of(&bar, Title::Go), "~/Work").map(|e| e.action),
            Some(Action::Place(1))
        );
        assert_eq!(
            find(view, "Dark").map(|e| e.action),
            Some(Action::Run(Command::ThemeDark))
        );
    }

    fn equivalent(key: char, command: bool, shift: bool) -> KeyEquivalent {
        KeyEquivalent {
            key,
            command,
            option: false,
            shift,
            control: false,
        }
    }

    /// The shipped keymap's keys on the bar's rows: a Cmd chord, a bare
    /// letter, a capital (Shift), a function key, and nothing for a
    /// sequence.
    #[test]
    fn a_rows_one_chord_is_its_key_equivalent() {
        let keymap = Registry::defaults();
        let items = hamburger(&keymap, false, |_| false);
        let bar = bar(&items, &keymap);
        let key = |title: Title, row: &str| {
            find(menu_of(&bar, title), row)
                .unwrap_or_else(|| panic!("no {row}"))
                .key
        };
        assert_eq!(
            key(Title::File, "New window"),
            Some(equivalent('n', true, false))
        );
        assert_eq!(
            key(Title::File, "New tab"),
            Some(equivalent('t', false, false))
        );
        assert_eq!(
            key(Title::Edit, "Undo"),
            Some(equivalent('u', false, false))
        );
        assert_eq!(key(Title::Edit, "Redo"), Some(equivalent('u', false, true)));
        assert_eq!(
            key(Title::Edit, "Select all"),
            Some(equivalent('a', true, false))
        );
        assert_eq!(
            key(Title::View, "Command palette…"),
            Some(equivalent('p', true, false))
        );
        assert_eq!(
            key(Title::Help, "Keyboard shortcuts"),
            Some(equivalent('\u{F704}', false, false)),
            "F1"
        );
        // `g h` is two keys: no equivalent.
        assert_eq!(key(Title::Go, "~"), None);
    }

    /// Chords as AppKit's key equivalents: the modifiers by role (the keymap's
    /// `ctrl` is ⌘ on a Mac), Shift folded into a punctuation mark's glyph
    /// and kept apart from a letter, and the keys that type nothing as
    /// AppKit's own characters.
    #[test]
    fn chords_become_appkits_key_equivalents() {
        let of = |text: &str| key_equivalent(parse_chord(text).expect(text));
        assert_eq!(
            of("ctrl+alt+x"),
            Some(KeyEquivalent {
                key: 'x',
                command: true,
                option: true,
                shift: false,
                control: false,
            })
        );
        assert_eq!(of("G"), Some(equivalent('g', false, true)));
        assert_eq!(of("<"), Some(equivalent('<', false, false)));
        assert_eq!(of("ctrl+,"), Some(equivalent(',', true, false)));
        assert_eq!(of("f1").map(|k| k.key), Some('\u{F704}'));
        assert_eq!(of("f12").map(|k| k.key), Some('\u{F70F}'));
        assert_eq!(of("enter").map(|k| k.key), Some('\r'));
        assert_eq!(of("esc").map(|k| k.key), Some('\u{1b}'));
        assert_eq!(of("tab").map(|k| k.key), Some('\t'));
        assert_eq!(of("backspace").map(|k| k.key), Some('\u{8}'));
        assert_eq!(of("delete").map(|k| k.key), Some('\u{F728}'));
        assert_eq!(of("up").map(|k| k.key), Some('\u{F700}'));
        assert_eq!(of("down").map(|k| k.key), Some('\u{F701}'));
        assert_eq!(of("left").map(|k| k.key), Some('\u{F702}'));
        assert_eq!(of("right").map(|k| k.key), Some('\u{F703}'));
        assert_eq!(of("home").map(|k| k.key), Some('\u{F729}'));
        assert_eq!(of("end").map(|k| k.key), Some('\u{F72B}'));
        assert_eq!(of("pageup").map(|k| k.key), Some('\u{F72C}'));
        assert_eq!(of("pagedown").map(|k| k.key), Some('\u{F72D}'));
        assert_eq!(of("space").map(|k| k.key), Some(' '));
        assert_eq!(
            key_equivalent(Chord::new(
                Mods {
                    super_key: true,
                    ..Mods::NONE
                },
                Key::Char('k')
            ))
            .map(|k| k.control),
            Some(true)
        );
        assert_eq!(key_equivalent(Chord::plain(Key::F(36))), None);
    }

    /// The system's own items keep the system's keys.
    #[test]
    fn the_system_items_keep_the_systems_keys() {
        assert_eq!(System::Quit.key(), Some(equivalent('q', true, false)));
        assert_eq!(System::Hide.key(), Some(equivalent('h', true, false)));
        assert_eq!(System::HideOthers.key().map(|k| k.option), Some(true));
        assert_eq!(System::Minimize.key(), Some(equivalent('m', true, false)));
        assert_eq!(System::Zoom.key(), None);
        assert_eq!(System::Quit.title(), "Quit delightfile");
        assert!(window_menu().contains(&Row::System(System::BringAllToFront)));
    }
}
