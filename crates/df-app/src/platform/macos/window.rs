//! The window, as macOS is asked for it.
//!
//! **Option is a modifier, not a composition key.** Without
//! `OptionAsAlt::Both` winit hands over Option+p as the chord `alt+p` *and*
//! the text `π`, and an Option+letter with nothing bound to it types the
//! character it composes. With it the composed text is gone, and with it
//! dead-key composition in a prompt (Option+e, e → é): the trade a
//! keyboard-driven program makes, logged in `plans/other-platforms/02-macos.md`.
//! There is no `app_id` on macOS; the bundle identifier is what the system
//! knows a program by.
//!
//! **The size fits the screen** ([`crate::platform::fit`]): on a screen whose
//! visible frame — the screen less the menu bar and the Dock — cannot hold
//! the opening size with its title bar, the size is cut to fit. Only the
//! size: AppKit puts a new window on screen itself when it is shown.
#![allow(unsafe_code)] // `frameRectForContentRect:styleMask:` is a pure AppKit class method, declared `unsafe` by objc2's generator

use objc2_app_kit::{NSScreen, NSWindow, NSWindowStyleMask};
use objc2_foundation::{MainThreadMarker, NSPoint, NSRect, NSSize};
use winit::event_loop::ActiveEventLoop;
use winit::platform::macos::{OptionAsAlt, WindowAttributesExtMacOS};
use winit::window::{Window, WindowAttributes};

use crate::app::WINDOW_SIZE;
use crate::platform::fit::{fit, Area, Frame};

/// The attributes the one window is created with: `title`, the opening
/// size fitted to the screen, and Option read as Alt.
pub fn attributes(title: &str, _app_id: &str, _event_loop: &ActiveEventLoop) -> WindowAttributes {
    let (width, height) = fitted_size().unwrap_or(WINDOW_SIZE);
    Window::default_attributes()
        .with_title(title)
        .with_inner_size(winit::dpi::LogicalSize::new(width, height))
        .with_option_as_alt(OptionAsAlt::Both)
}

/// The opening size cut to the main screen's visible frame, in points;
/// `None` when it fits, or when there is no screen to ask.
fn fitted_size() -> Option<(f64, f64)> {
    let mtm = MainThreadMarker::new()?;
    let visible = NSScreen::mainScreen(mtm)?.visibleFrame();
    // winit's default window: titled, closable, miniaturizable, resizable.
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable;
    let content = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(100.0, 100.0));
    // SAFETY: a class method that computes a rectangle from two values and
    // touches no window; called on the main thread, as `mtm` proves.
    let frame = unsafe { NSWindow::frameRectForContentRect_styleMask(content, style, mtm) };
    let title_bar = (frame.size.height - content.size.height).max(0.0);
    let work = Area {
        x: 0.0,
        y: 0.0,
        width: visible.size.width,
        height: visible.size.height,
    };
    let frame = Frame {
        top: title_bar,
        ..Frame::default()
    };
    fit(WINDOW_SIZE, work, frame).map(|fitted| fitted.size)
}
