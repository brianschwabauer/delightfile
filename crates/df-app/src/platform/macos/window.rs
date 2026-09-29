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

use winit::platform::macos::{OptionAsAlt, WindowAttributesExtMacOS};
use winit::window::{Window, WindowAttributes};

use crate::app::WINDOW_SIZE;

/// The attributes the one window is created with: `title`, the opening
/// size, and Option read as Alt.
pub fn attributes(title: &str, _app_id: &str) -> WindowAttributes {
    Window::default_attributes()
        .with_title(title)
        .with_inner_size(winit::dpi::LogicalSize::new(WINDOW_SIZE.0, WINDOW_SIZE.1))
        .with_option_as_alt(OptionAsAlt::Both)
}
