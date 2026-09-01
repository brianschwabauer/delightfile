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

fn preview_media() -> WhenFlags {
    WhenFlags {
        in_list: false,
        in_preview: true,
        in_parent: false,
        media_hovered: true,
    }
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
    let f = WhenFlags::LIST;
    for (keys, expected) in [
        ("q", Command::Quit),
        ("Q", Command::QuitNoCwdFile),
        ("ctrl+c", Command::CloseTab),
        ("up", Command::CursorUp),
        ("down", Command::CursorDown),
        ("ctrl+u", Command::HalfPageUp),
        ("ctrl+d", Command::HalfPageDown),
        ("ctrl+b", Command::PageUp),
        ("ctrl+f", Command::PageDown),
        ("g g", Command::CursorTop),
        ("G", Command::CursorBottom),
        ("left", Command::Leave),
        ("right", Command::EnterOrPreview),
        ("alt+left", Command::HistoryBack),
        ("alt+right", Command::HistoryForward),
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
        ("y", Command::Yank),
        ("x", Command::YankCut),
        ("p", Command::Paste),
        ("P", Command::PasteForce),
        ("Y", Command::CopyToClipboard),
        ("X", Command::Unyank),
        ("-", Command::SymlinkAbsolute),
        ("_", Command::SymlinkRelative),
        ("ctrl+-", Command::Hardlink),
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
        ("m n", Command::LinemodeNone),
        ("c c", Command::CopyPath),
        ("c d", Command::CopyDirname),
        ("c f", Command::CopyFilename),
        ("c n", Command::CopyStem),
        ("c t", Command::CopyFileText),
        ("f", Command::Filter),
        ("/", Command::FindNext),
        ("?", Command::FindPrev),
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
        ("g f", Command::FollowSymlink),
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
        ("~", Command::Help),
        ("f1", Command::Help),
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
            press(&km, &files(), WhenFlags::LIST, &keys),
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

    let first = km.dispatch(&mut state, &stack, WhenFlags::LIST, chord("g"), now);
    assert!(matches!(first, Dispatch::Pending { .. }), "{first:?}");
    assert_eq!(state.pending(), &[chord("g")]);

    let second = km.dispatch(&mut state, &stack, WhenFlags::LIST, chord("g"), now);
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
    km.dispatch(&mut state, &stack, WhenFlags::LIST, chord("g"), now);
    let result = km.dispatch(&mut state, &stack, WhenFlags::LIST, chord("q"), now);
    assert_eq!(result, Dispatch::NoMatch);
    assert!(!state.is_pending(), "a typo must not leave the chord armed");
}

/// PLAN §4: "`[which]` ordering preserves declaration order, not alphabetical."
#[test]
fn pending_lists_continuations_in_declaration_order() {
    let km = Registry::defaults();
    let stack = files();
    let Dispatch::Pending { continuations, .. } = press(&km, &stack, WhenFlags::LIST, "m") else {
        panic!("`m` should be a prefix");
    };
    let labels: Vec<String> = continuations.iter().map(|c| c.next.label()).collect();
    assert_eq!(labels, vec!["s", "p", "b", "m", "o", "n", "u"]);
    // Every row carries the description the which-key card prints.
    assert_eq!(continuations[0].description, "Linemode: size");
    assert_eq!(continuations[0].command, Command::LinemodeSize);
    assert!(continuations[0].rest.is_empty());

    // The sort chord is the same shape, and its shifted twins are separate
    // rows: `, m` and `, M` are two different sorts.
    let Dispatch::Pending { continuations, .. } = press(&km, &stack, WhenFlags::LIST, ",") else {
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
    km.dispatch(&mut state, &files(), WhenFlags::LIST, chord("g"), start);
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
        press(&km, &stack, WhenFlags::LIST, "tab"),
        Dispatch::Match(Command::Spot)
    );
    // …and once the spot panel is up, the same key closes it.
    stack.push(Context::Spot);
    assert_eq!(
        press(&km, &stack, WhenFlags::LIST, "tab"),
        Dispatch::Match(Command::OverlayClose)
    );
    // Popping it puts the browser's meaning back.
    assert!(stack.pop(Context::Spot));
    assert_eq!(
        press(&km, &stack, WhenFlags::LIST, "tab"),
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
    let media = WhenFlags {
        media_hovered: true,
        ..WhenFlags::default()
    };
    assert_eq!(
        press(&km, &stack, media, "k"),
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
        press(&km, &stack, WhenFlags::LIST, "c"),
        Dispatch::Pending { .. }
    ));
    assert_eq!(
        press(&km, &stack, WhenFlags::LIST, "c c"),
        Dispatch::Match(Command::CopyPath)
    );
}

// ── `when` predicates ───────────────────────────────────────────────────────

/// PLAN §2.1: focus does not change the context stack; the predicates do the
/// work. The four keys the list and the preview both want are the proof.
#[test]
fn focus_changes_meaning_without_changing_the_stack() {
    let km = Registry::defaults();
    let stack = files();
    let list = WhenFlags::LIST;
    let preview = preview_media();

    // `,` is the sort chord in the list and the frame step in the preview.
    assert!(matches!(
        press(&km, &stack, list, ","),
        Dispatch::Pending { .. }
    ));
    assert_eq!(
        press(&km, &stack, preview, ","),
        Dispatch::Match(Command::FrameStepBack)
    );
    // `.` hides files, or steps a frame.
    assert_eq!(
        press(&km, &stack, list, "."),
        Dispatch::Match(Command::ToggleHidden)
    );
    assert_eq!(
        press(&km, &stack, preview, "."),
        Dispatch::Match(Command::FrameStepForward)
    );
    // `m` is the linemode chord, or the mute.
    assert!(matches!(
        press(&km, &stack, list, "m"),
        Dispatch::Pending { .. }
    ));
    assert_eq!(
        press(&km, &stack, preview, "m"),
        Dispatch::Match(Command::Mute)
    );
    // `Space` selects, or plays.
    assert_eq!(
        press(&km, &stack, list, "space"),
        Dispatch::Match(Command::ToggleSelect)
    );
    assert_eq!(
        press(&km, &stack, preview, "space"),
        Dispatch::Match(Command::PlayPause)
    );
    // …and on a *document* in the preview, Space pages instead of playing.
    let preview_doc = WhenFlags {
        in_preview: true,
        ..WhenFlags::default()
    };
    assert_eq!(
        press(&km, &stack, preview_doc, "space"),
        Dispatch::Match(Command::PreviewPageDown)
    );
    assert_eq!(
        press(&km, &stack, preview_doc, "g g"),
        Dispatch::Match(Command::PreviewTop)
    );
    assert_eq!(
        press(&km, &stack, preview_doc, "ctrl+d"),
        Dispatch::Match(Command::PreviewHalfPageDown)
    );
}

/// PLAN §4.3: on a non-media file the transport keys are inert — "no beep, no
/// surprise". Which means the row simply is not there.
#[test]
fn transport_is_inert_on_a_file_that_cannot_play() {
    let km = Registry::defaults();
    let stack = files();
    let media = WhenFlags {
        in_list: true,
        media_hovered: true,
        ..WhenFlags::default()
    };
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
            press(&km, &stack, media, keys),
            Dispatch::Match(expected),
            "`{keys}`"
        );
        assert_eq!(
            press(&km, &stack, WhenFlags::LIST, keys),
            Dispatch::NoMatch,
            "`{keys}` must do nothing on a text file"
        );
    }
}

#[test]
fn the_parent_pane_has_its_own_cursor() {
    let km = Registry::defaults();
    let stack = files();
    let parent = WhenFlags {
        in_parent: true,
        ..WhenFlags::default()
    };
    assert_eq!(
        press(&km, &stack, parent, "up"),
        Dispatch::Match(Command::ParentPrev)
    );
    assert_eq!(
        press(&km, &stack, parent, "enter"),
        Dispatch::Match(Command::ParentEnter)
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
    assert!(listed(WhenFlags::LIST, Command::ToggleHidden));
    assert!(!listed(WhenFlags::LIST, Command::PlayPause));
    assert!(listed(preview_media(), Command::PlayPause));
    // Most specific first, so the help sheet reads the way dispatch resolves.
    let mut stack = stack;
    stack.push(Context::Tasks);
    let bindings = km.active_bindings(&stack, WhenFlags::LIST);
    assert_eq!(bindings[0].context, Context::Tasks);
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
            press(&km, &stack, WhenFlags::LIST, keys),
            Dispatch::Match(expected),
            "{keys}"
        );
        assert_eq!(km.binding_label(expected).as_deref(), Some(keys));
        let listed = km.active_bindings(&stack, WhenFlags::LIST);
        let row = listed
            .iter()
            .find(|b| b.command == expected)
            .unwrap_or_else(|| panic!("{keys} is not in the help sheet"));
        assert_eq!(row.description, label);
        assert_eq!(row.context, Context::Confirm);
    }
    // The dialog's keys stay the dialog's: `s` is still search in the browser.
    assert_eq!(
        press(&km, &files(), WhenFlags::LIST, "s"),
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
        press(&km, &stack, WhenFlags::LIST, "p"),
        Dispatch::Match(Command::TaskPauseResume)
    );
    assert_eq!(
        press(&km, &stack, WhenFlags::LIST, "x"),
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
    let media = WhenFlags {
        in_list: true,
        media_hovered: true,
        ..WhenFlags::default()
    };
    assert_eq!(
        press(&km, &files(), media, "j"),
        Dispatch::Match(Command::ShuttleReverse)
    );
    // …and the *other* line in the same file still applied (PLAN §3).
    assert_eq!(
        press(&km, &files(), WhenFlags::LIST, "g m"),
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
        press(&km, &stack, WhenFlags::LIST, "g m"),
        Dispatch::Match(Command::MountManager)
    );
    assert_eq!(press(&km, &stack, WhenFlags::LIST, "q"), Dispatch::NoMatch);
    assert_eq!(
        press(&km, &stack, WhenFlags::LIST, "d"),
        Dispatch::Match(Command::DeletePermanently)
    );
    assert_eq!(
        press(&km, &stack, WhenFlags::LIST, "f5"),
        Dispatch::Match(Command::Help)
    );
    // A rebound row inherits the command's description, so which-key does not
    // suddenly show a bare command id.
    let bound = km
        .bindings()
        .iter()
        .find(|b| b.command == Command::MountManager)
        .expect("bound");
    assert_eq!(bound.description, "Mount manager");
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
        press(&km, &files(), WhenFlags::LIST, "f8"),
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
        press(&km, &files(), WhenFlags::LIST, "q"),
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
    }];
    let warnings = km.apply_bookmarks(&bookmarks, Path::new("delightfile.toml"));
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(
        press(&km, &files(), WhenFlags::LIST, "g m"),
        Dispatch::Match(Command::Goto(0))
    );
    // The shipped chords are gone, since the table replaced them.
    assert_eq!(
        press(&km, &files(), WhenFlags::LIST, "g w"),
        Dispatch::NoMatch
    );
    // …but the goto chords that are not bookmarks stayed.
    assert_eq!(
        press(&km, &files(), WhenFlags::LIST, "g r"),
        Dispatch::Match(Command::GotoGitRoot)
    );
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
