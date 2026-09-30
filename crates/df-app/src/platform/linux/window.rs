//! The window, as Linux is asked for it: a title, a size, and the Wayland
//! `app_id` a window rule and the desktop file know it by.
//!
//! The size is not fitted to the screen as it is on Windows and macOS
//! ([`crate::platform::fit`]): a Wayland client is told the size of no screen,
//! and the compositor places and, if it must, shrinks the window itself.

use winit::event_loop::ActiveEventLoop;
use winit::platform::wayland::WindowAttributesExtWayland;
use winit::window::{Window, WindowAttributes};

use crate::app::WINDOW_SIZE;

/// The attributes the one window is created with: `title`, the opening
/// size, and `app_id` as the Wayland `app_id`.
pub fn attributes(title: &str, app_id: &str, _event_loop: &ActiveEventLoop) -> WindowAttributes {
    Window::default_attributes()
        .with_title(title)
        .with_inner_size(winit::dpi::LogicalSize::new(WINDOW_SIZE.0, WINDOW_SIZE.1))
        .with_name(app_id, app_id)
}
