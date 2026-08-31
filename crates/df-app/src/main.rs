//! delightfile — a native, GPU-rendered, keyboard-first file manager for
//! Wayland (PLAN §1).
//!
//! Phase 0 is the shell alone: a winit event loop, an egui-wgpu surface, and a
//! window that repaints only when something happened. Everything after it hangs
//! off the two decisions made here — the `Wake` user event as the single
//! cross-thread wakeup, and workers being startable before the window exists.

mod app;
mod graphics;

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
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();

    let event_loop = match EventLoop::<Wake>::with_user_event().build() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("error: could not start the event loop: {e}");
            std::process::exit(1);
        }
    };
    // Built before the window (PLAN §6's cold-start ordering): the directory
    // read and the preview decoders should already be running while wgpu is
    // still negotiating an adapter, and they can only be started once there is
    // something for them to wake.
    let waker = app::Waker::new(event_loop.create_proxy());

    let mut app = app::App::new(waker);
    if let Err(e) = event_loop.run_app(&mut app) {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
