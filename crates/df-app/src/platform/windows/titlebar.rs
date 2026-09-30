//! The window's own top row in the title bar, on Windows
//! (`plans/other-platforms/04-windows.md` W4.39): one header where there were
//! two, the way Explorer, Terminal and Edge have one.
//!
//! **The frame stays the system's.** The window keeps its caption style, so
//! it keeps its rounded corners, its shadow and its resize edges; what
//! changes is where the client area begins. [`adopt`] puts a window procedure
//! in front of winit's (`SetWindowLongPtrW(GWLP_WNDPROC)`: winit has no hook
//! for these messages) and asks for the frame to be measured again:
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
//!
//! The frame is extended one pixel into the client area
//! (`DwmExtendFrameIntoClientArea`), the least that keeps DWM treating the
//! window as one with a frame of its own.
//!
//! **The caption buttons are the window's** (Brian's call, 2026-09-30, as
//! Terminal, Chrome and Zed draw theirs). DWM draws the extended frame behind
//! the client area, and the window's DX12 surface is opaque, so buttons of
//! DWM's would be covered; the window draws three at the band's right end
//! instead ([`crate::chrome::caption_buttons`]), and the hit test calls them
//! `HTMINBUTTON`, `HTMAXBUTTON` and `HTCLOSE`, which is what brings Snap
//! Layouts up under Maximize. Over them the pointer is the title bar's, so
//! what it does comes as the non-client mouse messages, which
//! [`caption::track`] reads: `WM_NCMOUSEMOVE` lights a button (with
//! `TrackMouseEvent(TME_NONCLIENT)`, so `WM_NCMOUSELEAVE` puts it out),
//! `WM_NCLBUTTONDOWN` presses one and `WM_NCLBUTTONUP` on the same one is its
//! click, posted as the `WM_SYSCOMMAND` the system's own button would send —
//! `SC_MINIMIZE`, `SC_MAXIMIZE` or `SC_RESTORE`, `SC_CLOSE`. A press and a
//! release on a button go no further: the default procedure would track
//! buttons of the system's metrics, which are not the ones drawn. Moves go
//! on, so the system sees the pointer over Maximize. A change asks the
//! window for a frame (`RedrawWindow(RDW_INTERNALPAINT)`, as winit's
//! `request_redraw` does), and the frame reads [`caption_pointer`].
//!
//! **The band.** [`title_band`] tells the layout to keep the three buttons'
//! width clear and draw them there, and after every layout the window says
//! where the band, the buttons and its own controls are ([`title_regions`]);
//! the hit test reads what it said last. Both run on the UI thread, which is
//! the thread that gets the messages; the lock is held for a copy and never
//! across a call into the system.
//!
//! At `WM_NCDESTROY` winit's procedure is put back before the message goes
//! on, so the window leaves as it came. A window with no caption — winit's
//! full screen takes it away — is left alone by every message here, and has
//! no band.
#![allow(unsafe_code)] // the window procedure subclassed and put back, its messages' pointers read, a handful of Win32 and DWM calls; each block says why it holds

use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::Mutex;

use windows_sys::Win32::Foundation::{BOOL, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows_sys::Win32::Graphics::Dwm::{
    DwmExtendFrameIntoClientArea, DwmSetWindowAttribute, DWMWA_USE_IMMERSIVE_DARK_MODE,
};
use windows_sys::Win32::Graphics::Gdi::{RedrawWindow, ScreenToClient, RDW_INTERNALPAINT};
use windows_sys::Win32::UI::Controls::MARGINS;
use windows_sys::Win32::UI::HiDpi::{GetDpiForWindow, GetSystemMetricsForDpi};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    TrackMouseEvent, TME_LEAVE, TME_NONCLIENT, TRACKMOUSEEVENT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallWindowProcW, DefWindowProcW, GetClientRect, GetWindowLongPtrW, IsZoomed, PostMessageW,
    SetWindowLongPtrW, SetWindowPos, GWLP_WNDPROC, GWL_STYLE, HTCAPTION, HTCLIENT, HTCLOSE,
    HTMAXBUTTON, HTMINBUTTON, HTTOP, HTTOPLEFT, HTTOPRIGHT, NCCALCSIZE_PARAMS, SC_CLOSE,
    SC_MAXIMIZE, SC_MINIMIZE, SC_RESTORE, SM_CXPADDEDBORDER, SM_CYSIZEFRAME, SWP_FRAMECHANGED,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOOWNERZORDER, SWP_NOSIZE, SWP_NOZORDER, WM_NCCALCSIZE,
    WM_NCDESTROY, WM_NCHITTEST, WM_NCLBUTTONDBLCLK, WM_NCLBUTTONDOWN, WM_NCLBUTTONUP,
    WM_NCMOUSELEAVE, WM_NCMOUSEMOVE, WM_SYSCOMMAND, WNDPROC, WS_CAPTION,
};
use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
use winit::window::{Theme, Window};

use crate::platform::caption::{self, Frame, Hit, Mouse, Px, Regions};
use crate::ui::{CaptionButton, CaptionPointer, TitleBand};

/// Windows 11's caption button width, in logical points: the three the
/// window draws are this wide each.
const CAPTION_BUTTON: f32 = 46.0;

/// winit's window procedure, while this one stands in front of it; `0`
/// before [`adopt`] and after the window is gone.
static WINITS: AtomicIsize = AtomicIsize::new(0);

/// The window this program adopted: there is one per process
/// (`crate::window`'s essay).
static ADOPTED: AtomicIsize = AtomicIsize::new(0);

/// What the layout last said about the band, and what the pointer is doing
/// to the caption buttons.
struct State {
    regions: Regions,
    pointer: CaptionPointer,
    /// Whether `WM_NCMOUSELEAVE` has been asked for since the last one came.
    tracking: bool,
}

static STATE: Mutex<State> = Mutex::new(State {
    regions: Regions::NONE,
    pointer: CaptionPointer {
        hover: None,
        pressed: None,
    },
    tracking: false,
});

/// Put this module's window procedure in front of winit's, extend the frame
/// a pixel and have the frame measured again. Once, right after the window
/// is made and before anything draws into it.
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
    let margins = MARGINS {
        cxLeftWidth: 0,
        cxRightWidth: 0,
        cyTopHeight: 1,
        cyBottomHeight: 0,
    };
    // SAFETY: the margins are live for the call.
    let extended = unsafe { DwmExtendFrameIntoClientArea(hwnd, &margins) };
    if extended < 0 {
        log::warn!("titlebar: the frame would not extend ({extended:#x})");
    }
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

/// The band the layout is given: the three caption buttons the window
/// draws kept clear at its right end, in logical points. `None` before
/// [`adopt`], and for a window with no caption.
pub fn title_band(window: &Window) -> Option<TitleBand> {
    adopted(window)?;
    Some(TitleBand {
        height: 0.0,
        left_inset: 0.0,
        right_inset: CAPTION_BUTTON * 3.0,
        buttons: CAPTION_BUTTON,
    })
}

/// Where the band, the caption buttons and the window's own controls in the
/// band are, in logical points: what the hit test answers the next point
/// with.
pub fn title_regions(
    window: &Window,
    band: egui::Rect,
    buttons: Option<[egui::Rect; 3]>,
    controls: &[egui::Rect],
) {
    if adopted(window).is_none() {
        return;
    }
    let scale = window.scale_factor();
    let regions = Regions {
        band: Px::around(band, scale),
        buttons: buttons.map(|buttons| buttons.map(|button| Px::around(button, scale))),
        controls: controls
            .iter()
            .map(|control| Px::around(*control, scale))
            .collect(),
    };
    if let Ok(mut state) = STATE.lock() {
        state.regions = regions;
    }
}

/// What the pointer is doing to the caption buttons, for the frame that
/// draws them.
pub fn caption_pointer(window: &Window) -> CaptionPointer {
    if adopted(window).is_none() {
        return CaptionPointer::default();
    }
    STATE.lock().map(|state| state.pointer).unwrap_or_default()
}

/// The window's side, for winit and for DWM's frame around it.
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
        WM_NCMOUSEMOVE => {
            let over = button_of(wparam);
            heard(hwnd, Mouse::Move(over));
            if over.is_some() {
                track_leave(hwnd);
            }
            forward()
        }
        WM_NCMOUSELEAVE => {
            if let Ok(mut state) = STATE.lock() {
                state.tracking = false;
            }
            heard(hwnd, Mouse::Leave);
            forward()
        }
        WM_NCLBUTTONDOWN | WM_NCLBUTTONDBLCLK => {
            if heard(hwnd, Mouse::Down(button_of(wparam))) {
                0
            } else {
                forward()
            }
        }
        WM_NCLBUTTONUP => {
            if heard(hwnd, Mouse::Up(button_of(wparam))) {
                0
            } else {
                forward()
            }
        }
        _ => forward(),
    }
}

/// Follow a non-client mouse message on the caption buttons: a frame when
/// they look different, the command when one is clicked. Whether the message
/// was theirs alone.
fn heard(hwnd: HWND, mouse: Mouse) -> bool {
    let Some(heard) = STATE
        .lock()
        .ok()
        .map(|mut state| caption::track(&mut state.pointer, mouse))
    else {
        return false;
    };
    if heard.changed {
        // SAFETY: a live window asked for a paint message, which winit turns
        // into the frame that draws the buttons as they are now.
        unsafe { RedrawWindow(hwnd, std::ptr::null(), 0, RDW_INTERNALPAINT) };
    }
    if let Some(button) = heard.click {
        let command = match button {
            CaptionButton::Minimize => SC_MINIMIZE,
            CaptionButton::Maximize if zoomed(hwnd) => SC_RESTORE,
            CaptionButton::Maximize => SC_MAXIMIZE,
            CaptionButton::Close => SC_CLOSE,
        };
        // SAFETY: a message posted to a live window, handled when this one
        // has returned; the default procedure carries the command out.
        unsafe { PostMessageW(hwnd, WM_SYSCOMMAND, command as WPARAM, 0) };
    }
    heard.ours
}

/// Ask for `WM_NCMOUSELEAVE`, once per stay over the title bar: without it
/// the system does not say when the pointer has left a button for the
/// window or for somewhere else.
fn track_leave(hwnd: HWND) {
    let asked = STATE
        .lock()
        .map(|mut state| std::mem::replace(&mut state.tracking, true))
        .unwrap_or(true);
    if asked {
        return;
    }
    let mut track = TRACKMOUSEEVENT {
        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
        dwFlags: TME_LEAVE | TME_NONCLIENT,
        hwndTrack: hwnd,
        dwHoverTime: 0,
    };
    // SAFETY: one TRACKMOUSEEVENT, sized and live for the call, naming a
    // live window.
    if unsafe { TrackMouseEvent(&mut track) } == 0 {
        if let Ok(mut state) = STATE.lock() {
            state.tracking = false;
        }
    }
}

/// The caption button a hit-test code names.
fn button_of(code: WPARAM) -> Option<CaptionButton> {
    match code as u32 {
        HTMINBUTTON => Some(CaptionButton::Minimize),
        HTMAXBUTTON => Some(CaptionButton::Maximize),
        HTCLOSE => Some(CaptionButton::Close),
        _ => None,
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
    let frame = Frame {
        width: client_width(hwnd)?,
        resize: resize_edge(hwnd),
        maximized: zoomed(hwnd),
    };
    let hit = {
        let state = STATE.lock().ok()?;
        caption::classify((at.x, at.y), &frame, &state.regions)
    };
    let code = match hit {
        Hit::Client => HTCLIENT,
        Hit::Caption => HTCAPTION,
        Hit::Top => HTTOP,
        Hit::TopLeft => HTTOPLEFT,
        Hit::TopRight => HTTOPRIGHT,
        Hit::Button(CaptionButton::Minimize) => HTMINBUTTON,
        Hit::Button(CaptionButton::Maximize) => HTMAXBUTTON,
        Hit::Button(CaptionButton::Close) => HTCLOSE,
    };
    Some(code as LRESULT)
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
