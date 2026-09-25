//! The shipped keymap — PLAN §4.1–4.3.
//!
//! **Defaults ARE Brian's yazi config** (PLAN §3), so this table is his
//! `~/.config/yazi/keymap.toml` ported binding-for-binding, with the three
//! changes the plan calls for:
//!
//! 1. `hjkl` navigation is gone (PLAN's opening paragraph). Arrows move the
//!    cursor; `j` `k` `l` are delightviewer's shuttle keys, bound in Global so
//!    they work wherever you are, and `H`/`L` (yazi's history) retire in favour of
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
    use When::{Always, MediaHovered};

    // Hand-aligned on purpose. rustfmt turns a 200-row tuple list into 900
    // lines of one field per line, and this table's entire value is that a
    // person can read down a column and see what every key does.
    #[rustfmt::skip]
    let rows: &[(Context, &str, Command, &str, When)] = &[
        // ── Global ──────────────────────────────────────────────────────────
        // Esc is layered by the router, not by the table: one command whose
        // meaning is the §4.1 ladder (close the overlay → cancel the chord →
        // clear the filter → clear the selection).
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
        // The key that opens a program's menu in GTK, Qt and Windows alike —
        // and one no file-manager habit has a claim on.
        (Global, "f10",          C::AppMenu,        "Open the app menu",     Always),
        (Global, "ctrl+shift+z", C::Undo,           "Undo last operation",   Always),

        // ── Transport, on the hovered media file, from anywhere (§4.3) ──────
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

        // ── The preview, driven from the list (§4.3) ────────────────────────
        // The keyboard never leaves the list (PLAN §2.1), so every one of these
        // acts on the file the cursor is standing on and none of them may take
        // a key the list already owns. **Ctrl+arrow is the preview's arrow
        // family**: one modifier, four directions, and the pair that points
        // sideways reads as the frame step on a clip and as the page turn on
        // anything else — the same `MediaHovered` split the transport already
        // uses, declared media-first so a video wins.
        (Global, "ctrl+left",  C::FrameStepBack,    "Step back one frame",    MediaHovered),
        (Global, "ctrl+right", C::FrameStepForward, "Step forward one frame", MediaHovered),
        (Global, "ctrl+m",     C::Mute,             "Mute",                   MediaHovered),

        (Global, "ctrl+up",      C::PreviewUp,           "Preview: up a line / layer",   Always),
        (Global, "ctrl+down",    C::PreviewDown,         "Preview: down a line / layer", Always),
        (Global, "ctrl+left",    C::PreviewLeft,         "Preview: previous page",       Always),
        (Global, "ctrl+right",   C::PreviewRight,        "Preview: next page",           Always),
        (Global, "ctrl+shift+u", C::PreviewHalfPageUp,   "Preview: half page up",        Always),
        (Global, "ctrl+shift+d", C::PreviewHalfPageDown, "Preview: half page down",      Always),
        (Global, "shift+space",  C::PreviewPageDown,     "Preview: page down",           Always),
        (Global, "ctrl+home",    C::PreviewTop,          "Preview: top",                 Always),
        (Global, "ctrl+end",     C::PreviewBottom,       "Preview: bottom",              Always),
        // **The zoom family is on Ctrl.** The bare `+`/`=`/`-`/`0` it used to
        // wear are the view-scale ladder now (PLAN §4.1): those are keys you
        // press dozens of times a session on the thing you are looking *at*,
        // and the preview's zoom is a thing you reach for on the one file you
        // are peering into. One modifier for the whole family also ends the
        // odd-one-out `Alt+-` that the old symlink binding had forced on zoom
        // out — three keys, one Ctrl, no exceptions.
        (Global, "ctrl+=",       C::PreviewZoomIn,       "Preview: zoom in",             Always),
        (Global, "ctrl++",       C::PreviewZoomIn,       "Preview: zoom in",             Always),
        (Global, "ctrl+-",       C::PreviewZoomOut,      "Preview: zoom out",            Always),
        (Global, "ctrl+0",       C::PreviewZoomReset,    "Preview: reset zoom",          Always),

        // ── Files: quitting ─────────────────────────────────────────────────
        (Files, "q",      C::Quit,          "Quit",                                 Always),
        (Files, "Q",      C::QuitNoCwdFile, "Quit without writing cwd-file",        Always),
        (Files, "ctrl+c", C::CloseTab,      "Close tab, or quit if it is the last", Always),

        // ── Files: the cursor ───────────────────────────────────────────────
        (Files, "up",             C::CursorUp,       "Previous file",                     Always),
        (Files, "down",           C::CursorDown,     "Next file",                         Always),
        (Files, "ctrl+u",         C::HalfPageUp,     "Half page up",                      Always),
        (Files, "ctrl+d",         C::HalfPageDown,   "Half page down",                    Always),
        (Files, "ctrl+b",         C::PageUp,         "Page up",                           Always),
        (Files, "ctrl+f",         C::PageDown,       "Page down",                         Always),
        (Files, "pageup",         C::PageUp,         "Page up",                           Always),
        (Files, "pagedown",       C::PageDown,       "Page down",                         Always),
        (Files, "shift+pageup",   C::HalfPageUp,     "Half page up",                      Always),
        (Files, "shift+pagedown", C::HalfPageDown,   "Half page down",                    Always),
        (Files, "g g",            C::CursorTop,      "Go to top",                         Always),
        (Files, "G",              C::CursorBottom,   "Go to bottom",                      Always),
        (Files, "left",           C::Leave,          "Back to the parent directory",      Always),
        // `→` enters a directory (and an archive, §7.3). On a plain file there
        // is nothing to the right to go into, so it does nothing at all.
        (Files, "right",          C::EnterDirectory, "Enter directory",                   Always),
        (Files, "alt+left",       C::HistoryBack,    "Back to previous directory",        Always),
        (Files, "alt+right",      C::HistoryForward, "Forward to next directory",         Always),
        // No yazi ancestor. `Ctrl+l` is the location bar in every browser and
        // in the GTK and KDE file dialogs, so it is the key a hand already
        // reaches for when it wants to type where to go.
        (Files, "ctrl+l",         C::GotoPath,       "Type a path to go to",              Always),

        // ── Files: selection ────────────────────────────────────────────────
        (Files, "space",  C::ToggleSelect,    "Toggle selection and advance", Always),
        (Files, "ctrl+a", C::SelectAll,       "Select all files",             Always),
        (Files, "ctrl+r", C::InvertSelection, "Invert the selection",         Always),
        (Files, "v",      C::VisualMode,      "Visual (selection) mode",      Always),
        (Files, "V",      C::VisualUnset,     "Visual (unset) mode",          Always),
        // yazi parity: nudge the preview without leaving the list.
        (Files, "K",      C::SeekPreviewUp,   "Seek preview up 5",            Always),
        (Files, "J",      C::SeekPreviewDown, "Seek preview down 5",          Always),
        (Files, "tab",    C::Spot,            "Spot the hovered file",        Always),
        // No yazi ancestor: grid view is delightfile's own. ctrl+g is free in
        // yazi's mgr table, and g alone is the goto prefix — close cousins.
        (Files, "ctrl+g", C::ToggleView,      "Toggle list / grid view",      Always),

        // ── Files: how big the list draws itself (§4.1) ─────────────────────
        // Explorer's view slider as two keys. `-` and `=` are the pair every
        // program on the machine uses for smaller/bigger, and `+` is the same
        // key with Shift on it — nobody who means "bigger" should have to
        // notice which one they hit. The ladder's top step *is* the grid, so
        // `=` off the largest list lands in the tiles and `-` climbs back out.
        // These take the keys the preview's zoom used to have; the zoom is on
        // Ctrl now (see the Global block above), and the symlink family that
        // used to own `-`/`_`/`Ctrl+-` is unbound (see `Command`).
        (Files, "-",      C::ViewScaleDown,   "Smaller rows / leave the grid", Always),
        (Files, "=",      C::ViewScaleUp,     "Bigger rows / into the grid",   Always),
        (Files, "+",      C::ViewScaleUp,     "Bigger rows / into the grid",   Always),

        // ── Files: opening ──────────────────────────────────────────────────
        (Files, "o",           C::Open,            "Open",       Always),
        (Files, "enter",       C::Open,            "Open",       Always),
        (Files, "O",           C::OpenInteractive, "Open with…", Always),
        (Files, "shift+enter", C::OpenInteractive, "Open with…", Always),
        // No yazi ancestor: yazi has no button to stand in for. `Ctrl+Enter`
        // is the "submit the form" key everywhere a plain `Enter` already
        // means something closer to hand — here, walking into a folder.
        (Files, "ctrl+enter",  C::Choose,
            "Choose — what the dialog's Select / Choose folder / Save button does", Always),

        // ── Files: the clipboard and the file operations (§5) ────────────────
        (Files, "y",      C::Yank,              "Yank (copy)",                         Always),
        (Files, "x",      C::YankCut,           "Yank (cut)",                          Always),
        (Files, "p",      C::Paste,             "Paste",                               Always),
        (Files, "P",      C::PasteForce,        "Paste, overwriting",                  Always),
        (Files, "Y",      C::CopyToClipboard,   "Copy to the system clipboard",        Always),
        (Files, "X",      C::Unyank,            "Cancel the yank",                     Always),
        // No symlink or hardlink row. `-`/`_`/`Ctrl+-` were yazi's, and all
        // three are worth more as the view-scale ladder and the preview's zoom
        // than as an operation somebody performs deliberately once a month.
        // `symlink-absolute`, `symlink-relative` and `hardlink` are still
        // commands — a `keymap.toml` line puts any of them on any key.
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
        (Files, "b",      C::YankToggle,        "Add to the yank, or take back out",   Always),
        (Files, "B",      C::YankShow,          "Show what is yanked",                 Always),
        (Files, "e",      C::ArchiveExtractHere,      "Extract here",                  Always),
        (Files, "E",      C::ArchiveExtractSubfolder, "Extract to folder",             Always),

        // ── Files: what is shown ────────────────────────────────────────────
        (Files, ".",   C::ToggleHidden,        "Toggle hidden files",   Always),
        (Files, "m s", C::LinemodeSize,        "Linemode: size",        Always),
        (Files, "m p", C::LinemodePermissions, "Linemode: permissions", Always),
        (Files, "m b", C::LinemodeBtime,       "Linemode: created",     Always),
        (Files, "m m", C::LinemodeMtime,       "Linemode: modified",    Always),
        (Files, "m o", C::LinemodeOwner,       "Linemode: owner",       Always),
        (Files, "m n", C::LinemodeNone,        "Linemode: none",        Always),
        (Files, "m u", C::DiskUsage,           "Show disk usage",       Always),

        // ── Files: copy the path, four ways, and the contents ───────────────
        (Files, "c c", C::CopyPath,     "Copy the file path",                      Always),
        (Files, "c d", C::CopyDirname,  "Copy the directory path",                 Always),
        (Files, "c f", C::CopyFilename, "Copy the filename",                       Always),
        (Files, "c n", C::CopyStem,     "Copy the filename without extension",     Always),
        (Files, "c t", C::CopyFileText, "Copy the text contents (yank if binary)", Always),

        // ── Files: filter, find, search, jump ───────────────────────────────
        (Files, "f",      C::Filter,        "Filter this folder, hide the rest", Always),
        // The vi set, minus the half of it that would cost the help key: `/`
        // searches forward, `n` is the next match and `N` the previous one.
        // Backwards *search* (`find-prev`) keeps its command id for anybody who
        // wants it back in `keymap.toml`; `?` is help (see Global above), which
        // is what the key is for in every other program on the machine.
        (Files, "/",      C::FindNext,      "Jump to a name in this folder",     Always),
        (Files, "n",      C::FindArrowNext, "Next match",                        Always),
        (Files, "N",      C::FindArrowPrev, "Previous match",                    Always),
        (Files, "s",      C::SearchName,    "Search everywhere by name",         Always),
        (Files, "S",      C::SearchContent, "Search everywhere inside files",    Always),
        (Files, "ctrl+s", C::CancelSearch,  "Cancel the search",                 Always),
        (Files, "z",      C::FuzzyJump,     "Jump to a file or directory",       Always),
        (Files, "Z",      C::ZoxideJump,    "Jump by frecency (zoxide)",         Always),

        // ── Files: sort. The time and size sorts also switch the linemode, as
        // in the yazi config this is ported from — the column you just sorted
        // by is the column you want to see.
        (Files, ", m", C::SortMtime,               "Sort by modified",              Always),
        (Files, ", M", C::SortMtimeReverse,        "Sort by modified (reverse)",    Always),
        (Files, ", b", C::SortBtime,               "Sort by created",               Always),
        (Files, ", B", C::SortBtimeReverse,        "Sort by created (reverse)",     Always),
        (Files, ", e", C::SortExtension,           "Sort by extension",             Always),
        (Files, ", E", C::SortExtensionReverse,    "Sort by extension (reverse)",   Always),
        (Files, ", a", C::SortAlphabetical,        "Sort alphabetically",           Always),
        (Files, ", A", C::SortAlphabeticalReverse, "Sort alphabetically (reverse)", Always),
        (Files, ", n", C::SortNatural,             "Sort naturally",                Always),
        (Files, ", N", C::SortNaturalReverse,      "Sort naturally (reverse)",      Always),
        (Files, ", s", C::SortSize,                "Sort by size",                  Always),
        (Files, ", S", C::SortSizeReverse,         "Sort by size (reverse)",        Always),
        (Files, ", r", C::SortRandom,              "Sort randomly",                 Always),

        // ── Files: the goto chords that are not bookmarks. The bookmark rows
        // (`g h`, `g w`, …) are registered from the config table below, so the
        // paths stay editable in one place (PLAN §3).
        (Files, "g r",     C::GotoGitRoot,     "Go to the git root",         Always),
        (Files, "g space", C::GotoInteractive, "Jump interactively",         Always),
        (Files, "g f",     C::FollowSymlink,   "Follow the hovered symlink", Always),
        // yazi binds `g t` to `/tmp`; PLAN §4.1 dropped that row, so the slot is
        // free and the trash — which PLAN §7.4 wants a virtual location for —
        // takes it. `t` for trash, one key from a list, next to the other places.
        (Files, "g t",     C::OpenTrash,       "Browse the trash",           Always),

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

        // ── Input: the shared line editor (§4.2) ────────────────────────────
        // Readline's map, and nothing modal: there is no Normal mode to reach,
        // so every unmodified key types itself and only the modified chords are
        // commands. That is why no bare letter appears in this table.
        //
        // **This table is dispatched, not merely listed.** A prompt resolves
        // every chord against it before the editor's own built-in map (see the
        // app's `prompt_action`), so an `[input]` line in `keymap.toml` really
        // does move an editing key — and a chord with no row here falls back to
        // [`crate::input::InputBuffer::binding`], which is where the readline
        // vocabulary and "a printable key is text" live.
        (Input, "ctrl+c",      C::OverlayClose,          "Cancel input",                       Always),
        (Input, "enter",       C::OverlaySubmit,         "Submit",                             Always),
        (Input, "esc",         C::Escape,                "Cancel",                             Always),
        (Input, "left",        C::InputMoveLeft,         "Back a character",                   Always),
        (Input, "right",       C::InputMoveRight,        "Forward a character",                Always),
        (Input, "ctrl+b",      C::InputMoveLeft,         "Back a character",                   Always),
        (Input, "ctrl+f",      C::InputMoveRight,        "Forward a character",                Always),
        (Input, "alt+b",       C::InputWordBackward,     "Back a word",                        Always),
        (Input, "alt+f",       C::InputWordForward,      "Forward a word",                     Always),
        (Input, "ctrl+left",   C::InputWordBackward,     "Back a word",                        Always),
        (Input, "ctrl+right",  C::InputWordForward,      "Forward a word",                     Always),
        (Input, "ctrl+a",      C::InputMoveBol,          "Start of line",                      Always),
        (Input, "ctrl+e",      C::InputMoveEol,          "End of line",                        Always),
        (Input, "home",        C::InputMoveBol,          "Start of line",                      Always),
        (Input, "end",         C::InputMoveEol,          "End of line",                        Always),
        (Input, "shift+left",  C::InputSelectLeft,       "Select a character back",            Always),
        (Input, "shift+right", C::InputSelectRight,      "Select a character forward",         Always),
        (Input, "shift+home",  C::InputSelectBol,        "Select to the start of the line",    Always),
        (Input, "shift+end",   C::InputSelectEol,        "Select to the end of the line",      Always),
        (Input, "ctrl+shift+left",  C::InputSelectWordBackward, "Select a word back",          Always),
        (Input, "ctrl+shift+right", C::InputSelectWordForward,  "Select a word forward",       Always),
        // The smaller word: the same motions, stopping at every `_` too. A
        // prompt takes every key before the browser sees it, so these do not
        // collide with `[files]`'s history on the same chords.
        (Input, "alt+left",    C::InputSubwordBackward,  "Back a word, stopping at _",         Always),
        (Input, "alt+right",   C::InputSubwordForward,   "Forward a word, stopping at _",      Always),
        (Input, "alt+shift+left",  C::InputSelectSubwordBackward, "Select a word back, stopping at _",    Always),
        (Input, "alt+shift+right", C::InputSelectSubwordForward,  "Select a word forward, stopping at _", Always),
        (Input, "backspace",   C::InputBackspace,        "Delete the character before",        Always),
        (Input, "delete",      C::InputDeleteUnder,      "Delete the character under",         Always),
        (Input, "ctrl+h",      C::InputBackspace,        "Delete the character before",        Always),
        (Input, "ctrl+d",      C::InputDeleteUnder,      "Delete the character under",         Always),
        (Input, "ctrl+u",      C::InputKillBol,          "Kill back to the start of the line", Always),
        (Input, "ctrl+k",      C::InputKillEol,          "Kill to the end of the line",        Always),
        (Input, "ctrl+w",      C::InputKillWordBackward, "Kill the word before",               Always),
        (Input, "alt+d",       C::InputKillWordForward,  "Kill the word after",                Always),
        (Input, "ctrl+z",      C::InputUndo,             "Undo",                               Always),
        (Input, "ctrl+y",      C::InputRedo,             "Redo",                               Always),
        // The two the editor has always answered to and the table never said
        // out loud. The sheet lists this table, so a row that is missing here
        // is a key nobody can find.
        (Input, "ctrl+shift+z", C::InputRedo,            "Redo",                               Always),
        (Input, "ctrl+[",      C::Escape,                "Cancel",                             Always),
        (Input, "ctrl+v",      C::InputPaste,            "Paste from the clipboard",           Always),

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
        (Pick, "tab",    C::SearchToggle,  "Names ⟷ contents",  Always),

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
        // The sheet takes the keyboard whole while it is up, so every key that
        // walks a list has to have a row here. A page key with no row used to
        // fall through to the pane *behind* the scrim, and the list scrolled
        // under a sheet that could not show it — the reader's `PageDown` moved
        // something they could not see.
        (Help, "esc",     C::Escape,       "Clear the filter, or close", Always),
        (Help, "ctrl+c",  C::OverlayClose, "Close the help",             Always),
        // The key that opened it closes it, as `w` does for the task panel and
        // `Tab` for the spot card. Only `F1`: `?` and `~` are printable, and a
        // printable key in front of this sheet is filter text.
        (Help, "f1",      C::OverlayClose, "Close the help",             Always),
        (Help, "up",      C::OverlayPrev,  "Previous line",              Always),
        (Help, "down",    C::OverlayNext,  "Next line",                  Always),
        (Help, "pageup",  C::HelpPageUp,       "A page up",              Always),
        (Help, "pagedown",C::HelpPageDown,     "A page down",            Always),
        (Help, "ctrl+u",  C::HelpHalfPageUp,   "Half a page up",         Always),
        (Help, "ctrl+d",  C::HelpHalfPageDown, "Half a page down",       Always),
        // `home`/`end` rather than `g g`/`G`: the sheet's other job is to be
        // typed into, and a `g` that armed a chord would be a `g` the filter
        // never received.
        (Help, "home",    C::HelpTop,          "First binding",          Always),
        (Help, "end",     C::HelpBottom,       "Last binding",           Always),
        (Help, "f",       C::HelpFilter,   "Filter — or just type",      Always),

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
