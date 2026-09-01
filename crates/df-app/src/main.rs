//! delightfile — a native, GPU-rendered, keyboard-first file manager for
//! Wayland (PLAN §1).
//!
//! Phase 1 is the browser: three miller columns, a cursor that obeys
//! `scrolloff`, the yazi keymap driving it, and a directory model that keeps up
//! with the filesystem underneath it. Everything hangs off the two decisions
//! made in Phase 0 — the `Wake` user event as the single cross-thread wakeup,
//! and workers being startable before the window exists.

mod app;
mod archive;
mod basket;
mod bulk;
mod chrome;
mod cli;
mod clipboard;
mod dbus;
mod dialog;
mod dnd;
mod finder;
mod flip;
mod focus;
mod format;
mod fuzzy;
mod graphics;
mod grid;
mod help;
mod hover;
mod icons;
mod input;
mod keys;
mod menu;
mod motion;
mod mounts;
mod mouse;
mod open;
mod overlay;
mod panel;
mod playback;
mod preview;
/// Remote services browsed as directories (PLAN §7.6).
mod remote;
mod ripple;
mod search;
mod select;
mod sha256;
mod spot;
mod tab;
mod tabs;
mod theme;
mod toast;
/// The trash, browsed as a directory (PLAN §7.4).
mod trashview;
mod ui;
mod usage;
mod viewport;
/// More than one window (PLAN §2): `Ctrl+N`, and dragging a tab out.
mod watchdog;
mod wayland;
mod whichkey;
mod window;

use winit::event_loop::EventLoop;

/// The only thing anyone sends the event loop: "a worker has something".
///
/// Zero-sized on purpose. It carries no payload because it is not a message —
/// results travel on the workers' own channels, and this is just the bell that
/// tells the loop to go and look. That keeps the loop's wakeup path one branch
/// wide no matter how many kinds of worker exist (PLAN §1).
#[derive(Debug, Clone, Copy)]
pub struct Wake;

fn main() {
    // Milliseconds, not seconds. PLAN §6's cold-start audit is a measurement of
    // the first few hundred milliseconds of the process — "window mapped" and
    // "first listing" land in the same second as the launch, so a
    // second-resolution timestamp cannot express the answer at all.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_millis()
        .init();

    let args = match cli::parse(std::env::args().skip(1)) {
        cli::Outcome::Run(args) => args,
        cli::Outcome::Print(text) => {
            print!("{text}");
            return;
        }
        cli::Outcome::Fail(message) => {
            eprintln!("error: {message}");
            std::process::exit(2);
        }
    };

    let event_loop = match EventLoop::<Wake>::with_user_event().build() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("error: could not start the event loop: {e}");
            std::process::exit(1);
        }
    };
    // Built before the window (PLAN §6's cold-start ordering): the directory
    // read is already in flight while wgpu is still negotiating an adapter, and
    // it can only be started once there is something for it to wake.
    let waker = app::Waker::new(event_loop.create_proxy());

    let mut app = app::App::new(waker, args);
    if let Err(e) = event_loop.run_app(&mut app) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
