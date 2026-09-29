# df-app platform inventory

`crates/df-app` is the `delightfile` binary (`[[bin]] name = "delightfile"`, `path = "src/main.rs"`, crates/df-app/Cargo.toml:9-11): the winit 0.30.13 event loop, egui 0.35 + egui-wgpu + wgpu 29 rendering, and every UI module, sitting on df-core. It is 74 `.rs` files under `src/` totalling 97,260 lines (68,069 outside `#[cfg(test)] mod` blocks, 29,191 inside them) plus the integration test `tests/portal.rs` (530 lines), with 917 `#[test]` functions. The crate contains **no** `#[cfg(target_os = …)]`, `#[cfg(unix)]`, `#[cfg(windows)]` or `#![windows_subsystem]` anywhere: every module in main.rs:10-65 is compiled on every target. Section 1 records 153 sites (table rows) in 27 source files, plus 5 rows for the two Cargo manifests, and summarises the four Linux-only modules — `wayland/` (1,838 lines), `dbus.rs` (2,976), `portal/` (1,921), `mounts.rs` (2,875) — by line count, public API and outside call sites instead of line by line. **egui-winit clipboard:** the workspace takes `egui-winit = "0.35.0"` with default features (Cargo.toml:62; crates/df-app/Cargo.toml:35). egui-winit 0.35.0's `default` is `["clipboard", "links", "wayland", "winit/default", "x11"]` and `clipboard = ["arboard", "bytemuck", "smithay-clipboard"]` (its Cargo.toml `[features]`), and Cargo.lock:963-977 lists `arboard` 3.6.1, `smithay-clipboard` 0.7.3 and `webbrowser` 1.2.4 as egui-winit dependencies, so **the `clipboard` feature is enabled in this build**. egui-winit's `Clipboard` compiles `arboard` on every target except android/ios, and `smithay-clipboard` only on linux/BSD (egui-winit src/clipboard.rs:8-24). It is constructed inside `egui_winit::State::new` at graphics.rs:172-179. df-app never calls egui's copy/paste API (no `copy_text`, `OutputCommand` or `Event::Paste` anywhere). egui-winit still reads the OS clipboard on its own paste chord (`modifiers.command && V`, egui-winit src/lib.rs:1007-1011, 1406-1410), and df-app forwards egui's platform output at app.rs:16208-16211. **wgpu backend selection:** `Gfx::new` (graphics.rs:77-198) builds the first instance with `backends: wgpu::Backends::VULKAN` over `InstanceDescriptor::new_without_display_handle()` and then `.with_env()` (graphics.rs:91-96). If that yields no adapter and `WGPU_BACKEND` was not set, it retries once with `new_without_display_handle_from_env()`, which means every compiled backend (graphics.rs:97-115). The present mode is `PresentMode::Mailbox` when `caps.present_modes` contains it, else `PresentMode::Fifo` (graphics.rs:148-152). The surface format prefers non-sRGB `Bgra8Unorm | Rgba8Unorm` (graphics.rs:161-168). wgpu 29 compiles its `vulkan` backend only on windows/linux/android/freebsd, and on Apple only with `vulkan-portability` (wgpu-29.0.4/build.rs:17-23; wgpu-core-29.0.4/build.rs:19-22). Nothing enables `vulkan-portability`. The compiled backend set comes from dv-playback's `wgpu = "29"` with default features (`dx12`, `metal`, `gles`, `vulkan`, …); egui-wgpu takes wgpu with `default-features = false, features = ["std", "wgsl"]`. Four comments carry Wayland-specific reasoning:
- **graphics.rs:47-54** says `Presented::Occluded` is "dead on this platform" because wgpu only returns `Occluded` from Metal and winit sends no `WindowEvent::Occluded` on Wayland. On macOS both halves exist: winit documents `Occluded` as unsupported only on Android/Wayland/Windows/Orbital (winit src/event.rs:421).
- **graphics.rs:78-90** justifies the Vulkan-only first instance by EGL/GL start-up cost measured on "Hyprland + RTX 2070".
- **graphics.rs:99-106** says the all-backend retry on Wayland fails with "gl not compatible with provided surface" because the instance has no display handle to give EGL.
- **graphics.rs:135-147** justifies Mailbox over Fifo because a Wayland Fifo acquire waits on the compositor's frame callback (Hyprland + NVIDIA, hypridle).

---

## 1. Sites by module

Classification column: **Linux-only** = needs a Linux-only facility (Wayland, D-Bus bus/udisks2/portal, gio/gvfs, wl-clipboard, `setsid`, `/usr/share/fonts`, `/run/user`, XDG layout); **Unix-only** = `std::os::unix`, `libc` Unix calls or POSIX process semantics (compiles and works on macOS, not on Windows); **Windows-differs** = compiles everywhere but behaves differently with Windows paths/env/shell; **macOS-differs** = behaves differently on macOS. `(compile)` marks a site that stops the crate compiling on the named platforms; `(runtime)` marks a behaviour difference only.

### Cargo manifests (not source, but compile-relevant)

| Line | What | Class | Used by | Note |
|---|---|---|---|---|
| Cargo.toml:37 | `wayland-client = { version = "0.31", features = ["system"] }` (workspace dep) | Linux-only (compile) | crates/df-app/Cargo.toml:49 `wayland-client.workspace = true` → `src/wayland/mod.rs` | Declared unconditionally in `[dependencies]`, not under a `[target.'cfg(...)']` table. winit 0.30.13 gates its own wayland deps to `cfg(all(unix, not(any(target_os = "redox", target_family = "wasm", target_os = "android", target_os = "ios", target_os = "macos"))))` (winit Cargo.toml target tables), so on macOS/Windows nothing unifies in `dlopen`. `wayland-sys` 0.31.11 build.rs probes pkg-config `wayland-client` unless feature `dlopen` is set. `wayland-backend` 0.3.17 source uses `std::os::unix` (e.g. src/rs/socket.rs, src/sys/client_impl/mod.rs). ✓ S1.20 |
| Cargo.toml:31 / crates/df-app/Cargo.toml:37-40 | `libc = "0.2"`; df-app comment: "Only for `localtime_r`" | Unix-only (compile, via use) | format.rs:199-222 | The `libc` crate builds on Windows; `libc::localtime_r` does not exist there (Windows CRT has `localtime_s`). |
| crates/df-app/Cargo.toml:3 | `description = "… file manager for Wayland."` | text | — | — |
| crates/df-app/Cargo.toml:63-66 | `pdfium-render = { version = "0.9.3", default-features = false, features = ["pdfium_7881", "thread_safe"] }` | — | preview/doc/pdf.rs | No `static`: library is dlopen'd at runtime (see preview/doc/pdf.rs). |
| Cargo.toml:62 | `egui-winit = "0.35.0"` default features | — | graphics.rs:172 | `clipboard`/`links`/`wayland`/`x11` on (see intro). ✓ S1.20 |

### src/app.rs (22,396 lines; non-test 1-17287)

| Line | What | Class | Used by (file:fn) | Note |
|---|---|---|---|---|
| 90-92 | `const APP_ID: &str = "delightfile"` — doc: "Wayland `app_id`. Matches the desktop file" | Linux-only (runtime meaning) | app.rs:App::init_gfx (2119) | Must equal build/delightfile.desktop basename and `StartupWMClass` (desktop file lines 7-9, 17). |
| 94-102 | `const PICKER_APP_ID: &str = "delightfile-picker"` — "The Wayland `app_id` (and X11 class) while the window is somebody else's file dialog" | Linux-only (runtime meaning) | App::init_gfx (2117) | Window-rule handle for picker windows. |
| 104-109 | `const PICKER_TITLE: &str = "file-picker"` — matches `config/hypr/lua/apps/file-picker.lua` | Linux-only (runtime meaning) | App::init_gfx (2116) | Hyprland rule. |
| 158-167 | `const OCCLUDED_PROBE: Duration = 2 s` — doc: "the event does not exist on Wayland (winit) and the occlusion answer does not exist there either (wgpu), so on the one platform this program targets the pair is dead code" | macOS-differs | App::redraw_inner (16234) | On macOS both `WindowEvent::Occluded` (winit) and `CurrentSurfaceTexture::Occluded` (wgpu Metal) exist, so this path becomes live. |
| 169-181 | `const CLIPBOARD_ANSWER: Duration = 10 s` | Linux-only | App::expire_clipboard (12854-12880) | Timeout for the Wayland thread's `Copied`/`Pasted` answer, then falls back to wl-copy/wl-paste. |
| 183-194 | `const WL_COPY_SETTLE: Duration = 150 ms` | Linux-only | App::settle_wl_copy (13171-13220), App::next_deadline (16104-16108) | wl-copy fallback settle window. |
| 979-982 | `fn home() -> Option<PathBuf> { std::env::var_os("HOME").map(PathBuf::from) }` | Windows-differs (runtime) | App::palette_rows (6176, 6183, 6205), App::go_to_path (7945), app/places.rs (88, 352, 358, 365, 414, 429, 444, 478, 495, 502, 507, 515, 563), app/syncing.rs:99 | Only `HOME` is read; used for `~` shortening/expansion and the Places home row. |
| 1510-1513 | field `data_device: Option<crate::wayland::DataDevice>` | Linux-only (compile, via module) | set App::init_gfx (2188), dropped App::finish (16365) | — ✓ S1.22 |
| 1782-1800 | `struct WlCopy { child: std::process::Child, message: Option<String>, started: Instant }` | Linux-only (runtime) | App::copy_via_wl_copy, App::settle_wl_copy, App::retire_wl_copy | Holds the running `wl-copy --foreground`. |
| 2111-2112 | `fn init_gfx` … `use winit::platform::wayland::WindowAttributesExtWayland;` | Linux-only (compile) | ApplicationHandler::resumed (17057-17064) | winit's `platform::wayland` module is `#[cfg(any(wayland_platform, docsrs))]` (winit src/platform/mod.rs:15-16). ✓ S1.27 |
| 2114-2124 | `Window::default_attributes().with_title(title).with_inner_size(LogicalSize::new(1400.0, 900.0)).with_name(app_id, app_id)` | Linux-only (compile) | App::init_gfx | `with_name` is the `WindowAttributesExtWayland` method (winit src/platform/wayland.rs:108; X11 has its own at x11.rs:159). These three are the only window attributes set: no icon, no decorations/transparency/theme settings. ✓ S1.27 |
| 2133 | `self.nerd = crate::icons::install(&gfx.egui_ctx)` | Linux-only (font dirs) | App::init_gfx | See icons.rs. ✓ S1.29 |
| 2182-2193 | `self.data_device = Self::start_data_device(event_loop, &gfx.window, self.waker.named("wayland"))`; `log::info!("no wayland data device — drag out and drop in are off")` | Linux-only | App::init_gfx | — ✓ S1.22 |
| 2198-2229 | `fn start_data_device(event_loop: &ActiveEventLoop, window: &Window, waker: Waker) -> Option<crate::wayland::DataDevice>`: `let RawDisplayHandle::Wayland(display) = event_loop.display_handle().ok()?.as_raw() else { return None }` (2211), same for `RawWindowHandle::Wayland(surface)` (2214), then `#[allow(unsafe_code)] unsafe { crate::wayland::DataDevice::start(display.display, surface.surface, waker) }` (2224-2227) | Linux-only (compile via `crate::wayland`; runtime `None` on non-Wayland handles) | App::init_gfx (2189) | The only `unsafe` call site into `crate::wayland` outside that module (comment 2220-2222). ✓ S1.22 |
| 4055-4078 | `fn show_trash`: `df_core::ops::Trash::home()` (4057), `trash.list()` | df-core seam (freedesktop trash) | App::open_trash (4044), App::refresh_trash (4081) | See trashview.rs; df-core owns the trash layout. |
| 4105-4140 | `fn trash_restore` → `crate::trashview::restore_refusal` (4115), `df_core::ops::trash::restore(item, &ctx)` (4119) | df-core seam | key `Enter`/`r` in trash view | — |
| 4142-4190 | `fn trash_purge` → FnJob `df_core::ops::purge(item, ctx)` (4158) | df-core seam | `D`, "Empty trash" (4410-4472) | — |
| 5107-5134 | `fn launch(&mut self, choice: &open::Choice, paths: Vec<PathBuf>, now: Instant)` → `open::spawn_detached(&choice.command, &paths, &cwd)` (5131) | Linux-only (runtime: opener strings) + Unix-only (via open.rs) | `o`/`O`/menu Open-with | Opener strings are df-core `DEFAULT_OPENERS` (crates/df-core/src/config.rs:192-275): `setsid uwsm-app -- …`, `"${TERMINAL:-ghostty}"`, `zeditor`, `google-chrome-stable`, `delightviewer`, `pinta`, `xdg-open "$1"`, `mpv`, `system-cmd-*`, `$(dirname "$1")`, `>/dev/null 2>&1`. |
| 5136-5184 | `fn run_shell(&mut self, snippet: &str, paths: Vec<PathBuf>, block: bool, now: Instant)`: non-block → `open::spawn_detached` (5145); block → FnJob (Lane::Micro) → `open::run_blocking(&snippet, &paths, &cwd)` (5162) | Unix-only (compile via open.rs:131) | `;`, `:`, `block = true` openers | — |
| 5528-5561 | `fn route_keys`: `press.text` used only when `press.chord` is `None` (multi-char commits 5536-5549, single char fallback 5553-5559) | macOS-differs | App::frame (13696) | See §3. |
| 6278-6282 | `fn udisks(&mut self) -> &crate::mounts::Mounts` → `Mounts::start(Arc::new(move \|\| waker.wake()))` | Linux-only | open_mounts, mount_device, unmount_selected, eject_selected, refresh_mounts, poll_mounts | See mounts.rs block. ✓ S1.24 |
| 6283-6475 | `open_mounts` (6284), `mount_action` (6301), `mount_device` (6348), `mount_selected` (6362), `unmount_selected` (6396), `eject_selected` (6431), `refresh_mounts` (6467) → `Request::{List, Mount, Unmount, UnmountShare, Eject}` | Linux-only | `M` card keys `Enter`/`m`/`u`/`e`/`r` | — ✓ S1.24 |
| 6485-6545 | `fn connect(&mut self, url: String, now: Instant)`: FnJob → `crate::mounts::connect(&job_url)` (6498) | Linux-only | `PromptKind::Connect` submit (7773 via `mounts::connect_url`) | gio mount on the task engine. ✓ S1.24 |
| 6547-6589 | `fn connected(…, result: crate::mounts::Connected, …)`: `NeedsTerminal` → `open::spawn_detached(crate::mounts::TERMINAL_MOUNT, &[PathBuf::from(url)], &cwd)` (6575-6579) | Linux-only | App::poll_connects (6540) | — ✓ S1.24 |
| 6603-6655 | `fn poll_mounts` → `Reply::{Listing, Mounted, Unmounted, Ejected, Failed}` | Linux-only | frame | — ✓ S1.24 |
| 7201-7255 | `fn set_mode(&mut self, mode: u32, now: Instant)`: `use std::os::unix::fs::PermissionsExt;` (7212); `std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode))` (7243); comment 7226 "there is no `lchmod` on Linux" | Unix-only (compile) | App::spot_action (7185, `spot::Action::SetMode`) | Spot panel permission chips. |
| 12078-12090 | `fn open_window(&mut self, dir: &Path, now: Instant) -> bool` → `self.windows.open(dir)` | portable (spawn); see window.rs | `C::NewWindow` in App::run (8900-8902), App::release_tab_drag (12404) | Second window = second process. |
| 12428-12461 | `fn tick_drag`: `dnd::verb_for(pointer.toggle, pointer.alt)` (12442); `if !window.contains(at) && pointer.down { self.hand_off_drag(now) }` (12458-12461) | Linux-only (drag-out) | App::frame | winit has no drag-source API on macOS/Windows. |
| 12680-12722 | `fn hand_off_drag`: `device.drag(dnd::offer(&paths), count, rgba(surface1), rgba(text), scale)` (12708-12716); on `false`/no device, `spring_home` (12717-12721) | Linux-only | App::tick_drag | — |
| 12724-12806 | `fn poll_data_device`: matches `crate::wayland::Event::{Enter, Motion, Leave, Drop, DragEnded, Selection, Copied, Pasted}` (12732-12795) | Linux-only | App::frame (14674) | Only producer of external drops, selection mirror, paste answers. ✓ S1.22 |
| 12815-12846 | `fn clipboard_thread_gone`: re-sends stranded copies via `copy_via_wl_copy`, pastes via `paste_via_wl_paste`; notice "press Ctrl+v again" (12845) | Linux-only | poll_data_device (12809) | — |
| 12849-12880 | `fn expire_clipboard`: after `CLIPBOARD_ANSWER`, `clipboard::text_offer` + `clipboard::paste` (12893-12894) / wl-copy | Linux-only | poll_data_device (12812) | — |
| 12906-12924 | `fn paste_via_wl_paste` → `crate::clipboard::paste(&mime)` (12908) | Linux-only | clipboard_thread_gone, expire_clipboard | Blocking `wl-paste` on the UI thread. |
| 12926-12953 | `fn take_external_drop(&mut self, paths: Vec<PathBuf>, ours: bool, at: Option<egui::Pos2>, now: Instant)` → `Clipboard::yank(paths)` → `self.paste_into(&clip, dest, false, now)` | consumer of Linux-only events | poll_data_device (12746) | Drop-in always copies. |
| 12978-13015 | `fn yank_to_system` (`Y`): `clipboard::branch_for` → `offer(Some(mime), …)` / `offer(None, …)` / `offer(Some("text/uri-list"), clipboard::uri_list(&paths), …)` (13008-13013) | Linux-only (via offer) | key `Y` | — |
| 13048-13075 | `fn copy_piece`: `Piece::Dirname` → `path.parent().unwrap_or(Path::new("/"))` (13061-13065) | Windows-differs | `c c`/`c d`/`c f`/`c n` | — |
| 13097-13128 | `fn offer(&mut self, mime: Option<&str>, bytes: &[u8], message: String, now: Instant)`: `device.ready() && device.set_selection(crate::clipboard::offer_mimes(mime), bytes.to_vec())` (13110-13113) else `copy_via_wl_copy` (13127) | Linux-only | yank_to_system, copy_file_text, copy_piece, copy contents | — |
| 13129-13165 | `fn copy_via_wl_copy` → `retire_wl_copy()` (13147), `crate::clipboard::copy(mime, bytes)` (13148) | Linux-only | offer, copy_answered, clipboard_thread_gone, expire_clipboard | — |
| 13166-13220 | `fn settle_wl_copy`: `state.child.try_wait()` (13175) | Linux-only | frame | — |
| 13222-13235 | `fn retire_wl_copy` → `crate::clipboard::reap(&mut state.child)` (13233) | Linux-only | copy_via_wl_copy, exiting (17238) | kill + wait. |
| 13237-13265 | `fn copy_answered(&mut self, ok: bool, now: Instant)` | Linux-only | poll_data_device (12759) | Falls back to wl-copy on refusal. |
| 13315-13400 | `fn paste_system(&mut self, force: bool, now: Instant)`: native `device.receive(seq, mime.clone())` (13350-13353); else `crate::clipboard::offered_types()` (13361) and `crate::clipboard::paste(&mime)` (13382), both synchronous | Linux-only | `p` with nothing yanked | — |
| 13402-13455 | `fn paste_into_prompt(&mut self, now: Instant)`: same shape (13423-13426, 13439, 13450) | Linux-only | `ctrl+v` in `[input]` (prompt_key) | — |
| 13560-13600 | `fn paste_clipboard_files(…)` → `crate::clipboard::parse_uri_list(&text)` (13562) | Windows-differs (via clipboard.rs) | App::take_pasted (13549) | — |
| 13600-13645 | `fn save_clipboard(&mut self, bytes: Vec<u8>, extension: &str, cwd: PathBuf, now: Instant)`: name `clipboard_{}.{extension}` with `crate::format::file_stamp(SystemTime::now())` (13614-13618) | Unix-only (via format.rs `localtime_r`) | App::take_pasted | — |
| 13785-13787 | pointer modifiers: `shift: i.modifiers.shift, toggle: i.modifiers.command \|\| i.modifiers.ctrl, alt: i.modifiers.alt` | macOS-differs | App::frame → click (10698), bulk_press (11588, 11601), tick_drag (12442), preview_gesture (5359), rubber band (14576) | egui `command` = Cmd on macOS. See §3. |
| 14674-14705 | `self.poll_data_device(now)`; incoming Wayland drag position feeds drop-target highlight (14684-14703) | Linux-only | App::frame | — |
| 15782-15788 | external-drop ring: `paint.drop_window(area)` when `self.incoming` is `Some` and `!incoming.ours` | Linux-only | App::frame | — |
| 16208-16211 | `gfx.egui_state.handle_platform_output(&gfx.window, platform_output)` | egui-winit (arboard/smithay-clipboard, webbrowser) | App::redraw_inner | — ✓ S1.20 |
| 16217-16236 | `Presented::Occluded` arm: "But not on Wayland, where neither this nor that event exists"; `repaint_at = now + OCCLUDED_PROBE` | macOS-differs | App::redraw_inner | — |
| 16345-16367 | `fn finish`: `crate::cli::write_chooser_file(&chooser.out, &self.chosen)` (16348); `crate::cli::write_cwd_file(path, &cwd)` (16359); `self.data_device = None` (16365) "before the `wl_surface`" | Linux-only (data device) / see cli.rs | CloseRequested (17093-17096), RedrawRequested quit (17142-17146) | — |
| 16727-16750 | `fn start_directory(requested: Option<&Path>) -> (PathBuf, Option<String>)`: fallback `std::env::current_dir().unwrap_or_else(\|_\| PathBuf::from("/"))` (16732) | Windows-differs | App::assemble (1928) | — |
| 16752-16770 | `fn save_target(dir: &Path, text: &str) -> Result<PathBuf, String>`: refuses only `name.contains('/')` (16767-16769) | Windows-differs | App::save_as (5005) | `\`, `:`, reserved names are not checked. |
| 16901-16912 | `fn nearest_existing(path: &Path) -> PathBuf` — fallback `PathBuf::from("/")` (16911) | Windows-differs | App::poll_workers (2388) | — |
| 16914-16950 | `fn typed_path(text: &str, cwd: &Path, home: Option<&Path>) -> PathBuf`: expands `~` and `~/…` only (`rest.starts_with('/')`, 16927-16932) | Windows-differs | App::go_to_path (7945) | — |
| 17066-17162 | `fn window_event`: arms `CloseRequested`, `Resized`, `Moved \| ScaleFactorChanged`, `Occluded(false)` (17115), `Focused`, `ModifiersChanged` (17127), `KeyboardInput` (17128-17140), `RedrawRequested`; no arm for `DroppedFile`, `HoveredFile`, `HoveredFileCancelled` or `Ime` | macOS/Windows: drop-in events unused | winit | winit `DroppedFile(PathBuf)`/`HoveredFile(PathBuf)` carry no position (winit src/event.rs:176-192); implementations exist in winit platform_impl for windows (drop_handler.rs), macos (window_delegate.rs), x11. `set_ime_allowed` is never called. ✓ S1.32 |
| 17108-17118 | comment on `Occluded(false)`: "**Never delivered on Wayland**" | macOS-differs | window_event | — |
| 17209-17262 | `fn exiting`: doc "the only point at which the Wayland connection is still alive. egui-winit's clipboard worker must be joined here"; `self.retire_wl_copy()` (17238) | Linux-only | winit | — |

### src/app/places.rs (1,287 lines; non-test 1-586)

| Line | What | Class | Used by (file:fn) | Note |
|---|---|---|---|---|
| 118-120 | `fn shown(written: &str, home: Option<&Path>) -> String` → `finder::shorten_home(Path::new(&expand_home(written)), home)` | Windows-differs | `said` (188) | `expand_home` is df-core (config.rs:767). |
| 138-183 | `fn brief(text: &str, max: usize) -> String`: finds `"~/"` or `"://"` head, splits the rest on `'/'` (148, 154) and rejoins with `/` (167, 182) | Windows-differs (display) | `said` (188), `card_places` (323), 568, 838 | — |
| 190-196 | `pub(super) fn written(dir: &Path, home: Option<&Path>) -> String` — "How a folder is written into the state file when it is pinned: under `~`" → `finder::shorten_home` | Windows-differs | App pin/unpin (358, 478, 495, 502) | Pins persisted as `~/…` strings. |

### src/app/syncing.rs (1,110 lines; non-test 1-478)

| Line | What | Class | Used by (file:fn) | Note |
|---|---|---|---|---|
| 99 | `crate::finder::shorten_home(&dest, home().as_deref())` | Windows-differs (display) | App::paste_sync | — |
| 123-124 | `if !df_core::sync::rsync::available() { return Err("Sync to a server needs rsync") }` | df-core seam (rsync/ssh) | App::paste_sync remote branch | rsync/ssh spawning is df-core sync/rsync.rs. |
| 411-424 | `fn server_path(path: &str, root: &str) -> PathBuf`: `raw == "/"`, `raw.starts_with('/')`, `format!("{}/{raw}", root.trim_end_matches('/'))` | Windows-differs (server POSIX path held in `PathBuf`) | `remote_sync` (436) | Handed to rsync as an argument. |

### src/appearance.rs (added after this inventory: 1,057 lines at 7ad55ad; row added 2026-09-29 by the df-app agent)

| Line | What | Class | Used by (file:fn) | Note |
|---|---|---|---|---|
| 68-71, 135-140, 224-605 | `crate::dbus::{Bus, Hangup, Message, …}`; `Connect = Arc<dyn Fn() -> Result<Bus, String>>`, `session()` = `Bus::session`; `Desktop::watch_over` starts the `df-appearance` thread that asks `org.freedesktop.portal.Settings` `ReadOne`/`Read` for `org.freedesktop.appearance` `color-scheme` on the session bus and listens for `SettingChanged` and `NameOwnerChanged`; `fake_bus` (test) is a `UnixStream::pair` | Linux-only (compile, via `dbus`) | App::new (`desktop_bus = Some(session())`), App::follow_desktop, App::poll_workers (`drain`), App::init_gfx (`wait_first`), app/tests/appearance.rs | `Scheme`, `Link` and `RETRY` are portable; so is `Scheme::appearance`. ✓ S1.35 |

### src/archive.rs (1,067 lines; non-test 1-661)

| Line | What | Class | Used by (file:fn) | Note |
|---|---|---|---|---|
| 98-104 | `Browse::inner`: `display.strip_prefix(&self.path)` then `rest.to_string_lossy().replace('\\', "/")` | already separator-agnostic | tab.rs:Tab::show_archive (636) and callers | Archive-inner paths are always `/`; `display_path` (86-92) is `self.path.join(inner)`. |
| 106-113 | `Browse::real`: `self.path.parent()` else `PathBuf::from("/")` (112) | Windows-differs | Tab::show_archive (643), App::local_origin | — |

### src/bulk.rs (2,400 lines; non-test 1-1401)

| Line | What | Class | Used by (file:fn) | Note |
|---|---|---|---|---|
| 126-130 | `pub const NAME_MAX: usize = 255;` — "Linux's own `NAME_MAX`", in bytes | Windows-differs | `problems` (1319) | — |
| 1306-1331 | `pub fn problems(names: &[&str], others: &HashSet<String>) -> Vec<Option<Problem>>`: `Unusable` only for `""`, `"."`, `".."`, or `name.contains('/')` (1316) | Windows-differs | bulk rename validation | `\ : * ? " < > \|` and reserved device names are not checked. |
| 775, 781, 838, 842, 851, 859 | hard-coded `Ctrl` chords in the bulk editor | macOS-differs | see §3 | — |

### src/chrome.rs (4,992 lines; non-test 1-3663)

| Line | What | Class | Used by (file:fn) | Note |
|---|---|---|---|---|
| 853-888 | `pub fn crumbs(path: &std::path::Path) -> Vec<Crumb>`: `Component::RootDir` → crumb labelled `"/"` with `here.push("/")` (865-871); `Component::Normal` → one crumb each; every other component (`Prefix`, `CurDir`, `ParentDir`) is pushed onto the accumulator with no crumb (882-885) | Windows-differs | App::sync_path_bar (10398) | A drive `Prefix` gets no chip; the first chip reads `/`. |

### src/cli.rs (802 lines; non-test 1-478)

| Line | What | Class | Used by (file:fn) | Note |
|---|---|---|---|---|
| 158-189 | `pub const USAGE`: "delightfile — a keyboard-first file manager for Wayland" (161); `--chooser-file` "(`Ctrl+Enter`)" (170); `--portal` "serve the xdg-desktop-portal file chooser on the session bus; D-Bus starts this, not a person" (184-185) | text (Linux) | `-h`/`--help` | — ✓ S1.31 |
| 221, 246-252 | `"--portal" => portal = true`; `Outcome::Portal` only when it is the sole argument | Linux-only (feature) | main.rs:91 | — ✓ S1.31 |
| 214-217 | `-V`/`--version` → `format!("delightfile {}\n", env!("CARGO_PKG_VERSION"))` | portable | main | — |
| 449-459 | `pub fn write_cwd_file(path: &Path, cwd: &Path)`: `std::fs::write(path, cwd.as_os_str().as_encoded_bytes())` | Windows-differs (bytes are WTF-8 on Windows; no newline) | App::finish (16359) | Consumer is the `Super+F` shell function / yazi-style wrapper. |
| 461-478 | `pub fn write_chooser_file(path: &Path, paths: &[PathBuf])`: each `as_encoded_bytes()` + `b'\n'` | Windows-differs | App::finish (16348) | Consumer: termfilechooser wrapper / `portal::request::read_picked`. |
| 44, 226, 362-372, 541 (at 7ad55ad; row added 2026-09-29 by the df-app agent) | `use std::os::unix::ffi::OsStrExt`; `parse` tests `arg.as_bytes().starts_with(b"-")`; `flag` strips `--name=` off `arg.as_bytes()` and rebuilds the value with `OsStr::from_bytes`; the test `a_path_that_is_not_utf8_is_kept_byte_for_byte` uses `OsStringExt::from_vec` | Unix-only (compile; Windows) | main (`cli::parse(std::env::args_os().skip(1))`) | Arrived with 7ad55ad. Second pass: S1.36 (through `df_core::platform::os`). |

### src/clipboard.rs (693 lines; non-test 1-501)

| Line | What | Class | Used by (file:fn) | Note |
|---|---|---|---|---|
| 42 | `use std::process::{Child, Command, Stdio};` | — | copy/offered_types/paste | — |
| 208-224 | `fn unreserved(byte)`, `pub fn file_uri(path: &Path) -> String`: `use std::os::unix::ffi::OsStrExt;` (215); percent-encodes every byte of `path.as_os_str().as_bytes()` (217) except `[A-Za-z0-9-._~/]` | Unix-only (compile) | `uri_list` (270) ← dnd::offer (631), App::yank_to_system (13008) | Produces `file://` + the raw path bytes, percent-encoded; assumes the path starts with `/`. |
| 226-261 | `pub fn parse_file_uri(text: &str) -> Option<PathBuf>`: `use std::os::unix::ffi::OsStringExt;` (232); accepts `file:///…`, `file://localhost/…`; requires `/` after `file://` (245) and keeps it; `PathBuf::from(OsString::from_vec(out))` (260) | Unix-only (compile); Windows-differs (`file:///C:/x` → `/C:/x`) | `parse_uri_list` (281) ← dnd::paths_from (681, 683), App::paste_clipboard_files (13562) | — |
| 318-325 | `ClipError::Missing` text: "{tool} is not installed — install wl-clipboard" (322) | Linux-only (text) | App::clip_failed | — |
| 330-383 | `pub fn copy(mime: Option<&str>, bytes: &[u8]) -> Result<Child, ClipError>`: `Command::new("wl-copy")`, `--foreground`, optional `--type <mime>`, stdin piped, stdout/stderr null (340-353) | Linux-only | App::copy_via_wl_copy (13148) | — |
| 385-392 | `pub fn reap(child: &mut Child)`: `child.kill()` + `child.wait()` | portable | App::retire_wl_copy (13233) | — |
| 394-415 | `pub fn offered_types() -> Result<Vec<String>, ClipError>`: `wl-paste --list-types` `.output()` | Linux-only | App::paste_system (13361), App::paste_into_prompt (13439) | Blocking, on the UI thread. |
| 417-431 | `pub fn paste(mime: &str) -> Result<Vec<u8>, ClipError>`: `wl-paste --no-newline --type <mime>` `.output()` | Linux-only | App::paste_via_wl_paste (12908), App::expire_clipboard (12894), App::paste_system (13382), App::paste_into_prompt (13450) | Blocking, on the UI thread. |

### src/dbus.rs — Linux-only module (see block at end of this section) ✓ S1.21 (now `src/platform/linux/dbus.rs`)

### src/dnd.rs (1,140 lines; non-test 1-697)

| Line | What | Class | Used by (file:fn) | Note |
|---|---|---|---|---|
| 76-87 | `pub fn verb_for(ctrl: bool, alt: bool) -> Verb`: Ctrl → `Copy`, Alt → `Link`, none → `Move` | macOS-differs | App::tick_drag (12442), fed `pointer.toggle` (= Cmd \|\| Ctrl) | §3. |
| 589-613 | `pub const SELF_MIME: &str = "application/x-delightfile-drag"`; `pub fn self_mime() -> &'static str` = `"{SELF_MIME};pid={std::process::id()}"` | Linux-only (only read off `wl_data_offer` mime lists) | `offer` (644), `is_ours` (618) ← wayland/mod.rs:1242 | — |
| 623-646 | `pub fn offer(paths: &[PathBuf]) -> Vec<(String, Vec<u8>)>`: `text/uri-list` (`clipboard::uri_list`), `text/plain;charset=utf-8` and `text/plain` (paths `to_string_lossy()` joined by `\n`), then `self_mime()` with empty bytes | Unix-only (via clipboard::file_uri) | App::hand_off_drag (12709) | — |
| 648-668 | `pub fn wanted_mime(offered: &[String]) -> Option<String>`: `text/uri-list` > `text/x-moz-url` > first `text/plain*` | Linux-only consumer | wayland/mod.rs:1243 (Dispatch<WlDataDevice> Enter) | — |
| 670-696 | `pub fn paths_from(mime: &str, bytes: &[u8]) -> Vec<PathBuf>`: uri-list via `clipboard::parse_uri_list`; plain-text fallback keeps only lines with `line.starts_with('/')` (693) | Windows-differs | wayland/mod.rs:907 (State::take_drop) | — |

### src/finder.rs (701 lines; non-test 1-468)

| Line | What | Class | Used by (file:fn) | Note |
|---|---|---|---|---|
| 336-359 | `pub fn shorten_home(path: &Path, home: Option<&Path>) -> String`: `text.strip_prefix(home)`, then `Some("") => "~"`, `Some(rest) if rest.starts_with('/') => format!("~{rest}")` (356) | Windows-differs | finder::place_row (332), App::palette_rows (6183), places::{shown (119), written (195), Place::label (221)}, App::paste_sync (syncing.rs:99) | — |

### src/format.rs (331 lines; non-test 1-224)

| Line | What | Class | Used by (file:fn) | Note |
|---|---|---|---|---|
| 19-33 | `pub fn linemode_text(entry: &Entry, mode: LineMode) -> String`: `LineMode::Permissions => entry.permissions_string()` (23), `LineMode::Owner => entry.owner_label()` (30) | Windows-differs (df-core renders POSIX mode / uid:gid) | ui.rs:Painting::row (1223), dialog.rs:Facts::read (291), Facts::of (317) | — |
| 189-222 | `#[allow(unsafe_code)] fn civil_local(time: SystemTime) -> Option<(i32, u32, u32, u32, u32)>`: `let t = secs as libc::time_t;` (207), `let mut tm: libc::tm = unsafe { std::mem::zeroed() };` (208), `unsafe { !libc::localtime_r(&t, &mut tm).is_null() }` (212) | Unix-only (compile) | `local_stamp` (185) ← `time_text` (35) ← `linemode_text`; `long_stamp` (150) ← spot::rows (263, 265), archive::card_rows (446), remote::card_rows (581); `file_stamp` (165) ← App::save_clipboard (13617) | Doc 193-197 cites `/usr/share/zoneinfo`. |

### src/graphics.rs (394 lines; no tests)

| Line | What | Class | Used by (file:fn) | Note |
|---|---|---|---|---|
| 38-67 | `pub enum Presented { Shown, Retry, Occluded }`; `Occluded` doc "Dead on this platform, and kept anyway" | macOS-differs | App::redraw_inner (`match gfx.present` at 16218) | — ✓ S1.28 |
| 77-115 | `Gfx::new`: `Backends::from_env().is_some()` (91); `InstanceDescriptor { backends: wgpu::Backends::VULKAN, ..InstanceDescriptor::new_without_display_handle() }.with_env()` (92-96); retry `new_without_display_handle_from_env()` (109-112) unless pinned | macOS-differs (no Vulkan backend compiled → always retries); Windows-differs (Vulkan before DX12) | App::init_gfx (2130) | Comments 78-90, 99-106 are Wayland/Hyprland measurements. ✓ S1.28 |
| 134-157 | `surface_config.present_mode = if caps.present_modes.contains(&PresentMode::Mailbox) { Mailbox } else { Fifo }` | runtime-differs | Gfx::new | Comment 135-147 is Wayland frame-callback reasoning. ✓ S1.28 |
| 171-179 | `egui_winit::State::new(egui_ctx.clone(), egui::ViewportId::ROOT, &window, Some(window.scale_factor() as f32), None, Some(max_texture_dimension_2d))` | egui-winit clipboard created here | Gfx::new | — ✓ S1.20 |
| 262-284 | `Cst::Timeout` reconfigure; `Cst::Occluded` (278-284) not reconfigured | runtime-differs | Gfx::present | — |

### src/grid.rs (1,552 lines; non-test 1-1121)

| Line | What | Class | Used by (file:fn) | Note |
|---|---|---|---|---|
| 627 | `df_core::preview::cached_thumb(&want.path)` | Unix-only via df-core | `tile_image` (grid tile worker, thread spawned at 464) | df-core preview/cache.rs:128-131 `cache_dir()` = `std::env::temp_dir()/yazi-<uid>`, `uid()` = `libc::getuid()` (cache.rs:206-210); `cache_key` uses `std::os::unix::fs::MetadataExt` ctime (cache.rs:136-150). |

### src/icons.rs (948 lines; non-test 1-584)

| Line | What | Class | Used by (file:fn) | Note |
|---|---|---|---|---|
| 72-82 | `const FONT_DIRS: &[&str] = &["/usr/share/fonts/TTF", "/usr/share/fonts/truetype", "/usr/share/fonts/OTF", "/usr/share/fonts/nerd-fonts", "/usr/local/share/fonts"]` | Linux-only | `find_nerd_font` (101) | Doc 11-12: "the places a Nerd Font is installed on Arch". ✓ S1.29 |
| 84-93 | `const PREFERRED: &[&str] = &["JetBrainsMonoNerdFont-Regular", "FiraCodeNerdFont-Regular", "CaskaydiaMonoNerdFont-Regular", "HackNerdFont-Regular", "SymbolsNerdFont-Regular"]` | portable names | `find_nerd_font` | — |
| 95-141 | `fn find_nerd_font() -> Option<(PathBuf, Vec<u8>)>`: adds `$HOME/.local/share/fonts` and `$HOME/.fonts` (102-105); non-recursive `read_dir` per dir; file must end `.ttf`/`.otf`; rank by `PREFERRED`, else any stem containing `NerdFont` ending `-Regular` | Linux-only dirs; Windows-differs (`HOME`) | `install` (149) | The list contains no macOS or Windows font directory. ✓ S1.29 |
| 142-172 | `pub fn install(ctx: &egui::Context) -> bool`: none found → `log::info!("no Nerd Font found; row icons fall back to ls-style classifiers")`, `false` | — | App::init_gfx (2133) | — |

### src/keys.rs (203 lines; non-test 1-119)

| Line | What | Class | Used by (file:fn) | Note |
|---|---|---|---|---|
| 20-33 | `pub fn chord(event: &KeyEvent, mods: ModifiersState) -> Option<Chord>`: `use winit::platform::modifier_supplement::KeyEventExtModifierSupplement;` (26); `chord_from(&event.key_without_modifiers(), mods).or_else(\|\| chord_from(&event.logical_key, mods))` | macOS-differs | App::window_event (17131) | `modifier_supplement` is compiled on windows/macos/x11/wayland/orbital (winit src/platform/mod.rs:44-52). |
| 35-49 | `pub fn text(event: &KeyEvent) -> Option<String>`: `event.text`, rejected if empty or any `char::is_control` | macOS-differs | App::window_event (17132) | On macOS Option+letter composes into `text`. |
| 51-66 | `pub fn chord_from(key: &WinitKey, mods: ModifiersState) -> Option<Chord>`: `Mods { ctrl: mods.control_key(), alt: mods.alt_key(), shift: mods.shift_key() \|\| implied_shift, super_key: mods.super_key() }` | macOS-differs (Cmd → `super_key`) | `chord` | — |

### src/main.rs (119 lines; no tests)

| Line | What | Class | Used by (file:fn) | Note |
|---|---|---|---|---|
| 1-9 | crate doc "file manager for Wayland"; no crate-level attributes (no `#![windows_subsystem = "windows"]`) | Windows-differs | — | Without the attribute a Windows build is a console-subsystem binary and opens a console window. |
| 10-65 | unconditional `mod` list including `clipboard` (15), `dbus` (16), `mounts` (34), `portal` (41), `wayland` (63) | Linux-only modules compiled everywhere | — | — ✓ S1.21 |
| 83-85 | `env_logger::Builder::from_env(Env::default().default_filter_or("info")).format_timestamp_millis().init()` | portable (stderr) | main | — |
| 87 | `cli::parse(std::env::args().skip(1))` | all platforms | main | `std::env::args()` panics on an argument that is not valid Unicode. ✓ S1.31 |
| 88-92 | comment "building a loop would open a Wayland connection"; `cli::Outcome::Portal => std::process::exit(portal::run())` | Linux-only | main | — ✓ S1.31 |

### src/mounts.rs — Linux-only module (see block at end of this section) ✓ S1.21 (worker now `src/platform/linux/mounts.rs`)

### src/open.rs (615 lines; non-test 1-378)

| Line | What | Class | Used by (file:fn) | Note |
|---|---|---|---|---|
| 1-23 | module doc: snippets run as `$SHELL -c '<snippet>' delightfile <path> …`; "Detaching … is `setsid`: the opener commands … already say `setsid uwsm-app --`" | Unix/Linux (doc) | — | — |
| 40-47 | `pub fn shell_program() -> String`: `std::env::var("SHELL")`, non-blank, else `"/bin/sh"` | Unix-only (runtime) | spawn_detached (103), run_blocking (124) | — |
| 49-64 | `pub fn shell_argv(shell: &str, snippet: &str, paths: &[PathBuf]) -> Vec<String>`: `[shell, "-c", snippet, "delightfile", paths (to_string_lossy)…]` | Unix-only (runtime: POSIX `sh -c` + positional `$0/$1/$@`) | spawn_detached, run_blocking | Non-UTF-8 path bytes are replaced lossily. |
| 66-84 | `pub fn detached_argv(argv: Vec<String>) -> Vec<String>`: prefixes `["setsid", "--fork"]` when `which("setsid")` | Linux-only (util-linux) | spawn_detached (104) | — |
| 86-92 | `fn which(name: &str) -> Option<PathBuf>`: `split_paths($PATH)` → `dir.join(name).is_file()` | Windows-differs | detached_argv | No `PATHEXT`/`.exe` handling. |
| 94-99 | `fn command_from(argv: &[String], cwd: &Path) -> Option<Command>` | — | spawn_detached, run_blocking | — |
| 101-119 | `pub fn spawn_detached(snippet: &str, paths: &[PathBuf], cwd: &Path) -> std::io::Result<()>`: stdio null; `command.spawn()` (112); `child.wait()` only when `setsid` prefixed (113-117) | Linux-only (detach) | App::launch (5131), App::run_shell (5145), App::connected (6575) | — |
| 121-134 | `pub fn run_blocking(snippet: &str, paths: &[PathBuf], cwd: &Path) -> std::io::Result<i32>`: `command.status()` (127); `use std::os::unix::process::ExitStatusExt;` (131); `128 + status.signal().unwrap_or(0)` (132) | Unix-only (compile) | App::run_shell FnJob (5162) | — |

### src/overlay.rs (1,057 lines; non-test 1-827)

| Line | What | Class | Used by (file:fn) | Note |
|---|---|---|---|---|
| 549 | `"Type to search. Esc to close, Ctrl+s to stop."` | text | paint_search | §3. |
| 727-760 | `fn path_text(painter, pos, path: &str, bright, dim, max_width) -> f32`: `path.rfind('/')` splits dim directory / bright name (741) | Windows-differs (display) | overlay::paint_search (618, 638) with `hit.relative` from search.rs | — |

### src/portal/mod.rs, src/portal/request.rs — Linux-only module (see block at end of this section) ✓ S1.21 (now `src/platform/linux/portal/`, with `show.rs` since 2ec71f3)

### src/preview/decode.rs (1,570 lines; non-test 1-1098)

| Line | What | Class | Used by (file:fn) | Note |
|---|---|---|---|---|
| 86 | `use df_core::preview::{store_thumb, PreviewKind, PreviewToken, STILL_SKIP};` | — | — | — |
| 1047-1098 | `fn write_thumb(path: &Path, width: u32, height: u32, pixels: &[u8])` (1054): `store_thumb(path, STILL_SKIP)` (1055) → JPEG → temp `target.with_extension(format!("df{}.tmp", std::process::id()))` (1088) → `std::fs::rename(&temp, &target)` (1093) | Unix-only via df-core | decode worker (call at 421) | df-core `store_thumb` (preview/cache.rs:195-204) creates `temp_dir()/yazi-<getuid()>`; shared with yazi/delightviewer. |

### src/preview/doc/pdf.rs (225 lines; non-test 1-185)

| Line | What | Class | Used by (file:fn) | Note |
|---|---|---|---|---|
| 3-19 | doc: search order `$DF_PDFIUM_LIB`, `~/.local/lib/delightfile/libpdfium.so`, `~/.local/lib/delightviewer/libpdfium.so`, `$CARGO_MANIFEST_DIR/../../target/libpdfium.so`, system loader | — | — | — ✓ S1.30 |
| 41-61 | `fn candidates() -> Vec<PathBuf>`: `var_os("DF_PDFIUM_LIB")` (44-45); `var_os("HOME")` + `.local/lib/delightfile/libpdfium.so` (47-49) and `.local/lib/delightviewer/libpdfium.so` (53); `Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target").join("libpdfium.so")` (55-59) | Linux-only file name (`.so`); Windows-differs (`HOME`) | `pdfium()` (74) | The three fixed candidates hard-code `libpdfium.so`. ✓ S1.30 |
| 63-100 | `pub fn pdfium() -> Option<&'static Pdfium>`: `Pdfium::bind_to_library(&path)` per candidate (76), then `Pdfium::bind_to_system_library()` (86) | portable at the system step | `available()` (105), `Doc::open` (121) ← preview/doc/mod.rs:load (487, 490) | pdfium-render's system name is `libloading::library_filename("pdfium")` (pdfium-render-0.9.3/src/pdfium.rs:168-170): `libpdfium.so` / `libpdfium.dylib` / `pdfium.dll`. |
| 184 | `const MISSING: &str = "PDF pages need libpdfium — see the README";` | text | Doc::open | Logged, not shown. |

### src/preview/listing.rs (1,019 lines; non-test 1-613)

| Line | What | Class | Used by (file:fn) | Note |
|---|---|---|---|---|
| 232-239 | `fn seven_zip() -> Option<PathBuf>`: `df_core::archive::external_extractor()` filtered to `ExtractorKind::SevenZip` | df-core seam (PATH lookup) | `list` (182) ← preview/body.rs:289 | — |
| 316-362 | `fn tar_through_7z(program: &Path, path: &Path, stop) -> Option<ArchiveTree>`: `Command::new(program).args(["x", "-so", "--"]).arg(path)`, stdin null, stdout piped, stderr null (324-330) | external binary | `list_with` (188) | — |
| 364-450 | `fn list_with_7z(program, path, format, stop) -> Result<Listing, String>`: `Command::new(program).args(["l", "-ba", "-slt", "--"]).arg(path)` (379-385); doc "stdin is `/dev/null`" (367) = `Stdio::null()` | external binary | `list_with` (188) | — |
| 604 | `raw.ends_with('/') \|\| raw.ends_with('\\')` | already separator-agnostic | `row_from_slt` | — |

### src/remote.rs (1,184 lines; non-test 1-749)

| Line | What | Class | Used by (file:fn) | Note |
|---|---|---|---|---|
| 107-122 | `pub fn display(at: &VfsPath) -> PathBuf { PathBuf::from(at.to_url()) }`; `pub fn at_of(display: &Path) -> Option<VfsPath> { VfsPath::parse(&display.to_string_lossy()) }` | Windows-differs | throughout app.rs; e.g. `scannable` (16555) → `spawnable_cwd`/`child_cwd`, App::jump_to (6785), App::remote_at | `sftp://host/path` URLs are carried as `PathBuf`s; any `Path` method on them follows the platform's separator rules (`\` and `/` on Windows). |
| 290-297 | `pub fn is_remote(path: &Path) -> bool` | same | App guards, e.g. App::jump_to (6785) | — |
| 428-535 | `pub struct Temps`: ledger of files from df-core `Vfs::download_to_temp`, documented as `$TMPDIR/delightfile-vfs-<pid>/` (433-435); `clear` removes files then `remove_dir` (492-521) | df-core seam | App::exiting (17225-17231) | Directory is chosen by df-core. |

### src/search.rs (1,065 lines; non-test 1-757)

| Line | What | Class | Used by (file:fn) | Note |
|---|---|---|---|---|
| 138-145 | `pub fn binary(self) -> &'static str`: `Mode::Names => "fd"`, `Mode::Content => "rg"` | external binaries | `Search::spawn` (480), `start_failure` (490) | One fixed name per mode; no alternate name is tried. |
| 196-212 | `impl Drop for Running`: `child.kill()` + `child.wait()` | portable | respawn/close/cancel | — |
| 466-523 | `fn spawn(&mut self, query: &str)`: `build(...)`, `.current_dir(&self.root)`, stdout piped, stderr/stdin null, `process.spawn()` (487); reader thread `"df-search"` (508-510) | external binaries | `Search::tick` (377) | — |
| 525-565 | `fn build(mode: Mode, query: &str, hidden: bool) -> Process`: fd `--color=never [--hidden] -- <query>`; rg `--color=never --smart-case --line-number --column --no-heading --null --max-columns 200 [--hidden] -- <query>` | external binaries | `spawn` | — |
| 686-731 | `pub(crate) fn parse(mode: Mode, root: &Path, query: &str, line: &str) -> Option<Hit>`: Names → `line.trim_end_matches('/')` (697), `root.join(&relative)`; Content → `line.split_once('\0')` (713), `root.join(relative)` | Windows-differs | `read` (645) | Only a trailing `/` is trimmed. |

### src/spot.rs (1,519 lines; non-test 1-1112)

| Line | What | Class | Used by (file:fn) | Note |
|---|---|---|---|---|
| 116-130 | `pub const BITS: [(u32, char, &str); 9]` (120): `(0o400, 'r', "owner may read")` … `(0o001, 'x', "everyone may execute")` | Windows-differs (POSIX mode) | chip painting (940), `toggle` (379-383), `rwx` (1001-1005) | — |
| 150-200 | `pub struct Facts { …, pub mode: u32, pub uid: u32, pub gid: u32, … }` (157-159) filled from `entry.mode/uid/gid` (192-194) | Windows-differs | Spot panel | — |
| 239-249 | `rows`: "Where" row = `facts.path.parent()` else `"/"` (248) | Windows-differs | Spot panel | — |
| 268-275 | "Owner" row: `df_core::fs::owner::owner_label(facts.uid, facts.gid)` + `uid:gid` | Unix-only semantics via df-core | Spot panel | — |
| 369-383, 532 | `pub fn octal(mode: u32) -> String`, `pub fn toggle(mode: u32, index: usize) -> u32`; `Action::SetMode(toggle(self.facts.mode, self.bit))` (532) | Unix-only (applied by app.rs:7243 `from_mode`) | App::spot_action (7185) | — |

### src/tab.rs (1,559 lines; non-test 1-963)

| Line | What | Class | Used by (file:fn) | Note |
|---|---|---|---|---|
| 770-790 | `Tab::show_trash`: `Listing::new(crate::trashview::URL, mgr, sort, now)` (788) | Windows-differs (pseudo-URL `trash://` held as a `Path`) | App::show_trash (4074) | Compared elsewhere with `Path::new(crate::trashview::URL)`: App::navigate (app.rs:2486), App::jump_to (6785), `scannable` (16555). |

### src/trashview.rs (626 lines; non-test 1-388)

| Line | What | Class | Used by (file:fn) | Note |
|---|---|---|---|---|
| 72-78 | `pub const URL: &str = "trash://";` | Windows-differs | tab.rs:788, app.rs:2486 (App::navigate), 6785 (App::jump_to), 16555 (`scannable`) | `crumbs()` (117) is used at app.rs:10389. |
| 130-150 | `pub fn row(item: &TrashedItem) -> Entry`: `std::fs::symlink_metadata(&path)`; `std::fs::metadata` when the lstat is a symlink | runtime-differs (symlink semantics) | `rows` (127) ← tab.rs:Tab::show_trash (784) | — |
| 163-248 | `pub fn row_from(item: &TrashedItem, meta: Option<&Metadata>, target: Option<&Metadata>) -> Entry`: `use std::os::unix::fs::MetadataExt;` (176); `.map(\|m\| m.mode()).unwrap_or(if is_dir { 0o040_755 } else { 0o100_644 })` (212-214); `uid: facts.map(\|m\| m.uid())` (240), `gid: … m.gid()` (241) | Unix-only (compile) | `row` (149), tests | — |
| 276-293 | `fn shorten(path: &str) -> String`: `std::env::var_os("HOME")` (281); `rest.starts_with('/')` (290) | Windows-differs | `notes` (273) ← App::show_trash (4071) | — |
| 297-326 | `pub fn deleted_at(text: &str) -> Option<SystemTime>`: parses `YYYY-MM-DDThh:mm:ss` as UTC | portable | `row_from` (237) | Format is df-core's trashinfo `DeletionDate`. |
| 339-381 | `pub fn restore_refusal(item: &TrashedItem) -> Option<String>`: `symlink_metadata` on `item.files_path()`, `original`, `original.parent()` | portable API | App::trash_restore (4115) | — |

### src/wayland/mod.rs, src/wayland/icon.rs — Linux-only module (see block below) ✓ S1.21 (now `src/platform/linux/wayland/`; `icon.rs` is `src/platform/icon.rs`, S1.22)

### src/window.rs (319 lines; non-test 1-208)

| Line | What | Class | Used by (file:fn) | Note |
|---|---|---|---|---|
| 30-54 | module doc: a window is a process; cross-window drag works "because … a drag out of one window offers `text/uri-list` on a `wl_data_source`, and a drop into another takes `text/uri-list` off a `wl_data_offer`" | Linux-only (reasoning) | — | On macOS/Windows there is no drag source (see §2 dnd). |
| 101-109 | `pub fn spawn_args(dir: &Path) -> Vec<OsString>` = `["--", dir]` | portable | `Windows::open` (129) | Child gets no `--cwd-file`. |
| 122-141 | `pub fn open(&mut self, dir: &Path) -> std::io::Result<()>`: `Command::new(std::env::current_exe()?)` (127-128), `.current_dir(dir)` if `dir.is_dir()` (136-138), `.spawn()` (139); stdio inherited; no detach | portable (compile) | App::open_window (12079) | — |
| 143-152 | `fn reap(&mut self)`: `retain_mut` dropping children where `try_wait()` is `Ok(Some(_)) \| Err(_)` | portable | `open` (126) | — |


### Linux-only module blocks

#### `src/wayland/` — `mod.rs` 1,447 lines (non-test 1-1378, `mod tests` 1379-1447, 5 tests) + `icon.rs` 391 lines (non-test 1-267, `mod tests` 268-391, 6 tests) ✓ S1.21

What it rests on: the crate `wayland_client` (imports mod.rs:146-164) adopting winit's `wl_display` via `Backend::from_foreign_display` (mod.rs:335), `std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd}` (mod.rs:139), and `libc::pipe2` (437), `libc::write` (452), `libc::poll`/`pollfd` (459-472), `libc::read` (485), `libc::memfd_create` (847), all under `#![allow(unsafe_code)]` (mod.rs:133). It runs one thread named `df-wayland` (mod.rs:348-350) that binds its own `wl_seat`/`wl_pointer`/`wl_keyboard` for input serials, and has 5 s `SEND_TIMEOUT` / `RECEIVE_TIMEOUT` (176, 181). `icon.rs` is pure pixel code: premultiplied `Argb8888` into a `wl_shm` pool, with little-endian `B G R A` byte order hard-coded (icon.rs:18-21, 149). It uses no OS API itself but is only reachable through `mod icon;` (mod.rs:135).

Full public API:

| Line | Item (signature verbatim) | First doc line |
|---|---|---|
| mod.rs:166 | `pub use icon::Rgba;` | — ✓ S1.22 |
| mod.rs:185-227 | `pub enum Event { Enter { at: (f32, f32), ours: bool }, Motion { at: (f32, f32) }, Leave, Drop { paths: Vec<PathBuf>, ours: bool }, DragEnded, Selection { mimes: Vec<String> }, Copied { ok: bool }, Pasted { seq: u64, bytes: Result<Vec<u8>, PasteFailure> } }` | What the pointer's own drag machinery learns from the compositor. ✓ S1.22 |
| mod.rs:239-248 | `pub enum PasteFailure { Gone, Stalled { got: usize }, Broken { got: usize } }` (+ `impl Display`, 250-266) | Why a paste came back with nothing usable. ✓ S1.22 |
| mod.rs:297 | `pub struct DataDevice` (private fields `commands`, `events`, `bell: OwnedFd`, `ready: Arc<AtomicBool>`, `thread`) | The handle the app holds: a doorbell, two channels and a thread. |
| mod.rs:329 | `impl DataDevice` · `pub unsafe fn start(display: NonNull<c_void>, surface: NonNull<c_void>, waker: Waker) -> Option<DataDevice>` | Adopt winit's connection and start the thread. |
| mod.rs:363 | `impl DataDevice` · `pub fn ready(&self) -> bool` | Is there a data device to talk to? |
| mod.rs:374 | `impl DataDevice` · `#[must_use] pub fn set_selection(&self, mimes: Vec<String>, bytes: Vec<u8>) -> bool` | Take the clipboard, offering `bytes` under every name in `mimes`. |
| mod.rs:381 | `impl DataDevice` · `#[must_use] pub fn receive(&self, seq: u64, mime: String) -> bool` | Ask the clipboard for `mime`. |
| mod.rs:389 | `impl DataDevice` · `#[must_use] pub fn drag(&self, offers: Vec<(String, Vec<u8>)>, count: usize, card: Rgba, ink: Rgba, scale: i32) -> bool` | Start a drag out of the window, offering `offers` and carrying an icon drawn for `count` files. |
| mod.rs:407 | `impl DataDevice` · `pub fn poll(&self) -> Vec<Event>` | Everything that has happened since the last frame. |
| mod.rs:424 | `impl Drop for DataDevice` (sends `Command::Exit`, joins) | — |
| icon.rs:48 | `pub const HOTSPOT: (i32, i32) = (PAD as i32 + 16, PAD as i32 + CARD_H as i32 / 2);` | Where the pointer sits on the icon: the top card's grab point, matching [`crate::dnd::GHOST_GRAB`] … ✓ S1.22 |
| icon.rs:51 | `pub struct Icon { pub width: u32, pub height: u32, pub pixels: Vec<u8> }` | A drawn icon, ready to be copied into a shm pool. ✓ S1.22 |
| icon.rs:61 | `pub struct Rgba(pub u8, pub u8, pub u8, pub u8);` | One straight-alpha colour on the way in. ✓ S1.22 |
| icon.rs:72 | `pub fn draw(count: usize, card: Rgba, ink: Rgba) -> Icon` | Draw the stack for a drag of `count` items. ✓ S1.22 |

Call sites outside the module (the seam):

| File:line | fn | Use |
|---|---|---|
| app.rs:1513 | `App` field | `data_device: Option<crate::wayland::DataDevice>` ✓ S1.22 |
| app.rs:2188-2193 | `App::init_gfx` | `Self::start_data_device(event_loop, &gfx.window, self.waker.named("wayland"))` ✓ S1.22 |
| app.rs:2203-2229 | `App::start_data_device` | `RawDisplayHandle::Wayland` / `RawWindowHandle::Wayland` → `unsafe { crate::wayland::DataDevice::start(display.display, surface.surface, waker) }` (2225) ✓ S1.22 |
| app.rs:12700, 12708-12716 | `App::hand_off_drag` | `crate::wayland::Rgba(r, g, b, a)`; `device.drag(dnd::offer(&paths), count, …, scale)` ✓ S1.22 |
| app.rs:12726-12797 | `App::poll_data_device` | `device.ready()` (12729), `device.poll()` (12730), match on all eight `crate::wayland::Event` variants (12732-12795) ✓ S1.22 |
| app.rs:13110-13113 | `App::offer` | `device.ready() && device.set_selection(crate::clipboard::offer_mimes(mime), bytes.to_vec())` |
| app.rs:13329, 13350-13353 | `App::paste_system` | `d.ready()`, `device.receive(seq, mime.clone())` |
| app.rs:13411, 13423-13426 | `App::paste_into_prompt` | `d.ready()`, `device.receive(seq, mime.clone())` |
| app.rs:16365 | `App::finish` | `self.data_device = None` (drop joins the thread) |
| (inbound) wayland/mod.rs:907 | `State::take_drop` | calls `crate::dnd::paths_from(mime, &bytes)` |
| (inbound) wayland/mod.rs:1242-1243 | `Dispatch<WlDataDevice>::event` (`Enter`) | calls `crate::dnd::is_ours(&mimes)`, `crate::dnd::wanted_mime(&mimes)` |
| (inbound) wayland/mod.rs:168, 257, 262 | — | `crate::app::Waker`, `crate::format::human_size` |

#### `src/dbus.rs` — 2,976 lines (non-test 1-1808, except `#[cfg(test)]` items `Value::bytes` 261-265, `encode_message` 1135-1161, `push_field` 1445-1451, `push_signature_field` 1453-1459; tests `mod tests` 1809-2387, `mod hostile` 2388-2482, `mod generic` 2483-2976, 32 tests) ✓ S1.21

What it rests on: `std::os::unix::ffi::OsStringExt` (55), `std::os::unix::net::UnixStream` (56; fields at 460, 849, 902; `try_clone` 562). `SYSTEM_BUS = "/run/dbus/system_bus_socket"` (106), overridden by `DBUS_SYSTEM_BUS_ADDRESS` (485). `session_address()` (1044-1056) reads `DBUS_SESSION_BUS_ADDRESS`, then `DBUS_STARTER_ADDRESS`, then `$XDG_RUNTIME_DIR/bus`. SASL `AUTH EXTERNAL` uses `uid()`, which reads `/proc/self` via `std::os::unix::fs::MetadataExt::uid` (961-968, used at 587). `BusSocket::Abstract` connects with `std::os::linux::net::SocketAddrExt` + `std::os::unix::net::SocketAddr::from_abstract_name` (984-989); that one is Linux-only, not available on macOS. `bus_socket` builds `OsString::from_vec` paths (1006). The client is hand-rolled; there is no zbus.

Full public API (non-test):

| Line | Item (signature verbatim) | First doc line |
|---|---|---|
| 62 | `pub const MSG_METHOD_CALL: u8 = 1;` | — |
| 63 | `pub const MSG_METHOD_RETURN: u8 = 2;` | — |
| 64 | `pub const MSG_ERROR: u8 = 3;` | — |
| 69 | `pub const MSG_SIGNAL: u8 = 4;` | A broadcast. |
| 73 | `pub const FLAG_NO_REPLY_EXPECTED: u8 = 0x1;` | The header flag a caller sets when it will not read the answer. |
| 106 | `pub const SYSTEM_BUS: &str = "/run/dbus/system_bus_socket";` | Where the system bus lives when the environment does not say. |
| 114 | `pub const CALL_TIMEOUT: Duration = Duration::from_secs(90);` | How long a call may take before the client gives up. |
| 117 | `pub const MAX_MESSAGE: usize = 32 * 1024 * 1024;` | The largest message this client will accept. |
| 126 | `pub const MAX_NESTING: u32 = 32;` | The deepest a value may nest before it is refused. |
| 140 | `pub enum Value { Bool(bool), U8(u8), U16(u16), I16(i16), U32(u32), I32(i32), U64(u64), I64(i64), F64(f64), Str(String), Path(String), Signature(String), Array(Vec<Value>), Dict(Vec<(Value, Value)>), Struct(Vec<Value>), Variant(String, Box<Value>) }` | A value on the wire, whatever its type — read off it, or about to go on it. |
| 171 | `impl Value` · `pub fn variant(sig: &str, value: Value) -> Value` | `value` as a variant of type `sig`. |
| 177 | `impl Value` · `pub fn peeled(&self) -> &Value` | This value with any variant wrappers taken off: what a caller reading a property means by "the value". |
| 186 | `impl Value` · `pub fn into_peeled(self) -> Value` | [`Value::peeled`], by value. |
| 194 | `impl Value` · `pub fn as_str(&self) -> Option<&str>` | — |
| 201 | `impl Value` · `pub fn as_bool(&self) -> Option<bool>` | — |
| 210 | `impl Value` · `pub fn as_u64(&self) -> Option<u64>` | Any unsigned integer, widened. |
| 225 | `impl Value` · `pub fn as_bytes(&self) -> Option<Vec<u8>>` | A `ay`, exactly as sent — NULs and all. |
| 244 | `impl Value` · `pub fn as_bytestring(&self) -> Option<String>` | A `ay` — udisks2's spelling for a device node or a mount point, which are byte arrays because a unix path is bytes and not text. |
| 253 | `impl Value` · `pub fn as_bytestrings(&self) -> Option<Vec<String>>` | An `aay` — a list of byte strings, which is how mount points arrive. |
| 269 | `pub struct Message { pub kind: u8, pub flags: u8, pub serial: u32, pub path: Option<String>, pub interface: Option<String>, pub member: Option<String>, pub error_name: Option<String>, pub reply_serial: Option<u32>, pub destination: Option<String>, pub sender: Option<String>, pub signature: Option<String>, pub body: Vec<u8> }` | One message, off the wire or about to go on it. |
| 295 | `impl Message` · `pub fn error_text(&self) -> String` | The error's name and its human-readable text, for a toast. |
| 304 | `impl Message` · `pub fn method_call(destination: &str, path: &str, interface: &str, member: &str) -> Message` | A call to `member` on `destination`, with no arguments yet. |
| 319 | `impl Message` · `pub fn method_return(call: &Message) -> Message` | The (so far empty) answer to `call`, addressed to whoever made it. |
| 330 | `impl Message` · `pub fn error(call: &Message, name: &str, text: &str) -> Message` | An error answering `call`: a D-Bus error name and a sentence. |
| 349 | `impl Message` · `pub fn with_args(mut self, sig: &str, args: &[Value]) -> Result<Message, String>` | This message with `args` as its body, marshalled as `sig`. |
| 356 | `impl Message` · `pub fn args(&self) -> Result<Vec<Value>, String>` | The body, read back as the values its signature says it holds. |
| 365 | `impl Message` · `pub fn wants_reply(&self) -> bool` | Whether whoever sent this call is waiting for an answer. |
| 377 | `impl Message` · `pub fn encode(&self) -> Result<Vec<u8>, String>` | The whole message as bytes: the fixed header and its field array — which is itself just the value `(yyyyuua(yv))` — then the body on its own 8-byte boundary. |
| 436 | `pub fn readable_error(name: &str, text: &str) -> String` | Turn a D-Bus error into a sentence a person can act on. |
| 455 | `pub const BUS_DRIVER: &str = "org.freedesktop.DBus";` | The name the bus itself answers to, and the only sender its own replies may carry. |
| 459 | `pub struct Bus` (private fields incl. `sock: UnixStream`) | A connection to a bus: the system bus for udisks2, the session bus for the portal. |
| 484 | `impl Bus` · `pub fn system() -> Result<Bus, String>` | Connect to the system bus, authenticate, and say Hello. |
| 491 | `impl Bus` · `pub fn session() -> Result<Bus, String>` | Connect to the session bus, authenticate, and say Hello. |
| 531 | `impl Bus` · `pub fn request_name(&mut self, name: &str) -> Result<(), String>` | Own `name`, or say who does. |
| 559 | `impl Bus` · `pub fn into_service(self) -> Result<(Inbox, Outbox), String>` | Hand the connection over to a service: one [`Inbox`] for the single loop that reads calls, and one [`Outbox`] the request threads share to answer them. |
| 717 | `impl Bus` · `pub fn call(&mut self, destination: &str, path: &str, interface: &str, member: &str, signature: Option<&str>, body: &[u8]) -> Result<Vec<u8>, String>` | Make one method call and block until its reply comes back. |
| 823 | `impl Bus` · `pub fn read_message(&mut self, deadline: std::time::Instant) -> Result<Message, String>` | Read exactly one message off the wire, no later than `deadline`. |
| 848 | `pub struct Inbox` (private `sock: UnixStream`, …) | The reading half of a service's connection ([`Bus::into_service`]). |
| 864 | `impl Inbox` · `pub fn next(&mut self) -> Result<Message, String>` | The next message, however long it takes to come: first anything that arrived before the service was reading, then the socket. |
| 901 | `pub struct Outbox` (private `sock: UnixStream`, …) | The writing half of a service's connection, shared behind a lock by every thread that answers a call. |
| 909 | `impl Outbox` · `pub fn send(&mut self, mut msg: Message) -> Result<u32, String>` | Stamp `msg` with the next serial and send it. |
| 951 | `pub fn auth_line(uid: u32) -> String` | The SASL line, split out so the handshake is a test rather than a socket. |
| 972 | `pub enum BusSocket { Path(std::path::PathBuf), Abstract(Vec<u8>) }` | The socket a bus address names. |
| 997 | `pub fn bus_socket(address: &str) -> Result<BusSocket, String>` | The socket out of a bus address: the first `unix:` entry with a `path=` or an `abstract=`. |
| 1065 | `pub fn frame_len(head: &[u8]) -> Result<usize, String>` | How many bytes the message starting at `head` occupies, from its first 16. |
| 1083 | `pub fn parse_message(bytes: &[u8]) -> Result<Message, String>` | One complete message, understood. |
| 1163 | `pub fn marshal_string(out: &mut Vec<u8>, s: &str)` | — |
| 1182 | `pub fn marshal_array(out: &mut Vec<u8>, elem_align: usize, contents: impl FnOnce(&mut Vec<u8>))` | Any array: the length word, the first element on its own boundary, the contents, and the length back-patched once they are there. |
| 1197 | `pub fn marshal_no_options(out: &mut Vec<u8>)` | The empty `a{sv}` every udisks2 method takes as its options argument. |
| 1207 | `pub fn marshal_body(sig: &str, values: &[Value]) -> Result<Vec<u8>, String>` | Put `values` on the wire as a body of signature `sig`, one complete type per value. |
| 1231 | `pub fn marshal(out: &mut Vec<u8>, sig: &str, value: &Value) -> Result<(), String>` | Put one value of the single complete type `sig` on the wire, aligned from wherever `out` has got to. |
| 1378 | `pub fn check_signature(sig: &str) -> Result<(), String>` | Whether `sig` is a signature the bus will accept: at most 255 bytes of complete types, containers closed, dictionary keys basic, no empty struct. |
| 1428 | `pub fn check_object_path(path: &str) -> Result<(), String>` | Whether `path` is an object path: `/`, or `/`-separated non-empty elements of `[A-Za-z0-9_]` with no trailing slash. |
| 1471 | `pub struct Reader<'a>` | — |
| 1477 | `impl<'a> Reader<'a>` · `pub fn new(bytes: &'a [u8]) -> Reader<'a>` | — |
| 1507 | `impl<'a> Reader<'a>` · `pub fn u32(&mut self) -> Result<u32, String>` | — |
| 1524 | `impl<'a> Reader<'a>` · `pub fn string(&mut self) -> Result<String, String>` | — |
| 1532 | `impl<'a> Reader<'a>` · `pub fn signature(&mut self) -> Result<String, String>` | — |
| 1555 | `impl<'a> Reader<'a>` · `pub fn value(&mut self, sig: &str) -> Result<Value, String>` | Read one value of `sig`. |
| 1689 | `impl<'a> Reader<'a>` · `pub fn values(&mut self, sig: &str) -> Result<Vec<Value>, String>` | Read every value a body of signature `sig` holds, in order. |
| 1702 | `impl<'a> Reader<'a>` · `pub fn skip(&mut self, sig: &str) -> Result<(), String>` | Step over a value of `sig` without interpreting it. |
| 1767 | `pub type Interfaces = HashMap<String, HashMap<String, Value>>;` | One object's interfaces, and each interface's properties. |
| 1774 | `pub fn parse_managed_objects(body: &[u8]) -> Result<Vec<(String, Interfaces)>, String>` | Parse an `ObjectManager.GetManagedObjects` reply — `a{oa{sa{sv}}}`. |

Call sites outside the module (only `mounts` and `portal` use it):

| File:line | fn | Use |
|---|---|---|
| mounts.rs:88 | — | `use crate::dbus::{Bus, Interfaces, Value};` |
| mounts.rs:175-265 | `devices_from` | `Interfaces`, `Value::{as_bool, as_str, as_bytestring (205), as_bytestrings (218), as_u64}` |
| mounts.rs:1020, 1034-1040 | `run`, `udisks` | `Option<Bus>`; `Bus::system()` (1036) |
| mounts.rs:1057-1092 | `handle` | `crate::dbus::marshal_no_options` (1073, 1080, 1086); `bus.call(SERVICE, object, FILESYSTEM, "Mount", Some("a{sv}"), &args)` (1074); `crate::dbus::Reader::new(&body).string()` (1075); `"Unmount"` (1081); `bus.call(SERVICE, drive, DRIVE, "Eject", …)` (1087) |
| mounts.rs:1095-1106 | `list_devices` | `bus.call(SERVICE, MANAGER_PATH, OBJECT_MANAGER, "GetManagedObjects", None, &[])`; `crate::dbus::parse_managed_objects(&body)` (1104) |
| portal/mod.rs:53 | — | `use crate::dbus::{Bus, Inbox, Message, Outbox, Value, MSG_METHOD_CALL};` |
| portal/mod.rs:92-117 | `run` | `Bus::session()` (94), `bus.request_name(BUS_NAME)` (101), `bus.into_service()` (105) |
| portal/mod.rs:149-160 | `Service::serve` | `inbox.next()` (151) |
| portal/mod.rs:164-460 | `Service::answer`, `with_args`, `properties`, `machine_id`, … | `call.kind`, `call.wants_reply()`, `call.path/member/interface`, `call.args()`, `Message::method_return`, `Message::error`, `.with_args`, `Value::*`, `Outbox::send` (393) |
| portal/request.rs:27 | — | `use crate::dbus::Value;` (`Dialog::from_args` 122-190 uses `Value::{Path, Str, Dict, Array, Struct, U32}`, `as_str`, `as_bool`, `as_bytes`, `peeled`) |

#### `src/portal/` — `mod.rs` 696 lines (non-test 1-578, tests 579-696, 3 tests) + `request.rs` 1,225 lines (non-test 1-713, tests 714-1225, 11 tests) ✓ S1.21

What it rests on: `crate::dbus` on the **session** bus (`Bus::session`, mod.rs:94). It owns `org.freedesktop.impl.portal.desktop.delightfile` at `/org/freedesktop/portal/desktop` and answers `org.freedesktop.impl.portal.FileChooser.{OpenFile, SaveFile, SaveFiles}`, `org.freedesktop.impl.portal.Request.Close`, `Properties`, `Introspectable` and `Peer`. `Peer.GetMachineId` reads `/etc/machine-id` (mod.rs:458-464). Each dialog runs on its own `portal-request` thread (mod.rs:254-256). On the request.rs side:
- It imports `std::os::unix::ffi::{OsStrExt, OsStringExt}` (18) and `std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt}` (19).
- `Setup::from_env` (345-356) reads `DELIGHTFILE_PICKER_EXE`, `XDG_RUNTIME_DIR` (→ `$XDG_RUNTIME_DIR/delightfile`), `XDG_STATE_HOME`/`HOME` (via `df_core::state::state_path_from`, → `portal-last-dir`) and `HOME`.
- `picker_exe` (365-378) falls back to `current_exe()`, stripping a `" (deleted)"` suffix.
- `run_window` (470-525) spawns `<exe> --chooser-file=<out> --chooser-request=<req>` with stdin null (483-487), polls `try_wait` every `POLL` = 50 ms (45, 503, 522), and `Pending::close` kills the child (393-401).
- `path_from_bytes` uses `OsString::from_vec` and requires `is_absolute()` (212-217).
- `file_uri` percent-encodes `as_bytes()` (580-590).
- `read_picked` keeps lines whose first byte is `b'/'` and builds paths with `OsStr::from_bytes` (594-603).
- `remember` writes the folder with `as_bytes()` (628-650).
- `private_dir` creates `DirBuilder::mode(0o700)` + `set_permissions(from_mode(0o700))` (691-697). `private_file` creates `OpenOptions::mode(0o600).create_new(true)` (701-713).

Crate-visible API: `request` is a private child module (`mod request;`, mod.rs:49), so its `pub` items (listed second) are reachable only from `portal/mod.rs`.

| Line | Item (signature verbatim) | First doc line |
|---|---|---|
| mod.rs:57 | `pub const BUS_NAME: &str = "org.freedesktop.impl.portal.desktop.delightfile";` | The well-known name the `.portal` file points xdg-desktop-portal at. |
| mod.rs:60 | `pub const OBJECT_PATH: &str = "/org/freedesktop/portal/desktop";` | Where every portal backend lives. |
| mod.rs:92 | `pub fn run() -> i32` | `delightfile --portal`: own the name and serve until the bus goes away. |
| request.rs:30 | `pub const SUCCESS: u32 = 0;` | The portal's response codes. |
| request.rs:31 | `pub const CANCELLED: u32 = 1;` | — |
| request.rs:34 | `pub const OTHER: u32 = 2;` | "The interaction was ended in some other way": closed by the portal, a window that could not be started, a window that died. |
| request.rs:49 | `pub enum Kind { #[default] Open, Save, SaveFiles }` | Which of the three file-chooser methods a call was. |
| request.rs:58 | `impl Kind` · `pub fn of_method(member: &str) -> Option<Kind>` | — |
| request.rs:67 | `impl Kind` · `pub fn method(self) -> &'static str` | — |
| request.rs:91 | `pub struct Dialog { pub handle: String, pub kind: Kind, pub title: Option<String>, pub accept: Option<String>, pub multiple: bool, pub directory: bool, pub folder: Option<PathBuf>, pub name: Option<String>, pub file: Option<PathBuf>, pub filters: Vec<TypeFilter>, pub current_filter: Option<String>, pub files: Vec<Vec<u8>> }` | Everything a file-chooser call asked for that the window can use. |
| request.rs:122 | `impl Dialog` · `pub fn from_args(kind: Kind, args: &[Value]) -> Result<Dialog, String>` | Read a call's `(handle, app_id, parent_window, title, options)`. |
| request.rs:195 | `pub fn strip_mnemonic(label: &str) -> String` | A GTK label with its mnemonic marks taken out: `_Upload` → `Upload`, and the escaped `__` → a literal `_`. |
| request.rs:262 | `pub fn request_toml(dialog: &Dialog, folder: Option<&Path>) -> String` | The dialog as the TOML `--chooser-request` reads (see `crate::cli::parse_request`), starting in `folder`. |
| request.rs:333 | `pub struct Setup { pub picker: Option<PathBuf>, pub runtime_dir: Option<PathBuf>, pub last_dir_file: Option<PathBuf>, pub home: Option<PathBuf> }` | Everything a request reads from the environment, read once at startup so a test can hand the service another one. |
| request.rs:345 | `impl Setup` · `pub fn from_env() -> Setup` | — |
| request.rs:383 | `pub struct Pending` (private `closed`, `child: Option<Child>`, `files`) | A dialog's state while its window is up, shared with the reader loop so `Close` can reach it. |
| request.rs:393 | `impl Pending` · `pub fn close(&mut self)` | `Request.Close`: kill the window if there is one, and make sure there never will be. |
| request.rs:404 | `impl Pending` · `pub fn abandon(&mut self)` | The service is going: close, and clear the files, because the thread that would have is going with it. |
| request.rs:414 | `pub struct Answer { pub response: u32, pub uris: Vec<String> }` | What a dialog answered. |
| request.rs:420 | `impl Answer` · `pub fn cancelled() -> Answer` | — |
| request.rs:427 | `impl Answer` · `pub fn other() -> Answer` | — |
| request.rs:436 | `pub fn ask(dialog: &Dialog, setup: &Setup, number: u64, pending: &Mutex<Pending>) -> Answer` | Put the dialog on screen and wait for it. |
| request.rs:531 | `pub fn answer_for(dialog: &Dialog, picked: &[PathBuf], cleanly: bool) -> Answer` | The answer to a dialog whose window has exited, given what it picked. |
| request.rs:558 | `pub fn save_files_uris(folder: &Path, names: &[Vec<u8>]) -> Vec<String>` | `SaveFiles`: each of the caller's names inside the chosen folder, in the caller's order. |
| request.rs:580 | `pub fn file_uri(path: &Path) -> String` | A `file://` URI for an absolute path, percent-encoded byte by byte. |

Call sites outside the module:

| File:line | fn | Use |
|---|---|---|
| main.rs:91 | `main` | `cli::Outcome::Portal => std::process::exit(portal::run())` ✓ S1.31 |
| cli.rs:221, 246-252 | `cli::parse` | produces `Outcome::Portal` for `--portal` ✓ S1.31 |
| cli.rs:327-448 | `read_request`, `parse_request` | read the TOML `request.rs::request_toml` writes (`--chooser-request`) — the file protocol between the portal process and the picker process |
| (inbound) request.rs:26 | — | `use crate::cli::TypeFilter;` (= `df_core::fs::TypeFilter`) |
| (inbound) request.rs:351 | `Setup::from_env` | `df_core::state::state_path_from(var("XDG_STATE_HOME"), var("HOME"))` |
| tests/portal.rs | integration | runs `CARGO_BIN_EXE_delightfile --portal` on a private `dbus-daemon` (see §7) |

#### `src/mounts.rs` — 2,875 lines (non-test 1-1887, `mod tests` 1888-2875, 21 tests) ✓ S1.21 (split), S1.24 (stubs)

What it rests on:
- **udisks2 over the system bus** through `crate::dbus` (88-96; `Bus::system` 1036).
- **gvfs through the `gio` CLI**: `gio mount -l` (682-695), `gio mount -u <url>` (736-752), `gio mount <url>` (844-880).
- **gvfs-fuse's directory** `/run/user/<uid>/gvfs`, from `gvfs_root()` (657-659) using `df_core::ops::trash::uid()` (= `libc::getuid`, df-core ops/trash.rs:588-594).
- **`TERMINAL_MOUNT`** = `setsid uwsm-app -- "${TERMINAL:-ghostty}" -e gio mount "$1"` (282).
- **A worker thread** `df-mounts` (973-991).

Full public API:

| Line | Item (signature verbatim) | First doc line |
|---|---|---|
| 101 | `pub const ROWS: usize = 10;` | How many rows the card shows before it scrolls. |
| 105 | `pub struct Device { pub object: String, pub drive: Option<String>, pub node: String, pub label: String, pub fs: String, pub size: u64, pub mount: Option<PathBuf>, pub removable: bool, pub ejectable: bool, pub hardware: String }` | One mountable filesystem. |
| 130 | `impl Device` · `pub fn is_mounted(&self) -> bool` | — |
| 135 | `impl Device` · `pub fn status(&self) -> String` | The right-hand text: where it is, or what it is. |
| 144 | `impl Device` · `pub fn detail(&self) -> String` | The dim second line: size, filesystem, device node, and whether it comes out. |
| 175 | `pub fn devices_from(objects: &[(String, Interfaces)]) -> Vec<Device>` | Turn a `GetManagedObjects` reply into the rows the card shows. |
| 273 | `pub const SCHEMES: &[&str] = &["smb", "sftp", "ftp", "dav", "davs", "nfs"];` | What the connect prompt takes: gvfs's network-share backends, and not every scheme gvfs can mount. |
| 282 | `pub const TERMINAL_MOUNT: &str = r#"setsid uwsm-app -- "${TERMINAL:-ghostty}" -e gio mount "$1""#;` | The command a mount with questions to ask is re-run under, in a terminal where it can ask them. |
| 292 | `pub struct Share { pub url: String, pub label: String, pub scheme: String, pub path: PathBuf }` | One mounted network share: a row in the Network section. |
| 313 | `pub struct Address { pub scheme: String, pub user: Option<String>, pub host: String, pub port: Option<u16>, pub path: String }` | A server address taken apart: `sftp://me@host:2222/srv` is scheme `sftp`, user `me`, host `host`, port 2222 and path `/srv`. |
| 324 | `impl Address` · `pub fn parse(url: &str) -> Option<Address>` | `None` for anything that is not `scheme://…`. |
| 367 | `impl Address` · `pub fn label(&self) -> String` | What a row, a toast and a job call the share. |
| 447 | `pub struct Spec` (private fields) | A gvfs mount spec: the `type:key=value,…` gvfs-fuse names a mount's directory after — `smb-share:server=nas,share=media`, `sftp:host=example.org,user=me`. |
| 465 | `impl Spec` · `pub fn of(address: &Address) -> Spec` | The spec gvfs would build for `address`, backend by backend. |
| 517 | `impl Spec` · `pub fn dir_name(&self) -> String` | The directory name gvfs-fuse gives a mount with this spec. |
| 532 | `impl Spec` · `pub fn from_dir_name(name: &str) -> Option<Spec>` | Read a gvfs-fuse directory name back into a spec. |
| 590 | `pub fn gio_mounts(listing: &str) -> Vec<(String, String)>` | The mounts `gio mount -l` lists at the left margin, as `(name, url)`. |
| 617 | `pub fn shares_from(listing: &str, root: &Path, entries: &[String]) -> Vec<Share>` | The Network section's rows: [`gio_mounts`] out of `listing`, each with the directory under `root` that gvfs-fuse shows it as. |
| 657 | `pub fn gvfs_root() -> PathBuf` | Where gvfs-fuse shows its mounts: `/run/user/<uid>/gvfs`. |
| 717 | `pub fn list_shares() -> Vec<Share>` | Every share gvfs has mounted, now. |
| 760 | `pub fn connect_url(text: &str) -> Result<String, String>` | Check what was typed at the connect prompt, and return the address to hand to `gio mount`. |
| 785 | `pub enum Attempt { Mounted, NeedsTerminal, Failed(String) }` | What one `gio mount` with nobody to answer its questions came to. |
| 809 | `pub fn attempt(success: bool, stdout: &str, stderr: &str) -> Attempt` | Read a finished `gio mount`. |
| 830 | `pub enum Connected { Mounted(Option<PathBuf>), NeedsTerminal, Failed(String) }` | What [`connect`] came to. |
| 844 | `pub fn connect(url: &str) -> Connected` | `gio mount <url>` with stdin closed, and then where the mount is. |
| 895 | `pub fn landing(shares: &[Share], address: &Address) -> Option<(PathBuf, PathBuf)>` | Where `address` lands among the mounted shares: the share it is on, and the path inside that share the address went on to name. |
| 932 | `pub enum Request { List, Mount(String), Unmount(String), Eject(String), UnmountShare(String) }` | What the event loop asks the worker to do. |
| 944 | `pub enum Reply { Listing { devices: Vec<Device>, shares: Vec<Share> }, Mounted(PathBuf), Unmounted, Ejected, Failed(String) }` | What comes back. |
| 965 | `pub struct Mounts` | The worker, and the two channels either side of it. |
| 973 | `impl Mounts` · `pub fn start(notify: df_core::fs::Notifier) -> Mounts` | Start the worker. |
| 993 | `impl Mounts` · `pub fn ask(&self, request: Request)` | — |
| 999 | `impl Mounts` · `pub fn drain(&self) -> Vec<Reply>` | — |
| 1113 | `pub struct Place { pub name: String, pub detail: String, pub target: PathBuf, pub remote: bool, pub pinned: bool }` | One row of the Places section: a pinned folder or a `[goto]` row, as the app's Places list describes it. |
| 1130 | `pub const PLACES_EMPTY: &str = "Nothing pinned · g b pins this folder";` | What the Places section says when there is nothing in it: what it is for, and the key that fills it. |
| 1134 | `pub enum Item { Place(usize), Disk(usize), Share(usize), Connect }` | Something the cursor can be on. |
| 1147 | `pub enum Line { Section(&'static str), Empty(&'static str), Item(Item) }` | One line of the card, from top to bottom. |
| 1166 | `pub struct Card` (pub `places: Vec<Place>`, `devices: Vec<Device>`, `shares: Vec<Share>`, `cursor: usize`, `first: usize`, `busy: Option<String>`, `loading: bool`, …) | The card's own state, while it is open. |
| 1187 | `impl Card` · `pub fn new() -> Card` | — |
| 1207 | `impl Card` · `pub fn with_places(places: Vec<Place>) -> Card` | A card with its Places section, the cursor on the first row after it. |
| 1218 | `impl Card` · `pub fn items(&self) -> Vec<Item>` | Everything the cursor can land on, in order: the places, the disks, the shares, and the connect row. |
| 1228 | `impl Card` · `pub fn lines(&self) -> Vec<Line>` | Every line, headings and empty states included. |
| 1249 | `impl Card` · `pub fn selected(&self) -> Option<Item>` | — |
| 1253 | `impl Card` · `pub fn selected_device(&self) -> Option<&Device>` | — |
| 1260 | `impl Card` · `pub fn selected_share(&self) -> Option<&Share>` | — |
| 1267 | `impl Card` · `pub fn selected_place(&self) -> Option<&Place>` | — |
| 1277 | `impl Card` · `pub fn set_places(&mut self, places: Vec<Place>)` | Take a new Places list — a pin came off it — keeping the cursor at the same height, so it lands on the row that moved up into the gap rather than jumping back to the top. |
| 1286 | `impl Card` · `pub fn move_cursor(&mut self, delta: isize)` | `↑`/`↓`, clamping. |
| 1293 | `impl Card` · `pub fn select(&mut self, item: Item)` | Put the cursor on `item`: a click. |
| 1316 | `impl Card` · `pub fn visible(&self) -> Vec<(Line, f32)>` | The lines drawn from [`Card::first`], each with its top measured from the top of the body, stopping at the bottom of the window. |
| 1331 | `impl Card` · `pub fn visible_items(&self) -> Vec<Item>` | The rows among [`Card::visible`]: what `Control::PanelRow(i)` indexes, for the pointer. |
| 1378 | `impl Card` · `pub fn update(&mut self, devices: Vec<Device>, shares: Vec<Share>)` | Take a new listing without losing the user's place. |
| 1404 | `impl Card` · `pub fn disks_empty(&self) -> Option<&'static str>` | What the disks section says when it has no rows, or `None` when it has some. |
| 1417 | `impl Card` · `pub fn shares_empty(&self) -> Option<&'static str>` | The same for the Network section. |
| 1516 | `pub struct Geometry { pub card: egui::Rect, pub body: egui::Rect, pub lines: Vec<(Line, egui::Rect)>, pub rows: Vec<egui::Rect>, pub close: Option<egui::Rect>, pub unpin: bool, … }` | Where the card's pieces are. |
| 1536 | `impl Geometry` · `pub fn row_at(&self, pos: egui::Pos2) -> Option<usize>` | The row under `pos`. |
| 1546 | `pub fn geometry(area: egui::Rect, card: &Card) -> Geometry` | Lay the card out, centred and biased above true centre (`delightful-ui` §16). |
| 1668 | `pub fn paint(paint: &crate::ui::Painting<'_>, area: egui::Rect, card: &Card, geometry: &Geometry, hovers: &crate::hover::Hovers<crate::ui::Control>, ripples: &crate::ripple::Ripples<crate::ui::Control>)` | Draw it. |

Call sites outside the module:

| File:line | fn | Use |
|---|---|---|
| app.rs:342-348 | `struct PendingConnect` | `label: String` (from `crate::mounts::Address::label`), `slot: Arc<std::sync::Mutex<Option<crate::mounts::Connected>>>` |
| app.rs:512 | `enum OverlayGeom` | `Mounts(crate::mounts::Geometry)` |
| app.rs:1355, 1357 | `App` fields | `udisks: Option<crate::mounts::Mounts>`, `mounts: Option<crate::mounts::Card>` ✓ S1.24 |
| app.rs:6278-6282 | `App::udisks` | `crate::mounts::Mounts::start(Arc::new(move \|\| waker.wake()))` ✓ S1.24 |
| app.rs:6287 | `App::open_mounts` | `ask(Request::List)` |
| app.rs:6302-6346 | `App::mount_action` | `Item::{Disk, Place, Share, Connect}` |
| app.rs:6348-6360 | `App::mount_device` | `ask(Request::Mount(device.object))` (6353) |
| app.rs:6362-6395 | `App::mount_selected` | `Item` dispatch |
| app.rs:6396-6430 | `App::unmount_selected` | `ask(Request::UnmountShare(url))` (6410), `ask(Request::Unmount(device.object))` (6427) |
| app.rs:6431-6465 | `App::eject_selected` | `ask(Request::Eject(drive))` (6461) |
| app.rs:6467-6470 | `App::refresh_mounts` | `ask(Request::List)` (6469) |
| app.rs:6486-6545 | `App::connect` | `Address::parse(&url).map(\|a\| a.label())` (6487-6488); FnJob `crate::mounts::connect(&job_url)` (6498) ✓ S1.24 |
| app.rs:6547-6589 | `App::connected` | `Connected::{Mounted, NeedsTerminal, Failed}`; `open::spawn_detached(crate::mounts::TERMINAL_MOUNT, …)` (6575-6579) ✓ S1.24 |
| app.rs:6603-6655 | `App::poll_mounts` | `Reply::{Listing, Mounted, Unmounted, Ejected, Failed}` (6617-6635); `ask(Request::List)` (6649) ✓ S1.24 |
| app.rs:7773 | `App::submit_prompt` | `PromptKind::Connect => crate::mounts::connect_url(&text)` |
| app.rs:9949 | `App::overlay_geometry` | `crate::mounts::geometry(area, card)` |
| app.rs:15692 | `App::frame` | `crate::mounts::paint(&paint, area, card, geometry, &self.hovers, &self.ripples)` |
| app/places.rs:315-331 | `card_places` | builds `crate::mounts::Place` |
| app/places.rs:514-523 | `App::card_places`, `App::mount_card` | `crate::mounts::Card::with_places(self.card_places())` |
| (inbound) mounts.rs:658 | `gvfs_root` | `df_core::ops::trash::uid()` |
| (inbound) mounts.rs:145 | `Device::detail` | `crate::format::human_size` |

### df-core calls made from df-app whose behaviour is platform-bound inside df-core

Listed as seams only; df-core is covered by the other inventory.

| df-app file:line (fn) | df-core API | Platform-bound part (in df-core) |
|---|---|---|
| app.rs:1813 (App::new) | `df_core::config::load()` | config file location |
| app.rs:1821 (App::new) | `df_core::config::config_dir()` | config directory (XDG) |
| app.rs:1834 (App::new) | `StateStore::load()` | state file location |
| portal/request.rs:351 (Setup::from_env) | `df_core::state::state_path_from(XDG_STATE_HOME, HOME)` | XDG state path |
| app.rs:1864 (App::assemble) | `Watcher::start` | filesystem watcher (inotify per README/workspace comment "df-core calls inotify through it", format.rs:197) |
| app.rs:4057, 4119, 4158 | `ops::Trash::home()`, `ops::trash::restore`, `ops::purge` | freedesktop trash |
| mounts.rs:658 | `ops::trash::uid()` | `libc::getuid` (df-core ops/trash.rs:588-594) |
| remote.rs:419-421 | `ops::trash::{MAX_TRASH_COLLISIONS, suffixed}` | 255-byte name clip |
| app.rs:4516 | `ops::symlink(target, &link, kind)` | symlink creation |
| app.rs:3209, 3219, 3514, 3640, … | `vfs::Vfs::start`, `download_to_temp`, `upload_new`, … | sftp via ssh; temp dir `$TMPDIR/delightfile-vfs-<pid>/` |
| app/syncing.rs:123 | `sync::rsync::available()` | spawns rsync/ssh (df-core sync/rsync.rs) |
| app.rs:2814, preview/listing.rs:236 | `archive::external_extractor()` | PATH lookup of 7z/unzip/bsdtar |
| app.rs:6227 | `zoxide::load` | zoxide database location |
| app.rs:9250, 9349, 9468 | `du::DuScanner::start`, `du::is_remote` | disk-usage walk |
| app.rs:10397, 10424 | `git::repo_root`, `git::Git::start` | spawns `git` |
| grid.rs:627; preview/decode.rs:1055 | `preview::cached_thumb`, `preview::store_thumb` | `temp_dir()/yazi-<getuid()>`, `MetadataExt` ctime key |
| format.rs:23, 30; remote.rs:585-586 | `Entry::permissions_string`, `Entry::owner_label` | POSIX mode string, uid/gid names |
| spot.rs:271 | `fs::owner::owner_label(uid, gid)` | uid/gid → name lookup |
| app.rs:7864 (App::create) | `ops::create(&cwd.join(text))` | trailing-`/` = folder convention |

---

## 2. Public surfaces that a platform layer must preserve

### 2.1 `clipboard` (src/clipboard.rs)

| Line | Item (signature verbatim) | First doc line |
|---|---|---|
| 52 | `pub const SIZE_CAP: u64 = 50 * 1024 * 1024;` | The largest file whose *contents* go on the clipboard. |
| 60 | `pub enum Branch { Image(&'static str), Text, Uris }` | How a file goes onto the clipboard. |
| 96 | `pub fn is_text_like(mime: &str) -> bool` | Is this mime worth putting on the clipboard as plain text? |
| 101 | `pub fn is_image(mime: &str) -> bool` | — |
| 114 | `pub fn branch_for(count: usize, mime: &str, size: u64) -> Branch` | Which branch a copy takes. |
| 162 | `pub fn image_label(mime: &str) -> String` | What a branch is called in a toast: "Copied image (PNG)". |
| 184 | `pub fn image_extension(mime: &str) -> &'static str` | The extension to save a pasted clipboard image under. |
| 214 | `pub fn file_uri(path: &Path) -> String` | One path as a `file://` URI. |
| 230 | `pub fn parse_file_uri(text: &str) -> Option<PathBuf>` | The other direction. |
| 267 | `pub fn uri_list(paths: &[PathBuf]) -> String` | The `text/uri-list` payload for a selection. |
| 277 | `pub fn parse_uri_list(text: &str) -> Vec<PathBuf>` | Read a `text/uri-list` back. |
| 296 | `pub fn offer_mimes(mime: Option<&str>) -> Vec<String>` | The mime types one copy is announced under. |
| 310 | `pub enum ClipError { Missing(&'static str), Failed(String) }` (+ `impl Display`, 318-327) | What can go wrong, in the two shapes the caller words differently. |
| 339 | `pub fn copy(mime: Option<&str>, bytes: &[u8]) -> Result<Child, ClipError>` | Put `bytes` on the clipboard, offered as `mime` (or as plain text when it is `None`) — the **fallback** copy, for a session with no data device. |
| 389 | `pub fn reap(child: &mut Child)` | Stop a `wl-copy` we are done with and collect it. |
| 397 | `pub fn offered_types() -> Result<Vec<String>, ClipError>` | The mime types the clipboard is currently offering, most specific first — `wl-paste --list-types`. |
| 418 | `pub fn paste(mime: &str) -> Result<Vec<u8>, ClipError>` | The clipboard's bytes, as `mime`. |
| 436 | `pub enum Offer { Files, Image(String), Text(String) }` | Which offered type this paste should ask for, and what to do with it. |
| 451 | `impl Offer` · `pub fn mime(&self) -> &str` | The mime to actually ask the clipboard for. |
| 466 | `pub fn text_offer(types: &[String]) -> Option<String>` | The type to ask for when what is wanted is **text** — a prompt's `Ctrl+v`. |
| 482 | `pub fn choose_offer(types: &[String]) -> Option<Offer>` | Pick the best offer out of `wl-paste --list-types`. |

**Two transports.** The primary copy is `wayland::DataDevice::set_selection(clipboard::offer_mimes(mime), bytes)`, called from `App::offer` (app.rs:13110-13113). The Wayland thread owns a `wl_data_source`, and the compositor's answer arrives as `Event::Copied { ok }`. The fallback is `clipboard::copy` → `wl-copy --foreground [--type <mime>]`, used when there is no data device, when the compositor refuses (`App::copy_answered`), after `CLIPBOARD_ANSWER` (10 s), or when the Wayland thread dies. Paste has the same two paths: `DataDevice::receive(seq, mime)` → `Event::Pasted`, or synchronous `wl-paste --list-types` / `wl-paste --no-newline --type <mime>`. The system clipboard's current types are mirrored by `Event::Selection { mimes }` into `App.clipboard_types` / `App.clipboard_seen` (app.rs:12755-12758).

**What is copied** (all through `App::offer`):
- `Y` (`App::yank_to_system`, app.rs:12978-13015) uses `branch_for(count, mime, size)` (clipboard.rs:114-130):
  - one file, ≤ 50 MiB, `image/*` → the file's bytes, offered under that image mime alone. `static_image_mime` maps it to one of 12 known image mimes, else `image/png` (138-158).
  - one file, ≤ 50 MiB, text-like (`text/*` or one of the 11 `TEXT_LIKE` mimes at 79-93) → the file's bytes, offered as `text/plain;charset=utf-8` and `text/plain`.
  - anything else, including every multi-file selection → `text/uri-list` **only** (13008-13013). The payload is `file://` + percent-encoded raw bytes per path, each line CRLF-terminated (`uri_list`, 267-274).
- `c t` (`App::copy_file_text`, 13019-13045): one text-like file's bytes → `offer(None, …)`.
- `c c` / `c d` / `c f` / `c n` (`App::copy_piece`, 13048-13078): path / dirname / file name / stem as text → `offer(None, …)`.
- The spot panel's `c c` (`App::copy_spot_cell`, 13082-13094): the focused cell's text → `offer(None, …)`.
- `offer_mimes(None)` = `["text/plain;charset=utf-8", "text/plain"]`; `offer_mimes(Some(m))` = `[m]` (296-304).

**File-list format:** `text/uri-list`, CRLF line endings. No `x-special/gnome-copied-files` (or any cut marker) is offered or read anywhere in df-app. The yank/cut clipboard (`y`/`x`/`p`) is df-core's in-process `ops::paste::Clipboard` and never touches the system clipboard. The drag payload (dnd.rs:630-646) additionally offers `text/plain` newline-joined paths; the clipboard does not.

**How paste is parsed.** `p` with nothing yanked goes to `App::paste_system` (app.rs:13315-13400). The internal clipboard always wins (doc 13317-13324). `choose_offer` (482-500) prefers `text/uri-list` (case-insensitive) → `Offer::Files`, then the first `image/*` → `Offer::Image(mime)`, then `text/plain*` or any `text/*` → `Offer::Text(mime)`. `App::take_pasted` (13525-13558) then does one of three things:
- `Files` → `parse_uri_list` (skips blank and `#` lines; `parse_file_uri` accepts `file:///p` and `file://localhost/p`, refuses any other authority, percent-decodes byte-wise into `OsString::from_vec`), keeps only `path.exists()` (13567) → `Clipboard::yank(paths)` → `paste_into` (a copy).
- `Image(mime)` → `std::fs::write` of `clipboard_<file_stamp>.<image_extension(mime)>` in the target directory (`App::save_clipboard`, 13605-13645).
- `Text` → the same, with `.txt`.
A prompt's `ctrl+v` (`App::paste_into_prompt`, 13402-13455) uses `text_offer` (`text/plain*` > `text/*` > `is_text_like`) and inserts the text at the caret.

**Parallel clipboard in egui-winit.** egui-winit's own arboard/smithay-clipboard instance exists alongside all of this (see the intro); df-app does not use it.

### 2.2 `dnd` (src/dnd.rs) and what `wayland::DataDevice` provides to it

| Line | Item (signature verbatim) | First doc line |
|---|---|---|
| 57 | `pub enum Verb { Move, Copy, Link }` | What a drop will do, and the word the chip beside the ghost says. |
| 66 | `impl Verb` · `pub fn label(self) -> &'static str` | The chip's text. |
| 81 | `pub fn verb_for(ctrl: bool, alt: bool) -> Verb` | Which verb the modifiers currently held mean. |
| 97 | `pub enum Target { Row(Column, usize), Pane(Column), Crumb(usize), Tab(usize) }` | Somewhere a drop can land. |
| 115 | `impl Target` · `pub fn is_row(self) -> bool` | Is this a row, and therefore something a hold can spring open? |
| 126 | `pub struct Zones { pub strip: Option<egui::Rect>, pub tabs: Vec<f32>, pub crumbs: Vec<egui::Rect>, pub list_pane: egui::Rect, pub list_content: egui::Rect, pub list_scroll: f32, pub list_grid: Option<crate::grid::Metrics>, pub list_rows: usize, pub scale: crate::ui::Scale, pub parent_pane: egui::Rect, pub parent_content: egui::Rect, pub parent_scroll: f32, pub parent_rows: usize }` | Where this frame drew everything a drop can land on. |
| 159 | `impl Zones` · `pub fn rect_of(&self, target: Target) -> Option<egui::Rect>` | Where a target was drawn — what the ring goes around. |
| 202 | `pub fn target_at(zones: &Zones, pos: egui::Pos2, is_dir: impl Fn(Column, usize) -> bool) -> Option<Target>` | What a drop at `pos` would land on, or `None` for the inert parts of the window. |
| 269 | `pub fn valid_dest(dest: &Path, dragged: &[PathBuf], verb: Verb) -> bool` | Could a drop of `dragged` land in `dest` at all? |
| 288 | `pub const GHOST_WIDTH: f32 = 176.0;` | The ghost card's width, in logical points. |
| 292 | `pub const GHOST_PAD: f32 = 4.0;` | The padding that lifts the card off the rows behind it, above and below the line of text it holds. |
| 302 | `pub fn ghost_height(row_height: f32) -> f32` | Its height — one row of the listing it came out of, plus [`GHOST_PAD`]. |
| 307 | `pub const GHOST_RADIUS: u8 = 7;` | The card's corner radius, and the badge's. |
| 314 | `pub const GHOST_STACK: usize = 3;` | How many cards are drawn *behind* the top one. |
| 318 | `pub const GHOST_STEP: f32 = 4.5;` | How far each card behind is offset from the one in front, in points. |
| 326 | `pub const GHOST_TILT: f32 = 0.030;` | How far each card behind is tilted, in radians — about 1.7°, alternating sign so the stack reads as *dropped* rather than as sheared. |
| 333 | `pub fn ghost_grab(row_height: f32) -> egui::Vec2` | Where the pointer sits on the top card. |
| 348 | `pub fn ghost_home(centre: egui::Pos2, row_height: f32) -> egui::Pos2` | The pointer position that puts the ghost's top card **centred on** `centre` — the middle of the row, or the chip, the drag came off. |
| 354 | `pub const GHOST_LIFT: f32 = 1.025;` | How far the top card is lifted, as a scale. |
| 358 | `pub struct Card { pub rect: egui::Rect, pub tilt: f32, pub alpha: f32 }` | One card of the stack, back to front. |
| 370 | `pub fn ghost_cards(at: egui::Pos2, count: usize, row_height: f32) -> Vec<Card>` | The stack for a drag of `count` items, with the pointer at `at`, sized for a listing whose rows are `row_height` tall. |
| 407 | `pub fn ghost_badge(count: usize) -> Option<usize>` | The count chip's text, or `None` when the stack already says the number by having that many cards in it. |
| 417 | `pub fn tilted(rect: egui::Rect, tilt: f32) -> Vec<egui::Pos2>` | A rectangle's corners, rotated about its own centre — how a tilted card is drawn, since egui has no rotated rounded rect. |
| 441 | `pub const EDGE_BAND: f32 = 26.0;` | How wide the band at a pane's top and bottom edges is, in logical points. |
| 450 | `pub const EDGE_ROWS_PER_SEC: f32 = 14.0;` | The fastest the listing travels while a drag sits at the very edge, in rows per second. |
| 459 | `pub fn autoscroll(content: egui::Rect, pos: egui::Pos2) -> f32` | How fast the listing under `pos` should be travelling, in rows per second — negative for up, and zero when the pointer is not in a band. |
| 492 | `pub const SPRING_OPEN: Duration = Duration::from_millis(700);` | How long a drag has to hover a directory before it springs open. |
| 496 | `pub struct SpringOpen` | The hold timer, and the badge that fills while it runs. |
| 503 | `impl SpringOpen` · `pub fn aim(&mut self, target: Option<Target>, now: Instant)` | Point the timer at whatever the drag is over now. |
| 513 | `impl SpringOpen` · `pub fn progress(&self, now: Instant) -> f32` | How full the commit badge is, 0…1 — delightviewer's `dismiss.rs` arc, on a timer instead of on a distance. |
| 524 | `impl SpringOpen` · `pub fn fired(&mut self, now: Instant) -> Option<Target>` | Has the hold earned its navigation? |
| 535 | `impl SpringOpen` · `pub fn deadline(&self) -> Option<Instant>` | When the next frame is owed, so a drag held perfectly still still opens the folder (PLAN §1: a waiter is a deadline, never a poll). |
| 543 | `pub const SPRING_BACK: Duration = Duration::from_millis(400);` | How long the ghost takes to fly home, PLAN §8's spring-back duration. |
| 551 | `pub struct SpringBack` | The ghost's flight home after a cancelled drag. |
| 559 | `impl SpringBack` · `pub fn new(from: egui::Pos2, to: egui::Pos2, count: usize, now: Instant) -> SpringBack` | — |
| 568 | `impl SpringBack` · `pub fn at(&self, now: Instant) -> egui::Pos2` | — |
| 573 | `impl SpringBack` · `pub fn count(&self) -> usize` | — |
| 580 | `impl SpringBack` · `pub fn alpha(&self, now: Instant) -> f32` | How solid the ghost still is. |
| 584 | `impl SpringBack` · `pub fn finished(&self, now: Instant) -> bool` | — |
| 597 | `pub const SELF_MIME: &str = "application/x-delightfile-drag";` | The private mime that marks an offer as delightfile's. |
| 610 | `pub fn self_mime() -> &'static str` | The same name with **this process's** pid on it, which is the one actually offered. |
| 618 | `pub fn is_ours(offered: &[String]) -> bool` | Was this offer started by *this* window? |
| 630 | `pub fn offer(paths: &[PathBuf]) -> Vec<(String, Vec<u8>)>` | What the drag offers, in the order it offers it. |
| 653 | `pub fn wanted_mime(offered: &[String]) -> Option<String>` | Which of an incoming drag's offered mimes to ask for, most useful first. |
| 678 | `pub fn paths_from(mime: &str, bytes: &[u8]) -> Vec<PathBuf>` | Turn what a drop handed over into paths. |

**Everything above line 597 is platform-neutral geometry and timing.** The internal drag is drawn from egui pointer input (`App::tick_drag`, app.rs:12428-). The verb comes from `verb_for(pointer.toggle, pointer.alt)`.

**Drag-out.** When the pointer is outside the window while the button is still down (app.rs:12458-12461), `App::hand_off_drag` (12687-12722) calls `DataDevice::drag(dnd::offer(&paths), count, card, ink, scale)`. `offer` yields, in order:
1. `text/uri-list` (CRLF `file://` lines);
2. `text/plain;charset=utf-8` and `text/plain` (paths joined by `\n`);
3. `application/x-delightfile-drag;pid=<pid>` with no bytes.

On the Wayland thread, `State::start_drag` (wayland/mod.rs:776-) creates a `wl_data_source`, sets `DndAction::Copy` only (806), calls `wl_data_device.start_drag` with the serial of a still-held button press (811), and attaches an icon surface drawn by `wayland::icon::draw` into a `memfd` `wl_shm` pool (842-852). `Event::DragEnded` clears the app's drag (app.rs:12748-12754). If there is no device, or `drag` returns `false`, the ghost springs home (12717-12721). winit 0.30 has no drag-source API on macOS or Windows, so this path is Wayland-only.

**Drop-in.** The path runs through six steps:
1. `wl_data_device.data_offer` / `enter` / `motion` / `leave` / `drop` are handled on the Wayland thread (`Dispatch<WlDataDevice>`, wayland/mod.rs:1212-1285).
2. On `Enter`, `dnd::is_ours(&mimes)` and `dnd::wanted_mime(&mimes)` choose the mime (1242-1243); the thread `accept`s it and sets actions `Copy`/`Copy` (1247-1250).
3. `Event::Enter { at, ours }` and `Event::Motion { at }` carry surface-local positions. The app stores them in `App.incoming` (app.rs:12732-12742), and they drive the drop-target highlight (14684-14703) and the external ring (15782-15788).
4. On `Drop`, `State::take_drop` (wayland/mod.rs:896-921) reads the pipe with `RECEIVE_TIMEOUT` and turns bytes into paths with `dnd::paths_from(mime, &bytes)`. That function uses `parse_uri_list` for `text/uri-list`, and for `text/plain` it takes absolute lines starting with `/`.
5. It then sends `Event::Drop { paths, ours }`.
6. `App::take_external_drop` (app.rs:12926-12953) resolves the target from the last frame's `Zones` via `dnd::target_at` (`App::dropped_target`, 12959-12970), then does `Clipboard::yank(paths)` → `paste_into(…, force = false)`: always a copy.

**winit `WindowEvent::DroppedFile` / `HoveredFile` / `HoveredFileCancelled` are not handled anywhere in df-app.** `App::window_event` (app.rs:17066-17162) has no arm for them, and grep finds no reference in `src/`.

### 2.3 `mounts` (src/mounts.rs)

The full public API with signatures and first doc lines is in the §1 mounts block.

**What a row is.** The `M` card has four item kinds (`Item`, 1134-1143):
- **`Place(usize)`**: a `Place { name, detail, target, remote, pinned }` row built by the app from pins and `[goto]` (app/places.rs:315-331).
- **`Disk(usize)`**: a `Device` from udisks2 `GetManagedObjects`. Only block objects that have both `org.freedesktop.UDisks2.Filesystem` and a `Drive` (not `/`) and are not `HintIgnore` qualify (`devices_from`, 175-265). Fields: `object` (block object path), `drive`, `node` (`/dev/…`, from byte array), `label` (`IdLabel` or last node component), `fs` (`IdType`), `size`, `mount` (first of `MountPoints`), `removable`, `ejectable`, `hardware` (`Vendor Model`).
- **`Share(usize)`**: a `Share { url, label, scheme, path }` from `gio mount -l`, matched to a directory under `/run/user/<uid>/gvfs` by the gvfs mount-spec name (`Spec`, `shares_from` 617-653).
- **`Connect`**: the always-present "connect to server" row.

**Actions and how each runs.** All udisks2 calls go over the system bus on the `df-mounts` worker, via `Mounts::ask(Request)` → `run` (1019) → `udisks` (1034)/`handle` (1057-1092), each with an empty `a{sv}` options argument.

| Action (key) | App fn | Request | Execution |
|---|---|---|---|
| list (`M`, `r`) | `open_mounts` 6284, `refresh_mounts` 6467, after each reply 6649 | `Request::List` | `ShareListing::start()` = `gio mount -l` child (682-695), then udisks2 `org.freedesktop.DBus.ObjectManager.GetManagedObjects` on `/org/freedesktop/UDisks2` (1095-1106), then `ShareListing::finish` (700-713) reads stdout and `gvfs_entries(/run/user/<uid>/gvfs)`. |
| mount disk (`Enter` on unmounted, `m`) | `mount_device` 6348 | `Request::Mount(object)` | `org.freedesktop.UDisks2.Filesystem.Mount` on the block object; reply `s` = mount path → `Reply::Mounted(PathBuf)` (1069-1076). |
| unmount disk (`u`) | `unmount_selected` 6396 | `Request::Unmount(object)` | `org.freedesktop.UDisks2.Filesystem.Unmount` (1078-1083). |
| eject (`e`) | `eject_selected` 6431 | `Request::Eject(drive)` | `org.freedesktop.UDisks2.Drive.Eject` on the drive object (1084-1089). |
| unmount share (`u` on share) | `unmount_selected` 6410 | `Request::UnmountShare(url)` | `gio mount -u <url>` `.output()` (736-752), without the bus (1022-1025). |
| connect (`Enter`/`m` on connect row → prompt) | `submit_prompt` 7773 → `connect` 6486 | FnJob, not the worker | `connect_url` validates the scheme against `SCHEMES` (760-782). Then `connect(url)` runs `gio mount <url>` with stdin null (844-852) and `attempt` classifies the output (809-827). On success, `list_shares()` plus polling up to `ARRIVAL` = 2 s (288) for the gvfs-fuse directory (866-874) gives the landing path via `landing`. `NeedsTerminal` → `open::spawn_detached(TERMINAL_MOUNT, &[url], cwd)` (app.rs:6575). |
| go there (`Enter` on mounted disk/share/place) | `mount_action` 6301 | — | navigates to `Device.mount` / `Share.path` / `Place.target`. |

### 2.4 `open` (src/open.rs)

| Line | Item (signature verbatim) | First doc line |
|---|---|---|
| 42 | `pub fn shell_program() -> String` | The shell, in `$SHELL` order of preference. |
| 54 | `pub fn shell_argv(shell: &str, snippet: &str, paths: &[PathBuf]) -> Vec<String>` | The full argv for running `snippet` over `paths`. |
| 77 | `pub fn detached_argv(argv: Vec<String>) -> Vec<String>` | Wrap an argv so the child leaves delightfile's process group. |
| 102 | `pub fn spawn_detached(snippet: &str, paths: &[PathBuf], cwd: &Path) -> std::io::Result<()>` | Start `snippet` and do not wait for it (`;`, and every non-blocking opener). |
| 123 | `pub fn run_blocking(snippet: &str, paths: &[PathBuf], cwd: &Path) -> std::io::Result<i32>` | Run `snippet` to completion (`:`, and `block = true` openers). |
| 137 | `pub fn exit_text(snippet: &str, code: i32) -> String` | How a finished blocking command reads in a toast. |
| 147 | `pub fn short(snippet: &str) -> String` | A snippet, cut to something that fits on one line of chrome. |
| 162 | `pub struct Choice { pub name: String, pub command: String, pub description: String, pub block: bool }` | One way to open the file under the cursor, already detached from the config so the picker can outlive the borrow. |
| 181 | `impl Choice` · `pub fn builtin(&self) -> Option<&str>` | The job name when this is something delightfile does itself rather than a shell command (`builtin:extract`). |
| 193 | `pub fn choices_for(config: &Config, entry: &Entry) -> Vec<Choice>` | Every opener that matches `entry`, in picker order — the first is what plain `o` runs. |
| 210 | `pub const MERGED_BUILTIN: &str = "extract-merged";` | The built-in that only makes sense for several archives at once. |
| 215 | `pub fn for_archives(mut choices: Vec<Choice>, archives: usize) -> Vec<Choice>` | The choices worth offering when the targets hold `archives` archives: "Extract all into one folder" of a single archive is "Extract to folder" with a worse name, so it is dropped below two. |
| 226 | `pub struct Picker { pub choices: Vec<Choice>, pub cursor: usize, pub paths: Vec<PathBuf>, pub anchor: egui::Rect }` | The card that asks which opener to use, anchored to the row it is about. |
| 236 | `impl Picker` · `pub fn new(choices: Vec<Choice>, paths: Vec<PathBuf>, anchor: egui::Rect) -> Picker` | — |
| 245 | `impl Picker` · `pub fn move_cursor(&mut self, delta: isize)` | — |
| 253 | `impl Picker` · `pub fn chosen(&self) -> Option<&Choice>` | — |
| 260 | `pub fn picker_geometry(area: egui::Rect, anchor: egui::Rect, count: usize) -> (egui::Rect, Vec<egui::Rect>)` | The picker's card and its rows, anchored under (or over) the row it is about. |
| 297 | `pub fn paint_picker(paint: &Painting<'_>, card: egui::Rect, rects: &[egui::Rect], picker: &Picker, hovers: &Hovers<Control>, ripples: &Ripples<Control>)` | — |

**How an opener string becomes a process:**
1. The string is df-core `Opener.command` (from `DEFAULT_OPENERS`, df-core config.rs:192-275, or user config). `builtin:<job>` is intercepted by `App::launch` (app.rs:5108-5124) and never spawned.
2. Shell: `shell_program()` = `$SHELL` if set and non-blank, else `/bin/sh` (42-47).
3. argv (`shell_argv`, 54-64): `[<shell>, "-c", <snippet>, "delightfile", <path1>, <path2>, …]`. The snippet is passed whole to `-c`. Paths are **positional arguments**, so `$0` = `delightfile`, `$1` = first path, `"$@"` = all paths. Paths are converted with `to_string_lossy()`. The snippet is never string-substituted by df-app; `$1`/`$@`/`${TERMINAL:-ghostty}`/`$(dirname "$1")` are expanded by the shell.
4. Detach (`detached_argv`, 77-84): if `setsid` is found on `$PATH` (`which`, 87-92: `split_paths(PATH)` + `is_file()`), the argv becomes `["setsid", "--fork", <shell>, "-c", …]`. Most shipped opener strings also start with their own `setsid uwsm-app -- …` inside the snippet; `open` (`xdg-open "$1"`), `play` (`mpv --force-window "$@"`), `bulk-rename`, `set-wallpaper` and `optimize-avif` do not (df-core config.rs:246-268).
5. `spawn_detached` (102-119) sets `current_dir(cwd)` and stdin/stdout/stderr `null`, spawns, and `wait()`s the direct child only when `setsid` was prefixed (`setsid --fork` exits right after forking). `cwd` is `App::child_cwd()` → `spawnable_cwd` (app.rs:3241-3244, 16567-16572): the pane directory if it is a local directory, else the remote/trash/archive origin.
6. `run_blocking` (123-134) uses the same argv without `setsid`, with inherited stdio, and blocks in `.status()` on a task-engine worker (FnJob, app.rs:5157-5184). The exit code is `status.code()`, or `128 + signal` via `ExitStatusExt`.
7. **terminal-at / terminal-here / edit** are opener strings, not code:
   - `terminal-here`: `setsid uwsm-app -- "${TERMINAL:-ghostty}" --working-directory="$1" >/dev/null 2>&1`
   - `terminal-at`: `setsid uwsm-app -- "${TERMINAL:-ghostty}" --working-directory="$(dirname "$1")" >/dev/null 2>&1`
   - `edit`: `setsid uwsm-app -- "${TERMINAL:-ghostty}" -e "${EDITOR:-vi}" "$@" >/dev/null 2>&1`
   - `open`: `xdg-open "$1"` (df-core config.rs:193-267)
   - The mount card's `TERMINAL_MOUNT` is the fourth such string (mounts.rs:282).
8. `;` (non-blocking) and `:` (blocking) typed commands take the same path through `App::run_shell` (app.rs:5142-5184).

### 2.5 `icons` (src/icons.rs)

| Line | Item (signature verbatim) | First doc line |
|---|---|---|
| 70 | `pub const ICON_FAMILY: &str = "df-icons";` | The egui family the icon column is drawn in. |
| 148 | `pub fn install(ctx: &egui::Context) -> bool` | Install the icon font, and say whether the real glyphs are available. |
| 177 | `pub struct Icon { pub glyph: char, pub color: egui::Color32 }` | What to draw in a row's icon column. |
| 397 | `pub fn glyph(nerd: bool, patched: char, plain: &'static str) -> String` | A chrome glyph: the patched font's icon when [`install`] found one, and a character the stock faces are known to carry otherwise. |
| 410 | `pub fn generic(palette: &Palette, nerd: bool) -> Icon` | The plain file glyph, for a card that stands for several files at once — the ghost of the whole clipboard, dragged off the yank chip (PLAN §7.1). |
| 420 | `pub fn archive(palette: &Palette, nerd: bool) -> Icon` | The archive kind's glyph and hue, for the preview's archive header: the same mark an archive's row wears when no logo outranks it, so the header and the kind column agree about what the file is. |
| 433 | `pub const LOCK: char = '\u{f023}';` | A padlock, for an archive member that needs a password (nf-fa-lock — the glyph the `lock` extension already wears). |
| 438 | `pub const LOCK_PLAIN: &str = "🗝";` | …and its stand-in without a patched font: a key, since egui's bundled faces have no padlock (`the_chrome_glyphs_all_render` holds it to one they do draw). |
| 442 | `pub fn folder(palette: &Palette, nerd: bool) -> Icon` | The plain directory glyph, for a card that stands for a *place* rather than for a file: the ghost of a tab being dragged out of the strip (PLAN §2). |
| 453 | `pub fn network(palette: &Palette, nerd: bool) -> Icon` | A place on another machine — an `sftp://` pin or bookmark — where [`folder`] is one on this disk (nf-fa-server). |
| 480 | `pub fn icon_for(entry: &Entry, theme: &Theme, palette: &Palette, nerd: bool) -> Icon` | The icon for one row. |
| 564 | `pub fn name_color(entry: &Entry, palette: &Palette) -> egui::Color32` | The colour a row's *name* is drawn in. |
| 581 | `pub fn to_color32(c: Color) -> egui::Color32` | — |

**Font discovery.** `find_nerd_font` (100-141) scans, in order, `/usr/share/fonts/TTF`, `/usr/share/fonts/truetype`, `/usr/share/fonts/OTF`, `/usr/share/fonts/nerd-fonts`, `/usr/local/share/fonts` (76-82), then `$HOME/.local/share/fonts` and `$HOME/.fonts` (102-105). Each directory is listed once, **not recursively**. A file qualifies if its name ends `.ttf` or `.otf`. It ranks by exact stem match (case-insensitive) against `JetBrainsMonoNerdFont-Regular`, `FiraCodeNerdFont-Regular`, `CaskaydiaMonoNerdFont-Regular`, `HackNerdFont-Regular`, `SymbolsNerdFont-Regular` (87-93); any other stem containing `NerdFont` and ending `-Regular` ranks last. The winner's bytes go to egui as family `df-icons` and are **appended** as a fallback to the Proportional and Monospace families (155-170). No fontconfig or system font API is used.

**No Nerd Font found.** `install` logs "no Nerd Font found; row icons fall back to ls-style classifiers" and returns `false` (149-152), which becomes `App.nerd` (app.rs:2133). Row glyphs then become the `ls -F` classifiers `'/'` directory, `'@'` link, `'*'` executable and `' '` file (192-198), coloured the same. Chrome glyphs use `glyph(nerd, patched, plain)`'s plain string, and `LOCK_PLAIN` is `"🗝"`.

### 2.6 `format` (src/format.rs) — localtime use

| Line | Item (signature verbatim) | First doc line |
|---|---|---|
| 20 | `pub fn linemode_text(entry: &Entry, mode: LineMode) -> String` | What a row's second column says. |
| 74 | `pub fn folder_size_text(size: Option<crate::folders::Size>, count: Option<df_core::du::ChildCount>) -> Option<String>` | The size column's text for a **directory**, from whatever the walk has said so far (PLAN §7.3). |
| 106 | `pub fn human_size(bytes: u64) -> String` | Bytes, the way a file manager says them. |
| 133 | `pub fn stamp(year: i32, month: u32, day: u32, hour: u32, minute: u32) -> String` | `25-08-31 14:22` — the format yazi's linemode uses, kept for the reason every default here is kept: the column has to look like the one it replaces. |
| 150 | `pub fn long_stamp(time: Option<SystemTime>) -> String` | A timestamp for the spot panel (PLAN §6): the **full** year, and a dash when there is nothing to say. |
| 165 | `pub fn file_stamp(time: SystemTime) -> String` | `20260831_142233` — a stamp that is safe in a **file name**. |

Local time comes from exactly one private function, `civil_local(time: SystemTime) -> Option<(i32, u32, u32, u32, u32)>` (198-222). It converts to signed epoch seconds (handling pre-1970), casts to `libc::time_t`, zero-inits a `libc::tm`, and calls `libc::localtime_r(&t, &mut tm)` inside `unsafe`; a null result gives `None`, which renders as `—`. It returns `(tm_year + 1900, tm_mon + 1, tm_mday, tm_hour, tm_min)`. Callers: `local_stamp` → `time_text` → `linemode_text` (mtime/btime linemodes), `long_stamp`, `file_stamp`. One test calls it directly (`the_epoch_lands_in_the_right_year`, 327-330).

### 2.7 `window` (src/window.rs) and window creation

| Line | Item (signature verbatim) | First doc line |
|---|---|---|
| 107 | `pub fn spawn_args(dir: &Path) -> Vec<OsString>` | The command line a new window is started with. |
| 118 | `pub struct Windows` (private `children: Vec<Child>`; `#[derive(Default)]`) | Every window this process has opened, so none of them becomes a zombie. |
| 125 | `impl Windows` · `pub fn open(&mut self, dir: &Path) -> std::io::Result<()>` | Open a window on `dir`. |
| 171 | `pub const DETACH_THRESHOLD: f32 = 40.0;` | How far a tab chip must travel *vertically* before letting go of it opens a window. |
| 185 | `pub fn armed(from: egui::Pos2, at: egui::Pos2, strip: egui::Rect) -> bool` | Whether a tab drag from `from`, now at `at`, would detach if it were let go. |
| 192 | `pub enum Release { Detach, SpringBack }` | What letting go of a tab drag does. |
| 201 | `pub fn release(from: egui::Pos2, at: egui::Pos2, strip: egui::Rect) -> Release` | The release, from the same three facts [`armed`] is decided on — so what the ghost has been advertising all through the drag is what letting go does. |

**Window creation.** There is exactly one winit window per process, created in `App::init_gfx` (app.rs:2111-2196) on the first `resumed`. Attributes: `with_title(title)`, `with_inner_size(LogicalSize::new(1400.0, 900.0))` (`WINDOW_SIZE`, app.rs:88), and `with_name(app_id, app_id)` from `WindowAttributesExtWayland` (2112, 2121-2124). `app_id`/`title` are `APP_ID`/`APP_ID` ("delightfile"), or in chooser mode `PICKER_APP_ID` ("delightfile-picker") with the caller's title or `PICKER_TITLE` ("file-picker") (2114-2120). No icon, decorations, transparency, theme, min size or IME settings are set. `window_event` ignores any `WindowId` other than the one window (17066-17075).

**Per-window process model.** `Ctrl+N` (`C::NewWindow`, app.rs:8900-8903) and a detached tab (`App::release_tab_drag`, 12376-12420) call `App::open_window` → `Windows::open(dir)`:
- The child is `Command::new(std::env::current_exe()?)` with args `["--", dir]` and `current_dir(dir)` when `dir.is_dir()`; stdio is inherited and nothing detaches it (window.rs:125-141).
- The child gets no `--cwd-file`; only the process launched with it writes it (module doc 84-94).
- Handles are kept and reaped with `try_wait` on the next `open` (145-152).
- Each window has its own undo journal, task engine, clipboard and wgpu device (module doc 55-63).
- `StateStore` is last-writer-wins across processes (doc 66-79).

**Wayland-specific:** cross-window file drags depend on the compositor data device (doc 46-54, `dnd::self_mime` carries the pid so another window's drag counts as external). `with_name` is the Wayland `app_id`.

### 2.8 `cli` (src/cli.rs)

| Line | Item (signature verbatim) | First doc line |
|---|---|---|
| 41 | `pub struct Args { pub start: Option<PathBuf>, pub cwd_file: Option<PathBuf>, pub chooser: Option<Chooser> }` | What the command line asked for. |
| 56 | `pub struct Chooser { pub out: PathBuf, pub multiple: bool, pub directory: bool, pub save: bool, pub title: Option<String>, pub accept: Option<String>, pub name: Option<String>, pub filters: Vec<TypeFilter>, pub current_filter: usize }` | A file dialog, as the portal described it (`--chooser-file` and its three switches). |
| 92 | `pub use df_core::fs::TypeFilter;` | One named file-type filter, as the file-chooser portal describes it: a label for the menu and the patterns a file has to match to be shown. |
| 97 | `impl Chooser` · `pub fn new(out: PathBuf) -> Chooser` | A plain dialog writing to `out`: one file, no title, no filters. |
| 114 | `pub enum PickMode { File, Files, Folder, Save }` | What a [`Chooser`] is choosing, read off its switches in one place. |
| 132 | `impl Chooser` · `pub fn mode(&self) -> PickMode` | Which of the four dialogs this is. |
| 147 | `pub enum Outcome { Run(Args), Portal, Print(String), Fail(String) }` | What `main` should do next. |
| 160 | `pub const USAGE: &str` | The `--help` text. |
| 191 | `pub fn parse<I: IntoIterator<Item = String>>(args: I) -> Outcome` | Parse everything after the program name. |
| 327 | `pub fn read_request(path: &Path, out: PathBuf) -> Result<(Chooser, Option<PathBuf>), String>` | Read a `--chooser-request` file into the chooser it describes, answering to `out`, and the path the window should start at. |
| 355 | `pub fn parse_request(text: &str, path: &Path, out: PathBuf) -> Result<(Chooser, Option<PathBuf>), String>` | The request file's text, understood — the format `crate::portal` writes: |
| 454 | `pub fn write_cwd_file(path: &std::path::Path, cwd: &std::path::Path)` | Write the directory a session ended in, for the shell wrapper to read. |
| 468 | `pub fn write_chooser_file(path: &std::path::Path, paths: &[std::path::PathBuf])` | Write the paths a chooser session picked, one per line, for the portal's wrapper to read. |

Flags, and what each one depends on:

| Flag | Behaviour | Platform dependency |
|---|---|---|
| `[path]` | start directory, or a file's directory with the cursor on it | — |
| `--` | end of options | — |
| `--cwd-file=<path>` | on `q`, writes the final local directory (raw `as_encoded_bytes`, no newline) | protocol of a shell wrapper (Hyprland `Super+F` per doc 10-13) |
| `--chooser-file=<path>` | picker mode; writes picked paths newline-separated on a pick, nothing on cancel | protocol of `xdg-desktop-portal-termfilechooser` and of `--portal` |
| `--chooser-multiple`, `--chooser-directory`, `--chooser-save` | dialog kind; require `--chooser-file` | same |
| `--chooser-request=<path>` | whole dialog as TOML (`kind`, `title`, `accept`, `multiple`, `directory`, `folder`, `name`, `file`, `current_filter`, `[[filter]] name/glob/mime`); requires `--chooser-file` | written only by `--portal` |
| `--portal` | run the xdg-desktop-portal FileChooser backend on the session bus; must be the only argument | Linux-only (D-Bus session bus, xdg-desktop-portal) |
| `-h`, `--help` / `-V`, `--version` | print `USAGE` / `delightfile <CARGO_PKG_VERSION>` | — |

### 2.9 `trashview` (src/trashview.rs) and what it needs from df-core's trash

| Line | Item (signature verbatim) | First doc line |
|---|---|---|
| 78 | `pub const URL: &str = "trash://";` | The path the pane's `DirState` is called while the trash is on screen. |
| 81 | `pub const LABEL: &str = "Trash";` | What the breadcrumb says. |
| 84 | `pub struct View { pub items: Vec<TrashedItem>, pub origin: PathBuf }` | The trash the list pane is showing. |
| 98 | `impl View` · `pub fn item(&self, name: &OsStr) -> Option<&TrashedItem>` | The item a row belongs to. |
| 106 | `impl View` · `pub fn items_for(&self, paths: &[PathBuf]) -> Vec<TrashedItem>` | The items a set of row paths names, in the pane's order. |
| 117 | `pub fn crumbs() -> Vec<crate::chrome::Crumb>` | The breadcrumb: one chip that says where you are, and nothing to click into. |
| 126 | `pub fn rows(items: &[TrashedItem]) -> Vec<Entry>` | Every item as a list-pane row. |
| 137 | `pub fn row(item: &TrashedItem) -> Entry` | One item, as a row. |
| 171 | `pub fn row_from(item: &TrashedItem, meta: Option<&std::fs::Metadata>, target: Option<&std::fs::Metadata>) -> Entry` | The pure half, so the mapping is a table test rather than something you have to delete a file to check. |
| 255 | `pub fn notes(items: &[TrashedItem]) -> std::collections::HashMap<String, String>` | What the linemode column shows for each row: the directory the file came out of, keyed by the row's name. |
| 303 | `pub fn deleted_at(text: &str) -> Option<SystemTime>` | `YYYY-MM-DDThh:mm:ss` (UTC, as [`df_core::ops::trash`] writes it) as a [`SystemTime`]. |
| 347 | `pub fn restore_refusal(item: &TrashedItem) -> Option<String>` | Why a restore was refused, in the words the toast shows. |

From df-core it needs:
- `df_core::ops::TrashedItem` with fields `name: OsString` (unique within `files/`), `original: PathBuf`, `deleted_at: String` (`YYYY-MM-DDThh:mm:ss` UTC) and `trash_root`, plus methods `files_path()` (a real path inside `…/Trash/files/`, which the preview opens directly) and `is_orphan()`.
- `df_core::ops::Trash::home()` → `.list()` (app.rs:4057-4070).
- `df_core::ops::trash::restore(item, &TaskCtx)` (4119).
- `df_core::ops::purge(item, ctx)` (4158).
- `TrashJob::new(paths)` for `d`/"Empty trash" (app.rs:4439).
- `df_core::fs::{Entry, Kind, LinkTarget, mime, classify}` to build rows.

Rows need `Entry.mode/uid/gid` from `MetadataExt` (176, 212-214, 240-241). The pane path is the literal `trash://`.

### 2.10 `remote` (src/remote.rs) and what it needs from df-core's vfs

| Line | Item (signature verbatim) | First doc line |
|---|---|---|
| 94 | `pub const PREVIEW_LIMIT: u64 = 2 * 1024 * 1024;` | How big a remote text file may be before the preview stops downloading it. |
| 105 | `pub const PREVIEW_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(220);` | How long the cursor has to rest on a remote file before its preview is downloaded. |
| 110 | `pub fn display(at: &VfsPath) -> PathBuf` | The display path for a remote place: the URL, in a [`PathBuf`]. |
| 120 | `pub fn at_of(display: &Path) -> Option<VfsPath>` | The remote place a display path names, or `None` when it is a local path. |
| 126 | `pub enum Leave { Up(VfsPath), Out(PathBuf) }` | Where `←` goes from a remote directory. |
| 142 | `pub fn leave(at: &VfsPath, origin: &Path) -> Leave` | `←` from `at`, given the local directory this remote session started in. |
| 155 | `pub struct Session { pub at: VfsPath, pub origin: PathBuf, pub pending: Option<(df_core::vfs::VfsToken, VfsPath)>, … }` | The remote service the list pane is on, and everything a tab has to remember to be on it. |
| 184 | `impl Session` · `pub fn new(at: VfsPath, origin: PathBuf) -> Session` | — |
| 193 | `impl Session` · `pub fn cached(&self, at: &VfsPath) -> Option<&Vec<Entry>>` | — |
| 197 | `impl Session` · `pub fn store(&mut self, at: &VfsPath, rows: Vec<Entry>)` | — |
| 203 | `impl Session` · `pub fn invalidate(&mut self, at: &VfsPath)` | Forget a directory, so the next visit re-reads it. |
| 207 | `impl Session` · `pub fn forget_all(&mut self)` | — |
| 219 | `pub fn crumbs(at: &VfsPath) -> Vec<crate::chrome::Crumb>` | The breadcrumb for a remote place: the service as a chip, then the path. |
| 255 | `pub fn inert_remotely(command: df_core::keymap::Command) -> bool` | Which commands are inert while the list pane is on a remote service. |
| 295 | `pub fn is_remote(path: &Path) -> bool` | Is this a remote display path rather than a path on this machine? |
| 301 | `pub enum Transfer { Local, Download, Upload, Across, Mixed }` | What `p` means, decided from the two ends rather than from a mode. |
| 318 | `impl Transfer` · `pub fn of(sources: &[PathBuf], dest: &Path) -> Transfer` | `sources` are whatever is in the clipboard; `dest` is the directory `p` was pressed in. |
| 348 | `pub fn plan_upload(sources: &[PathBuf], dest: &VfsPath, taken: &[Entry]) -> PastePlan` | What uploading `sources` into `dest` would do, given what the server says is already in that directory. |
| 443 | `pub struct Temps` (private `files: HashMap<String, PathBuf>`) | Every local file a remote session has produced, and the one place they are removed from. |
| 453 | `impl Temps` · `pub fn get(&self, url: &str) -> Option<&Path>` | The local file already downloaded for `url`, if it is still there. |
| 461 | `impl Temps` · `pub fn remember(&mut self, url: impl Into<String>, local: PathBuf)` | Remember a download. |
| 471 | `impl Temps` · `pub fn forget(&mut self, url: &str)` | Drop what is known about `url` and delete its file — what an upload over, a rename of, or a delete of the remote original owes the cache. |
| 479 | `impl Temps` · `pub fn len(&self) -> usize` | How many downloads are being held. |
| 483 | `impl Temps` · `pub fn is_empty(&self) -> bool` | — |
| 492 | `impl Temps` · `pub fn clear(&mut self) -> usize` | Remove every downloaded file, and the directory they were in when it empties. |
| 538 | `pub fn previewable(entry: &Entry) -> bool` | Whether a remote row is worth spending a download on for the preview pane. |
| 565 | `pub fn card_rows(entry: &Entry, service: &str) -> Vec<(String, String)>` | The facts card for a remote row, as label/value pairs. |
| 599 | `pub fn no_body_reason(entry: &Entry) -> &'static str` | Why a remote row has no preview body, in the words the pane shows. |
| 635 | `pub fn card(paint: &crate::ui::Painting<'_>, pane: egui::Rect, entry: &Entry, service: &str, body: Option<&str>, loading: bool)` | Draw the facts card for the row under the cursor on a remote service. |

From df-core `vfs` it needs:
- `VfsPath { service, path, … }` with `parse`, `new`, `to_url`, `parent`, `join`.
- `VfsToken`, `VfsUpdate::{Started, Failed, …}`, `Attrs`, `stat_entry`, `DEFAULT_SSH_PORT`, `CONNECT_TIMEOUT`.
- `Vfs::start(Notifier)` (app.rs:3209) with `warnings()`, `service(&name)`, `services()`, `scan(at)`, `cancel(token)`, `drain()`, `stat`, `download`, `download_to_temp(&at, ctx)`, `upload`, `upload_new`, `mkdir`, `rename`, `rmdir`, `remove`.

From df-core `ops` it needs:
- `ops::paste::{Conflict, PasteItem, PastePlan, PasteMode, unique_name}`.
- `ops::trash::{MAX_TRASH_COLLISIONS, suffixed}` (remote.rs:419-421).

The pane's paths are `sftp://…` strings inside `PathBuf`. `Temps` files live where `download_to_temp` puts them (`$TMPDIR/delightfile-vfs-<pid>/`, remote.rs:433-435).

### 2.11 Thumbnail cache root (df-core `preview::cache`, used by df-app)

df-app calls `df_core::preview::cached_thumb` (grid.rs:627) and `df_core::preview::store_thumb` (preview/decode.rs:1055). df-core re-exports these at preview/mod.rs:52:

| df-core preview/cache.rs line | Item (signature verbatim) | First doc line |
|---|---|---|
| 81 | `pub const STILL_SKIP: usize = 0;` | The `skip` for a still image — the whole of what a file manager wants. |
| 128 | `pub fn cache_dir() -> Option<PathBuf>` | The shared cache directory, if it exists. |
| 136 | `pub fn cache_key(path: &Path, meta: &std::fs::Metadata, skip: usize) -> String` | The cache file name yazi would write for `path` at `skip`, given `path`'s metadata. |
| 166 | `pub fn thumb_path(path: &Path, skip: usize) -> Option<PathBuf>` | Where a thumbnail for `path` lives, whether or not it has been written. |
| 178 | `pub fn cached_thumb(path: &Path) -> Option<PathBuf>` | The cached thumbnail for `path`, if one exists and is current. |
| 195 | `pub fn store_thumb(path: &Path, skip: usize) -> Option<PathBuf>` | Where delightfile should *write* a thumbnail for `path`, creating the shared directory if this is the first program to want it. |

The root is `std::env::temp_dir()` + `"yazi-"` + `uid()` (`CACHE_DIR_PREFIX` 72; 128-131, 196-197), with `uid()` = `libc::getuid()` (206-212). The key hashes path, len, `created()`, ctime rebuilt from `MetadataExt::ctime`/`ctime_nsec` (137, 146-148), `modified()` and `skip` with xxh3-128. df-app writes JPEG thumbnails there via a `df<pid>.tmp` sibling and `rename` (preview/decode.rs:1047-1098).

### 2.12 pdfium loading (src/preview/doc/pdf.rs)

| Line | Item (signature verbatim) | First doc line |
|---|---|---|
| 71 | `pub fn pdfium() -> Option<&'static Pdfium>` | The process's one pdfium instance, or `None` when there is no usable library. |
| 105 | `pub fn available() -> bool` | Whether PDF pages can be rendered at all on this machine. |
| 110 | `pub struct Doc` (private `document: Option<PdfDocument<'static>>`, `sizes: Vec<(f32, f32)>`) | An open PDF. |
| 120 | `impl Doc` · `pub fn open(path: &Path) -> Result<Doc, String>` | — |
| 138 | `impl Doc` · `pub fn page_count(&self) -> usize` | — |
| 143 | `impl Doc` · `pub fn page_size(&self, page: usize) -> Option<(f32, f32)>` | One page's size in points, which is what the fit is computed from. |
| 148 | `impl Doc` · `pub fn render(&self, page: usize, width: u32, height: u32) -> Result<Rgba, String>` | Rasterise one page at exactly `width × height` pixels. |

Search order (`candidates`, 41-61, then 86):
1. `$DF_PDFIUM_LIB` (exact path);
2. `$HOME/.local/lib/delightfile/libpdfium.so`;
3. `$HOME/.local/lib/delightviewer/libpdfium.so`;
4. `<CARGO_MANIFEST_DIR>/../../target/libpdfium.so`, a compile-time absolute path;
5. `Pdfium::bind_to_system_library()`, which loads `libloading::library_filename("pdfium")` through the OS loader.

Each candidate is tried with `Pdfium::bind_to_library(&path)`. The result, including `None`, is memoised in a `OnceLock` and the instance is `Box::leak`ed (71-100). Nothing is linked at build time (no `static` feature). A failure falls back to the cached thumbnail and a badge, and `MISSING` is only logged. The ABI is pinned to `pdfium_7881` (Cargo.toml:63-66). Only the preview doc worker thread enters pdfium (doc 25-32).

### 2.13 `main.rs`

- `mod` list (10-65): `app, archive, bulk, chrome, cli, clipboard, dbus, dialog, dnd, finder, flip, focus, folders, format, fuzzy, graphics, grid, help, hover, icons, input, keys, menu, motion, mounts, mouse, open, overlay, panel, playback, portal, preview, remote, ripple, scrollbar, search, select, spot, sync, tab, tabs, theme, toast, trashview, tray, ui, usage, viewport, watchdog, wayland, whichkey, window`. None is `#[cfg]`-gated. The modules whose compilation depends on Linux/Unix are `wayland` (+`icon`), `dbus`, `portal` (+`request`), `mounts` (through `dbus`), `clipboard`, `format`, `open`, `trashview` and `app` (through `WindowAttributesExtWayland` and `PermissionsExt`).
- `pub struct Wake;` (75-76): the single user event.
- `main` (78-119):
  - `env_logger` with default filter `info` and millisecond timestamps (83-85), writing to stderr.
  - `cli::parse(std::env::args().skip(1))` (87).
  - `Outcome::Portal` → `std::process::exit(portal::run())` **before** any event loop (89-91). `Print` → stdout; `Fail` → stderr and exit 2.
  - `EventLoop::<Wake>::with_user_event().build()` (102).
  - `app::Waker::new(event_loop.create_proxy())` (112) is built before the window. `App::new(waker, args)` (114). `event_loop.run_app(&mut app)` (115).
- There are no crate-level attributes. `#![windows_subsystem = "windows"]` is absent.

---

## 3. Keyboard

**What keys.rs does today.**
- `WindowEvent::ModifiersChanged` stores `modifiers.state()` in `App.modifiers` (app.rs:17127).
- Every pressed `KeyboardInput` (repeats included) becomes a `Press { repeat, chord, text }` (app.rs:17128-17140):
  - `chord = keys::chord(&event, self.modifiers)`. It calls `event.key_without_modifiers()` (winit `KeyEventExtModifierSupplement`), falls back to `event.logical_key` (keys.rs:25-33), and `chord_from` maps modifiers to `df_core::keymap::Mods { ctrl: control_key(), alt: alt_key(), shift: shift_key() || implied_shift, super_key: super_key() }` (keys.rs:52-66).
  - `translate` accepts a single-`char` `Key::Character` via `df_core::keymap::Key::from_char`. It returns `implied_shift` when df-core's US-layout `unshift` table maps the glyph (df-core keymap/key.rs:155-157). Named keys covered: Esc, Enter, Tab, Backspace, Delete, Insert, Space, arrows, Home, End, PageUp/Down, F1-F12 (keys.rs:70-118). Dead keys and multi-char strings give `None`.
  - `text = keys::text(&event)` is `event.text` unless empty or containing a control char (keys.rs:43-49).
- `App::route_keys` (app.rs:5512-5575):
  - A press with **no chord** and multi-char text goes to an open prompt (`prompt_text`) or the bulk card (`bulk_text`) whole (5530-5549).
  - Otherwise the chord is used, and `text` only when the chord is `None`, turned into a chord via `Chord::from_char` of its first char (5553-5559).
  - The chord then goes through `route_chord` → menu / help / prompt / overlay / keymap.
- Printable text in prompts comes from the **chord**, not from `event.text`: `InputBuffer::feed` types a char only when `!ctrl && !alt && !super_key`, and `printable()` applies `shifted_glyph` for Shift (df-core input/mod.rs:427-438, 872-879). The same rule is in bulk.rs:1244-1256 (`typed`) and in `App::help_typing`/`help_key` (app.rs:8151-8153, 8190-8194).
- IME: no `WindowEvent::Ime` arm and no `set_ime_allowed` call exist.
- `super_key`: parsed from `super`/`win`/`cmd`/`logo` in keymap files (df-core keymap/key.rs:410-413). df-core key.rs:214-216 notes that no default binding uses it.

**Bindings with Ctrl / Alt / Super in df-core defaults** (`crates/df-core/src/keymap/defaults.rs`; no default uses `super+`). Letter chords are marked **L**.

| Binding | Command | Where (defaults.rs line, context) |
|---|---|---|
| `ctrl+p` **L** | `CommandPalette` | 61, Global |
| `ctrl+n` **L** | `NewWindow` | 69, Global |
| `ctrl+shift+z` **L** | `Undo` | 73, Global |
| `ctrl+left` | `FrameStepBack` (MediaHovered) | 99, Global |
| `ctrl+right` | `FrameStepForward` (MediaHovered) | 100, Global |
| `ctrl+m` **L** | `Mute` (MediaHovered) | 101, Global |
| `ctrl+up` | `PreviewUp` | 103, Global |
| `ctrl+down` | `PreviewDown` | 104, Global |
| `ctrl+left` | `PreviewLeft` | 105, Global |
| `ctrl+right` | `PreviewRight` | 106, Global |
| `ctrl+shift+u` **L** | `PreviewHalfPageUp` | 107, Global |
| `ctrl+shift+d` **L** | `PreviewHalfPageDown` | 108, Global |
| `ctrl+home` | `PreviewTop` | 110, Global |
| `ctrl+end` | `PreviewBottom` | 111, Global |
| `ctrl+=` | `PreviewZoomIn` | 119, Global |
| `ctrl++` | `PreviewZoomIn` | 120, Global |
| `ctrl+-` | `PreviewZoomOut` | 121, Global |
| `ctrl+0` | `PreviewZoomReset` | 122, Global |
| `ctrl+c` **L** | `CloseTab` ("Close tab, or quit if it is the last") | 127, Files |
| `ctrl+u` **L** | `HalfPageUp` | 132, Files |
| `ctrl+d` **L** | `HalfPageDown` | 133, Files |
| `ctrl+b` **L** | `PageUp` | 134, Files |
| `ctrl+f` **L** | `PageDown` | 135, Files |
| `alt+left` | `HistoryBack` | 146, Files |
| `alt+right` | `HistoryForward` | 147, Files |
| `ctrl+l` **L** | `GotoPath` | 151, Files |
| `ctrl+a` **L** | `SelectAll` | 155, Files |
| `ctrl+r` **L** | `InvertSelection` | 156, Files |
| `ctrl+g` **L** | `ToggleView` | 165, Files |
| `ctrl+enter` | `Choose` (picker "Select") | 188, Files |
| `alt+p` **L** | `PasteSync` | 196, Files |
| `ctrl+s` **L** | `CancelSearch` | 249, Files |
| `alt+[` | `TabPrev` | 296, Files |
| `alt+]` | `TabNext` | 297, Files |
| `ctrl+c` **L** | `OverlayClose` ("Cancel input") | 312, Input |
| `ctrl+b` **L** | `InputMoveLeft` | 317, Input |
| `ctrl+f` **L** | `InputMoveRight` | 318, Input |
| `alt+b` **L** | `InputWordBackward` | 319, Input |
| `alt+f` **L** | `InputWordForward` | 320, Input |
| `ctrl+left` | `InputWordBackward` | 321, Input |
| `ctrl+right` | `InputWordForward` | 322, Input |
| `ctrl+a` **L** | `InputMoveBol` | 323, Input |
| `ctrl+e` **L** | `InputMoveEol` | 324, Input |
| `ctrl+shift+left` | `InputSelectWordBackward` | 331, Input |
| `ctrl+shift+right` | `InputSelectWordForward` | 332, Input |
| `alt+left` | `InputSubwordBackward` | 336, Input |
| `alt+right` | `InputSubwordForward` | 337, Input |
| `alt+shift+left` | `InputSelectSubwordBackward` | 338, Input |
| `alt+shift+right` | `InputSelectSubwordForward` | 339, Input |
| `ctrl+h` **L** | `InputBackspace` | 342, Input |
| `ctrl+d` **L** | `InputDeleteUnder` | 343, Input |
| `ctrl+u` **L** | `InputKillBol` | 344, Input |
| `ctrl+k` **L** | `InputKillEol` | 345, Input |
| `ctrl+w` **L** | `InputKillWordBackward` | 346, Input |
| `alt+d` **L** | `InputKillWordForward` | 347, Input |
| `ctrl+z` **L** | `InputUndo` | 348, Input |
| `ctrl+y` **L** | `InputRedo` | 349, Input |
| `ctrl+shift+z` **L** | `InputRedo` | 353, Input |
| `ctrl+[` | `Escape` | 354, Input |
| `ctrl+v` **L** | `InputPaste` | 355, Input |
| `ctrl+c` **L** | `OverlayClose` | 359, Confirm |
| `ctrl+c` **L** | `OverlayClose` | 378, Pick |
| `ctrl+s` **L** | `CancelSearch` | 383, Pick |
| `ctrl+c` **L** | `OverlayClose` | 388, Tasks |
| `ctrl+c` **L** | `OverlayClose` | 400, Spot |
| `ctrl+c` **L** | `OverlayClose` | 415, Help |
| `ctrl+u` **L** | `HelpHalfPageUp` | 424, Help |
| `ctrl+d` **L** | `HelpHalfPageDown` | 425, Help |
| `ctrl+c` **L** | `OverlayClose` | 435, Palette |
| `ctrl+p` **L** | `OverlayClose` | 436, Palette |

Outside `defaults.rs`, df-core also hard-codes chords in two editors: the readline fallback `InputBuffer::binding` (df-core input/mod.rs:478-) and the bulk-rename row editor (df-core rename/editor.rs:1373-1390). These are listed here only as locations.

**Modifier handling hard-coded in df-app** (not in the keymap):

| Binding / gesture | Effect | Where |
|---|---|---|
| `Ctrl+C` in the bulk template field | copy template selection | bulk.rs:775 (`Bulk::template_key`) |
| `Ctrl+X` in the bulk template field | cut template selection | bulk.rs:781 |
| `Ctrl+↑` / `Ctrl+↓` in bulk rows | row caret moves | bulk.rs:838, 842 (`Bulk::rows_key`) |
| `Ctrl+C` / `Ctrl+X` in bulk rows | copy / cut | bulk.rs:851, 859 |
| Ctrl / Alt / Super held → not text | bulk typing | bulk.rs:1246 (`typed`) |
| Ctrl / Alt / Super held → not typed into help filter | help sheet | app.rs:8151-8153 (`help_typing`), 8190-8194 (`help_key`) |
| pointer `toggle` = `i.modifiers.command \|\| i.modifiers.ctrl`; `shift`; `alt` | click modifiers | app.rs:13785-13787 (`App::frame`) |
| Shift-click | range select (list), extend caret selection (prompt field) | app.rs:10698 (`App::click` → `click_range`), 14513 (`App::frame`, prompt field click), 14576 (rubber band) |
| toggle-click (Ctrl, or Cmd via egui `command`) | toggle one row ("Ctrl-click" comment) | app.rs:10702-10707 (`App::click` → `toggle_row`), 14576 (band) |
| Alt-click in bulk rows | add a caret | app.rs:11601 (`App::bulk_press` → `bulk.click_row(pos, under, clicks, pointer.shift, pointer.alt)`) |
| Shift-click in bulk template | extend selection | app.rs:11588 |
| drag verb: toggle (Ctrl/Cmd) → Copy, Alt → Link, none → Move | internal drag | dnd.rs:81-87 via app.rs:12442 |
| toggle (Ctrl/Cmd) + wheel / drag in preview | zoom / gesture | app.rs:5359 (`input.ctrl = pointer.toggle`) → preview/gesture.rs:564, 672, 694 |

**Modifier names hard-coded in UI text (df-app):**

| File:line | Text |
|---|---|
| cli.rs:170 | `USAGE`: "…the Select button (`Ctrl+Enter`) writes the picked paths…" |
| overlay.rs:549 | "Type to search. Esc to close, Ctrl+s to stop." |
| app.rs:12393 | "This is the only tab — Ctrl+N opens another window" (`App::release_tab_drag`) |
| app.rs:12845 | "The clipboard stopped answering — press Ctrl+v again" (`App::clipboard_thread_gone`) |
| app.rs:16480 | `Hint::new("Ctrl+s", "stop", C::CancelSearch)` (`overlay_hints`, search overlay) |
| app.rs:16781 | "Pick files — Space or Ctrl-click selects, Enter or Select chooses" (`picker_greeting`) |

**Modifier names generated from the keymap** (df-core `Chord::label`, keymap/key.rs:323-345, prints `Ctrl+`, `Alt+`, `Super+`, `Shift+`) are rendered at:
- help.rs:205 (`all_rows`, help sheet);
- whichkey.rs:150 (`Row::of`);
- app.rs:6133 (`App::palette_rows`, command palette detail);
- app/places.rs:227 (`Place::key`), 419 (`App::key_taken`), 432 (`App::pinned_message`);
- menu.rs:474 (`command_row`) and 680 (`folder_items`), menu shortcut column.

The test keys.rs:164 asserts `"Alt+←"`.

---

## 4. Spawned processes

Non-test code in df-app. "UI thread" is the winit event-loop thread.

| File:line | Binary | Args shape | Blocking? | Detach mechanism | Kill/timeout | Notes |
|---|---|---|---|---|---|---|
| open.rs:96 via `spawn_detached` (102-119) | `setsid` when found on `$PATH`; otherwise `$SHELL` (else `/bin/sh`) | `setsid --fork <shell> -c <snippet> delightfile <path>…` (or without the `setsid --fork` prefix) | No. The spawn (112) runs on the UI thread; `wait()` (116) only reaps `setsid`, which exits after forking | `setsid --fork`; stdin/stdout/stderr → null; `current_dir(child_cwd)` | none | Callers: App::launch (app.rs:5131, non-blocking openers), App::run_shell (5145, `;`), App::connected (6575, `TERMINAL_MOUNT`). Without `setsid` the `Child` is dropped un-waited. The opener snippets themselves start `setsid uwsm-app -- …` (df-core config.rs:192-275). |
| open.rs:127 `run_blocking` (123-134) | `$SHELL` (else `/bin/sh`) | `<shell> -c <snippet> delightfile <path>…` | Yes: `.status()` on a task-engine worker (FnJob `Lane::Micro`, app.rs:5157-5184) | none; stdio inherited from delightfile | none (task cancel does not kill it) | Exit code = `code()` or `128 + signal()` (`ExitStatusExt`). Callers: `:` and `block = true` openers (e.g. df-core `bulk-rename` = `zeditor --new --wait "$@"`). |
| clipboard.rs:340 `copy` (339-383) | `wl-copy` | `--foreground [--type <mime>]`, bytes on stdin (then closed) | No: spawn + stdin write on the UI thread; `try_wait` each frame (app.rs:13175) with a 150 ms settle (`WL_COPY_SETTLE`) | none (`--foreground`: the child *is* the selection server, owned by the window) | `reap` = `kill()` + `wait()` (clipboard.rs:389-392) on the next copy (app.rs:13147) and at exit (17238) | Fallback path only (no data device, compositor refusal, 10 s no answer, Wayland thread gone). |
| clipboard.rs:398 `offered_types` | `wl-paste` | `--list-types` | Yes: `.output()` **on the UI thread** (app.rs:13361, 13439) | — | none | Fallback path only. |
| clipboard.rs:419 `paste` | `wl-paste` | `--no-newline --type <mime>` | Yes: `.output()` **on the UI thread** (app.rs:12894, 12908, 13382, 13450) | — | none | Fallback path only. |
| window.rs:128 `Windows::open` (125-141) | `std::env::current_exe()` (delightfile) | `-- <dir>`; `current_dir(dir)` if it is a directory | No (spawn on the UI thread) | none; stdio inherited; handle kept in `Windows.children` | never killed; reaped by `try_wait` on the next `open` (145-152) | Callers: App::open_window (app.rs:12079) ← `Ctrl+N` (8900-8903), tab tear-off (12404). |
| search.rs:487 `Search::spawn` (467-523), args from `build` (528-565) | `fd` (names) / `rg` (contents) | fd: `--color=never [--hidden] -- <query>`; rg: `--color=never --smart-case --line-number --column --no-heading --null --max-columns 200 [--hidden] -- <query>`; `current_dir(root)` | No: spawn on the UI thread after a 150 ms debounce (`Search::tick`, 368-378); stdout read on thread `df-search` | none; stdin/stderr null, stdout piped | `kill()` + `wait()` on `Drop for Running` (198-212: respawn, mode switch, cancel, close); `kill()` at `MAX_HITS` = 2000 (655); `wait()` at end (671) | Output parsed as `relative\n` (fd) or `relative\0line:col:text` (rg), joined to `root` (search.rs:690-731). |
| preview/listing.rs:324-330 `tar_through_7z` (320-362) | 7-Zip (`df_core::archive::external_extractor()` with `ExtractorKind::SevenZip`, listing.rs:235-239) | `x -so -- <archive>` | Yes, on the preview body worker (preview/body.rs:289 `listing::list`) | none; stdin null, stdout piped, stderr null | `kill()` + `wait()` when stopped early or with no stdout (333-334, 350-351); `wait()` otherwise (344) | stdout piped into df-core's tar lister. |
| preview/listing.rs:379-385 `list_with_7z` (370-450) | 7-Zip (same lookup) | `l -ba -slt -- <archive>` | Yes, same worker; stderr drained on a spawned thread (388-396) | none; stdin null ("stdin is `/dev/null`", doc 367) | `kill()` when capped (`total` reaches `df_core::archive::MAX_ENTRIES`, `Tally::push` 142-153) or stopped (439); `wait()` (443) | — |
| mounts.rs:683-688 `ShareListing::start` | `gio` | `mount -l` | Collected with `wait_with_output()` (704) on the `df-mounts` worker thread | none; stdin null, stdout piped, stderr null | none | Runs concurrently with the udisks2 call for `Request::List`. |
| mounts.rs:738-741 `unmount_share` | `gio` | `mount -u <url>` | Yes: `.output()` on the `df-mounts` worker | none; stdin null | none | `Request::UnmountShare`. |
| mounts.rs:845-848 `connect` | `gio` | `mount <url>` | Yes: `.output()` on a task-engine worker (FnJob, app.rs:6495-6503); then polls every 50 ms for up to 2 s for the gvfs-fuse directory (866-874) | none; stdin null | none | stdout/stderr classified by `attempt` (809-827). |
| app.rs:6575-6579 (via `open::spawn_detached`) | `setsid` → `$SHELL` → snippet | `setsid --fork <shell> -c 'setsid uwsm-app -- "${TERMINAL:-ghostty}" -e gio mount "$1"' delightfile <url>` | No | `setsid --fork` + the snippet's own `setsid uwsm-app --` | none | Connect needs a password or host key (`Connected::NeedsTerminal`). |
| portal/request.rs:483-487 `run_window` (470-525) | picker exe = `$DELIGHTFILE_PICKER_EXE` or `current_exe()` with `" (deleted)"` stripped (365-378) | `--chooser-file=<runtime>/portal-<pid>-<n>.out --chooser-request=<runtime>/portal-<pid>-<n>.toml` | Yes: the per-dialog `portal-request` thread polls `try_wait` every 50 ms (503, 522) | none; stdin null, stdout/stderr inherited | `kill()` on `Request.Close` (`Pending::close`, 393-401) and on `try_wait` error (516-517) | Only in the `--portal` process. |

Spawns inside df-core that df-app triggers (listed as locations only): `git` (df-core git/status.rs:279), 7-Zip / unzip / bsdtar / zstd (df-core archive/mod.rs:329, 393; archive/external.rs:207; archive/unpack.rs:600; archive/write/mod.rs:684), `rsync` and `ssh` (df-core sync/rsync.rs:70, 129, 134, 316, 518, 915), and the vfs's `ssh` command (df-core vfs/config.rs:183, 187).

Test-only spawns in df-app are listed in §7.

---

## 5. Build and packaging today

**`[[bin]]`.** `name = "delightfile"`, `path = "src/main.rs"` (crates/df-app/Cargo.toml:9-11). The package `description` says "…file manager for Wayland." (3). No `build.rs` exists in any workspace crate.

**`--version`.** cli.rs:215-217 prints `format!("delightfile {}\n", env!("CARGO_PKG_VERSION"))`. `CARGO_PKG_VERSION` comes from `version.workspace = true` → workspace `version = "0.1.0"` (Cargo.toml:12).

**Release profile** (Cargo.toml): `lto = "thin"`. Dev: `opt-level = 1`, dependencies `opt-level = 2`.

**CI.** There is no `.github/` directory and no `*.yml`/`*.yaml`, `rust-toolchain*`, `Makefile` or `justfile` at the repo root (`/bin/ls -a` of the root: `build`, `Cargo.lock`, `Cargo.toml`, `.claude`, `crates`, `docs`, `.git`, `.gitignore`, `LICENSE`, `PLAN.md`, `plans`, `README.md`, `target`, `TODO.md`). README.md:67-68 says building "Needs a Wayland session, a recent stable Rust, and FFmpeg 9 development libraries, which `ffmpeg-next` links against. On Arch that is `pacman -S rust ffmpeg`."

**`build/` contents:**

| File | What |
|---|---|
| `build/install.sh` | bash installer into `$HOME` (install / `--remove-portal` / `uninstall`). |
| `build/delightfile-wrapper.sh` | POSIX `sh` wrapper for `xdg-desktop-portal-termfilechooser`. It maps the portal's positional args (`multiple directory save path out debug`) to `--chooser-file="$out" [--chooser-multiple] [--chooser-directory] [--chooser-save] [-- "$path"]` and runs `delightfile "$@" \|\| true`. |
| `build/delightfile.desktop` | freedesktop desktop entry: `Name=Delight File`, `Exec=delightfile %f`, `Icon=delightfile`, `StartupWMClass=delightfile`, `Categories=System;FileTools;FileManager;`, `MimeType=inode/directory;`, `Actions=NewWindow` (`Exec=delightfile`). |
| `build/delightfile.portal` | xdg-desktop-portal backend descriptor: `DBusName=org.freedesktop.impl.portal.desktop.delightfile`, `Interfaces=org.freedesktop.impl.portal.FileChooser;`. |
| `build/org.freedesktop.impl.portal.desktop.delightfile.service.in` | D-Bus activation file template: `Name=org.freedesktop.impl.portal.desktop.delightfile`, `Exec=@BIN@ --portal`. |
| `build/delightfile.svg` | hand-authored scalable app icon. |
| `build/icons/delightfile-{48,128,256}.png` | bitmap app icons (859 B / 2.2 kB / 4.2 kB). |
| `build/test-assets.sh` | bash + `ffmpeg` generator for media fixtures into `build/assets/` (gitignored, `.gitignore`: `/build/assets`). |
| `build/assets/` | generated fixtures: `basic.mp4`, `chapters.mkv`, `cover.mp3`, `gappy.wav`, `longgop.mp4`, `still.png`, `tone.m4a`. |

**`build/install.sh`, step by step:**
1. `set -euo pipefail`; `repo` = parent of the script dir; `bin = $repo/target/release/delightfile` (23-26).
2. Paths (28-37):
   - `data = ${XDG_DATA_HOME:-$HOME/.local/share}`, `config = ${XDG_CONFIG_HOME:-$HOME/.config}`, `bindir = $HOME/.local/bin`;
   - `service = $data/dbus-1/services/org.freedesktop.impl.portal.desktop.delightfile.service`;
   - `portal_user = $data/xdg-desktop-portal/portals/delightfile.portal`;
   - `portal_system = /usr/share/xdg-desktop-portal/portals/delightfile.portal`;
   - `portals_conf = $config/xdg-desktop-portal/portals.conf`.
3. Argument dispatch (247-268): `--remove-portal` → `remove_portal` + restart note, exit. `uninstall` → `remove_portal`, `rm -f` of every installed file, `refresh_databases`, exit. No argument → install. Anything else → usage, exit 2.
4. Refuses if `$bin` is not executable ("run: cargo build --release") (270-273).
5. `install -Dm755 $bin $bindir/delightfile` (275).
6. `install -Dm644 build/delightfile.desktop $data/applications/delightfile.desktop` (278-279). The comment says the basename must stay `delightfile.desktop` because it is the Wayland `app_id`.
7. `install -Dm644 build/delightfile.svg $data/icons/hicolor/scalable/apps/delightfile.svg` (280-281); for 48/128/256, `install -Dm644 build/icons/delightfile-$size.png $data/icons/hicolor/${size}x${size}/apps/delightfile.png` (286-289).
8. `install -Dm755 build/delightfile-wrapper.sh $config/xdg-desktop-portal-termfilechooser/delightfile-wrapper.sh` (295-296).
9. `install_portal` (179-205):
   - `sed "s|@BIN@|$bindir/delightfile|"` the `.service.in` into `$service`;
   - install `delightfile.portal` into `$portal_user`;
   - detect the xdg-desktop-portal version by running `/usr/lib/xdg-desktop-portal`, `/usr/libexec/xdg-desktop-portal`, `/usr/lib/*/xdg-desktop-portal` or `/usr/local/libexec/xdg-desktop-portal --version` (43-56). If it is older than 1.20.1, also install to `/usr/share/…` via `sudo` (`as_root`, 65-75); if not found, print that command.
   - `prefer_delightfile` (109-152) adds `org.freedesktop.impl.portal.FileChooser=delightfile` under `[preferred]` in `portals.conf` unless another chooser is already named, and warns about `$XDG_CURRENT_DESKTOP`-specific `*-portals.conf`.
   - `reload_bus` (172-177): `busctl --user call org.freedesktop.DBus … ReloadConfig`.
10. `refresh_databases` (235-245): `update-desktop-database $data/applications` and `gtk-update-icon-cache -qtf $data/icons/hicolor` if present.
11. Warns if `$bindir` is not on `PATH` (307-310). It prints `xdg-mime default delightfile.desktop inode/directory` as the step that makes delightfile the file manager, notes the picker `app_id` `delightfile-picker`, and prints `systemctl --user restart xdg-desktop-portal` (312-321).

**Desktop files / icons and where they go:**

| Source | Destination |
|---|---|
| build/delightfile.desktop | `$XDG_DATA_HOME/applications/delightfile.desktop` |
| build/delightfile.svg | `$XDG_DATA_HOME/icons/hicolor/scalable/apps/delightfile.svg` |
| build/icons/delightfile-48.png, -128.png, -256.png | `$XDG_DATA_HOME/icons/hicolor/{48x48,128x128,256x256}/apps/delightfile.png` |
| build/delightfile.portal | `$XDG_DATA_HOME/xdg-desktop-portal/portals/delightfile.portal` (+ `/usr/share/xdg-desktop-portal/portals/` for xdg-desktop-portal < 1.20.1) |
| build/org.freedesktop.impl.portal.desktop.delightfile.service.in | `$XDG_DATA_HOME/dbus-1/services/org.freedesktop.impl.portal.desktop.delightfile.service` |
| build/delightfile-wrapper.sh | `$XDG_CONFIG_HOME/xdg-desktop-portal-termfilechooser/delightfile-wrapper.sh` |
| target/release/delightfile | `$HOME/.local/bin/delightfile` |

---

## 6. Vendored dv-* crates

Grepped for `cfg(`, `libc`, `std::os::`, `Command::new`, hard-coded paths and env reads in `crates/dv-core` (9,468 lines), `crates/dv-media` (4,873) and `crates/dv-playback` (6,646), then read each site. None of the three crates uses `libc`, `std::os::unix`, `nix` or `rustix`. None of their Cargo.toml files has a `[target.'cfg(...)']` table.

| Crate | File:line | Kind | What | In test code? |
|---|---|---|---|---|
| dv-media | src/decode.rs:633-636 | `cfg(target_os)` | `#[cfg(target_os = "linux")] let vaapi_ok = std::path::Path::new("/dev/dri/renderD128").exists();` / `#[cfg(not(target_os = "linux"))] let vaapi_ok = false;` inside `HwDevice::probe_all` (630-662) | no |
| dv-media | src/decode.rs:601-610 | hwaccel table | `PROBE_ORDER` = `[(AV_HWDEVICE_TYPE_VAAPI, DecodePath::Vaapi), (AV_HWDEVICE_TYPE_CUDA, DecodePath::Nvdec)]`; `DecodePath { Vaapi, Nvdec, Software }` (31-35), labels `vaapi`/`nvdec`/`sw` (38-45) | no |
| dv-media | src/decode.rs:634 | hard-coded path | `/dev/dri/renderD128` | no |
| dv-media | src/lib.rs:36, 51 | `cfg(feature = "transcribe")` | `pub mod transcribe;` and its re-export; the feature is off (workspace `dv-media = { …, default-features = false }`, and `transcribe` is not in `default`) | no |
| dv-media | src/proxy.rs:127 (`generate_proxy`, 92-235) | spawned process | `Command::new("ffmpeg")` with `-y -nostdin -v error -i <src> -map 0:v:0 [-map 0:a:0?] -c:v libx264 -crf … -preset … -g 1 -pix_fmt yuv420p -vf …`, stdin null, stdout piped, stderr to a `.part.log` file (176-178), `wait()` (215), then `rename(part, dst)` (228) | no; `generate_proxy` is not called from df-app or dv-playback (grep) |
| dv-media | src/transcribe.rs:53 | spawned process | `Command::new("ffmpeg")` | feature-gated off |
| dv-playback | src/controller.rs:1082 | hwaccel use | `let hw = HwDevice::probe_all();` once per controller thread | no |
| dv-playback | src/controller.rs:786-796 | labels | `decode_path()` maps 1/2/3 → `"vaapi"`/`"nvdec"`/`"sw"` | no |
| dv-playback | src/audio.rs:426 | audio host | `cpal::default_host()` → `default_output_device()` (cpal 0.16) | no |
| dv-core | src/names.rs:56, hash.rs:50, db.rs:1126, transcript.rs:198 | temp dirs | `std::env::temp_dir().join(…)` | yes (all inside `mod tests`) |
| dv-core | src/command.rs:262, 363, 372; edit.rs:2642, 3397, 3893; snapshot.rs:66; db.rs:1625 | Unix path literals | `"/tmp/…"`, `"/home/brian/My Videos/…"` | yes |
| dv-media | src/cache.rs:114; proxy.rs (test); transcribe.rs (test) | Unix path literals | `PathBuf::from("/cache/root")` etc. | yes |
| dv-media | tests/integration.rs:30, 46, 487, 509 | spawned process | `ffmpeg -version`; `bash build/test-assets.sh`; `ffmpeg` fixture makers | yes |
| dv-media | tests/silence.rs:13, 32 | spawned process | `ffmpeg` | yes |
| dv-media | tests/transcribe.rs:22, 31, 40, 67, 107, 151 | spawned process / env | `ffmpeg`, `espeak-ng`/`espeak`; `std::env::var("HOME")` → `.config/delightvideo/models/ggml-base.en.bin`; file is `#![cfg(feature = "transcribe")]` | yes |
| dv-playback | tests/mix_render.rs:22, 34 | spawned process | `ffmpeg -version`; `bash build/test-assets.sh` | yes |

**What decode.rs:633 chooses.** In `HwDevice::probe_all` (decode.rs:626-662) the cfg decides only whether the VAAPI row of `PROBE_ORDER` is tried:
- On Linux, `vaapi_ok` is true when `/dev/dri/renderD128` exists.
- On every other target, `vaapi_ok` is `false`, so the VAAPI row is skipped.

The loop still calls `ffi::av_hwdevice_ctx_create(&mut ctx, AV_HWDEVICE_TYPE_CUDA, null, null, 0)` for the CUDA row on every target. Whatever succeeds is pushed as a `HwDevice`, and an empty result means software decode. There is no VideoToolbox, D3D11VA, DXVA2 or Vulkan-video row. `VideoDecoder::open_with` trial-decodes per file and falls back to software (doc 626-629; dv-playback controller.rs:1081).

**Dependencies of note (manifests):**
- dv-media: `ffmpeg-next = "9.0"`, `default-features = false`, features `codec`, `format`, `software-resampling`, `software-scaling`. It links system FFmpeg, per README.md:67-68.
- dv-core: `rusqlite 0.40.1` with `bundled`, `zstd 0.13.3`, `blake3 1.8.5`.
- dv-playback: `wgpu = "29"` (default features), `cpal = "0.16"`, `nnnoiseless`, `ebur128`.

---

## 7. Tests with Unix assumptions

The scan covered every `#[test]` fn (brace-matched body) in df-app's test modules (`#[cfg(test)] mod …` to end of file; `src/app/tests/compress.rs`; `tests/portal.rs`). "Helpers" are non-`#[test]` code in the same test module. Kinds:
- **unix-ext**: `std::os::unix` / `PermissionsExt` / `MetadataExt` / `OsStrExt` / `OsStringExt` / `from_mode` / `DirBuilderExt` / `OpenOptionsExt`.
- **unix-socket**: `UnixStream` / `UnixListener`.
- **abs-path**: a string literal path starting with `/` (not `/org/…` D-Bus paths).
- **file-uri**: `file:///`.
- **spawn**: `Command::new`.
- **env**: `HOME` / `XDG_*` / `SHELL` / `PATH` / `TMPDIR`.
- **symlink**: `os::unix::fs::symlink` / `ops::symlink` / `symlink_metadata`.
- **mode-bits**: `mode: 0o…` / octal mode literals / `permissions_string`.
- **localtime**: `civil_local`.
- **sh**: `"$@"` / `"$1"` / `/bin/sh` / `/bin/zsh` / `setsid` / shebangs.

The kinds are what the code contains; no test was run on another platform.

Totals: 917 `#[test]` functions; 179 contain at least one of the kinds; 42 test modules/files are flagged (by a test or a helper).

| File (test region start) | Tests | With ≥1 kind | By kind (tests) | Helpers contain |
|---|---|---|---|---|
| src/app.rs (17288) | 108 | 14 | abs-path 12, unix-ext 1, spawn 1, mode-bits 1 | — |
| src/app/places.rs (587) | 15 | 5 | abs-path 5 | — |
| src/app/syncing.rs (479) | 20 | 6 | unix-ext 4, unix-socket 1, abs-path 2, mode-bits 3, sh 1 | — |
| src/app/tests/compress.rs (1) | 13 | 2 | abs-path 2 | — |
| src/archive.rs (662) | 12 | 6 | abs-path 6 | abs-path |
| src/bulk.rs (1402) | 32 | 8 | abs-path 8 | abs-path |
| src/chrome.rs (3664) | 29 | 9 | abs-path 9 | — |
| src/cli.rs (479) | 13 | 9 | abs-path 9 | abs-path |
| src/clipboard.rs (502) | 9 | 3 | abs-path 3, file-uri 2 | — |
| src/dbus.rs (1809) | 32 | 12 | unix-socket 6, abs-path 8, file-uri 1 | abs-path, file-uri, unix-socket |
| src/dialog.rs (2057) | 20 | 5 | abs-path 5, mode-bits 1 | abs-path |
| src/dnd.rs (698) | 15 | 4 | abs-path 4, file-uri 1 | — |
| src/finder.rs (469) | 11 | 4 | abs-path 4 | — |
| src/folders.rs (440) | 14 | 6 | abs-path 6 | abs-path |
| src/format.rs (225) | 6 | 1 | localtime 1 | — |
| src/fuzzy.rs (315) | 10 | 1 | abs-path 1 | — |
| src/icons.rs (585) | 15 | 0 | — | abs-path, mode-bits |
| src/input.rs (401) | 10 | 1 | abs-path 1 | — |
| src/mounts.rs (1888) | 21 | 10 | abs-path 9, file-uri 2 | abs-path |
| src/open.rs (379) | 7 | 4 | abs-path 3, sh 3 | abs-path, mode-bits |
| src/overlay.rs (828) | 8 | 1 | abs-path 1 | — |
| src/panel.rs (433) | 7 | 1 | abs-path 1 | — |
| src/portal/mod.rs (579) | 3 | 1 | file-uri 1 | — |
| src/portal/request.rs (714) | 11 | 10 | unix-ext 1, abs-path 10, file-uri 3, mode-bits 2 | — |
| src/preview/decode.rs (1099) | 12 | 0 | — | spawn |
| src/preview/doc/font.rs (899) | 12 | 0 | — | abs-path |
| src/preview/doc/pdf.rs (186) | 2 | 1 | env 1 | — |
| src/preview/highlight.rs (1433) | 16 | 1 | abs-path 1 | — |
| src/preview/listing.rs (614) | 9 | 1 | abs-path 1 | env, spawn |
| src/preview/mod.rs (1838) | 15 | 5 | abs-path 5 | — |
| src/preview/paint.rs (1668) | 21 | 2 | abs-path 2, mode-bits 2 | — |
| src/remote.rs (750) | 11 | 7 | abs-path 6, mode-bits 1 | mode-bits |
| src/search.rs (758) | 14 | 7 | abs-path 7 | abs-path |
| src/spot.rs (1113) | 15 | 4 | abs-path 2, mode-bits 2 | abs-path, mode-bits |
| src/sync.rs (1000) | 18 | 3 | abs-path 3 | abs-path |
| src/tab.rs (964) | 17 | 2 | abs-path 2 | mode-bits |
| src/trashview.rs (389) | 8 | 6 | unix-ext 1, abs-path 6, env 1, symlink 1 | abs-path |
| src/tray.rs (349) | 7 | 5 | abs-path 5 | — |
| src/ui.rs (1728) | 14 | 1 | abs-path 1 | — |
| src/usage.rs (352) | 10 | 7 | abs-path 7 | abs-path |
| src/window.rs (209) | 5 | 2 | abs-path 2 | — |
| tests/portal.rs (1) | 2 | 2 | abs-path 2, spawn 1, env 2 | unix-ext, spawn, mode-bits, sh |

Modules with tests and none of the kinds: flip, focus, grid, help, hover, keys, menu, motion, mouse, playback/mod, playback/strip, preview/body, preview/doc/gcode, preview/doc/mod, preview/doc/model, preview/gesture, preview/markdown, preview/prepare, ripple, scrollbar, select, tabs, theme, toast, viewport, wayland/icon, wayland/mod, whichkey.

**Specific tests, by kind, where the assumption is more than a path literal:**
- **unix-ext.** app.rs `a_click_on_the_spots_space_hint_toggles_the_chosen_bit` (`PermissionsExt`, test region 22020-22028). app/syncing.rs `a_run_with_problems_comes_back_as_a_card_naming_them`, `a_folder_that_could_not_be_read_opens_the_card_and_fails_the_task`, `a_socket_is_left_out_in_the_toast_and_is_no_problem`, `y_on_a_server_row_then_alt_p_syncs_it_down_through_rsync` (`PermissionsExt` at 728, 763, 891; `UnixListener::bind` at 790). trashview.rs `a_trashed_symlink_is_a_symlink_and_not_a_special_file` (`std::os::unix::fs::symlink`, 443). portal/request.rs `a_window_that_picks_nothing_is_a_cancel_or_a_failure` and `the_request_file_is_private_and_readable` (`PermissionsExt` mode checks, 1169-1214).
- **unix-socket.** dbus.rs: 6 tests build a peer with `UnixStream::pair()` (2261, 2308, 2349, 2838, 2895, 2929) and a `read_one(sock: &mut UnixStream)` helper (2238).
- **spawn.**
  - app.rs `an_extraction_here_lands_on_what_the_extractor_made`: `7z a …` at 17806.
  - preview/decode.rs helper `media_fixtures`: `ffmpeg -version` at 1371, `bash build/test-assets.sh` at 1381.
  - preview/listing.rs helpers `installed(name)` (PATH scan, 922-927) and `run(program, …)` (929-938), used by `a_7z_lists_through_7zip` with `7z`.
  - tests/portal.rs: `dbus-daemon`, `busctl`, `CARGO_BIN_EXE_delightfile --portal`, plus a shell-script picker stub written by `write_stub` (135-158) and made executable with `PermissionsExt`. It sets `XDG_RUNTIME_DIR`/`XDG_STATE_HOME` (182-183, 481-482) and is skipped when `dbus-daemon`/`busctl` are absent (162-164, 452-454).
- **sh.** open.rs `paths_reach_the_shell_as_arguments` (argv `["/bin/zsh", "-c", "zeditor \"$@\"", …]`), `detaching_only_prefixes_what_it_can_find` (`setsid --fork`), `opener_rules_pick_by_glob_then_mime` (asserts `edit.command.starts_with("setsid uwsm-app -- ")`, 472). app/syncing.rs `y_on_a_server_row_then_alt_p_syncs_it_down_through_rsync` (a fake `ssh` shell via `TEST_SHELL`, 403-409).
- **env.** trashview.rs `the_column_shows_where_each_row_came_from` reads `HOME` (535). preview/doc/pdf.rs `the_library_is_looked_for_in_the_documented_order` checks `HOME` and the `.local/lib/…/libpdfium.so` candidates (196-211).
- **localtime.** format.rs `the_epoch_lands_in_the_right_year` calls `civil_local(UNIX_EPOCH)` (327-330).
- **Font roots.** preview/doc/font.rs helper `a_system_font` scans `/usr/share/fonts`, `/usr/local/share/fonts`, `/Library/Fonts`, `/System/Library/Fonts` (907-918); there is no Windows root.
- **file-uri.** clipboard.rs `file_uris_round_trip_through_the_worst_names_there_are` (567-590: `/tmp/…` names through `file_uri`/`parse_file_uri`), `a_uri_list_round_trips_and_drops_what_is_not_a_file` (592-605), `the_lenient_uri_spellings_are_accepted` (607-624). dnd.rs `an_incoming_drag_is_read_as_files` (`file:///tmp/a.txt`, 1107-1139). portal/request.rs `a_path_becomes_a_uri_byte_for_byte` expects `file:///tmp/a%20b/…` (787-793), plus `save_files_names_land_in_the_chosen_folder_in_order` and `the_answer_follows_what_the_window_wrote`.

**dv-* tests (for completeness):**
- dv-core: 9 tests with Unix path literals (command.rs 1, db.rs 6, edit.rs 1, snapshot.rs 1).
- dv-media: cache.rs 1, proxy.rs 1, transcribe.rs 1 with path literals. tests/integration.rs, silence.rs and transcribe.rs spawn `ffmpeg`/`bash`/`espeak`.
- dv-playback: controller.rs 1 path literal; tests/mix_render.rs spawns `ffmpeg`/`bash`.
