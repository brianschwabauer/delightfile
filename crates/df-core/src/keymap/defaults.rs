//! The shipped keymap — PLAN §4.1–4.3.
//!
//! **Defaults ARE Brian's yazi config** (PLAN §3), so this table is his
//! `~/.config/yazi/keymap.toml` ported binding-for-binding, with the three
//! changes the plan calls for:
//!
//! 1. `hjkl` navigation is gone (PLAN's opening paragraph). Arrows move the
//!    cursor; `j` `k` `l` are delightviewer's shuttle keys, bound in Global so
//!    they work at any focus, and `H`/`L` (yazi's history) retire in favour of
//!    `Alt+←`/`Alt+→`, which is what every other program on the machine uses.
//! 2. `[`/`]` are transport, so the tab keys move to `Alt+[`/`Alt+]` and the
//!    swaps stay on `{`/`}` (PLAN §2).
//! 3. Three commands yazi did not have: `u` undo (PLAN §5), `Ctrl+p` command
//!    palette (§4.4), and a `Ctrl+n` new window (§2).
//!
//! **Row order is the which-key order** (PLAN §4), so the `g`, `m`, `c` and `,`
//! chords are grouped the way a person would want to read them, not sorted.
//!
//! ### Two ambiguities in PLAN §4, and how they were settled
//!
//! - §4.1 says the overlay contexts port yazi's defaults *verbatim*, but yazi
//!   navigates all of them with `j`/`k` and swipes the spot panel with `h`/`l`
//!   — which §4.3 hard-reserves. The reservation wins: it is the load-bearing
//!   half of "arrow onto a video and press `l`", and the plan's own premise is
//!   that `hjkl` is gone. Overlays navigate with `↑`/`↓` (which yazi also
//!   binds) and the spot panel swipes with `←`/`→` (likewise).
//! - §4.1 lists `?` on both the find row and the help row. yazi's `[mgr]` binds
//!   `?` to find-previous and reaches help through `~`/`F1`, but `?` is the key
//!   that means "what can I press" in every other program on the machine, and
//!   somebody who has just opened delightfile presses it long before they have
//!   any muscle memory to protect. So **`?` is help**, globally, and the find
//!   family keeps the rest of the vi set: `/` searches, `n` is the next match
//!   and `N` the previous one. `find-prev` is still a command id, so a
//!   `keymap.toml` can put backwards search back wherever it likes.

use std::path::Path;

use super::command::Command;
use super::key::parse_sequence;
use super::{Context, Registry, When};

/// Build the default registry. Panics if the table below breaks its own rules
/// (a duplicate binding, a reserved key, an unparseable chord) — that is a
/// programming error, and `defaults_are_installable` catches it in CI rather
/// than at a user's first keystroke.
pub(super) fn build() -> Registry {
    use Command as C;
    use Context::{Confirm, Files, Global, Help, Input, Palette, Pick, Spot, Tasks};
    use When::{Always, InList, InParent, InPreview, MediaHovered, PreviewMedia};

    // Hand-aligned on purpose. rustfmt turns a 200-row tuple list into 900
    // lines of one field per line, and this table's entire value is that a
    // person can read down a column and see what every key does.
    #[rustfmt::skip]
    let rows: &[(Context, &str, Command, &str, When)] = &[
        // ── Global ──────────────────────────────────────────────────────────
        // Esc is layered by the router, not by the table: one command whose
        // meaning is the §4.1 ladder (cancel drag → visual off → clear
        // selection → cancel search → focus List).
        (Global, "esc",          C::Escape,         "Cancel, or step back",  Always),
        (Global, "ctrl+p",       C::CommandPalette, "Command palette",       Always),
        // `?` is the key everybody presses for help, so it is the one that
        // opens it — everywhere, not only in the file list. `~` stays as the
        // yazi alias it was ported as, and `F1` as the one key that means help
        // in every program ever written.
        (Global, "?",            C::Help,           "Help / keymap browser", Always),
        (Global, "~",            C::Help,           "Help / keymap browser", Always),
        (Global, "f1",           C::Help,           "Help / keymap browser", Always),
        (Global, "ctrl+n",       C::NewWindow,      "New window",            Always),
        (Global, "ctrl+shift+z", C::Undo,           "Undo last operation",   Always),

        // ── Transport, on the hovered media file, at any focus (§4.3) ───────
        // Reserved keys, so Global is the only context they may live in — and
        // `MediaHovered` is how "on a non-media file these keys are inert" is
        // spelled: the row simply is not there, so nothing happens and the help
        // sheet does not offer it.
        (Global, "k",          C::PlayPause,      "Play / pause",             MediaHovered),
        (Global, "j",          C::ShuttleReverse, "Shuttle reverse",          MediaHovered),
        (Global, "l",          C::ShuttleForward, "Shuttle forward",          MediaHovered),
        (Global, "L",          C::ToggleLoop,     "Toggle loop",              MediaHovered),
        (Global, "[",          C::PrevEdge,       "Previous chapter / start", MediaHovered),
        (Global, "]",          C::NextEdge,       "Next chapter / end",       MediaHovered),
        (Global, "<",          C::SkipBack,       "Back 10 s",                MediaHovered),
        (Global, ">",          C::SkipForward,    "Forward 10 s",             MediaHovered),
        (Global, "shift+up",   C::VolumeUp,       "Volume up",                MediaHovered),
        (Global, "shift+down", C::VolumeDown,     "Volume down",              MediaHovered),

        // ── Preview focus, media (§4.3 "preview-focus extras") ──────────────
        // These keys belong to the list at every other moment, which is exactly
        // why they are guarded rather than moved: the list's rows are `InList`
        // and these are `PreviewMedia`, so the same key is the sort chord in one
        // pane and the frame step in the other, and neither is ever un-learned.
        // Declared before the plain `InPreview` rows below so a video wins over
        // the document reading of the key.
        (Global, "space", C::PlayPause,        "Play / pause",           PreviewMedia),
        (Global, ",",     C::FrameStepBack,    "Step back one frame",    PreviewMedia),
        (Global, ".",     C::FrameStepForward, "Step forward one frame", PreviewMedia),
        (Global, "m",     C::Mute,             "Mute",                   PreviewMedia),
        (Global, "up",    C::VolumeUp,         "Volume up",              PreviewMedia),
        (Global, "down",  C::VolumeDown,       "Volume down",            PreviewMedia),

        // ── Preview focus, documents and images (§4.3) ──────────────────────
        (Global, "up",     C::PreviewUp,           "Scroll up",                InPreview),
        (Global, "down",   C::PreviewDown,         "Scroll down",              InPreview),
        (Global, "left",   C::PreviewLeft,         "Previous page / pan left", InPreview),
        (Global, "right",  C::PreviewRight,        "Next page / pan right",    InPreview),
        (Global, "ctrl+u", C::PreviewHalfPageUp,   "Half page up",             InPreview),
        (Global, "ctrl+d", C::PreviewHalfPageDown, "Half page down",           InPreview),
        (Global, "space",  C::PreviewPageDown,     "Page down",                InPreview),
        (Global, "g g",    C::PreviewTop,          "Top of document",          InPreview),
        (Global, "G",      C::PreviewBottom,       "Bottom of document",       InPreview),
        (Global, "+",      C::PreviewZoomIn,       "Zoom in",                  InPreview),
        (Global, "=",      C::PreviewZoomIn,       "Zoom in",                  InPreview),
        (Global, "-",      C::PreviewZoomOut,      "Zoom out",                 InPreview),
        (Global, "0",      C::PreviewZoomReset,    "Reset zoom",               InPreview),

        // ── Files: quitting ─────────────────────────────────────────────────
        (Files, "q",      C::Quit,          "Quit",                                 Always),
        (Files, "Q",      C::QuitNoCwdFile, "Quit without writing cwd-file",        Always),
        (Files, "ctrl+c", C::CloseTab,      "Close tab, or quit if it is the last", Always),

        // ── Files: the cursor ───────────────────────────────────────────────
        (Files, "up",             C::CursorUp,       "Previous file",                     InList),
        (Files, "down",           C::CursorDown,     "Next file",                         InList),
        (Files, "ctrl+u",         C::HalfPageUp,     "Half page up",                      InList),
        (Files, "ctrl+d",         C::HalfPageDown,   "Half page down",                    InList),
        (Files, "ctrl+b",         C::PageUp,         "Page up",                           InList),
        (Files, "ctrl+f",         C::PageDown,       "Page down",                         InList),
        (Files, "pageup",         C::PageUp,         "Page up",                           InList),
        (Files, "pagedown",       C::PageDown,       "Page down",                         InList),
        (Files, "shift+pageup",   C::HalfPageUp,     "Half page up",                      InList),
        (Files, "shift+pagedown", C::HalfPageDown,   "Half page down",                    InList),
        (Files, "g g",            C::CursorTop,      "Go to top",                         InList),
        (Files, "G",              C::CursorBottom,   "Go to bottom",                      InList),
        (Files, "left",           C::Leave,          "Back to the parent directory",      InList),
        (Files, "right",          C::EnterOrPreview, "Enter directory, or focus preview", InList),
        (Files, "alt+left",       C::HistoryBack,    "Back to previous directory",        Always),
        (Files, "alt+right",      C::HistoryForward, "Forward to next directory",         Always),
        // The parent pane gets a cursor of its own once a click has focused it
        // — PLAN §2.1's `in_parent()`, which otherwise names a predicate with
        // nothing behind it.
        (Files, "up",             C::ParentPrev,     "Previous directory",                InParent),
        (Files, "down",           C::ParentNext,     "Next directory",                    InParent),
        (Files, "right",          C::ParentEnter,    "Enter directory",                   InParent),
        (Files, "enter",          C::ParentEnter,    "Enter directory",                   InParent),

        // ── Files: selection ────────────────────────────────────────────────
        (Files, "space",  C::ToggleSelect,    "Toggle selection and advance", InList),
        (Files, "ctrl+a", C::SelectAll,       "Select all files",             InList),
        (Files, "ctrl+r", C::InvertSelection, "Invert the selection",         InList),
        (Files, "v",      C::VisualMode,      "Visual (selection) mode",      InList),
        (Files, "V",      C::VisualUnset,     "Visual (unset) mode",          InList),
        // yazi parity: nudge the preview without leaving the list.
        (Files, "K",      C::SeekPreviewUp,   "Seek preview up 5",            InList),
        (Files, "J",      C::SeekPreviewDown, "Seek preview down 5",          InList),
        (Files, "tab",    C::Spot,            "Spot the hovered file",        Always),
        // No yazi ancestor: grid view is delightfile's own. ctrl+g is free in
        // yazi's mgr table, and g alone is the goto prefix — close cousins.
        (Files, "ctrl+g", C::ToggleView,      "Toggle list / grid view",      InList),

        // ── Files: opening ──────────────────────────────────────────────────
        (Files, "o",           C::Open,            "Open",       Always),
        (Files, "enter",       C::Open,            "Open",       InList),
        (Files, "O",           C::OpenInteractive, "Open with…", Always),
        (Files, "shift+enter", C::OpenInteractive, "Open with…", Always),

        // ── Files: the clipboard and the file operations (§5) ────────────────
        (Files, "y",      C::Yank,              "Yank (copy)",                         Always),
        (Files, "x",      C::YankCut,           "Yank (cut)",                          Always),
        (Files, "p",      C::Paste,             "Paste",                               Always),
        (Files, "P",      C::PasteForce,        "Paste, overwriting",                  Always),
        (Files, "Y",      C::CopyToClipboard,   "Copy to the system clipboard",        Always),
        (Files, "X",      C::Unyank,            "Cancel the yank",                     Always),
        // `-` is guarded because the preview owns it as the zoom-out.
        (Files, "-",      C::SymlinkAbsolute,   "Symlink (absolute)",                  InList),
        (Files, "_",      C::SymlinkRelative,   "Symlink (relative)",                  InList),
        (Files, "ctrl+-", C::Hardlink,          "Hardlink",                            Always),
        (Files, "d",      C::Trash,             "Trash",                               Always),
        (Files, "D",      C::DeletePermanently, "Delete permanently",                  Always),
        (Files, "a",      C::Create,            "Create (trailing / for a directory)", Always),
        (Files, "r",      C::Rename,            "Rename",                              Always),
        (Files, "R",      C::RenameEmptyStem,   "Rename with an empty stem",           Always),
        (Files, ";",      C::Shell,             "Shell command",                       Always),
        (Files, ":",      C::ShellBlock,        "Shell command (block)",               Always),
        (Files, "u",      C::Undo,              "Undo last operation",                 Always),
        (Files, "w",      C::TasksShow,         "Task manager",                        Always),
        (Files, "M",      C::MountManager,      "Mount manager",                       Always),
        (Files, "b",      C::BasketToggle,      "Toss into / out of the basket",       InList),
        (Files, "B",      C::BasketShow,        "Show the selection basket",           Always),
        (Files, "e",      C::ArchiveExtractHere,      "Extract the archive here",      InList),
        (Files, "E",      C::ArchiveExtractSubfolder, "Extract into a new folder",     InList),

        // ── Files: what is shown ────────────────────────────────────────────
        (Files, ".",   C::ToggleHidden,        "Toggle hidden files",   InList),
        (Files, "m s", C::LinemodeSize,        "Linemode: size",        InList),
        (Files, "m p", C::LinemodePermissions, "Linemode: permissions", InList),
        (Files, "m b", C::LinemodeBtime,       "Linemode: created",     InList),
        (Files, "m m", C::LinemodeMtime,       "Linemode: modified",    InList),
        (Files, "m o", C::LinemodeOwner,       "Linemode: owner",       InList),
        (Files, "m n", C::LinemodeNone,        "Linemode: none",        InList),
        (Files, "m u", C::DiskUsage,           "Show disk usage",       InList),

        // ── Files: copy the path, four ways, and the contents ───────────────
        (Files, "c c", C::CopyPath,     "Copy the file path",                      Always),
        (Files, "c d", C::CopyDirname,  "Copy the directory path",                 Always),
        (Files, "c f", C::CopyFilename, "Copy the filename",                       Always),
        (Files, "c n", C::CopyStem,     "Copy the filename without extension",     Always),
        (Files, "c t", C::CopyFileText, "Copy the text contents (yank if binary)", Always),

        // ── Files: filter, find, search, jump ───────────────────────────────
        (Files, "f",      C::Filter,        "Filter files",                InList),
        // The vi set, minus the half of it that would cost the help key: `/`
        // searches forward, `n` is the next match and `N` the previous one.
        // Backwards *search* (`find-prev`) keeps its command id for anybody who
        // wants it back in `keymap.toml`; `?` is help (see Global above), which
        // is what the key is for in every other program on the machine.
        (Files, "/",      C::FindNext,      "Find",                        InList),
        (Files, "n",      C::FindArrowNext, "Next match",                  InList),
        (Files, "N",      C::FindArrowPrev, "Previous match",              InList),
        (Files, "s",      C::SearchName,    "Search by name (fd)",         Always),
        (Files, "S",      C::SearchContent, "Search by content (rg)",      Always),
        (Files, "ctrl+s", C::CancelSearch,  "Cancel the search",           Always),
        (Files, "z",      C::FuzzyJump,     "Jump to a file or directory", Always),
        (Files, "Z",      C::ZoxideJump,    "Jump by frecency (zoxide)",   Always),

        // ── Files: sort. The time and size sorts also switch the linemode, as
        // in the yazi config this is ported from — the column you just sorted
        // by is the column you want to see.
        (Files, ", m", C::SortMtime,               "Sort by modified",              InList),
        (Files, ", M", C::SortMtimeReverse,        "Sort by modified (reverse)",    InList),
        (Files, ", b", C::SortBtime,               "Sort by created",               InList),
        (Files, ", B", C::SortBtimeReverse,        "Sort by created (reverse)",     InList),
        (Files, ", e", C::SortExtension,           "Sort by extension",             InList),
        (Files, ", E", C::SortExtensionReverse,    "Sort by extension (reverse)",   InList),
        (Files, ", a", C::SortAlphabetical,        "Sort alphabetically",           InList),
        (Files, ", A", C::SortAlphabeticalReverse, "Sort alphabetically (reverse)", InList),
        (Files, ", n", C::SortNatural,             "Sort naturally",                InList),
        (Files, ", N", C::SortNaturalReverse,      "Sort naturally (reverse)",      InList),
        (Files, ", s", C::SortSize,                "Sort by size",                  InList),
        (Files, ", S", C::SortSizeReverse,         "Sort by size (reverse)",        InList),
        (Files, ", r", C::SortRandom,              "Sort randomly",                 InList),

        // ── Files: the goto chords that are not bookmarks. The bookmark rows
        // (`g h`, `g w`, …) are registered from the config table below, so the
        // paths stay editable in one place (PLAN §3).
        (Files, "g r",     C::GotoGitRoot,     "Go to the git root",         InList),
        (Files, "g space", C::GotoInteractive, "Jump interactively",         InList),
        (Files, "g f",     C::FollowSymlink,   "Follow the hovered symlink", InList),
        // yazi binds `g t` to `/tmp`; PLAN §4.1 dropped that row, so the slot is
        // free and the trash — which PLAN §7.4 wants a virtual location for —
        // takes it. `t` for trash, one key from a list, next to the other places.
        (Files, "g t",     C::OpenTrash,       "Browse the trash",           InList),

        // ── Files: tabs (§2). `[`/`]` belong to transport, so Alt carries the
        // switch and the swaps keep the shifted brackets.
        (Files, "t",     C::TabCreate,    "New tab",                    Always),
        (Files, "1",     C::TabSwitch(0), "Switch to tab 1",            Always),
        (Files, "2",     C::TabSwitch(1), "Switch to tab 2",            Always),
        (Files, "3",     C::TabSwitch(2), "Switch to tab 3",            Always),
        (Files, "4",     C::TabSwitch(3), "Switch to tab 4",            Always),
        (Files, "5",     C::TabSwitch(4), "Switch to tab 5",            Always),
        (Files, "6",     C::TabSwitch(5), "Switch to tab 6",            Always),
        (Files, "7",     C::TabSwitch(6), "Switch to tab 7",            Always),
        (Files, "8",     C::TabSwitch(7), "Switch to tab 8",            Always),
        (Files, "9",     C::TabSwitch(8), "Switch to tab 9",            Always),
        (Files, "alt+[", C::TabPrev,      "Previous tab",               Always),
        (Files, "alt+]", C::TabNext,      "Next tab",                   Always),
        (Files, "{",     C::TabSwapPrev,  "Swap with the previous tab", Always),
        (Files, "}",     C::TabSwapNext,  "Swap with the next tab",     Always),

        // ── Input: the shared vi line editor (§4.2) ─────────────────────────
        // The bindings are the whole contract here; the buffer behind them is a
        // later phase. `h`/`l` are gone with the rest of `hjkl` — the arrows and
        // `Ctrl+b`/`Ctrl+f`, which yazi already bound alongside them, remain.
        (Input, "ctrl+c",    C::OverlayClose,          "Cancel input",                       Always),
        (Input, "enter",     C::OverlaySubmit,         "Submit",                             Always),
        (Input, "esc",       C::Escape,                "Back to normal mode, or cancel",     Always),
        (Input, "i",         C::InputInsert,           "Insert mode",                        Always),
        (Input, "I",         C::InputInsertBol,        "Insert at the start of the line",    Always),
        (Input, "a",         C::InputAppend,           "Append mode",                        Always),
        (Input, "A",         C::InputAppendEol,        "Append at the end of the line",      Always),
        (Input, "v",         C::InputVisual,           "Visual mode",                        Always),
        (Input, "V",         C::InputVisualLine,       "Select the whole line",              Always),
        (Input, "r",         C::InputReplace,          "Replace one character",              Always),
        (Input, "left",      C::InputMoveLeft,         "Back a character",                   Always),
        (Input, "right",     C::InputMoveRight,        "Forward a character",                Always),
        (Input, "ctrl+b",    C::InputMoveLeft,         "Back a character",                   Always),
        (Input, "ctrl+f",    C::InputMoveRight,        "Forward a character",                Always),
        (Input, "b",         C::InputWordBackward,     "Back a word",                        Always),
        (Input, "B",         C::InputWordBackwardFar,  "Back a WORD",                        Always),
        (Input, "w",         C::InputWordForward,      "Forward a word",                     Always),
        (Input, "W",         C::InputWordForwardFar,   "Forward a WORD",                     Always),
        (Input, "e",         C::InputWordEnd,          "End of word",                        Always),
        (Input, "E",         C::InputWordEndFar,       "End of WORD",                        Always),
        (Input, "alt+b",     C::InputWordBackward,     "Back a word",                        Always),
        (Input, "alt+f",     C::InputWordEnd,          "End of word",                        Always),
        (Input, "0",         C::InputMoveBol,          "Start of line",                      Always),
        (Input, "$",         C::InputMoveEol,          "End of line",                        Always),
        (Input, "^",         C::InputMoveFirstChar,    "First non-blank character",          Always),
        (Input, "_",         C::InputMoveFirstChar,    "First non-blank character",          Always),
        (Input, "ctrl+a",    C::InputMoveBol,          "Start of line",                      Always),
        (Input, "ctrl+e",    C::InputMoveEol,          "End of line",                        Always),
        (Input, "home",      C::InputMoveBol,          "Start of line",                      Always),
        (Input, "end",       C::InputMoveEol,          "End of line",                        Always),
        (Input, "backspace", C::InputBackspace,        "Delete the character before",        Always),
        (Input, "delete",    C::InputDeleteUnder,      "Delete the character under",         Always),
        (Input, "ctrl+h",    C::InputBackspace,        "Delete the character before",        Always),
        (Input, "ctrl+d",    C::InputDeleteUnder,      "Delete the character under",         Always),
        (Input, "ctrl+u",    C::InputKillBol,          "Kill back to the start of the line", Always),
        (Input, "ctrl+k",    C::InputKillEol,          "Kill to the end of the line",        Always),
        (Input, "ctrl+w",    C::InputKillWordBackward, "Kill the word before",               Always),
        (Input, "alt+d",     C::InputKillWordForward,  "Kill the word after",                Always),
        (Input, "d",         C::InputCut,              "Cut the selection",                  Always),
        (Input, "D",         C::InputCutEol,           "Cut to the end of the line",         Always),
        (Input, "c",         C::InputChange,           "Change the selection",               Always),
        (Input, "C",         C::InputChangeEol,        "Change to the end of the line",      Always),
        (Input, "s",         C::InputSubstitute,       "Substitute the character",           Always),
        (Input, "S",         C::InputSubstituteLine,   "Substitute the line",                Always),
        (Input, "x",         C::InputCutChar,          "Cut the character",                  Always),
        (Input, "y",         C::InputYank,             "Copy the selection",                 Always),
        (Input, "p",         C::InputPaste,            "Paste after",                        Always),
        (Input, "P",         C::InputPasteBefore,      "Paste before",                       Always),
        (Input, "u",         C::InputUndo,             "Undo",                               Always),
        (Input, "ctrl+r",    C::InputRedo,             "Redo",                               Always),

        // ── Confirm ─────────────────────────────────────────────────────────
        (Confirm, "esc",    C::OverlayClose,  "Cancel",        Always),
        (Confirm, "ctrl+c", C::OverlayClose,  "Cancel",        Always),
        (Confirm, "enter",  C::OverlaySubmit, "Confirm",       Always),
        (Confirm, "n",      C::OverlayClose,  "No",            Always),
        (Confirm, "y",      C::OverlaySubmit, "Yes",           Always),
        (Confirm, "up",     C::OverlayPrev,   "Previous line", Always),
        (Confirm, "down",   C::OverlayNext,   "Next line",     Always),
        // The conflict dialog's four answers (PLAN §5: "overwrite / skip /
        // rename / apply-to-all"). The dialog matched these keys literally
        // before they were rows, which worked and was invisible: a key with no
        // registry row is absent from the help sheet and from which-key, so the
        // only way to learn it was to be told. They are inert outside the
        // dialog because nothing else runs a conflict command.
        (Confirm, "o",      C::ConflictOverwrite, "Overwrite",    Always),
        (Confirm, "s",      C::ConflictSkip,      "Skip",         Always),
        (Confirm, "r",      C::ConflictRename,    "Rename",       Always),
        (Confirm, "a",      C::ConflictApplyAll,  "Apply to all", Always),

        // ── Pick (the `O` opener chooser) ───────────────────────────────────
        (Pick, "esc",    C::OverlayClose,  "Cancel",          Always),
        (Pick, "ctrl+c", C::OverlayClose,  "Cancel",          Always),
        (Pick, "enter",  C::OverlaySubmit, "Choose",          Always),
        (Pick, "up",     C::OverlayPrev,   "Previous option", Always),
        (Pick, "down",   C::OverlayNext,   "Next option",     Always),
        // The search panel stacks on Pick; this puts ctrl+s on its help sheet.
        (Pick, "ctrl+s", C::CancelSearch,  "Cancel the search", Always),

        // ── Tasks (`w`) ─────────────────────────────────────────────────────
        (Tasks, "esc",    C::OverlayClose, "Close the task manager", Always),
        (Tasks, "ctrl+c", C::OverlayClose, "Close the task manager", Always),
        (Tasks, "w",      C::OverlayClose, "Close the task manager", Always),
        (Tasks, "up",     C::OverlayPrev,  "Previous task",          Always),
        (Tasks, "down",   C::OverlayNext,  "Next task",              Always),
        (Tasks, "enter",  C::TaskInspect,  "Inspect the task",       Always),
        // PLAN §5's task engine is "pause/resume, cancel"; `p` is the half that
        // had no row, so it did not appear in the help sheet next to `x`.
        (Tasks, "p",      C::TaskPauseResume, "Pause / resume the task", Always),
        (Tasks, "x",      C::TaskCancel,   "Cancel the task",        Always),

        // ── Spot (`Tab`) ────────────────────────────────────────────────────
        (Spot, "esc",    C::OverlayClose,  "Close the spot panel",       Always),
        (Spot, "ctrl+c", C::OverlayClose,  "Close the spot panel",       Always),
        (Spot, "tab",    C::OverlayClose,  "Close the spot panel",       Always),
        (Spot, "up",     C::OverlayPrev,   "Previous line",              Always),
        (Spot, "down",   C::OverlayNext,   "Next line",                  Always),
        (Spot, "left",   C::SpotSwipePrev, "Swipe to the previous file", Always),
        (Spot, "right",  C::SpotSwipeNext, "Swipe to the next file",     Always),
        (Spot, "c c",    C::SpotCopyCell,  "Copy the selected cell",     Always),

        // ── Help (`~` / `F1`) ───────────────────────────────────────────────
        (Help, "esc",    C::Escape,       "Clear the filter, or close", Always),
        (Help, "ctrl+c", C::OverlayClose, "Close the help",             Always),
        (Help, "up",     C::OverlayPrev,  "Previous line",              Always),
        (Help, "down",   C::OverlayNext,  "Next line",                  Always),
        (Help, "f",      C::HelpFilter,   "Filter the help (or type)",  Always),

        // ── Palette (`Ctrl+p`, §4.4) ────────────────────────────────────────
        (Palette, "esc",    C::OverlayClose,  "Close the palette", Always),
        (Palette, "ctrl+c", C::OverlayClose,  "Close the palette", Always),
        (Palette, "ctrl+p", C::OverlayClose,  "Close the palette", Always),
        (Palette, "enter",  C::OverlaySubmit, "Run the command",   Always),
        (Palette, "up",     C::OverlayPrev,   "Previous command",  Always),
        (Palette, "down",   C::OverlayNext,   "Next command",      Always),
    ];

    let mut km = Registry::new();
    for (context, keys, command, description, when) in rows {
        let seq = parse_sequence(keys).unwrap_or_else(|e| panic!("default keymap: `{keys}`: {e}"));
        if let Err(e) = km.register(*context, seq, *command, *description, *when) {
            panic!("default keymap: `{keys}`: {e}");
        }
    }

    // The `g <key>` bookmark rows come from the config table so that the paths
    // and the chords stay a single list (PLAN §3).
    let warnings = km.apply_bookmarks(&crate::config::default_bookmarks(), Path::new("<defaults>"));
    assert!(
        warnings.is_empty(),
        "default bookmarks are not bindable: {warnings:?}"
    );

    km
}
