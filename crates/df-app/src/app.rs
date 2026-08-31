//! The event loop: winit's [`ApplicationHandler`] and the one place a frame is
//! decided on.
//!
//! The rule this file exists to enforce (PLAN §1) is **repaint on event, never
//! poll**. `ControlFlow::Wait` is the resting state; a frame happens because
//! something happened — a key, a pointer move, a resize, a worker ringing the
//! [`Wake`](crate::Wake) bell — or because an animation asked for its next one
//! by a deadline (`ControlFlow::WaitUntil`). An idle delightfile costs zero
//! repaints, and every animation added later has to be able to say when it has
//! arrived, or it breaks that.

use std::sync::Arc;
use std::time::{Duration, Instant};

use winit::application::ApplicationHandler;
use winit::event::WindowEvent;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoopProxy};
use winit::window::{Window, WindowId};

use crate::graphics::{Gfx, GfxError};
use crate::hover::{pressed_rect, Hovers};
use crate::ripple::Ripples;

/// Opening size, in logical pixels. Wide enough for the `[1, 4, 3]` miller
/// columns (PLAN §2) to each be usable at once — the middle column is the one
/// being read, and at much under this the preview stops being worth its share.
/// Tiled compositors override it immediately; it only decides the floating and
/// first-run case.
const WINDOW_SIZE: (f64, f64) = (1400.0, 900.0);

/// Wayland `app_id`. Matches the desktop file and whatever window rule the user
/// writes, so it must never change casually.
const APP_ID: &str = "delightfile";

/// Ignore an egui repaint deadline further out than this and just go to sleep.
/// egui signals "no repaint needed" as a duration near `Duration::MAX`; any
/// real animation is milliseconds away, so anything past an hour is that
/// sentinel wearing a number.
const REPAINT_HORIZON: Duration = Duration::from_secs(3600);

/// Wakes the event loop from a worker thread.
///
/// The **only** cross-thread wakeup mechanism in delightfile (PLAN §1). Workers
/// publish their results on their own channels and then ring this bell; the
/// loop drains the channels in `user_event`. The alternative — a short
/// `WaitUntil` poll while any work is in flight — is what delightviewer removed
/// when playback arrived, and starting without it means never having to.
///
/// Cheap to clone, and safe to hold after the loop has exited: a send to a
/// dead proxy is an error this deliberately drops, because "the window is
/// gone" is not something a worker can or should do anything about.
#[derive(Clone)]
pub struct Waker(Arc<dyn Fn() + Send + Sync>);

impl Waker {
    pub fn new(proxy: EventLoopProxy<crate::Wake>) -> Waker {
        Waker(Arc::new(move || {
            let _ = proxy.send_event(crate::Wake);
        }))
    }

    /// Ask the event loop for a pass through `user_event`.
    pub fn wake(&self) {
        (self.0)()
    }
}

impl std::fmt::Debug for Waker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Waker")
    }
}

/// The whole application.
pub struct App {
    /// `None` until `resumed` — winit only hands out a window once the platform
    /// is ready, and on Wayland that is not at construction time.
    gfx: Option<Gfx>,
    /// The handle worker threads take a clone of (Phase 1 onward: the directory
    /// reader, the task pool, the preview decoders). Built in `main` before the
    /// window exists, so that the cold-start ordering PLAN §6 asks for —
    /// workers started *before* the window — is possible without rewiring
    /// anything here.
    waker: Waker,
    /// When the next frame is owed, if one is. `None` means "asleep until
    /// something happens" — the resting state.
    repaint_at: Option<Instant>,
    /// Set once, so the cold-start line is logged for the first frame only.
    logged_first_frame: bool,
    /// Hover/press amounts for every control on screen (PLAN §8). One instance
    /// for the whole window rather than one per pane: the pointer is only ever
    /// over one thing, and a single list is also a single `animating()` to ask.
    hovers: Hovers<Control>,
    /// Click ripples in flight.
    ripples: Ripples<Control>,
}

/// Everything the pointer can be over. One flat enum for the window, the way
/// delightviewer keys its hover map off a single `Hit` — Phase 1 grows it into
/// rows, tabs and breadcrumb segments, and nothing about the hover or ripple
/// machinery changes when it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Control {
    /// The Phase 0 placeholder card. Deleted with the placeholder.
    Card,
}

impl App {
    pub fn new(waker: Waker) -> App {
        App {
            gfx: None,
            waker,
            repaint_at: None,
            logged_first_frame: false,
            hovers: Hovers::new(),
            ripples: Ripples::new(),
        }
    }

    fn init_gfx(&mut self, event_loop: &ActiveEventLoop) -> Result<(), GfxError> {
        use winit::platform::wayland::WindowAttributesExtWayland;

        let attrs = Window::default_attributes()
            .with_title("delightfile")
            .with_inner_size(winit::dpi::LogicalSize::new(WINDOW_SIZE.0, WINDOW_SIZE.1))
            .with_name(APP_ID, APP_ID);
        let window = Arc::new(
            event_loop
                .create_window(attrs)
                .map_err(|e| GfxError(format!("create window: {e}")))?,
        );
        let gfx = Gfx::new(window)?;

        // egui can decide it needs a frame from a thread that is not this one —
        // a loading spinner, an animation driven by a background value. Without
        // a callback here that request lands nowhere, because nothing is
        // polling the context. Routing it through the same bell keeps one
        // wakeup path (PLAN §1). Only immediate requests ring it: a *delayed*
        // one is already carried by `repaint_delay` in `redraw`, and waking now
        // for a frame wanted later is how a wait turns into a poll.
        let waker = self.waker.clone();
        gfx.egui_ctx.set_request_repaint_callback(move |info| {
            if info.delay.is_zero() {
                waker.wake();
            }
        });

        self.gfx = Some(gfx);
        Ok(())
    }

    /// Drain whatever the workers finished. Returns true when something changed
    /// and a frame is owed.
    ///
    /// Nothing to drain yet — the seam is here from the start because every
    /// worker added later reports through it, and a loop that grows a second
    /// way to notice work is a loop that starts polling.
    fn poll_workers(&mut self) -> bool {
        false
    }

    /// Everything this frame draws. One `&mut Ui` covering the window; painting
    /// is done through the painter rather than egui widgets, because the whole
    /// visual language (PLAN §8) is hand-drawn — rows, ripples, scrims — and
    /// mixing in a widget theme would only be a second set of rules to fight.
    fn frame(&mut self, ui: &mut egui::Ui) {
        let now = Instant::now();
        let area = ui.max_rect();
        let painter = ui.painter().clone();
        painter.rect_filled(area, 0, crate::graphics::BG);

        // The Phase 0 placeholder, promoted to a card so the motion vocabulary
        // (PLAN §8) has something to be verified on before there are rows to
        // put it on: hover fades in instantly and out over `hover::FADE`, the
        // card sinks under a press, and a click leaves a ripple.
        let card = egui::Rect::from_center_size(area.center(), CARD_SIZE);
        let (pointer, down, just_pressed) = ui.input(|i| {
            (
                i.pointer.interact_pos(),
                i.pointer.primary_down(),
                i.pointer.primary_pressed(),
            )
        });
        // Hit-testing against the *unpressed* rect: a control that shrinks out
        // from under the pointer it is reacting to would flicker its own hover
        // off at the moment of the click.
        let over = pointer.is_some_and(|p| card.contains(p));
        self.hovers.tick(
            over.then_some(Control::Card),
            (over && down).then_some(Control::Card),
            now,
        );
        if let Some(p) = pointer.filter(|_| over && just_pressed) {
            // On mouse-down, not on click: the ripple acknowledges the press,
            // and waiting for the release would put it after whatever the click
            // did.
            self.ripples.spawn(Control::Card, p, card, now);
        }
        self.ripples.tick(now);

        let hover = self.hovers.hover(Control::Card);
        let press = self.hovers.press(Control::Card);
        let rect = pressed_rect(card, press);
        painter.rect_filled(rect, CARD_RADIUS, mix(CARD_FILL, CARD_FILL_HOT, hover));
        // Ripples are clipped to the card, so an expanding circle stops at the
        // surface it belongs to. The clip is rectangular — egui has no rounded
        // clip — which costs a few pixels in the corners at exactly the moment
        // the ripple is faintest.
        let inside = painter.with_clip_rect(rect);
        for s in self.ripples.splashes(Control::Card, now) {
            inside.circle_filled(
                s.center,
                s.radius,
                egui::Color32::from_white_alpha((s.alpha * 255.0).round() as u8),
            );
        }
        painter.text(
            rect.center(),
            egui::Align2::CENTER_CENTER,
            "delightfile",
            egui::FontId::proportional(PLACEHOLDER_SIZE),
            PLACEHOLDER_COLOR,
        );

        // The repaint discipline, in one place (PLAN §1): a frame is asked for
        // only while something is actually moving. A pointer parked on the card
        // holds a 1.0 that will be 1.0 again next frame, and `animating()`
        // says so — idle costs zero frames, hover included.
        if self.hovers.animating() || self.ripples.animating(now) {
            ui.ctx().request_repaint();
        }
    }

    fn redraw(&mut self) {
        self.repaint_at = None;
        // Drain first, so this frame already carries whatever the workers
        // finished while it was being asked for.
        self.poll_workers();
        let (raw_input, ctx) = {
            let Some(gfx) = &mut self.gfx else { return };
            (
                gfx.egui_state.take_egui_input(&gfx.window),
                gfx.egui_ctx.clone(),
            )
        };

        let mut full_output = ctx.run_ui(raw_input, |ui| self.frame(ui));

        let platform_output = std::mem::take(&mut full_output.platform_output);
        let Some(gfx) = &mut self.gfx else { return };
        gfx.egui_state
            .handle_platform_output(&gfx.window, platform_output);

        let repaint_delay = full_output
            .viewport_output
            .get(&egui::ViewportId::ROOT)
            .map(|vp| vp.repaint_delay);

        if !gfx.present(full_output) {
            gfx.window.request_redraw();
            return;
        }
        if !self.logged_first_frame {
            self.logged_first_frame = true;
            log::info!(
                "window mapped {}x{}",
                gfx.surface_config.width,
                gfx.surface_config.height
            );
        }

        // Repaint policy: egui says when it next needs a frame. Zero means
        // "immediately" (something is mid-animation); anything past the horizon
        // is the "never" sentinel and is dropped so the loop can actually
        // sleep.
        match repaint_delay {
            Some(delay) if delay.is_zero() => gfx.window.request_redraw(),
            Some(delay) if delay < REPAINT_HORIZON => {
                self.repaint_at = Some(Instant::now() + delay);
            }
            _ => {}
        }
    }
}

impl ApplicationHandler<crate::Wake> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.gfx.is_none() {
            if let Err(e) = self.init_gfx(event_loop) {
                log::error!("{e}");
                event_loop.exit();
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        let Some(gfx) = &mut self.gfx else { return };
        let response = gfx.egui_state.on_window_event(&gfx.window, &event);
        if response.repaint {
            gfx.window.request_redraw();
        }
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                gfx.resize(size.width, size.height);
                gfx.window.request_redraw();
            }
            WindowEvent::RedrawRequested => self.redraw(),
            _ => {}
        }
    }

    /// A worker has something, or egui asked for a frame off-thread. The only
    /// cross-thread path into the loop.
    ///
    /// The redraw is unconditional rather than gated on `poll_workers`: a
    /// `Wake` is only ever sent by something that has already decided a frame
    /// is warranted, so second-guessing it here would drop egui's own requests
    /// on the floor. It stays event-driven — no `Wake`, no frame.
    fn user_event(&mut self, _event_loop: &ActiveEventLoop, _event: crate::Wake) {
        self.poll_workers();
        if let Some(gfx) = &self.gfx {
            gfx.window.request_redraw();
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        match self.repaint_at {
            Some(at) if at <= now => {
                self.repaint_at = None;
                if let Some(gfx) = &self.gfx {
                    gfx.window.request_redraw();
                }
            }
            Some(at) => event_loop.set_control_flow(ControlFlow::WaitUntil(at)),
            // The resting state, and the one that has to stay reachable: no
            // deadline, no poll, no frame until something happens.
            None => event_loop.set_control_flow(ControlFlow::Wait),
        }
    }

    /// winit's last callback, and the only point at which the Wayland
    /// connection is still alive. egui-winit's clipboard worker must be joined
    /// here rather than when `run_app` drops us — the same SIGSEGV-at-quit
    /// delightviewer hit — so anything holding a platform resource is dropped
    /// in this window and not later.
    fn exiting(&mut self, _event_loop: &ActiveEventLoop) {
        self.gfx = None;
    }
}

/// Placeholder label size, in logical points. Nothing depends on it; it exists
/// only until the miller columns land and is the first thing Phase 1 deletes.
const PLACEHOLDER_SIZE: f32 = 15.0;

/// catppuccin-mocha `text` (#cdd6f4) — the palette's default foreground, so the
/// placeholder is already the right color against `base` rather than a stand-in
/// that has to be re-picked.
const PLACEHOLDER_COLOR: egui::Color32 = egui::Color32::from_rgb(0xcd, 0xd6, 0xf4);

/// The placeholder card, in logical points. Wide enough that the ripple has
/// somewhere to travel and the fixed-pixel press squeeze reads as a squeeze;
/// nothing else depends on it.
const CARD_SIZE: egui::Vec2 = egui::vec2(240.0, 72.0);

/// Card corner radius, in logical points. PLAN §8's nested-rounding rule makes
/// this a number the contents will be derived *from* later (container radius =
/// element radius + gap), so it is picked as a container: round enough to read
/// as a card, not so round it reads as a pill.
const CARD_RADIUS: u8 = 12;

/// catppuccin-mocha `surface0` (#313244) — the first surface above `base`,
/// which is what a card sitting on the background is.
const CARD_FILL: egui::Color32 = egui::Color32::from_rgb(0x31, 0x32, 0x44);

/// catppuccin-mocha `surface1` (#45475a) — one step further up the same ramp.
/// Hovering moves a surface up its own ramp rather than tinting it, so the
/// hover state is the palette's own next value and not an invented colour.
const CARD_FILL_HOT: egui::Color32 = egui::Color32::from_rgb(0x45, 0x47, 0x5a);

/// Blend two opaque colours. Straight per-channel interpolation in sRGB: both
/// ends are neighbouring values on one palette ramp, so there is no hue to be
/// lost between them and a gamma-correct mix would land in the same place.
fn mix(a: egui::Color32, b: egui::Color32, t: f32) -> egui::Color32 {
    let t = t.clamp(0.0, 1.0);
    let ch = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    egui::Color32::from_rgb(ch(a.r(), b.r()), ch(a.g(), b.g()), ch(a.b(), b.b()))
}
