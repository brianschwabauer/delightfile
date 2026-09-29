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
//! | `appearance::Desktop` | `watch_over(Connect, Notifier) -> Desktop`, `drain(&mut self) -> Option<appearance::Scheme>`, `heard(&self) -> bool`, `link(&self) -> appearance::Link`, `started(&self) -> Instant`, `wait_first(&mut self, Duration) -> Option<Scheme>` | the portal's `color-scheme`, read and then heard on a thread | no thread; never heard, `Link::Gone` | S1.35 |
//! | `appearance::Connect` | `Clone` type the app holds and hands to `watch_over` | `Arc<dyn Fn() -> Result<Bus, String>>` | `Arc<dyn Fn() -> Result<Infallible, String>>` | S1.35 |
//! | `appearance::session` | `fn() -> Connect` | the session bus | a connection that is never made | S1.35 |
//! | `desktop::Desktop` | `ready(&self) -> bool`, `set_selection(&self, Vec<String>, Vec<u8>) -> bool`, `receive(&self, u64, String) -> bool`, `drag(&self, Vec<(String, Vec<u8>)>, usize, icon::Rgba, icon::Rgba, i32) -> bool`, `poll(&self) -> Vec<desktop::Event>` | the Wayland data device | no value exists | S1.22 |
//! | `desktop::start` | `fn(&ActiveEventLoop, &Window, app::Waker) -> Option<Desktop>` | adopts winit's Wayland connection | `None` | S1.22 |
//! | `desktop::{Event, PasteFailure}` | the events `Desktop::poll` answers in | portable, one definition | same | S1.22 |
//! | `mounts::run` | `fn(Receiver<mounts::Request>, Sender<mounts::Answer>, Notifier, mounts::Gio)`, the Places card worker's thread | udisks2 over the system bus, `gio mount -u`, `gio mount -li` | an empty listing; any other request `Failed("Not available on this platform")` | S1.24 |
//! | `mounts::{connect, mount_gio}` | `fn(&str, &mounts::Gio) -> mounts::Connected` | `gio mount`, and where the mount landed | `Connected::Failed` | S1.24 |
//! | `mounts::system_gio` | `fn() -> mounts::Gio` | runs `gio` | answers every run `Unsupported` | S1.24 |
//! | `mounts::Monitor` | `start(Notifier) -> Option<Monitor>`, `gone(&mut self) -> bool`, `started(&self) -> Instant`, `drain(&self) -> Vec<mounts::Event>` | `gio mount --monitor --detail` | `start` is `None`; no value exists | S1.24 |
//! | `mounts::TERMINAL_MOUNT` | `Option<&str>` | the shell line that re-runs `gio mount` in a terminal | `None` | S1.24 |
//! | `window::attributes` | `fn(title: &str, app_id: &str) -> WindowAttributes` | title, `app::WINDOW_SIZE`, Wayland `app_id` | macOS: title, size, Option read as Alt; Windows: title, size | S1.27 |
//! | `gfx::PREFERRED_BACKENDS` | `wgpu::Backends`, the first instance's | `VULKAN` | macOS `METAL`, Windows `DX12` | S1.28 |
//! | `gfx::PREFERRED_NAME` | `&str`, what the log calls them | `"Vulkan"` | `"Metal"`, `"DX12"` | S1.28 |
//! | `icon::{draw, Icon, Rgba, HOTSPOT}` | the drag icon's pixels | portable, drawn for the Wayland drag | same, unused until a native drag-out | S1.22 |
//!
//! The rows are the phase's checklist inside the code: each task of
//! `plans/other-platforms/01-platform-seam.md` §3 adds its own as it lands.
//! `desktop` and `icon` are the same on every target and live here rather
//! than in a target's module; `desktop` takes its `Desktop` and `start` from
//! the target's `device`.

pub mod desktop;
pub mod icon;

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
