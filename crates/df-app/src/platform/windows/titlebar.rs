//! The window's own top row in the title bar, on Windows
//! (`plans/other-platforms/04-windows.md` W4.39): one header where there were
//! two, the way Explorer, Terminal and Edge have one.
//!
//! **The frame stays the system's.** The window keeps its caption style, so
//! it keeps its rounded corners, its shadow, its resize edges and its three
//! caption buttons with Snap Layouts under Maximize; what changes is where
//! the client area begins. [`adopt`] puts a window procedure in front of
//! winit's (`SetWindowLongPtrW(GWLP_WNDPROC)`: winit has no hook for these
//! messages) and asks for the frame to be measured again, and the procedure
//! answers four messages before winit sees them:
//!
//! - `WM_NCCALCSIZE`: the default frame is computed and then its top given
//!   back, so the client area runs to the window's top edge — the caption is
//!   gone and the side and bottom resize borders stay. A maximized window
//!   hangs over the screen's edge by its frame on every side, so its top is
//!   brought in by the frame's depth and nothing is cut off.
//! - `WM_NCHITTEST`: the default first, which answers the side and bottom
//!   borders; a point it calls client is then [`caption::classify`]'d — a
//!   caption button, the resize edge along the top, one of the window's
//!   controls, or the title bar.
//! - `WM_DPICHANGED` and `WM_ACTIVATE`: the frame is extended into the client
//!   area again, by the caption's depth at the new DPI
//!   (`DwmExtendFrameIntoClientArea`), which is what has DWM go on drawing
//!   the caption buttons over the band.
//!
//! Every message goes to `DwmDefWindowProc` first, which answers for the
//! caption buttons DWM draws — their hover, their press, and `HTMAXBUTTON`
//! under Maximize, which is what brings up Snap Layouts — and what it does
//! not answer goes on to winit's procedure. At `WM_NCDESTROY` winit's
//! procedure is put back before the message goes on, so the window leaves as
//! it came.
//!
//! **The band.** The layout asks [`title_band`] how tall the band must be
//! and what to keep clear at its ends — the caption buttons, as DWM reports
//! them (`DWMWA_CAPTION_BUTTON_BOUNDS`), or three of the system's caption
//! button size when it does not — and after every layout says where the band
//! is and which rects in it are its controls ([`title_regions`]); the hit
//! test reads what it said last. Both run on the UI thread, which is the
//! thread that gets the messages; the lock is held for a copy and never
//! across a call into the system.
//!
//! **Light and dark.** The caption buttons are drawn on the window's side
//! only when DWM is told it (`DWMWA_USE_IMMERSIVE_DARK_MODE`); winit's own
//! theme call sets an undocumented composition attribute and not this one,
//! so [`set_theme`] sets both.
//!
//! A window with no caption — winit's full screen takes it away — is left
//! alone by every message here, and has no band.
#![allow(unsafe_code)] // the window procedure subclassed and put back, its messages' pointers read, four DWM calls; each block says why it holds

use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::Mutex;

use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{
    DwmDefWindowProc, DwmExtendFrameIntoClientArea, DwmGetWindowAttribute, DwmSetWindowAttribute,
    DWMWA_CAPTION_BUTTON_BOUNDS, DWMWA_USE_IMMERSIVE_DARK_MODE,
};
use windows_sys::Win32::Graphics::Gdi::{ClientToScreen, ScreenToClient};
use windows_sys::Win32::UI::Controls::MARGINS;
use windows_sys::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, GetDpiForWindow, GetSystemMetricsForDpi,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, DefWindowProcW, GetClientRect, GetWindowLongPtrW, GetWindowRect, IsZoomed,
    SetWindowLongPtrW, SetWindowPos, GWLP_WNDPROC, GWL_EXSTYLE, GWL_STYLE, HTCAPTION, HTCLIENT,
    HTCLOSE, HTMAXBUTTON, HTMINBUTTON, HTTOP, HTTOPLEFT, HTTOPRIGHT, NCCALCSIZE_PARAMS,
    SM_CXPADDEDBORDER, SM_CXSIZE, SM_CYSIZE, SM_CYSIZEFRAME, SWP_FRAMECHANGED, SWP_NOACTIVATE,
    SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER, WM_ACTIVATE, WM_DPICHANGED,
    WM_NCCALCSIZE, WM_NCDESTROY, WM_NCHITTEST, WNDPROC, WS_CAPTION,
};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::{Theme, Window};

use crate::platform::caption::{self, Frame, Hit, Px, Regions};
use crate::ui::TitleBand;

/// winit's window procedure, while this one stands in front of it; `0`
/// before [`adopt`] and after the window is gone.
static WINITS: AtomicIsize = AtomicIsize::new(0);

/// The window this program adopted: there is one per process
/// (`crate::window`'s essay).
static ADOPTED: AtomicIsize = AtomicIsize::new(0);

/// What the layout last said about the band, in physical pixels.
static REGIONS: Mutex<Regions> = Mutex::new(Regions::NONE);

/// Put this module's window procedure in front of winit's, extend the frame
/// over the caption and have the frame measured again. Once, right after
/// the window is made and before anything draws into it.
pub fn adopt(window: &Window) {
    let Some(hwnd) = hwnd_of(window) else {
        return;
    };
    if WINITS.load(Ordering::Relaxed) != 0 {
        return;
    }
    // SAFETY: `hwnd` is winit's live window, on the thread that made it;
    // reading its procedure and putting ours in its place are two calls on
    // that thread, so no message is dispatched between them, and ours
    // forwards to the one read, which stays valid for the window's life.
    unsafe {
        let winits = GetWindowLongPtrW(hwnd, GWLP_WNDPROC);
        if winits == 0 {
            log::warn!("titlebar: no window procedure to stand in front of");
            return;
        }
        WINITS.store(winits, Ordering::Relaxed);
        ADOPTED.store(hwnd, Ordering::Relaxed);
        if SetWindowLongPtrW(hwnd, GWLP_WNDPROC, procedure as *const () as isize) == 0 {
            log::warn!("titlebar: the window procedure would not change");
            WINITS.store(0, Ordering::Relaxed);
            ADOPTED.store(0, Ordering::Relaxed);
            return;
        }
    }
    extend(hwnd);
    // SAFETY: a live window, asked to recompute its frame (the next
    // `WM_NCCALCSIZE` is ours) without moving, sizing or reordering.
    unsafe {
        SetWindowPos(
            hwnd,
            0,
            0,
            0,
            0,
            0,
            SWP_FRAMECHANGED
                | SWP_NOMOVE
                | SWP_NOSIZE
                | SWP_NOZORDER
                | SWP_NOOWNERZORDER
                | SWP_NOACTIVATE,
        )
    };
}

/// The band the layout is given: at least as tall as the caption buttons
/// reach and clear of them at its right end, in logical points. `None`
/// before [`adopt`], and for a window with no caption.
pub fn title_band(window: &Window) -> Option<TitleBand> {
    let hwnd = adopted(window)?;
    let width = client_width(hwnd)?;
    Some(caption::band_of(
        buttons(hwnd, width),
        width,
        window.scale_factor(),
    ))
}

/// Where the band is and which rects in it are the window's own controls,
/// in logical points: what the hit test answers the next point with.
pub fn title_regions(window: &Window, band: egui::Rect, controls: &[egui::Rect]) {
    if adopted(window).is_none() {
        return;
    }
    let scale = window.scale_factor();
    let regions = Regions {
        band: Px::around(band, scale),
        controls: controls
            .iter()
            .map(|control| Px::around(*control, scale))
            .collect(),
    };
    if let Ok(mut held) = REGIONS.lock() {
        *held = regions;
    }
}

/// The window's side, for winit and for the caption buttons DWM draws.
pub fn set_theme(window: &Window, theme: Theme) {
    window.set_theme(Some(theme));
    let Some(hwnd) = hwnd_of(window) else {
        return;
    };
    let dark: BOOL = (theme == Theme::Dark).into();
    // SAFETY: the attribute is a BOOL, read from this live local for the
    // length of the call.
    unsafe {
        DwmSetWindowAttribute(
            hwnd,
            DWMWA_USE_IMMERSIVE_DARK_MODE as u32,
            (&dark as *const BOOL).cast(),
            std::mem::size_of::<BOOL>() as u32,
        )
    };
}

/// The procedure in front of winit's. It never panics: every answer is
/// arithmetic on what the system handed over, or the system's own.
unsafe extern "system" fn procedure(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let winits = WINITS.load(Ordering::Relaxed);
    if winits == 0 {
        // SAFETY: the default procedure takes any message for any window.
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    }
    // SAFETY: `winits` is the procedure `adopt` read off this window, a
    // valid `WNDPROC`, called with the message exactly as it came.
    let forward = || unsafe {
        let winits: WNDPROC = std::mem::transmute::<isize, WNDPROC>(winits);
        CallWindowProcW(winits, hwnd, message, wparam, lparam)
    };
    if message == WM_NCDESTROY {
        // Leaving as it came: winit's procedure back first, then the message
        // it expects as the window's last.
        // SAFETY: the window is still alive during `WM_NCDESTROY`.
        unsafe { SetWindowLongPtrW(hwnd, GWLP_WNDPROC, winits) };
        WINITS.store(0, Ordering::Relaxed);
        ADOPTED.store(0, Ordering::Relaxed);
        return forward();
    }
    if !captioned(hwnd) {
        return forward();
    }
    let mut answered: LRESULT = 0;
    // SAFETY: the message as it came, and a live LRESULT for the answer.
    if unsafe { DwmDefWindowProc(hwnd, message, wparam, lparam, &mut answered) } != 0 {
        return answered;
    }
    match message {
        WM_NCCALCSIZE if wparam != 0 => {
            let params = lparam as *mut NCCALCSIZE_PARAMS;
            // SAFETY: with `wparam` set, `lparam` is the system's
            // NCCALCSIZE_PARAMS for the length of this message; it is read
            // and written through the pointer, never held as a reference
            // across the call that also writes it.
            let top = unsafe { (*params).rgrc[0].top };
            let answer = forward();
            if answer != 0 {
                return answer;
            }
            let inset = if zoomed(hwnd) { resize_edge(hwnd) } else { 0 };
            // SAFETY: as above.
            unsafe { (*params).rgrc[0].top = top + inset };
            0
        }
        WM_NCHITTEST => {
            let answer = forward();
            if answer != HTCLIENT as LRESULT {
                return answer;
            }
            hit_test(hwnd, lparam).unwrap_or(answer)
        }
        WM_DPICHANGED | WM_ACTIVATE => {
            let answer = forward();
            extend(hwnd);
            answer
        }
        _ => forward(),
    }
}

/// What a point the default procedure calls client is: the screen point in
/// `lparam`, in the client area, classified.
fn hit_test(hwnd: HWND, lparam: LPARAM) -> Option<LRESULT> {
    let mut at = POINT {
        x: i32::from((lparam & 0xffff) as u16 as i16),
        y: i32::from(((lparam >> 16) & 0xffff) as u16 as i16),
    };
    // SAFETY: one POINT, live and writable for the call.
    if unsafe { ScreenToClient(hwnd, &mut at) } == 0 {
        return None;
    }
    let width = client_width(hwnd)?;
    let frame = Frame {
        width,
        resize: resize_edge(hwnd),
        maximized: zoomed(hwnd),
        buttons: Some(buttons(hwnd, width)),
    };
    let hit = {
        let regions = REGIONS.lock().ok()?;
        caption::classify((at.x, at.y), &frame, &regions)
    };
    let code = match hit {
        Hit::Client => HTCLIENT,
        Hit::Caption => HTCAPTION,
        Hit::Top => HTTOP,
        Hit::TopLeft => HTTOPLEFT,
        Hit::TopRight => HTTOPRIGHT,
        Hit::Minimize => HTMINBUTTON,
        Hit::Maximize => HTMAXBUTTON,
        Hit::Close => HTCLOSE,
    };
    Some(code as LRESULT)
}

/// Extend the frame into the client area by the caption's depth at the
/// window's DPI: the band DWM goes on drawing the caption buttons in.
fn extend(hwnd: HWND) {
    // SAFETY: a live window's style and DPI, and one RECT this function
    // owns, grown by the frame a window of that style has at that DPI.
    let top = unsafe {
        let style = GetWindowLongPtrW(hwnd, GWL_STYLE) as u32;
        let extended = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
        let mut frame = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        if AdjustWindowRectExForDpi(&mut frame, style, 0, extended, GetDpiForWindow(hwnd)) == 0 {
            return;
        }
        -frame.top
    };
    let margins = MARGINS {
        cxLeftWidth: 0,
        cxRightWidth: 0,
        cyTopHeight: top,
        cyBottomHeight: 0,
    };
    // SAFETY: the margins are live for the call.
    let result = unsafe { DwmExtendFrameIntoClientArea(hwnd, &margins) };
    if result < 0 {
        log::warn!("titlebar: the frame would not extend ({result:#x})");
    }
}

/// Where the caption buttons are, in client pixels: DWM's word for it, or
/// three of the system's caption button size in the top right corner.
fn buttons(hwnd: HWND, width: i32) -> Px {
    reported_buttons(hwnd).unwrap_or_else(|| {
        // SAFETY: two metrics at the window's DPI.
        let size = unsafe {
            let dpi = GetDpiForWindow(hwnd);
            (
                GetSystemMetricsForDpi(SM_CXSIZE, dpi),
                GetSystemMetricsForDpi(SM_CYSIZE, dpi),
            )
        };
        caption::fallback_buttons(width, size)
    })
}

/// DWM's caption button bounds, which are measured from the window's
/// corner, moved into the client area.
fn reported_buttons(hwnd: HWND) -> Option<Px> {
    let mut bounds = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    let mut window = bounds;
    let mut origin = POINT { x: 0, y: 0 };
    // SAFETY: each call writes one RECT or POINT this function owns, live
    // for its length.
    let read = unsafe {
        DwmGetWindowAttribute(
            hwnd,
            DWMWA_CAPTION_BUTTON_BOUNDS as u32,
            (&mut bounds as *mut RECT).cast(),
            std::mem::size_of::<RECT>() as u32,
        ) >= 0
            && GetWindowRect(hwnd, &mut window) != 0
            && ClientToScreen(hwnd, &mut origin) != 0
    };
    if !read || bounds.right <= bounds.left || bounds.bottom <= bounds.top {
        return None;
    }
    let bounds = Px {
        left: bounds.left,
        top: bounds.top,
        right: bounds.right,
        bottom: bounds.bottom,
    };
    Some(bounds.moved((window.left - origin.x, window.top - origin.y)))
}

/// How deep the resize edge is: the sizing frame and its padding at the
/// window's DPI, which is also how far a maximized window hangs over the
/// screen's edge.
fn resize_edge(hwnd: HWND) -> i32 {
    // SAFETY: two metrics at a live window's DPI.
    unsafe {
        let dpi = GetDpiForWindow(hwnd);
        GetSystemMetricsForDpi(SM_CYSIZEFRAME, dpi) + GetSystemMetricsForDpi(SM_CXPADDEDBORDER, dpi)
    }
}

fn client_width(hwnd: HWND) -> Option<i32> {
    let mut client = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };
    // SAFETY: one RECT, live and writable for the call.
    (unsafe { GetClientRect(hwnd, &mut client) } != 0).then_some(client.right - client.left)
}

fn zoomed(hwnd: HWND) -> bool {
    // SAFETY: a question about a live window.
    unsafe { IsZoomed(hwnd) != 0 }
}

/// Whether the window has a caption to draw into: winit's full screen takes
/// it away.
fn captioned(hwnd: HWND) -> bool {
    // SAFETY: a question about a live window.
    let style = unsafe { GetWindowLongPtrW(hwnd, GWL_STYLE) } as u32;
    style & WS_CAPTION == WS_CAPTION
}

/// `window`'s handle, when it is the window [`adopt`] took and it has a
/// caption.
fn adopted(window: &Window) -> Option<HWND> {
    let hwnd = hwnd_of(window)?;
    (hwnd == ADOPTED.load(Ordering::Relaxed) && captioned(hwnd)).then_some(hwnd)
}

/// winit's window handle for `window`.
fn hwnd_of(window: &Window) -> Option<HWND> {
    match window.window_handle().ok()?.as_raw() {
        RawWindowHandle::Win32(handle) => Some(handle.hwnd.get()),
        _ => None,
    }
}
