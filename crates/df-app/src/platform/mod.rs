//! Everything in df-app that differs by operating system, behind one set of
//! names (`plans/other-platforms/00-ground-rules.md` §2).
//!
//! The rest of the program is written once and calls what this module
//! exports. Which body answers is decided here, by the `cfg` on the `mod`
//! declarations below, and nowhere else: code outside this directory never
//! names an operating system. There are no traits and no `dyn` — three
//! targets, one caller each, and the CI matrix compiling all three is what
//! keeps their shapes the same.
//!
//! `linux/` is the program as it has always run, moved here whole: the
//! Wayland data device, the D-Bus client, the file-chooser portal, udisks2
//! and gvfs. `macos/` and `windows/` are honest stand-ins until Phases 2 and
//! 4 give them bodies: a feature with no native body yet answers "not here" —
//! a refusal, an empty listing, `None` — and never panics or pretends to have
//! done something.
//!
//! ## The contract
//!
//! Every target exports exactly what this table lists, with these
//! signatures, and a row changes in the same change as its signature. The
//! Linux modules hold more than this — the portal and the mounts worker are
//! whole programs — but nothing outside `platform/` may use anything the
//! table does not name, because on the other targets nothing else is there.
//!
//! | Item | Signature | Linux | macOS, Windows | Task |
//! |---|---|---|---|---|
//! | `HAS_PORTAL` | `const bool`: whether `--portal` exists | `true` | `false` | S1.31 |
//! | `process::attach_parent_console` | `fn()`, first thing in `main` | nothing (the shared `unix` body) | macOS nothing; Windows: `AttachConsole(ATTACH_PARENT_PROCESS)`, and standard output and error that point nowhere reopened on `CONOUT$` | W4.1 |
//! | `portal::run` | `fn() -> i32`, the process's exit status | the file-chooser portal and `org.freedesktop.FileManager1` on the session bus | never reached (`--portal` is not accepted); says so and returns 2 | S1.31 |
//! | `cli::{REVEAL_USAGE, REQUEST_USAGE, EXTRA_USAGE}` | `&str`, the `--help` entries that differ | the `--reveal` and `--chooser-request` entries naming the portal, and `--portal`'s | the same two without it, and `""` | S1.31 |
//! | `clipboard::{copy, reap, offered_types, paste}` | `copy(Option<&str>, &[u8]) -> Result<Option<Child>, clipboard::ClipError>` (`Some` is a process the window owns until `reap(&mut Child)`, `None` a copy already made), `offered_types() -> Result<Vec<String>, ClipError>`, `paste(&str) -> Result<Vec<u8>, ClipError>`: the clipboard without a data device | `wl-copy --foreground`, `wl-paste` | macOS: `NSPasteboard`, synchronous, so `copy` is `Ok(None)` (M2.10); Windows: every call `ClipError::Missing("clipboard")`; `reap` stops and collects | S1.23 |
//! | `clipboard::missing` | `fn(&str) -> String`, what `ClipError::Missing` says | "… is not installed — install wl-clipboard" | macOS: "The pasteboard is not available", never said; Windows: "No clipboard transport on this platform" | S1.23 |
//! | `appearance::Desktop` | `watch_over(Connect, Notifier) -> Desktop`, `drain(&mut self) -> Option<appearance::Scheme>`, `heard(&self) -> bool`, `link(&self) -> appearance::Link`, `started(&self) -> Instant`, `wait_first(&mut self, Duration) -> Option<Scheme>` | the portal's `color-scheme`, read and then heard on a thread | macOS: `NSApp.effectiveAppearance` under key-value observing, the first answer in before `watch_over` returns, `Link::Gone` off the main thread (M2.30); Windows: no thread, never heard, `Link::Gone` | S1.35 |
//! | `appearance::Connect` | `Clone` type the app holds and hands to `watch_over` | `Arc<dyn Fn() -> Result<Bus, String>>` | macOS: a unit struct, the application being always there to ask; Windows: `Arc<dyn Fn() -> Result<Infallible, String>>` | S1.35 |
//! | `appearance::session` | `fn() -> Connect` | the session bus | macOS: the application; Windows: a connection that is never made | S1.35 |
//! | `desktop::Desktop` | `ready(&self) -> bool`, `set_selection(&self, Vec<String>, Vec<u8>) -> bool`, `receive(&self, u64, String) -> bool`, `drag(&self, Vec<(String, Vec<u8>)>, usize, icon::Rgba, icon::Rgba, i32) -> bool`, `poll(&self) -> Vec<desktop::Event>` | the Wayland data device | macOS: the pasteboard, each request done at once and answered on the next `poll`, the mirror taken from its change count; `drag` refused until M2.12 (M2.13). Windows: no value exists | S1.22 |
//! | `desktop::start` | `fn(&ActiveEventLoop, &Window, app::Waker) -> Option<Desktop>` | adopts winit's Wayland connection | macOS: always `Some`; Windows: `None` | S1.22 |
//! | `desktop::HANDS_OFF_ON_CURSOR_MOVED` | `const bool`: whether the window hands a drag off from its `CursorMoved` arm rather than from the frame | `false` | macOS `true`, since AppKit begins a drag only inside the mouse event that is dragging (M2.12); Windows `false` | M2.12 |
//! | `desktop::pointer_position` | `fn(&Window) -> Option<(f32, f32)>`, logical points, for a drop winit reports | `None` (Wayland has no query, and never reports one) | macOS: the window's pointer (`mouseLocationOutsideOfEventStream`) in winit's flipped view (M2.11); Windows: `None` until W4.17 | S1.32 |
//! | `desktop::{Event, PasteFailure}` | the events `Desktop::poll` answers in | portable, one definition | same | S1.22 |
//! | `mounts::run` | `fn(Receiver<mounts::Request>, Sender<mounts::Answer>, Notifier, mounts::Gio)`, the Places card worker's thread | udisks2 over the system bus, `gio mount -u`, `gio mount -li` | macOS: `NSFileManager`'s mounted volumes, the local ones disks and the rest shares, unmount and eject through `NSWorkspace`, `Mount` answered that macOS mounts disks itself (M2.14); Windows: an empty listing, any other request `Failed("Not available on this platform")` | S1.24 |
//! | `mounts::{connect, mount_gio}` | `fn(&str, &mounts::Gio) -> mounts::Connected` | `gio mount`, and where the mount landed | macOS: `connect` hands `smb://`, `nfs://` and `ftp://` to Finder and waits up to 10 s for the share under `/Volumes`, and refuses `sftp://` and WebDAV (M2.15), `mount_gio` `Failed`; Windows: `Connected::Failed` | S1.24 |
//! | `mounts::system_gio` | `fn() -> mounts::Gio` | runs `gio` | answers every run `Unsupported` | S1.24 |
//! | `mounts::Monitor` | `start(Notifier) -> Option<Monitor>`, `gone(&mut self) -> bool`, `started(&self) -> Instant`, `drain(&self) -> Vec<mounts::Event>` | `gio mount --monitor --detail` | `start` is `None`; no value exists | S1.24 |
//! | `mounts::TERMINAL_MOUNT` | `Option<&str>` | the shell line that re-runs `gio mount` in a terminal | `None` | S1.24 |
//! | `mounts::CONNECT_UNSEEN` | `Option<&str>`: what a connect back with no share to go to says in place of "Connected to" | `None`: `gio mount` has said it is done | macOS: "Finder was asked to connect to", since Finder's dialog may still be asking (M2.15); Windows: `None` | M2.15 |
//! | `open::{spawn_detached, run_blocking}` | `spawn_detached(&str, &[PathBuf], &Path) -> io::Result<()>`, `run_blocking(&str, &[PathBuf], &Path) -> io::Result<i32>`: an opener's command over paths, from a directory | `$SHELL -c '<snippet>' delightfile <path>…` (the shared `unix` body), detached with `setsid --fork`; `run_blocking`'s code from `df_core::platform::process::exit_code` | macOS: the same shell body, in a process group of its own and collected by a thread when it exits (M2.16); Windows: an argument list with `$1`, `$@`, `$dir` put in (`argv`), started directly in a process group of its own (W4.3) | S1.26 |
//! | `open::{spawn_typed, run_typed}` | the same signatures: a line typed at `;`/`:` | the shell body, as an opener | macOS the same; Windows: `%COMSPEC% /S /C "<line> <paths…>"` (W4.3) | W4.3 |
//! | `open::shell_open` | `fn(&OsStr) -> io::Result<()>`: `builtin:shell-open`, the system's own "open" | `Unsupported` (no table names it) | macOS `Unsupported`; Windows `ShellExecuteW(…, "open", …)` (W4.3) | W4.3 |
//! | `pdfium::LIBRARY_NAME` | `&str`, the library's file name | `libpdfium.so` | `libpdfium.dylib`, `pdfium.dll` | S1.30 |
//! | `pdfium::candidates` | `fn() -> Vec<PathBuf>`, most specific first, before the system loader | `$DF_PDFIUM_LIB`, `~/.local/lib/{delightfile,delightviewer}/`, the dev `target/` | macOS: `$DF_PDFIUM_LIB`, `<exe>/../Frameworks/`, `~/.local/lib/delightfile/`; Windows: `$DF_PDFIUM_LIB`, the executable's directory | S1.30 |
//! | `window::attributes` | `fn(title: &str, app_id: &str, &ActiveEventLoop) -> WindowAttributes` | title, `app::WINDOW_SIZE`, Wayland `app_id` | macOS: title, the size cut to the main screen's visible frame ([`fit`]), Option read as Alt; Windows: title, the size cut to the primary monitor's work area and centred there when it would not fit | S1.27 |
//! | `fonts::dirs` | `fn() -> Vec<PathBuf>`, where a Nerd Font is looked for, in order | `/usr/share/fonts/…`, `/usr/local/share/fonts`, `~/.local/share/fonts`, `~/.fonts` | macOS: `~/Library/Fonts`, `/Library/Fonts`, `/System/Library/Fonts{,/Supplemental}`; Windows: `%LOCALAPPDATA%\Microsoft\Windows\Fonts`, `C:\Windows\Fonts` | S1.29 |
//! | `trash::{LISTED_NOTE, EMPTIED_NOTE}` | `Option<&str>`: what the trash view says it cannot see, under an empty view and after "Empty trash" | `None`: the view is the whole freedesktop trash | macOS: only what delightfile trashed is listed and emptied, Finder's Trash may hold more (M2.9); Windows: `None` until Phase 4 | M2.9 |
//! | `keys::mods` | `fn(winit::keyboard::ModifiersState) -> df_core::keymap::Mods`, the held modifiers as the keymap names them | each as itself | macOS: Command or Control is `ctrl`, never `super_key`; Windows: as Linux | M2.20 |
//! | `keys::composed` | `fn(ModifiersState, text: Option<&str>) -> bool`: the press is a character the modifiers composed, so text and not a chord | `false` | macOS `false`; Windows: Ctrl and Alt both held with printable text (AltGr) | W4.25 |
//! | `gfx::PREFERRED_BACKENDS` | `wgpu::Backends`, the first instance's | `VULKAN` | macOS `METAL`, Windows `DX12` | S1.28 |
//! | `gfx::PREFERRED_NAME` | `&str`, what the log calls them | `"Vulkan"` | `"Metal"`, `"DX12"` | S1.28 |
//! | `gfx::ALLOW_FALLBACK_ADAPTER` | `const bool`: whether a failed adapter request is retried for the software adapter before other backends | `false` | macOS `false`; Windows `true` (WARP) | W4.23 |
//! | `icon::{draw, Icon, Rgba, HOTSPOT}` | the drag icon's pixels | portable, drawn for the Wayland drag | same, unused until a native drag-out | S1.22 |
//!
//! The rows are the phase's checklist inside the code: each task of
//! `plans/other-platforms/01-platform-seam.md` §3 adds its own as it lands.
//! `desktop` and `icon` are the same on every target and live here rather
//! than in a target's module; `desktop` takes its `Desktop` and `start` from
//! the target's `device`. `unix` holds what Linux and macOS share (the shell
//! body of `open`), which their modules re-export beside what is their own.

pub mod desktop;
pub mod icon;

/// What Linux and macOS share: taken from by their modules, named by no one
/// else.
#[cfg(unix)]
mod unix;

/// The pasteboard's type names for the clipboard's mimes: macOS's, and
/// compiled in every target's tests so the table is checked where it is
/// edited.
#[cfg(any(target_os = "macos", test))]
mod pasteboard;

/// The Places card's rows from the volumes macOS has mounted, and where its
/// connect prompt's address goes: macOS's, and compiled in every target's
/// tests for the same reason.
#[cfg(any(target_os = "macos", test))]
mod volumes;

/// What a drag out of the window carries on macOS, and its picture's
/// pixels: macOS's, and compiled in every target's tests.
#[cfg(any(target_os = "macos", test))]
mod dragout;

/// An opener's command split into a program and its arguments, the paths
/// put in: Windows', which has no shell to read it, and compiled in every
/// target's tests.
#[cfg(any(windows, test))]
mod argv;

/// The opening size cut to a screen too small for it: macOS's and
/// Windows', which can say how big the screen is, and compiled in every
/// target's tests.
#[cfg(any(target_os = "macos", windows, test))]
mod fit;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::*;

/// Stands in on macOS until Phase 2 (`plans/other-platforms/02-macos.md`).
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::*;

/// Stands in on Windows until Phase 4 (`plans/other-platforms/04-windows.md`).
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;
