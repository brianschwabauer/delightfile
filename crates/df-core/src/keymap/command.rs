//! Every command delightfile can be asked to run, as one flat enum.
//!
//! It is flat and `Copy` on purpose. A command is an *identity*, not a payload:
//! the keymap, the which-key card, the `?` help browser and the command palette
//! all need to compare, sort and list commands, and the moment one variant
//! carries a `String` the whole registry stops being cheap to pass around. The
//! two variants that do carry data (`Goto`, `TabSwitch`) carry a small index
//! into a table that lives in config, which keeps the enum `Copy` and keeps the
//! bookmark *paths* where a user can change them (PLAN §3).
//!
//! Every variant has a stable kebab-case id, because that id is the vocabulary
//! of `keymap.toml` — see [`super`]'s header for the file format. Ids are not
//! generated from the variant name at runtime; they are written out, so
//! renaming a variant cannot silently break a user's config.

/// Which vi-style mode the shared line editor is in (PLAN §4.2).
///
/// The editor's buffer logic lands in a later phase; this enum exists now
/// because the Input context's bindings are written against it and the router
/// has to be able to name the mode a binding switches into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum InputMode {
    /// Motions and operators — where `i`, `a`, `v`, `r` are doors, not text.
    Normal,
    /// Typing. Bindings other than `Esc` do not apply.
    #[default]
    Insert,
    /// A selection is live; operators act on it.
    Visual,
    /// The next keystroke replaces one character and returns to Normal.
    Replace,
}

macro_rules! commands {
    ($($variant:ident => $id:literal),* $(,)?) => {
        /// One thing delightfile can do.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
        pub enum Command {
            $($variant,)*
            /// Jump to bookmark *n* of the `[goto]` table (PLAN §3), 0-based.
            /// The index rather than the path so the enum stays `Copy` and the
            /// paths stay editable.
            Goto(u8),
            /// Switch to tab *n*, 0-based — the `1`–`9` row.
            TabSwitch(u8),
        }

        impl Command {
            /// The stable id used in `keymap.toml` and the command palette.
            pub fn id(self) -> String {
                match self {
                    $(Command::$variant => $id.to_string(),)*
                    Command::Goto(n) => format!("goto-{}", n + 1),
                    Command::TabSwitch(n) => format!("tab-switch-{}", n + 1),
                }
            }

            /// The inverse of [`Command::id`].
            pub fn from_id(id: &str) -> Option<Command> {
                Some(match id {
                    $($id => Command::$variant,)*
                    _ => return indexed_from_id(id),
                })
            }

            /// Every non-indexed command, for the palette's "what else is
            /// there" list and for the round-trip test.
            pub fn all() -> Vec<Command> {
                vec![$(Command::$variant,)*]
            }
        }
    };
}

/// The two commands whose id carries a slot number.
fn indexed_from_id(id: &str) -> Option<Command> {
    if let Some(n) = id.strip_prefix("goto-").and_then(parse_slot) {
        return Some(Command::Goto(n));
    }
    id.strip_prefix("tab-switch-")
        .and_then(parse_slot)
        .map(Command::TabSwitch)
}

/// `"3"` → `Some(2)`. 1-based in the id because a person writing `goto-1`
/// means the first one; rejected past 99 so a typo cannot become a slot.
fn parse_slot(text: &str) -> Option<u8> {
    let n: u8 = text.parse().ok()?;
    if n == 0 || n > 99 {
        return None;
    }
    Some(n - 1)
}

commands! {
    // ── Global ────────────────────────────────────────────────────────────
    Escape => "escape",
    Quit => "quit",
    QuitNoCwdFile => "quit-no-cwd-file",
    CloseTab => "close-tab",
    NewWindow => "new-window",
    CommandPalette => "command-palette",
    Help => "help",
    Undo => "undo",
    Redo => "redo",

    // ── Cursor & navigation (PLAN §4.1) ───────────────────────────────────
    CursorUp => "cursor-up",
    CursorDown => "cursor-down",
    CursorTop => "cursor-top",
    CursorBottom => "cursor-bottom",
    HalfPageUp => "half-page-up",
    HalfPageDown => "half-page-down",
    PageUp => "page-up",
    PageDown => "page-down",
    Leave => "leave",
    EnterOrPreview => "enter-or-preview",
    HistoryBack => "history-back",
    HistoryForward => "history-forward",
    // The parent pane has its own cursor once the mouse has focused it —
    // PLAN §2.1's `in_parent()` predicate, which otherwise has no user.
    ParentPrev => "parent-prev",
    ParentNext => "parent-next",
    ParentEnter => "parent-enter",
    GotoGitRoot => "goto-git-root",
    GotoInteractive => "goto-interactive",
    FollowSymlink => "follow-symlink",
    FuzzyJump => "fuzzy-jump",
    ZoxideJump => "zoxide-jump",

    // ── Selection ─────────────────────────────────────────────────────────
    ToggleSelect => "toggle-select",
    SelectAll => "select-all",
    InvertSelection => "invert-selection",
    VisualMode => "visual-mode",
    VisualUnset => "visual-unset",

    // ── File operations (PLAN §5) ─────────────────────────────────────────
    Open => "open",
    OpenInteractive => "open-interactive",
    Yank => "yank",
    YankCut => "yank-cut",
    Unyank => "unyank",
    Paste => "paste",
    PasteForce => "paste-force",
    CopyToClipboard => "copy-to-clipboard",
    SymlinkAbsolute => "symlink-absolute",
    SymlinkRelative => "symlink-relative",
    Hardlink => "hardlink",
    Trash => "trash",
    DeletePermanently => "delete-permanently",
    Create => "create",
    Rename => "rename",
    RenameEmptyStem => "rename-empty-stem",
    Shell => "shell",
    ShellBlock => "shell-block",
    MountManager => "mount-manager",

    // ── View ──────────────────────────────────────────────────────────────
    ToggleHidden => "toggle-hidden",
    Spot => "spot",
    TasksShow => "tasks-show",
    SeekPreviewUp => "seek-preview-up",
    SeekPreviewDown => "seek-preview-down",

    // ── Copy the path, in its four useful shapes, plus the contents ───────
    CopyPath => "copy-path",
    CopyDirname => "copy-dirname",
    CopyFilename => "copy-filename",
    CopyStem => "copy-stem",
    CopyFileText => "copy-file-text",

    // ── Linemode (PLAN §4.1 `m` chord) ────────────────────────────────────
    LinemodeSize => "linemode-size",
    LinemodePermissions => "linemode-permissions",
    LinemodeBtime => "linemode-btime",
    LinemodeMtime => "linemode-mtime",
    LinemodeOwner => "linemode-owner",
    LinemodeNone => "linemode-none",

    // ── Search, filter, find ──────────────────────────────────────────────
    Filter => "filter",
    FindNext => "find-next",
    FindPrev => "find-prev",
    FindArrowNext => "find-arrow-next",
    FindArrowPrev => "find-arrow-prev",
    SearchName => "search-name",
    SearchContent => "search-content",
    CancelSearch => "cancel-search",

    // ── Sort (PLAN §4.1 `,` chord) ────────────────────────────────────────
    SortMtime => "sort-mtime",
    SortMtimeReverse => "sort-mtime-reverse",
    SortBtime => "sort-btime",
    SortBtimeReverse => "sort-btime-reverse",
    SortExtension => "sort-extension",
    SortExtensionReverse => "sort-extension-reverse",
    SortAlphabetical => "sort-alphabetical",
    SortAlphabeticalReverse => "sort-alphabetical-reverse",
    SortNatural => "sort-natural",
    SortNaturalReverse => "sort-natural-reverse",
    SortSize => "sort-size",
    SortSizeReverse => "sort-size-reverse",
    SortRandom => "sort-random",

    // ── Tabs (PLAN §2) ────────────────────────────────────────────────────
    TabCreate => "tab-create",
    TabPrev => "tab-prev",
    TabNext => "tab-next",
    TabSwapPrev => "tab-swap-prev",
    TabSwapNext => "tab-swap-next",

    // ── Transport, global on the hovered media file (PLAN §4.3) ───────────
    PlayPause => "play-pause",
    ShuttleReverse => "shuttle-reverse",
    ShuttleForward => "shuttle-forward",
    ToggleLoop => "toggle-loop",
    PrevEdge => "prev-edge",
    NextEdge => "next-edge",
    SkipBack => "skip-back",
    SkipForward => "skip-forward",
    VolumeUp => "volume-up",
    VolumeDown => "volume-down",
    FrameStepBack => "frame-step-back",
    FrameStepForward => "frame-step-forward",
    Mute => "mute",

    // ── Preview focus: the pane decides scroll vs. volume vs. page ────────
    PreviewUp => "preview-up",
    PreviewDown => "preview-down",
    PreviewLeft => "preview-left",
    PreviewRight => "preview-right",
    PreviewHalfPageUp => "preview-half-page-up",
    PreviewHalfPageDown => "preview-half-page-down",
    PreviewPageDown => "preview-page-down",
    PreviewTop => "preview-top",
    PreviewBottom => "preview-bottom",
    PreviewZoomIn => "preview-zoom-in",
    PreviewZoomOut => "preview-zoom-out",
    PreviewZoomReset => "preview-zoom-reset",

    // ── Overlay contexts ──────────────────────────────────────────────────
    OverlayClose => "overlay-close",
    OverlaySubmit => "overlay-submit",
    OverlayPrev => "overlay-prev",
    OverlayNext => "overlay-next",
    TaskInspect => "task-inspect",
    TaskCancel => "task-cancel",
    SpotSwipePrev => "spot-swipe-prev",
    SpotSwipeNext => "spot-swipe-next",
    SpotCopyCell => "spot-copy-cell",
    HelpFilter => "help-filter",

    // ── Input: the shared vi line editor (PLAN §4.2) ──────────────────────
    InputInsert => "input-insert",
    InputInsertBol => "input-insert-bol",
    InputAppend => "input-append",
    InputAppendEol => "input-append-eol",
    InputVisual => "input-visual",
    InputVisualLine => "input-visual-line",
    InputReplace => "input-replace",
    InputMoveLeft => "input-move-left",
    InputMoveRight => "input-move-right",
    InputMoveBol => "input-move-bol",
    InputMoveEol => "input-move-eol",
    InputMoveFirstChar => "input-move-first-char",
    InputWordForward => "input-word-forward",
    InputWordForwardFar => "input-word-forward-far",
    InputWordBackward => "input-word-backward",
    InputWordBackwardFar => "input-word-backward-far",
    InputWordEnd => "input-word-end",
    InputWordEndFar => "input-word-end-far",
    InputBackspace => "input-backspace",
    InputDeleteUnder => "input-delete-under",
    InputKillBol => "input-kill-bol",
    InputKillEol => "input-kill-eol",
    InputKillWordBackward => "input-kill-word-backward",
    InputKillWordForward => "input-kill-word-forward",
    InputCut => "input-cut",
    InputCutEol => "input-cut-eol",
    InputChange => "input-change",
    InputChangeEol => "input-change-eol",
    InputSubstitute => "input-substitute",
    InputSubstituteLine => "input-substitute-line",
    InputCutChar => "input-cut-char",
    InputYank => "input-yank",
    InputPaste => "input-paste",
    InputPasteBefore => "input-paste-before",
    InputUndo => "input-undo",
    InputRedo => "input-redo",
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The id is the user-facing name of a command, so it has to survive a
    /// round trip — a `keymap.toml` written against the palette's list must
    /// parse back to the same command.
    #[test]
    fn every_id_round_trips() {
        for cmd in Command::all() {
            let id = cmd.id();
            assert_eq!(Command::from_id(&id), Some(cmd), "{id}");
        }
        for n in 0..9u8 {
            assert_eq!(
                Command::from_id(&Command::Goto(n).id()),
                Some(Command::Goto(n))
            );
            assert_eq!(
                Command::from_id(&Command::TabSwitch(n).id()),
                Some(Command::TabSwitch(n))
            );
        }
    }

    #[test]
    fn ids_are_unique() {
        let mut ids: Vec<String> = Command::all().iter().map(|c| c.id()).collect();
        ids.sort();
        let before = ids.len();
        ids.dedup();
        assert_eq!(ids.len(), before, "two commands share an id");
    }

    #[test]
    fn unknown_ids_are_none() {
        assert_eq!(Command::from_id("nope"), None);
        assert_eq!(Command::from_id("goto-0"), None);
        assert_eq!(Command::from_id("goto-x"), None);
    }
}
