//! The window, as Windows is asked for it: a title and a size. There is no
//! `app_id` on Windows — the taskbar groups a program by its executable —
//! so there is nothing else to set.
//!
//! **The size fits the screen** ([`crate::platform::fit`]). The primary
//! monitor's work area — the screen less the taskbar — is asked of the system
//! (`SystemParametersInfoW(SPI_GETWORKAREA)`, in physical pixels: winit makes
//! the process per-monitor DPI aware before any window exists), and the frame
//! a default window has at that monitor's DPI of `AdjustWindowRectExForDpi`.
//! A work area that holds the opening size and its frame changes nothing, and
//! the system places the window as it always would; a smaller one gets a
//! window cut to fit and centred in it. The primary monitor is the one asked
//! because it is the one a new window opens on.
#![allow(unsafe_code)] // two Win32 queries, each writing one RECT this function owns

use windows_sys::Win32::Foundation::RECT;
use windows_sys::Win32::UI::HiDpi::AdjustWindowRectExForDpi;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    SystemParametersInfoW, SPI_GETWORKAREA, WS_OVERLAPPEDWINDOW,
};
use winit::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use winit::event_loop::ActiveEventLoop;
use winit::window::{Window, WindowAttributes};

use crate::app::WINDOW_SIZE;
use crate::platform::fit::{fit, Area, Fitted, Frame};

/// The attributes the one window is created with: `title` and the opening
/// size, fitted to the work area when it would not fit.
pub fn attributes(title: &str, _app_id: &str, event_loop: &ActiveEventLoop) -> WindowAttributes {
    let attributes = Window::default_attributes()
        .with_title(title)
        .with_inner_size(LogicalSize::new(WINDOW_SIZE.0, WINDOW_SIZE.1));
    match fitted(event_loop) {
        Some(fitted) => attributes
            .with_inner_size(PhysicalSize::new(fitted.size.0, fitted.size.1))
            .with_position(PhysicalPosition::new(fitted.at.0, fitted.at.1)),
        None => attributes,
    }
}

/// The opening size and place, in physical pixels, when the primary
/// monitor's work area cannot hold the window; `None` when it can, or when
/// the system will not say.
fn fitted(event_loop: &ActiveEventLoop) -> Option<Fitted> {
    let scale = event_loop
        .primary_monitor()
        .map_or(1.0, |monitor| monitor.scale_factor());
    let mut work = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: SPI_GETWORKAREA writes one RECT through the pointer, which is
    // a live, writable RECT for the length of the call.
    let read =
        unsafe { SystemParametersInfoW(SPI_GETWORKAREA, 0, (&mut work as *mut RECT).cast(), 0) };
    if read == 0 || work.right <= work.left || work.bottom <= work.top {
        return None;
    }
    let mut frame = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    let dpi = (96.0 * scale).round() as u32;
    // SAFETY: the RECT is live and writable for the length of the call, which
    // grows it by the frame a window of this style has at `dpi`.
    let framed = unsafe { AdjustWindowRectExForDpi(&mut frame, WS_OVERLAPPEDWINDOW, 0, 0, dpi) };
    let frame = if framed == 0 {
        Frame::default()
    } else {
        Frame {
            left: f64::from(-frame.left),
            top: f64::from(-frame.top),
            right: f64::from(frame.right),
            bottom: f64::from(frame.bottom),
        }
    };
    let work = Area {
        x: f64::from(work.left),
        y: f64::from(work.top),
        width: f64::from(work.right - work.left),
        height: f64::from(work.bottom - work.top),
    };
    fit((WINDOW_SIZE.0 * scale, WINDOW_SIZE.1 * scale), work, frame)
}
