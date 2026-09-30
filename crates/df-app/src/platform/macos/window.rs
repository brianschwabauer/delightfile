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
//!
//! **The title bar is the window's top row** (M2.37), as Finder's and
//! Safari's are one with their toolbars. The window is made with a
//! see-through title bar, no title, and its content running up under it
//! (`with_titlebar_transparent`, `with_title_hidden`,
//! `with_fullsize_content_view`), so the top row is drawn at the top of the
//! window with the traffic lights over its left end. [`title_band`] tells the
//! layout how far in the lights reach, read from the three buttons AppKit
//! draws, and the layout starts the row a gap past them; the band is as
//! deep as the row, the lights being shallower.
//!
//! **The lights sit level with the row** (Brian's call, 2026-09-30), as
//! Electron's `trafficLightPosition` and VS Code put them: AppKit keeps
//! them in a title bar some 28 points deep, level with the row's upper
//! half. After each layout [`band_row`] is told where the row is, and the
//! title bar (the close button's superview's superview, as Electron finds
//! it) is made twice as deep as the row's middle is low, its top still the
//! window's, and each button centred in it ([`caption::lights_at`]). The
//! buttons are read each frame and written only when they are not where
//! they should be: the first time, when the row moves, and when AppKit has
//! laid its title bar out again and put them back, which it does on its
//! own occasions (Electron's source names a resize, and a change to the
//! buttons' hidden state on macOS 26). With the band gone — full screen —
//! they are put back where AppKit had them, and its own title bar comes
//! down over the window as it always does.
//!
//! What is left of the band is still a title bar. The see-through title bar
//! lets a press through to the window, so the window answers for it:
//! [`title_regions`] is told after every layout where the band and the
//! window's controls in it are, and a press in the band on none of them
//! ([`title_press`], asked by the window before anything else sees the
//! press) is handed to winit's `drag_window` — AppKit's
//! `performWindowDragWithEvent:` with the press itself — or, pressed twice,
//! does what System Settings says a double click on a title bar does. The
//! controls keep their clicks.
//!
//! A window in full screen has no title bar over it until the pointer
//! brings one down, and no band: the layout is the one a Linux window has.
#![allow(unsafe_code)] // AppKit's window, its three buttons and the event being handled, through objc2; each call says why it holds

use std::cell::{Cell, RefCell};

use objc2::rc::Retained;
use objc2::ClassType;
use objc2_app_kit::{
    NSApplication, NSButton, NSEventType, NSScreen, NSView, NSWindow, NSWindowButton,
    NSWindowStyleMask,
};
use objc2_foundation::{ns_string, MainThreadMarker, NSPoint, NSRect, NSSize, NSUserDefaults};
use winit::event_loop::ActiveEventLoop;
use winit::platform::macos::{OptionAsAlt, WindowAttributesExtMacOS};
use winit::window::{Theme, Window, WindowAttributes};

use crate::app::WINDOW_SIZE;
use crate::platform::caption::{self, DoubleClick, Frame as Edges, Hit, Px, Regions};
use crate::platform::fit::{fit, Area, Frame};
use crate::ui::{CaptionPointer, TitleBand};

/// How far in from the window's left edge the traffic lights reach, in
/// points, when AppKit will not say: their place on a standard window.
const LIGHTS: f32 = 78.0;

thread_local! {
    /// What the layout last said about the band, in physical pixels: read
    /// by [`title_press`], on the main thread, where both run.
    static REGIONS: RefCell<Regions> = const { RefCell::new(Regions::NONE) };

    /// Where AppKit had the traffic lights before they were first moved:
    /// its title bar's depth and each button's origin, close to zoom.
    static HOME: RefCell<Option<(f64, [NSPoint; 3])>> = const { RefCell::new(None) };

    /// Whether the lights are where [`band_row`] put them, so that the band
    /// going puts them back once.
    static MOVED: Cell<bool> = const { Cell::new(false) };
}

/// Nothing to do to a window once it is made: the attributes asked for the
/// title bar it has.
pub fn adopt(_window: &Window) {}

/// The band the title bar shares with the top row: the traffic lights kept
/// clear at its left end, as far in as the rightmost of them reaches, and
/// nothing at its right. `None` in full screen, or for a window whose
/// content does not run up under its title bar; and then the lights go back
/// where AppKit had them.
pub fn title_band(window: &Window) -> Option<TitleBand> {
    let ns_window = view_of(window)?.window()?;
    let style = ns_window.styleMask();
    if !style.contains(NSWindowStyleMask::FullSizeContentView)
        || style.contains(NSWindowStyleMask::FullScreen)
    {
        put_lights_back(&ns_window);
        return None;
    }
    // A window not yet laid out may say its buttons are nowhere.
    let (left_inset, height) = lights(&ns_window)
        .filter(|(right, _)| *right >= 1.0)
        .unwrap_or((LIGHTS, 0.0));
    Some(TitleBand {
        height,
        left_inset,
        right_inset: 0.0,
        buttons: 0.0,
    })
}

/// How far in from the window's left edge the traffic lights reach, and how
/// far down from its top: the three buttons' frames in the window's own
/// coordinates, which run up from its bottom edge. `None` when AppKit has
/// no such buttons for the window.
fn lights(ns_window: &NSWindow) -> Option<(f32, f32)> {
    let top = ns_window.frame().size.height;
    let mut reach: Option<(f64, f64)> = None;
    for kind in [
        NSWindowButton::NSWindowCloseButton,
        NSWindowButton::NSWindowMiniaturizeButton,
        NSWindowButton::NSWindowZoomButton,
    ] {
        let button = ns_window.standardWindowButton(kind)?;
        let frame = button.convertRect_toView(button.bounds(), None);
        let right = frame.origin.x + frame.size.width;
        let bottom = top - frame.origin.y;
        reach = Some(match reach {
            Some((r, b)) => (r.max(right), b.max(bottom)),
            None => (right, bottom),
        });
    }
    reach.map(|(right, bottom)| (right as f32, bottom.max(0.0) as f32))
}

/// The row that went up into the band, its first line, in logical points
/// from the window's top-left: the traffic lights are centred on it, and
/// moved only when they are not already there.
pub fn band_row(window: &Window, row: egui::Rect) {
    let Some(ns_window) = view_of(window).and_then(|view| view.window()) else {
        return;
    };
    let Some((buttons, bar)) = light_views(&ns_window) else {
        return;
    };
    let button = buttons[0].frame().size.height;
    let Some((depth, y)) = caption::lights_at(f64::from(row.center().y), button) else {
        return;
    };
    // Taken the first time, while they are still where AppKit put them.
    HOME.with(|home| {
        home.borrow_mut().get_or_insert_with(|| {
            (
                bar.frame().size.height,
                buttons.each_ref().map(|button| button.frame().origin),
            )
        });
    });
    MOVED.set(true);
    let placed = (bar.frame().size.height - depth).abs() < 0.5
        && buttons
            .iter()
            .all(|button| (button.frame().origin.y - y).abs() < 0.5);
    if !placed {
        let origins = buttons
            .each_ref()
            .map(|button| NSPoint::new(button.frame().origin.x, y));
        lay(&bar, &buttons, depth, origins);
    }
}

/// Put the traffic lights back where AppKit had them, if [`band_row`] has
/// moved them since they were last put back.
fn put_lights_back(ns_window: &NSWindow) {
    if !MOVED.replace(false) {
        return;
    }
    let Some(home) = HOME.with(|home| *home.borrow()) else {
        return;
    };
    if let Some((buttons, bar)) = light_views(ns_window) {
        lay(&bar, &buttons, home.0, home.1);
    }
}

/// The three traffic lights, close to zoom, and the title bar they sit in:
/// the close button's superview's superview, as Electron finds it. Asked
/// anew each time, since AppKit may put new buttons in. `None` while any
/// is missing.
fn light_views(ns_window: &NSWindow) -> Option<([Retained<NSButton>; 3], Retained<NSView>)> {
    let [Some(close), Some(minimize), Some(zoom)] = [
        NSWindowButton::NSWindowCloseButton,
        NSWindowButton::NSWindowMiniaturizeButton,
        NSWindowButton::NSWindowZoomButton,
    ]
    .map(|kind| ns_window.standardWindowButton(kind)) else {
        return None;
    };
    // SAFETY: two reads of a live view's superview.
    let bar = unsafe { close.superview()?.superview()? };
    Some(([close, minimize, zoom], bar))
}

/// Make the title bar `depth` points deep, its top edge where it is, and
/// put the buttons at `origins` in it.
fn lay(bar: &NSView, buttons: &[Retained<NSButton>; 3], depth: f64, origins: [NSPoint; 3]) {
    let frame = bar.frame();
    let top = frame.origin.y + frame.size.height;
    // SAFETY: AppKit's own title bar and buttons, on the main thread, given
    // new frames as Electron gives them; AppKit lays them out again as it
    // pleases, and the next frame puts them back.
    unsafe {
        bar.setFrame(NSRect::new(
            NSPoint::new(frame.origin.x, top - depth),
            NSSize::new(frame.size.width, depth),
        ));
        for (button, origin) in buttons.iter().zip(origins) {
            button.setFrameOrigin(origin);
        }
    }
}

/// Where the band and the window's controls in it are, in logical points:
/// what [`title_press`] answers the next press with. There are no caption
/// buttons of the window's own; the traffic lights are AppKit's.
pub fn title_regions(
    window: &Window,
    band: egui::Rect,
    _buttons: Option<[egui::Rect; 3]>,
    controls: &[egui::Rect],
) {
    let scale = window.scale_factor();
    let regions = Regions {
        band: Px::around(band, scale),
        buttons: None,
        controls: controls
            .iter()
            .map(|control| Px::around(*control, scale))
            .collect(),
    };
    REGIONS.with(|slot| *slot.borrow_mut() = regions);
}

/// A primary press, asked of the platform before the window sees it: if it
/// is in the band on none of the window's controls, it is the title bar's,
/// and the window is moved by it — or, the second of a double click, zoomed
/// or minimized as System Settings says — and `true` says the window is not
/// to see it. Asked inside the `mouseDown:` it came in, which AppKit's
/// window drag needs.
pub fn title_press(window: &Window) -> bool {
    let Some(mtm) = MainThreadMarker::new() else {
        return false;
    };
    let Some(event) = NSApplication::sharedApplication(mtm).currentEvent() else {
        return false;
    };
    // SAFETY: reads of the event being handled.
    let (kind, clicks, at) =
        unsafe { (event.r#type(), event.clickCount(), event.locationInWindow()) };
    if kind != NSEventType::LeftMouseDown {
        return false;
    }
    let Some(view) = view_of(window) else {
        return false;
    };
    // winit's view is flipped: into it, the point is top-down, in points.
    let at = view.convertPoint_fromView(at, None);
    let scale = window.scale_factor();
    let edges = Edges {
        width: (view.bounds().size.width * scale).round() as i32,
        resize: 0,
        maximized: false,
    };
    let pixel = ((at.x * scale).floor() as i32, (at.y * scale).floor() as i32);
    let hit = REGIONS.with(|regions| caption::classify(pixel, &edges, &regions.borrow()));
    if hit != Hit::Caption {
        return false;
    }
    if clicks >= 2 {
        let Some(ns_window) = view.window() else {
            return true;
        };
        // SAFETY: the window's own actions, sent as a title bar's double
        // click sends them, on the main thread.
        unsafe {
            match double_click_setting() {
                DoubleClick::Zoom => ns_window.performZoom(None),
                DoubleClick::Minimize => ns_window.performMiniaturize(None),
                DoubleClick::Nothing => {}
            }
        }
    } else if let Err(error) = window.drag_window() {
        log::debug!("title bar: the window would not be dragged: {error}");
    }
    true
}

/// What a double click on a title bar does, as the user has set it.
fn double_click_setting() -> DoubleClick {
    // SAFETY: reads of the user's defaults, which search the global domain
    // after the application's.
    let (action, minimize) = unsafe {
        let defaults = NSUserDefaults::standardUserDefaults();
        (
            defaults
                .stringForKey(ns_string!("AppleActionOnDoubleClick"))
                .map(|action| action.to_string()),
            defaults.boolForKey(ns_string!("AppleMiniaturizeOnDoubleClick")),
        )
    };
    caption::double_click(action.as_deref(), minimize)
}

/// The traffic lights are AppKit's: there are no buttons of the window's
/// own to point at.
pub fn caption_pointer(_window: &Window) -> CaptionPointer {
    CaptionPointer::default()
}

/// The window's side, for the title bar AppKit draws.
pub fn set_theme(window: &Window, theme: Theme) {
    window.set_theme(Some(theme));
}

/// The attributes the one window is created with: `title` (hidden, but the
/// Window menu lists the window by it), the opening size fitted to the
/// screen, Option read as Alt, and the title bar see-through over the
/// window's top row.
pub fn attributes(title: &str, _app_id: &str, _event_loop: &ActiveEventLoop) -> WindowAttributes {
    let (width, height) = fitted_size().unwrap_or(WINDOW_SIZE);
    Window::default_attributes()
        .with_title(title)
        .with_inner_size(winit::dpi::LogicalSize::new(width, height))
        .with_option_as_alt(OptionAsAlt::Both)
        .with_titlebar_transparent(true)
        .with_fullsize_content_view(true)
        .with_title_hidden(true)
}

/// winit's view for `window`: `None` for a window that is not an AppKit
/// one, or off the main thread.
fn view_of(window: &Window) -> Option<Retained<NSView>> {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    MainThreadMarker::new()?;
    let RawWindowHandle::AppKit(handle) = window.window_handle().ok()?.as_raw() else {
        return None;
    };
    // SAFETY: the handle is winit's `NSView`, alive as long as the window,
    // and retained here so it outlives this borrow.
    let view: &NSView = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
    Some(view.retain())
}

/// The opening size cut to the main screen's visible frame, in points;
/// `None` when it fits, or when there is no screen to ask.
fn fitted_size() -> Option<(f64, f64)> {
    let mtm = MainThreadMarker::new()?;
    let visible = NSScreen::mainScreen(mtm)?.visibleFrame();
    // winit's default window — titled, closable, miniaturizable, resizable —
    // with its content under the title bar, as `attributes` asks: AppKit
    // says how much taller than its content such a window is.
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable
        | NSWindowStyleMask::FullSizeContentView;
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
