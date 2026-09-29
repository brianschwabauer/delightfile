//! The desktop device on Linux: the Wayland data device
//! ([`super::wayland::DataDevice`]), adopted from winit's own connection.
//! Its API is the platform's: `ready`, `set_selection`, `receive`, `drag` and
//! `poll`, answering in [`crate::platform::desktop::Event`]s.

use winit::event_loop::ActiveEventLoop;
use winit::window::Window;

use crate::app::Waker;

pub use super::wayland::DataDevice as Desktop;

/// Adopt winit's Wayland connection for [`super::wayland`].
///
/// `None` on X11, on a compositor without the protocol, or when the handles
/// cannot be had — all of which are "this desktop does not do that", not
/// errors.
pub fn start(event_loop: &ActiveEventLoop, window: &Window, waker: Waker) -> Option<Desktop> {
    use winit::raw_window_handle::{
        HasDisplayHandle, HasWindowHandle, RawDisplayHandle, RawWindowHandle,
    };
    let RawDisplayHandle::Wayland(display) = event_loop.display_handle().ok()?.as_raw() else {
        return None;
    };
    let RawWindowHandle::Wayland(surface) = window.window_handle().ok()?.as_raw() else {
        return None;
    };
    // SAFETY: both handles describe objects winit owns and keeps alive for
    // the lifetime of the window, and the device is dropped in `finish`
    // before the window is. The workspace warns on `unsafe_code`; this is
    // the one call site in df-app outside `platform::linux::wayland`, and it is
    // here rather than inside that module because this is where the two handles
    // — and the promise about their lifetime — actually come from.
    #[allow(unsafe_code)]
    unsafe {
        super::wayland::DataDevice::start(display.display, surface.surface, waker)
    }
}

/// Where the pointer is over `window`, in logical points, for a drop winit
/// reports without a position (`App::winit_drop`).
///
/// `None`: winit has no way to ask on Wayland, and on Wayland it never
/// reports such a drop either — the data device does, with its own position.
/// Under X11, where it does, the caller falls back to the last position egui
/// saw, which is the last `CursorMoved`.
pub fn pointer_position(_window: &Window) -> Option<(f32, f32)> {
    None
}
