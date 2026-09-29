//! The window, as Windows is asked for it: a title and a size. There is no
//! `app_id` on Windows — the taskbar groups a program by its executable —
//! so there is nothing else to set.

use winit::window::{Window, WindowAttributes};

use crate::app::WINDOW_SIZE;

/// The attributes the one window is created with: `title` and the opening
/// size.
pub fn attributes(title: &str, _app_id: &str) -> WindowAttributes {
    Window::default_attributes()
        .with_title(title)
        .with_inner_size(winit::dpi::LogicalSize::new(WINDOW_SIZE.0, WINDOW_SIZE.1))
}
