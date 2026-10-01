//! The keymap is a muscle-memory contract (PLAN §4.1's own words), so it is
//! tested like one: every claim the plan makes about a key gets an assertion.

use std::path::Path;
use std::time::{Duration, Instant};

use super::*;

fn chord(text: &str) -> Chord {
    parse_chord(text).unwrap_or_else(|e| panic!("`{text}`: {e}"))
}

/// Feed a whole sequence and return the last result.
fn press(km: &Registry, stack: &ContextStack, flags: WhenFlags, keys: &str) -> Dispatch {
    let mut state = KeymapState::new();
    let now = Instant::now();
    let mut last = Dispatch::NoMatch;
    for text in keys.split_whitespace() {
        last = km.dispatch(&mut state, stack, flags, chord(text), now);
    }
    last
}

fn files() -> ContextStack {
    ContextStack::browser()
}

// ── The table itself ────────────────────────────────────────────────────────

/// A panic here means a default row breaks the table's own rules — a duplicate
/// binding, a reserved key outside Global, or an unparseable chord.
#[test]
fn defaults_are_installable() {
    let km = Registry::defaults();
    assert!(km.bindings().len() > 150, "{}", km.bindings().len());
}

/// PLAN §4.1's contract, key by key. If one of these changes, a habit breaks.
#[test]
fn the_files_table_is_the_muscle_memory_contract() {
    let km = Registry::defaults();
    let stack = files();
    let f = WhenFlags::NONE;
    for (keys, expected) in [
        ("q", Command::Quit),
        ("Q", Command::QuitNoCwdFile),
        // On a Mac Cmd+C copies, and Cmd+W closes the tab instead
        // (`platform::defaults::KEYMAP_OVERRIDES`).
        (
            "ctrl+c",
            if cfg!(target_os = "macos") {
                Command::CopyToClipboard
            } else {
                Command::CloseTab
            },
        ),
        ("up", Command::CursorUp),
        ("down", Command::CursorDown),
        ("ctrl+u", Command::HalfPageUp),
        ("ctrl+d", Command::HalfPageDown),
        ("ctrl+b", Command::PageUp),
        ("ctrl+f", Command::PageDown),
        ("g g", Command::CursorTop),
        ("G", Command::CursorBottom),
        ("left", Command::Leave),
        ("right", Command::EnterDirectory),
        ("alt+left", Command::HistoryBack),
        ("alt+right", Command::HistoryForward),
        // The location bar's key everywhere else: the `Go to:` prompt.
        ("ctrl+l", Command::GotoPath),
        ("space", Command::ToggleSelect),
        ("ctrl+a", Command::SelectAll),
        ("ctrl+r", Command::InvertSelection),
        ("v", Command::VisualMode),
        ("V", Command::VisualUnset),
        ("K", Command::SeekPreviewUp),
        ("J", Command::SeekPreviewDown),
        ("tab", Command::Spot),
        ("o", Command::Open),
        ("enter", Command::Open),
        ("O", Command::OpenInteractive),
        // A picker's primary button, from the keyboard.
        ("ctrl+enter", Command::Choose),
        // A search hit's own folder (PLAN §7.2).
        ("alt+enter", Command::Reveal),
        ("y", Command::Yank),
        ("x", Command::YankCut),
        ("p", Command::Paste),
        ("P", Command::PasteForce),
        ("Y", Command::CopyToClipboard),
        ("X", Command::Unyank),
        ("-", Command::ViewScaleDown),
        ("=", Command::ViewScaleUp),
        ("+", Command::ViewScaleUp),
        ("d", Command::Trash),
        ("D", Command::DeletePermanently),
        ("a", Command::Create),
        ("r", Command::Rename),
        ("R", Command::RenameEmptyStem),
        (";", Command::Shell),
        (":", Command::ShellBlock),
        (".", Command::ToggleHidden),
        ("s", Command::SearchName),
        ("S", Command::SearchContent),
        ("z", Command::FuzzyJump),
        ("Z", Command::ZoxideJump),
        ("M", Command::MountManager),
        ("m s", Command::LinemodeSize),
        ("m p", Command::LinemodePermissions),
        ("m b", Command::LinemodeBtime),
        ("m m", Command::LinemodeMtime),
        ("m o", Command::LinemodeOwner),
        ("m t", Command::LinemodeTags),
        ("T", Command::Tag),
        ("m n", Command::LinemodeNone),
        ("c c", Command::CopyPath),
        ("c d", Command::CopyDirname),
        ("c f", Command::CopyFilename),
        ("c n", Command::CopyStem),
        ("c t", Command::CopyFileText),
        ("f", Command::Filter),
        ("/", Command::FindNext),
        ("n", Command::FindArrowNext),
        ("N", Command::FindArrowPrev),
        (", m", Command::SortMtime),
        (", M", Command::SortMtimeReverse),
        (", s", Command::SortSize),
        (", S", Command::SortSizeReverse),
        (", a", Command::SortAlphabetical),
        (", r", Command::SortRandom),
        ("g r", Command::GotoGitRoot),
        ("g space", Command::GotoInteractive),
        ("g b", Command::PinToggle),
        ("g f", Command::FollowSymlink),
        // yazi's `/tmp` slot, reused for PLAN §7.4's trash view — the one goto
        // chord that does not lead to a directory.
        ("g t", Command::OpenTrash),
        ("t", Command::TabCreate),
        ("1", Command::TabSwitch(0)),
        ("9", Command::TabSwitch(8)),
        ("alt+[", Command::TabPrev),
        ("alt+]", Command::TabNext),
        ("{", Command::TabSwapPrev),
        ("}", Command::TabSwapNext),
        ("w", Command::TasksShow),
        ("u", Command::Undo),
        ("ctrl+p", Command::CommandPalette),
        // The menu key on every desktop: the app menu under the top row's
        // button.
        ("f10", Command::AppMenu),
        ("~", Command::Help),
        ("f1", Command::Help),
        // `?` is help, not find-backwards: the key that means "what can I
        // press" everywhere else means it here too, in every context.
        ("?", Command::Help),
    ] {
        assert_eq!(
            press(&km, &stack, f, keys),
            Dispatch::Match(expected),
            "`{keys}` must be {expected:?}"
        );
    }
}

/// The `g` chord's destinations come from the config bookmark table, and the
/// indices have to line up with it or `g w` goes somewhere else.
#[test]
fn goto_chords_index_the_bookmark_table() {
    let km = Registry::defaults();
    let bookmarks = crate::config::default_bookmarks();
    for (i, bookmark) in bookmarks.iter().enumerate() {
        let keys = format!("g {}", bookmark.key);
        assert_eq!(
            press(&km, &files(), WhenFlags::NONE, &keys),
            Dispatch::Match(Command::Goto(i as u8)),
            "`{keys}` should be bookmark {i}"
        );
    }
    // …and the one everybody presses actually goes to ~/Work.
    assert_eq!(bookmarks[3].key, "w");
    assert_eq!(bookmarks[3].path, "~/Work");
}

// ── Chords ──────────────────────────────────────────────────────────────────

#[test]
fn a_chord_is_pending_until_it_completes() {
    let km = Registry::defaults();
    let stack = files();
    let mut state = KeymapState::new();
    let now = Instant::now();

    let first = km.dispatch(&mut state, &stack, WhenFlags::NONE, chord("g"), now);
    assert!(matches!(first, Dispatch::Pending { .. }), "{first:?}");
    assert_eq!(state.pending(), &[chord("g")]);

    let second = km.dispatch(&mut state, &stack, WhenFlags::NONE, chord("g"), now);
    assert_eq!(second, Dispatch::Match(Command::CursorTop));
    // A completed chord clears itself, or the next `g` would finish the old one.
    assert!(!state.is_pending());
}

#[test]
fn a_wrong_second_key_abandons_the_chord() {
    let km = Registry::defaults();
    let stack = files();
    let mut state = KeymapState::new();
    let now = Instant::now();
    km.dispatch(&mut state, &stack, WhenFlags::NONE, chord("g"), now);
    let result = km.dispatch(&mut state, &stack, WhenFlags::NONE, chord("q"), now);
    assert_eq!(result, Dispatch::NoMatch);
    assert!(!state.is_pending(), "a typo must not leave the chord armed");
}

/// PLAN §4: "`[which]` ordering preserves declaration order, not alphabetical."
#[test]
fn pending_lists_continuations_in_declaration_order() {
    let km = Registry::defaults();
    let stack = files();
    let Dispatch::Pending { continuations, .. } = press(&km, &stack, WhenFlags::NONE, "m") else {
        panic!("`m` should be a prefix");
    };
    let labels: Vec<String> = continuations.iter().map(|c| c.next.label()).collect();
    assert_eq!(labels, vec!["s", "p", "b", "m", "o", "t", "n", "u"]);
    // Every row carries the description the which-key card prints.
    assert_eq!(continuations[0].description, "Linemode: size");
    assert_eq!(continuations[0].command, Command::LinemodeSize);
    assert!(continuations[0].rest.is_empty());

    // The sort chord is the same shape, and its shifted twins are separate
    // rows: `, m` and `, M` are two different sorts.
    let Dispatch::Pending { continuations, .. } = press(&km, &stack, WhenFlags::NONE, ",") else {
        panic!("`,` should be a prefix");
    };
    let labels: Vec<String> = continuations.iter().map(|c| c.next.label()).collect();
    assert_eq!(
        labels,
        vec!["m", "M", "b", "B", "e", "E", "a", "A", "n", "N", "s", "S", "r"]
    );
}

/// The which-key card is for hesitation, not for typing.
#[test]
fn which_key_waits_the_delay_out() {
    let km = Registry::defaults();
    let mut state = KeymapState::new();
    let start = Instant::now();
    km.dispatch(&mut state, &files(), WhenFlags::NONE, chord("g"), start);
    assert!(!state.which_key_visible(start));
    assert!(!state.which_key_visible(start + Duration::from_millis(174)));
    assert!(state.which_key_visible(start + WHICH_KEY_DELAY));
    assert_eq!(state.which_key_due(), Some(start + WHICH_KEY_DELAY));
    assert_eq!(WHICH_KEY_DELAY, Duration::from_millis(175));
    state.cancel();
    assert_eq!(state.which_key_due(), None);
}

// ── Context precedence ──────────────────────────────────────────────────────

#[test]
fn the_most_recently_pushed_context_wins() {
    let km = Registry::defaults();
    let mut stack = files();
    // In the browser, Tab spots the hovered file…
    assert_eq!(
        press(&km, &stack, WhenFlags::NONE, "tab"),
        Dispatch::Match(Command::Spot)
    );
    // …and once the spot panel is up, the same key closes it.
    stack.push(Context::Spot);
    assert_eq!(
        press(&km, &stack, WhenFlags::NONE, "tab"),
        Dispatch::Match(Command::OverlayClose)
    );
    // Popping it puts the browser's meaning back.
    assert!(stack.pop(Context::Spot));
    assert_eq!(
        press(&km, &stack, WhenFlags::NONE, "tab"),
        Dispatch::Match(Command::Spot)
    );
}

#[test]
fn global_is_the_floor_and_is_never_popped() {
    let mut stack = ContextStack::new();
    assert!(!stack.pop(Context::Global));
    stack.push(Context::Global); // a no-op: it is already the bottom
    assert_eq!(stack.active(), vec![Context::Global]);
    stack.push(Context::Files);
    stack.push(Context::Palette);
    assert_eq!(
        stack.active(),
        vec![Context::Palette, Context::Files, Context::Global]
    );
    assert_eq!(stack.top(), Context::Palette);
    // Global still answers from under an overlay: the transport works anywhere.
    let km = Registry::defaults();
    assert_eq!(
        press(&km, &stack, WhenFlags::MEDIA, "k"),
        Dispatch::Match(Command::PlayPause)
    );
}

/// A prefix in a nearer context shadows a complete binding in a further one —
/// that is what "most specific wins" has to mean, or a chord could never start
/// with a key the Global table has bound.
#[test]
fn a_nearer_prefix_beats_a_further_exact_match() {
    let mut km = Registry::new();
    km.register(
        Context::Global,
        parse_sequence("c").expect("chord"),
        Command::Quit,
        "Quit",
        When::Always,
    )
    .expect("register");
    km.register(
        Context::Files,
        parse_sequence("c c").expect("chord"),
        Command::CopyPath,
        "Copy path",
        When::Always,
    )
    .expect("register");
    let stack = files();
    assert!(matches!(
        press(&km, &stack, WhenFlags::NONE, "c"),
        Dispatch::Pending { .. }
    ));
    assert_eq!(
        press(&km, &stack, WhenFlags::NONE, "c c"),
        Dispatch::Match(Command::CopyPath)
    );
}

// ── `when` predicates ───────────────────────────────────────────────────────

/// PLAN §2.1: the keyboard is always in the list, so the four keys the list and
/// the preview used to fight over are the list's, full stop — and the preview's
/// versions of them are the modified spellings that nothing else wanted.
#[test]
fn the_list_owns_every_plain_key() {
    let km = Registry::defaults();
    let stack = files();
    // On a clip, which is the case that used to change all four meanings.
    let media = WhenFlags::MEDIA;

    // `,` is the sort chord, and only the sort chord.
    assert!(matches!(
        press(&km, &stack, media, ","),
        Dispatch::Pending { .. }
    ));
    // `.` hides files. `m` is the linemode chord. `Space` selects.
    assert_eq!(
        press(&km, &stack, media, "."),
        Dispatch::Match(Command::ToggleHidden)
    );
    assert!(matches!(
        press(&km, &stack, media, "m"),
        Dispatch::Pending { .. }
    ));
    assert_eq!(
        press(&km, &stack, media, "space"),
        Dispatch::Match(Command::ToggleSelect)
    );
    // …and the arrows move the cursor whatever is in the preview pane.
    assert_eq!(
        press(&km, &stack, media, "up"),
        Dispatch::Match(Command::CursorUp)
    );
    assert_eq!(
        press(&km, &stack, media, "left"),
        Dispatch::Match(Command::Leave)
    );
    assert_eq!(
        press(&km, &stack, media, "right"),
        Dispatch::Match(Command::EnterDirectory)
    );
    assert_eq!(
        press(&km, &stack, media, "g g"),
        Dispatch::Match(Command::CursorTop)
    );
    assert_eq!(
        press(&km, &stack, media, "ctrl+d"),
        Dispatch::Match(Command::HalfPageDown)
    );
    assert_eq!(
        press(&km, &stack, media, "-"),
        Dispatch::Match(Command::ViewScaleDown)
    );
}

/// The other half of the same contract: every preview key still exists, on a
/// modified spelling, and works from the list on whatever the cursor is on.
#[test]
fn the_preview_keys_are_the_modified_ones() {
    let km = Registry::defaults();
    let stack = files();
    for (keys, expected) in [
        ("ctrl+up", Command::PreviewUp),
        ("ctrl+down", Command::PreviewDown),
        ("ctrl+shift+u", Command::PreviewHalfPageUp),
        ("ctrl+shift+d", Command::PreviewHalfPageDown),
        ("shift+space", Command::PreviewPageDown),
        ("ctrl+home", Command::PreviewTop),
        ("ctrl+end", Command::PreviewBottom),
        ("ctrl+=", Command::PreviewZoomIn),
        ("ctrl++", Command::PreviewZoomIn),
        ("ctrl+-", Command::PreviewZoomOut),
        ("ctrl+0", Command::PreviewZoomReset),
    ] {
        assert_eq!(
            press(&km, &stack, WhenFlags::NONE, keys),
            Dispatch::Match(expected),
            "`{keys}`"
        );
        // …and none of them changes meaning because a clip is hovered.
        assert_eq!(
            press(&km, &stack, WhenFlags::MEDIA, keys),
            Dispatch::Match(expected),
            "`{keys}` on a clip"
        );
    }
    // The one pair that does split, and the split is on what is under the
    // cursor rather than on where the keyboard is: sideways is the page turn
    // on a document and the frame step on a clip.
    assert_eq!(
        press(&km, &stack, WhenFlags::NONE, "ctrl+left"),
        Dispatch::Match(Command::PreviewLeft)
    );
    assert_eq!(
        press(&km, &stack, WhenFlags::NONE, "ctrl+right"),
        Dispatch::Match(Command::PreviewRight)
    );
    assert_eq!(
        press(&km, &stack, WhenFlags::MEDIA, "ctrl+left"),
        Dispatch::Match(Command::FrameStepBack)
    );
    assert_eq!(
        press(&km, &stack, WhenFlags::MEDIA, "ctrl+right"),
        Dispatch::Match(Command::FrameStepForward)
    );
    // Mute is transport, so it is only there when there is something to mute.
    assert_eq!(
        press(&km, &stack, WhenFlags::MEDIA, "ctrl+m"),
        Dispatch::Match(Command::Mute)
    );
    assert_eq!(
        press(&km, &stack, WhenFlags::NONE, "ctrl+m"),
        Dispatch::NoMatch
    );
}

/// PLAN §4.3: on a non-media file the transport keys are inert — "no beep, no
/// surprise". Which means the row simply is not there.
#[test]
fn transport_is_inert_on_a_file_that_cannot_play() {
    let km = Registry::defaults();
    let stack = files();
    for (keys, expected) in [
        ("k", Command::PlayPause),
        ("j", Command::ShuttleReverse),
        ("l", Command::ShuttleForward),
        ("L", Command::ToggleLoop),
        ("[", Command::PrevEdge),
        ("]", Command::NextEdge),
        ("<", Command::SkipBack),
        (">", Command::SkipForward),
        ("shift+up", Command::VolumeUp),
        ("shift+down", Command::VolumeDown),
    ] {
        assert_eq!(
            press(&km, &stack, WhenFlags::MEDIA, keys),
            Dispatch::Match(expected),
            "`{keys}`"
        );
        assert_eq!(
            press(&km, &stack, WhenFlags::NONE, keys),
            Dispatch::NoMatch,
            "`{keys}` must do nothing on a text file"
        );
    }
}

/// The parent column has no cursor of its own any more (PLAN §2.1). It was
/// only ever reachable by clicking the column to move the keyboard into it,
/// and clicking one of its rows has always just gone there — so the three
/// `parent-*` commands went with the focus that was their only door.
#[test]
fn the_parent_column_has_no_keyboard_cursor() {
    assert_eq!(Command::from_id("parent-prev"), None);
    assert_eq!(Command::from_id("parent-next"), None);
    assert_eq!(Command::from_id("parent-enter"), None);
    // …and `→` is plain "enter the directory" now, not enter-or-focus.
    assert_eq!(Command::from_id("enter-or-preview"), None);
    assert_eq!(
        Command::from_id("enter-directory"),
        Some(Command::EnterDirectory)
    );
}

/// `?` opens the help from wherever you are, and nothing in a nearer context
/// takes it away — the point of moving find-backwards off it.
#[test]
fn the_question_mark_is_help_in_every_context() {
    let km = Registry::defaults();
    for context in [Context::Tasks, Context::Spot, Context::Confirm] {
        let mut stack = files();
        stack.push(context);
        assert_eq!(
            press(&km, &stack, WhenFlags::NONE, "?"),
            Dispatch::Match(Command::Help),
            "`?` in {context:?}"
        );
    }
    // …and the find keys are the vi set that is left: `/`, `n`, `N`.
    let stack = files();
    assert_eq!(
        press(&km, &stack, WhenFlags::NONE, "/"),
        Dispatch::Match(Command::FindNext)
    );
    assert_eq!(
        press(&km, &stack, WhenFlags::NONE, "N"),
        Dispatch::Match(Command::FindArrowPrev)
    );
}

#[test]
fn the_help_browser_only_lists_what_is_reachable() {
    let km = Registry::defaults();
    let stack = files();
    let listed = |flags: WhenFlags, command: Command| {
        km.active_bindings(&stack, flags)
            .iter()
            .any(|b| b.command == command)
    };
    assert!(listed(WhenFlags::NONE, Command::ToggleHidden));
    assert!(!listed(WhenFlags::NONE, Command::PlayPause));
    assert!(listed(WhenFlags::MEDIA, Command::PlayPause));
    // Most specific first, so the help sheet reads the way dispatch resolves.
    let mut stack = stack;
    stack.push(Context::Tasks);
    let bindings = km.active_bindings(&stack, WhenFlags::NONE);
    assert_eq!(bindings[0].context, Context::Tasks);
}

/// The help sheet takes the keyboard whole, so `[help]` has to have a row for
/// every key that walks a list.
///
/// The sheet is dispatched against `[help]` **alone** (see `App::help_key`),
/// which is what this asserts: matched in that context by itself, every
/// list key resolves — and the browser's own keys, which used to be reached
/// *through* the sheet, do not. A `PageDown` that fell through to `[files]`
/// scrolled the pane behind the scrim, and the reader had no way of seeing
/// what they had moved.
#[test]
fn the_help_sheet_binds_every_key_that_walks_a_list() {
    let km = Registry::defaults();
    let stack = ContextStack::with(&[Context::Help]);
    for (keys, expected) in [
        ("esc", Command::Escape),
        ("ctrl+c", Command::OverlayClose),
        ("f1", Command::OverlayClose),
        ("up", Command::OverlayPrev),
        ("down", Command::OverlayNext),
        ("pageup", Command::HelpPageUp),
        ("pagedown", Command::HelpPageDown),
        ("ctrl+u", Command::HelpHalfPageUp),
        ("ctrl+d", Command::HelpHalfPageDown),
        ("home", Command::HelpTop),
        ("end", Command::HelpBottom),
        ("f", Command::HelpFilter),
    ] {
        assert_eq!(
            press(&km, &stack, WhenFlags::NONE, keys),
            Dispatch::Match(expected),
            "{keys}"
        );
    }
    // …and nothing else does. Every one of these is a `[files]` or `[global]`
    // key that reached the listing from behind the sheet: `q` quit it, `j`
    // moved its cursor, `g g` armed a chord in it.
    for keys in [
        "q", "j", "k", "g", "d", "space", "enter", "ctrl+b", "delete",
    ] {
        assert_eq!(
            press(&km, &stack, WhenFlags::NONE, keys),
            Dispatch::NoMatch,
            "{keys} reaches the browser from inside the help sheet"
        );
    }
    // No chord in `[help]`: the sheet is typed into, and a `g` that armed a
    // sequence would be a `g` the filter never received.
    assert!(km
        .active_bindings(&stack, WhenFlags::NONE)
        .iter()
        .all(|b| b.seq.len() == 1));
}

/// PLAN §5's conflict dialog answers — overwrite / skip / rename /
/// apply-to-all — dispatch from the Confirm context, so the help sheet and
/// which-key can offer them instead of the user having to be told.
#[test]
fn the_conflict_dialog_answers_are_registry_rows() {
    let km = Registry::defaults();
    let mut stack = files();
    stack.push(Context::Confirm);
    for (keys, expected, label) in [
        ("o", Command::ConflictOverwrite, "Overwrite"),
        ("s", Command::ConflictSkip, "Skip"),
        ("r", Command::ConflictRename, "Rename"),
        ("a", Command::ConflictApplyAll, "Apply to all"),
    ] {
        assert_eq!(
            press(&km, &stack, WhenFlags::NONE, keys),
            Dispatch::Match(expected),
            "{keys}"
        );
        assert_eq!(km.binding_label(expected).as_deref(), Some(keys));
        let listed = km.active_bindings(&stack, WhenFlags::NONE);
        let row = listed
            .iter()
            .find(|b| b.command == expected)
            .unwrap_or_else(|| panic!("{keys} is not in the help sheet"));
        assert_eq!(row.description, label);
        assert_eq!(row.context, Context::Confirm);
    }
    // The dialog's keys stay the dialog's: `s` is still search in the browser.
    assert_eq!(
        press(&km, &files(), WhenFlags::NONE, "s"),
        Dispatch::Match(Command::SearchName)
    );
}

/// The `w` panel's two verbs (PLAN §5: "pause/resume, cancel").
#[test]
fn the_task_panel_binds_pause_resume_and_cancel() {
    let km = Registry::defaults();
    let mut stack = files();
    stack.push(Context::Tasks);
    assert_eq!(
        press(&km, &stack, WhenFlags::NONE, "p"),
        Dispatch::Match(Command::TaskPauseResume)
    );
    assert_eq!(
        press(&km, &stack, WhenFlags::NONE, "x"),
        Dispatch::Match(Command::TaskCancel)
    );
    assert_eq!(
        km.binding_label(Command::TaskPauseResume).as_deref(),
        Some("p")
    );
}

/// The ids are the vocabulary of `keymap.toml`, so they are pinned here as
/// well as round-tripped: renaming one silently breaks a user's config.
#[test]
fn the_new_dialog_command_ids_are_stable() {
    for (command, id) in [
        (Command::ConflictOverwrite, "conflict-overwrite"),
        (Command::ConflictSkip, "conflict-skip"),
        (Command::ConflictRename, "conflict-rename"),
        (Command::ConflictApplyAll, "conflict-apply-all"),
        (Command::TaskPauseResume, "task-pause-resume"),
    ] {
        assert_eq!(command.id(), id);
        assert_eq!(Command::from_id(id), Some(command));
    }
}

// ── Reserved transport keys ─────────────────────────────────────────────────

/// PLAN §4.3's hard rule: `j k l [ ]` belong to transport everywhere, so
/// binding one outside Global is an error rather than a preference.
#[test]
fn reserved_transport_keys_cannot_be_bound_outside_global() {
    let mut km = Registry::new();
    for key in RESERVED_TRANSPORT_KEYS {
        for context in [Context::Files, Context::Input, Context::Tasks] {
            let err = km.register(
                context,
                vec![Chord::plain(*key)],
                Command::Quit,
                "Quit",
                When::Always,
            );
            assert!(
                matches!(err, Err(KeymapError::ReservedKey(_))),
                "{context:?} {key:?} was allowed"
            );
            // A *continuation* is fine, and has to be: with a prefix pending
            // the transport never sees the key, which is what lets `g l` exist.
            km.register(
                context,
                vec![Chord::plain(Key::Char('g')), Chord::plain(*key)],
                Command::Quit,
                "Quit",
                When::Always,
            )
            .expect("a continuation is not a transport press");
        }
        // Global is the one place they belong.
        km.register(
            Context::Global,
            vec![Chord::plain(*key)],
            Command::PlayPause,
            "Play",
            When::Always,
        )
        .expect("global transport binding");
    }
}

/// …and the shifted forms are *not* reserved, because PLAN §4.1's own table
/// binds `J`/`K` (preview seek) and `{`/`}` (tab swap) in the Files context.
#[test]
fn the_shift_is_the_escape_hatch() {
    let mut km = Registry::new();
    for text in ["J", "K", "L", "{", "}"] {
        km.register(
            Context::Files,
            parse_sequence(text).expect("chord"),
            Command::Quit,
            "Quit",
            When::Always,
        )
        .unwrap_or_else(|e| panic!("`{text}` should be bindable: {e}"));
    }
}

/// The same rule has to survive `keymap.toml`.
#[test]
fn a_user_override_cannot_steal_a_transport_key() {
    let mut km = Registry::defaults();
    let warnings = km.apply_overrides(
        "[files]\n\"j\" = \"cursor-down\"\n\"g m\" = \"mount-manager\"\n",
        Path::new("keymap.toml"),
    );
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].message.contains("reserved"), "{warnings:?}");
    assert_eq!(warnings[0].line, 2);
    // The transport still owns `j`…
    assert_eq!(
        press(&km, &files(), WhenFlags::MEDIA, "j"),
        Dispatch::Match(Command::ShuttleReverse)
    );
    // …and the *other* line in the same file still applied (PLAN §3).
    assert_eq!(
        press(&km, &files(), WhenFlags::NONE, "g m"),
        Dispatch::Match(Command::MountManager)
    );
}

// ── keymap.toml ─────────────────────────────────────────────────────────────

#[test]
fn overrides_replace_rebind_and_unbind() {
    let mut km = Registry::defaults();
    let warnings = km.apply_overrides(
        r#"
        [files]
        "g m" = "mount-manager"   # a new chord
        "q" = ""                  # unbind a default
        "d" = "delete-permanently" # replace a default
        [global]
        "f5" = "help"
        "#,
        Path::new("keymap.toml"),
    );
    assert!(warnings.is_empty(), "{warnings:?}");
    let stack = files();
    assert_eq!(
        press(&km, &stack, WhenFlags::NONE, "g m"),
        Dispatch::Match(Command::MountManager)
    );
    assert_eq!(press(&km, &stack, WhenFlags::NONE, "q"), Dispatch::NoMatch);
    assert_eq!(
        press(&km, &stack, WhenFlags::NONE, "d"),
        Dispatch::Match(Command::DeletePermanently)
    );
    assert_eq!(
        press(&km, &stack, WhenFlags::NONE, "f5"),
        Dispatch::Match(Command::Help)
    );
    // A rebound row inherits the command's description, so which-key does not
    // suddenly show a bare command id.
    let bound = km
        .bindings()
        .iter()
        .find(|b| b.command == Command::MountManager)
        .expect("bound");
    assert_eq!(
        bound.description,
        "Places: drives, phones, network and pins"
    );
}

/// A `keymap.toml` written against the old pane-focus model named a `[preview]`
/// context and the three `parent-*` commands. None of them exists any more
/// (PLAN §2.1), and a config that mentions them **warns and keeps going** — the
/// file is not rejected, and every other line in it still applies (PLAN §3).
#[test]
fn a_config_from_the_pane_focus_era_warns_and_is_ignored() {
    let mut km = Registry::defaults();
    let warnings = km.apply_overrides(
        r#"
        [preview]
        "up" = "preview-up"
        [files]
        "f9" = "parent-enter"
        "f10" = "help"
        "#,
        Path::new("keymap.toml"),
    );
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert!(
        warnings[0].message.contains("unknown context"),
        "{warnings:?}"
    );
    assert!(warnings[0].message.contains("preview"), "{warnings:?}");
    assert!(
        warnings[1].message.contains("unknown command"),
        "{warnings:?}"
    );
    // …and the line under them still took effect.
    assert_eq!(
        press(&km, &files(), WhenFlags::NONE, "f10"),
        Dispatch::Match(Command::Help)
    );
}

#[test]
fn bad_override_lines_warn_and_the_rest_still_applies() {
    let mut km = Registry::defaults();
    let warnings = km.apply_overrides(
        r#"
        [nowhere]
        "a" = "quit"
        [files]
        "not a key" = "quit"
        "f6" = "no-such-command"
        "f7" = 3
        "f8" = "help"
        "#,
        Path::new("keymap.toml"),
    );
    assert_eq!(warnings.len(), 4, "{warnings:?}");
    assert!(warnings[0].message.contains("unknown context"));
    assert!(warnings[1].message.contains("not a key chord"));
    assert!(warnings[2].message.contains("unknown command"));
    assert!(warnings[3].message.contains("expected a command id"));
    assert_eq!(
        press(&km, &files(), WhenFlags::NONE, "f8"),
        Dispatch::Match(Command::Help)
    );
}

/// PLAN §3: a missing file is silence.
#[test]
fn a_missing_keymap_file_says_nothing() {
    let mut km = Registry::defaults();
    let warnings = km.apply_overrides_from_dir(Path::new("/nonexistent/delightfile-test"));
    assert!(warnings.is_empty());
    assert_eq!(
        press(&km, &files(), WhenFlags::NONE, "q"),
        Dispatch::Match(Command::Quit)
    );
}

#[test]
fn user_bookmarks_rebuild_the_goto_chords() {
    let mut km = Registry::defaults();
    let bookmarks = vec![crate::config::Bookmark {
        key: "m".to_string(),
        path: "/mnt".to_string(),
        description: "Go to /mnt".to_string(),
        name: None,
    }];
    let warnings = km.apply_bookmarks(&bookmarks, Path::new("delightfile.toml"));
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(
        press(&km, &files(), WhenFlags::NONE, "g m"),
        Dispatch::Match(Command::Goto(0))
    );
    // The shipped chords are gone, since the table replaced them.
    assert_eq!(
        press(&km, &files(), WhenFlags::NONE, "g d"),
        Dispatch::NoMatch
    );
    // …but the goto chords that are not bookmarks stayed.
    assert_eq!(
        press(&km, &files(), WhenFlags::NONE, "g r"),
        Dispatch::Match(Command::GotoGitRoot)
    );
}

fn bookmark(key: &str, path: &str) -> crate::config::Bookmark {
    crate::config::Bookmark {
        key: key.to_string(),
        path: path.to_string(),
        description: format!("Go to {path}"),
        name: None,
    }
}

/// How many rows answer to `g <key>` in the browser.
fn rows_on(km: &Registry, keys: &str) -> usize {
    let seq = parse_sequence(keys).unwrap_or_else(|e| panic!("`{keys}`: {e}"));
    km.bindings()
        .iter()
        .filter(|b| b.context == Context::Files && b.seq == seq)
        .count()
}

/// The table is re-applied whenever it changes, so a second application
/// has to *replace* the first: one row per key, the slots renumbered, and a
/// key the new table dropped gone rather than left pointing at a slot that
/// now means something else.
#[test]
fn applying_the_bookmarks_twice_does_not_stack_them() {
    let mut km = Registry::defaults();
    let first = vec![bookmark("m", "/mnt"), bookmark("x", "/x")];
    assert!(km.apply_bookmarks(&first, Path::new("t")).is_empty());
    let second = vec![bookmark("x", "/x"), bookmark("m", "/mnt")];
    assert!(km.apply_bookmarks(&second, Path::new("t")).is_empty());
    assert!(km.apply_bookmarks(&second, Path::new("t")).is_empty());

    assert_eq!(rows_on(&km, "g m"), 1);
    assert_eq!(rows_on(&km, "g x"), 1);
    let gotos = km
        .bindings()
        .iter()
        .filter(|b| matches!(b.command, Command::Goto(_)))
        .count();
    assert_eq!(
        gotos, 2,
        "one row per bookmark, however often it is applied"
    );
    assert_eq!(
        press(&km, &files(), WhenFlags::NONE, "g m"),
        Dispatch::Match(Command::Goto(1)),
        "the slot is the row's place in the latest table"
    );
    let under_g = km.continuations(&files(), WhenFlags::NONE, &[chord("g")]);
    let mut keys: Vec<Chord> = under_g.iter().map(|c| c.next).collect();
    let before = keys.len();
    keys.sort_by_key(|c| c.label());
    keys.dedup();
    assert_eq!(keys.len(), before, "the card lists every key once");
}

/// A `[goto]` row on `b` wins `g b` from `pin-toggle` — the hand-written table
/// is the owner's — and says so rather than leaving the command keyless in
/// silence.
#[test]
fn a_goto_row_that_takes_a_built_in_key_says_so() {
    let mut km = Registry::defaults();
    let warnings = km.apply_bookmarks(&[bookmark("b", "/b")], Path::new("delightfile.toml"));
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    let text = warnings[0].to_string();
    assert!(text.contains("pin-toggle"), "{text}");
    assert!(text.contains("g b"), "{text}");
    assert_eq!(
        press(&km, &files(), WhenFlags::NONE, "g b"),
        Dispatch::Match(Command::Goto(0))
    );
    assert_eq!(km.binding_label(Command::PinToggle), None);
}

/// The pins' layer binds only keys nobody holds: not the `[goto]` table's, not
/// a built-in chord's, not one another's — and rebuilt from the same base, it
/// is the same keymap every time.
#[test]
fn the_pin_layer_takes_no_key_from_anything() {
    let base = Registry::defaults();
    let first = crate::config::default_bookmarks().len();
    let pins = vec![
        bookmark("h", "/pinned/home"),
        bookmark("g", "/pinned/top"),
        bookmark("space", "/pinned/space"),
        bookmark("x", "/pinned/x"),
        bookmark("x", "/pinned/x-again"),
        bookmark("y", "/pinned/y"),
    ];
    for _ in 0..2 {
        let mut km = base.clone();
        let refused = km.add_bookmarks(&pins, first);
        assert_eq!(
            refused,
            vec![
                KeymapError::Conflict("g h".to_string()),
                KeymapError::Conflict("g g".to_string()),
                KeymapError::Conflict("g Space".to_string()),
                KeymapError::Conflict("g x".to_string()),
            ]
        );
        let at = |keys: &str| press(&km, &files(), WhenFlags::NONE, keys);
        assert_eq!(
            at("g h"),
            Dispatch::Match(Command::Goto(0)),
            "[goto] kept it"
        );
        assert_eq!(at("g g"), Dispatch::Match(Command::CursorTop));
        assert_eq!(at("g space"), Dispatch::Match(Command::GotoInteractive));
        assert_eq!(at("g x"), Dispatch::Match(Command::Goto((first + 3) as u8)));
        assert_eq!(at("g y"), Dispatch::Match(Command::Goto((first + 5) as u8)));
        assert_eq!(rows_on(&km, "g x"), 1);
        assert_eq!(km.bindings().len(), base.bindings().len() + 2);
    }
}

// ── The four ways of finding something ─────────────────────────────────────

/// `f`, `/`, `s` and `S` all find something, and the only way to tell them
/// apart from the help sheet or the palette is what each one says it does —
/// so each says *where* it looks. Two stay in this folder and two go
/// everywhere below it, and the words are the thing that has to carry that.
#[test]
fn the_find_family_says_where_each_one_looks() {
    let km = Registry::defaults();
    let listed = km.active_bindings(&files(), WhenFlags::NONE);
    for (command, description) in [
        (Command::Filter, "Filter this folder, hide the rest"),
        (Command::FindNext, "Jump to a name in this folder"),
        (Command::SearchName, "Search everywhere by name"),
        (Command::SearchContent, "Search everywhere inside files"),
    ] {
        let row = listed
            .iter()
            .find(|b| b.command == command)
            .unwrap_or_else(|| panic!("{} is not on the help sheet", command.id()));
        assert_eq!(row.description, description, "{}", command.id());
    }
}

/// `Tab` in the search panel swaps names for contents and back (the panel
/// stacks on `[pick]`, which is why the row lives there) — and it does not
/// leak into the browser, where `Tab` is still the spot panel.
#[test]
fn tab_in_the_search_panel_swaps_names_and_contents() {
    let km = Registry::defaults();
    let pick = ContextStack::with(&[Context::Pick]);
    assert_eq!(
        press(&km, &pick, WhenFlags::NONE, "tab"),
        Dispatch::Match(Command::SearchToggle)
    );
    let row = km
        .active_bindings(&pick, WhenFlags::NONE)
        .into_iter()
        .find(|b| b.command == Command::SearchToggle)
        .expect("the toggle is on the panel's help sheet");
    assert_eq!(row.description, "Names ⟷ contents");
    assert_eq!(
        press(&km, &files(), WhenFlags::NONE, "tab"),
        Dispatch::Match(Command::Spot)
    );
}

/// `Enter` in the search panel lists the hits; `Alt+Enter` is the one-hit
/// answer — the hit's folder — in the panel and in the listing alike, under
/// one command id with one description.
#[test]
fn alt_enter_goes_to_the_files_folder_from_the_panel_and_the_listing() {
    let km = Registry::defaults();
    let pick = ContextStack::with(&[Context::Pick]);
    for stack in [&pick, &files()] {
        assert_eq!(
            press(&km, stack, WhenFlags::NONE, "alt+enter"),
            Dispatch::Match(Command::Reveal)
        );
    }
    assert_eq!(
        press(&km, &pick, WhenFlags::NONE, "enter"),
        Dispatch::Match(Command::OverlaySubmit)
    );
    assert_eq!(Command::Reveal.id(), "reveal");
    let row = km
        .active_bindings(&files(), WhenFlags::NONE)
        .into_iter()
        .find(|b| b.command == Command::Reveal)
        .expect("reveal is on the help sheet");
    assert_eq!(row.description, "Go to this file's folder");
}

/// The `[pick]` cards page the way the listing does — a page on
/// `PageUp`/`PageDown` and `Ctrl+b`/`Ctrl+f`, half of one on
/// `Ctrl+u`/`Ctrl+d` and `Ctrl+↑`/`Ctrl+↓`, an end on `Home`/`End` — each
/// row on the cards' help sheet in the listing's words, through commands of
/// their own, so what the palette teaches for the listing's top is still
/// `g g`.
#[test]
fn the_pick_cards_page_and_go_to_either_end() {
    let km = Registry::defaults();
    let pick = ContextStack::with(&[Context::Pick]);
    let listed = km.active_bindings(&pick, WhenFlags::NONE);
    for (keys, command, description) in [
        ("pageup", Command::OverlayPageUp, "Page up"),
        ("ctrl+b", Command::OverlayPageUp, "Page up"),
        ("pagedown", Command::OverlayPageDown, "Page down"),
        ("ctrl+f", Command::OverlayPageDown, "Page down"),
        ("ctrl+u", Command::OverlayHalfPageUp, "Half page up"),
        ("ctrl+up", Command::OverlayHalfPageUp, "Half page up"),
        ("ctrl+d", Command::OverlayHalfPageDown, "Half page down"),
        ("ctrl+down", Command::OverlayHalfPageDown, "Half page down"),
        ("home", Command::OverlayTop, "Go to top"),
        ("end", Command::OverlayBottom, "Go to bottom"),
    ] {
        assert_eq!(
            press(&km, &pick, WhenFlags::NONE, keys),
            Dispatch::Match(command),
            "{keys}"
        );
        let row = listed
            .iter()
            .find(|b| b.context == Context::Pick && b.seq == [chord(keys)])
            .unwrap_or_else(|| panic!("{keys} is not on the cards' help sheet"));
        assert_eq!(row.description, description, "{keys}");
    }
    // The cheapest chords are the ones taught.
    for (command, label) in [
        (Command::OverlayPageUp, "PgUp"),
        (Command::OverlayPageDown, "PgDn"),
        (Command::OverlayTop, "Home"),
        (Command::OverlayBottom, "End"),
    ] {
        assert_eq!(km.binding_label(command).as_deref(), Some(label));
    }
    assert_eq!(km.binding_label(Command::CursorTop).as_deref(), Some("g g"));
    // The browser's own keys are the listing's still, and Global's
    // `Ctrl+↑` is still the preview's.
    for (keys, command) in [
        ("ctrl+u", Command::HalfPageUp),
        ("ctrl+f", Command::PageDown),
        ("pagedown", Command::PageDown),
        ("ctrl+up", Command::PreviewUp),
    ] {
        assert_eq!(
            press(&km, &files(), WhenFlags::NONE, keys),
            Dispatch::Match(command),
            "{keys}"
        );
    }
}

// ── Labels ──────────────────────────────────────────────────────────────────

/// The help sheet advertises the easiest binding, and prints it as the keys the
/// hand actually presses.
#[test]
fn advertised_labels_are_what_the_hand_does() {
    let km = Registry::defaults();
    assert_eq!(km.binding_label(Command::CursorTop).as_deref(), Some("g g"));
    assert_eq!(km.binding_label(Command::Quit).as_deref(), Some("q"));
    assert_eq!(
        km.binding_label(Command::DeletePermanently).as_deref(),
        Some("D")
    );
    assert_eq!(km.binding_label(Command::SkipBack).as_deref(), Some("<"));
    assert_eq!(km.binding_label(Command::SortMtime).as_deref(), Some(", m"));
    // Help is on `~` and on `F1`; `~` is Shift+backtick, so the unmodified
    // function key is the cheaper of the two and the one the sheet teaches.
    // Both work, which is the point of binding both.
    assert_eq!(km.binding_label(Command::Help).as_deref(), Some("F1"));
    // Undo is on `u` and on Ctrl+Shift+z; the cheap one is what gets taught.
    assert_eq!(km.binding_label(Command::Undo).as_deref(), Some("u"));
}
