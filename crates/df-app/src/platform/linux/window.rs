//! The window, as Linux is asked for it: a title, a size, and the Wayland
//! `app_id` a window rule and the desktop file know it by.
//!
//! The size is not fitted to the screen as it is on Windows and macOS
//! ([`crate::platform::fit`]): a Wayland client is told the size of no screen,
//! and the compositor places and, if it must, shrinks the window itself.
//!
//! There is no title bar to share (W4.39): the window's top row is already
//! the top of the window, so there is no band, nothing to adopt and nothing
//! to hit-test.

use winit::event_loop::ActiveEventLoop;
use winit::platform::wayland::WindowAttributesExtWayland;
use winit::window::{Theme, Window, WindowAttributes};

use crate::app::WINDOW_SIZE;
use crate::ui::TitleBand;

/// The attributes the one window is created with: `title`, the opening
/// size, and `app_id` as the Wayland `app_id`.
pub fn attributes(title: &str, app_id: &str, _event_loop: &ActiveEventLoop) -> WindowAttributes {
    Window::default_attributes()
        .with_title(title)
        .with_inner_size(winit::dpi::LogicalSize::new(WINDOW_SIZE.0, WINDOW_SIZE.1))
        .with_name(app_id, app_id)
}

/// Nothing to do to a window once it is made.
pub fn adopt(_window: &Window) {}

/// No title bar shares the window's top.
pub fn title_band(_window: &Window) -> Option<TitleBand> {
    None
}

/// Never called: there is no band.
pub fn title_regions(_window: &Window, _band: egui::Rect, _controls: &[egui::Rect]) {}

/// The window's side, for whatever the compositor draws around it.
pub fn set_theme(window: &Window, theme: Theme) {
    window.set_theme(Some(theme));
}
