//! wgpu surface + egui plumbing, kept apart from the event loop in `app.rs`
//! (the shape delightviewer's `dlv-app/src/{app,graphics}.rs` settled on, which
//! it in turn inherited from delightvideo).
//!
//! Deliberately not eframe (PLAN §1): delightfile owns its render loop so that
//! video frames can be drawn as wgpu textures under egui in the preview pane
//! (Phase 3), and so the cold-start path can decide exactly what the first
//! frame contains — the directory listing has to be on screen before the
//! preview workers have finished anything.
//!
//! wgpu is reached only through `egui_wgpu::wgpu`. That re-export is what
//! guarantees there is exactly one wgpu in the build, shared with `dv-playback`
//! when the preview pane arrives.

use std::sync::Arc;

use egui_wgpu::wgpu;
use winit::window::Window;

/// Everything that must exist before a pixel can be drawn.
pub struct Gfx {
    pub window: Arc<Window>,
    pub surface: wgpu::Surface<'static>,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub surface_config: wgpu::SurfaceConfiguration,
    pub renderer: egui_wgpu::Renderer,
    pub egui_ctx: egui::Context,
    pub egui_state: egui_winit::State,
    /// Whether the last acquire said the window is not being composited, so the
    /// log says so once per transition rather than once per attempted frame.
    occluded: bool,
    /// Consecutive timed-out acquires, for the same reason — and so the
    /// recovery line can say how many there were.
    timeouts: u32,
}

/// What one call to [`Gfx::present`] did.
pub enum Presented {
    /// A frame went to the compositor.
    Shown,
    /// The surface was not available and the swapchain has been reconfigured.
    /// The caller backs off and asks for another frame.
    Retry,
    /// The window is not visible. There is nothing to retry: a covered window
    /// does not start answering because it was asked again, and probing it
    /// costs an acquire — which can block for up to a second — for as long as
    /// it stays covered. The next frame comes from an event: winit's
    /// `WindowEvent::Occluded(false)`, a key, or anything else that asks for a
    /// redraw.
    Occluded,
}

/// The window background: catppuccin-mocha `base` (#1e1e2e), the same ground
/// the yazi config this replaces sits on (PLAN §3, §8). It is the clear color
/// as well as the pane background, so a resize never flashes a different
/// surface behind the panes before egui has repainted them.
pub const BG: egui::Color32 = egui::Color32::from_rgb(0x1e, 0x1e, 0x2e);

impl Gfx {
    pub fn new(window: Arc<Window>) -> Result<Gfx, GfxError> {
        let instance =
            wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
        let surface = instance
            .create_surface(window.clone())
            .map_err(|e| GfxError(format!("create surface: {e}")))?;
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::default(),
            compatible_surface: Some(&surface),
            force_fallback_adapter: false,
        }))
        .map_err(|e| GfxError(format!("no suitable adapter: {e}")))?;
        let info = adapter.get_info();
        log::info!(
            "adapter: {} ({:?}, {:?})",
            info.name,
            info.device_type,
            info.backend
        );
        let (device, queue) =
            pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
                .map_err(|e| GfxError(format!("request device: {e}")))?;

        let size = window.inner_size();
        let mut surface_config = surface
            // `.max(1)`: a compositor can hand us a zero-sized window before
            // the first configure, and a zero-sized surface is a validation
            // error rather than an empty picture.
            .get_default_config(&adapter, size.width.max(1), size.height.max(1))
            .ok_or_else(|| GfxError("surface not supported by adapter".into()))?;
        let caps = surface.get_capabilities(&adapter);
        // Mailbox over Fifo, when the driver offers it. This app paints only
        // when something happened (PLAN §1), so it never needs vsync to pace
        // itself — and Fifo has a cost that is not about pacing at all: on
        // Wayland the Fifo acquire blocks until the compositor answers a frame
        // callback, and a compositor does not answer for a window that is on
        // another workspace, fully covered, or on a screen hypridle has
        // switched off. wgpu gives the acquire a one-second timeout, so under
        // Fifo every redraw after the window came back could stall the whole
        // UI thread for a second and then fail (`present-fail` in
        // `DF_FRAME_LOG`), which is a file manager that stops answering keys
        // the moment you alt-tab. Measured on Hyprland + NVIDIA. Mailbox never
        // waits on the frame callback, and with no game loop behind it never
        // burns a frame either.
        surface_config.present_mode = if caps.present_modes.contains(&wgpu::PresentMode::Mailbox) {
            wgpu::PresentMode::Mailbox
        } else {
            wgpu::PresentMode::Fifo
        };
        log::info!(
            "present mode {:?} (offered {:?})",
            surface_config.present_mode,
            caps.present_modes
        );
        // egui outputs sRGB-encoded colors; give it a non-sRGB view format so
        // the hardware does not encode them a second time and wash the theme
        // out.
        if let Some(&fmt) = caps.formats.iter().find(|f| {
            matches!(
                f,
                wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm
            )
        }) {
            surface_config.format = fmt;
        }
        surface.configure(&device, &surface_config);

        let egui_ctx = egui::Context::default();
        let egui_state = egui_winit::State::new(
            egui_ctx.clone(),
            egui::ViewportId::ROOT,
            &window,
            Some(window.scale_factor() as f32),
            None,
            Some(device.limits().max_texture_dimension_2d as usize),
        );
        let renderer = egui_wgpu::Renderer::new(
            &device,
            surface_config.format,
            egui_wgpu::RendererOptions::default(),
        );

        Ok(Gfx {
            window,
            surface,
            device,
            queue,
            surface_config,
            renderer,
            egui_ctx,
            egui_state,
            occluded: false,
            timeouts: 0,
        })
    }

    pub fn resize(&mut self, width: u32, height: u32) {
        if width == 0 || height == 0 {
            return;
        }
        self.surface_config.width = width;
        self.surface_config.height = height;
        self.surface.configure(&self.device, &self.surface_config);
    }

    /// Tessellate and present one egui frame.
    pub fn present(&mut self, full_output: egui::FullOutput) -> Presented {
        use wgpu::CurrentSurfaceTexture as Cst;
        let frame = match self.surface.get_current_texture() {
            Cst::Success(f) | Cst::Suboptimal(f) => {
                if self.occluded {
                    self.occluded = false;
                    log::debug!("surface visible again");
                }
                if self.timeouts > 0 {
                    log::info!(
                        "surface acquire recovered after {} timeout(s)",
                        self.timeouts
                    );
                    self.timeouts = 0;
                }
                f
            }
            Cst::Lost | Cst::Outdated => {
                log::warn!("surface lost or outdated; reconfiguring");
                self.surface.configure(&self.device, &self.surface_config);
                return Presented::Retry;
            }
            // A timed-out acquire is the swapchain waiting on a compositor
            // that is not answering (see the present-mode note in `new`).
            // Reconfiguring hands the driver a fresh swapchain, which is the
            // one thing that has been seen to get it answering again; the
            // caller backs off before asking for another frame, so a
            // compositor that stays silent costs one acquire per retry rather
            // than a tight loop of them.
            Cst::Timeout => {
                // Once per run of timeouts. A compositor that has gone quiet
                // stays quiet, and a warning per attempt turns a stall into a
                // log flood that says one thing many times.
                self.timeouts = self.timeouts.saturating_add(1);
                if self.timeouts == 1 {
                    log::warn!("surface acquire timed out; reconfiguring");
                }
                self.surface.configure(&self.device, &self.surface_config);
                return Presented::Retry;
            }
            // **Not reconfigured.** Nothing is wrong with the swapchain: the
            // window is simply not on screen, and handing the driver a fresh
            // one every time it says so is work done on behalf of a window
            // nobody can see. Logged at debug, once, because it is a normal
            // thing for a window to be behind another one.
            Cst::Occluded => {
                if !self.occluded {
                    self.occluded = true;
                    log::debug!("surface occluded; frames paused until the window is shown");
                }
                return Presented::Occluded;
            }
            Cst::Validation => {
                log::warn!("surface frame unavailable (validation); skipping");
                return Presented::Retry;
            }
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());

        let pixels_per_point = full_output.pixels_per_point;
        let primitives = self
            .egui_ctx
            .tessellate(full_output.shapes, pixels_per_point);
        let screen = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [self.surface_config.width, self.surface_config.height],
            pixels_per_point,
        };

        for (id, delta) in &full_output.textures_delta.set {
            self.renderer
                .update_texture(&self.device, &self.queue, *id, delta);
        }
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("egui"),
            });
        let user_buffers = self.renderer.update_buffers(
            &self.device,
            &self.queue,
            &mut encoder,
            &primitives,
            &screen,
        );
        {
            let rpass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("egui"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: BG.r() as f64 / 255.0,
                            g: BG.g() as f64 / 255.0,
                            b: BG.b() as f64 / 255.0,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                ..Default::default()
            });
            let mut rpass = rpass.forget_lifetime();
            self.renderer.render(&mut rpass, &primitives, &screen);
        }
        for id in &full_output.textures_delta.free {
            self.renderer.free_texture(id);
        }
        self.queue
            .submit(user_buffers.into_iter().chain([encoder.finish()]));
        frame.present();
        Presented::Shown
    }
}

/// Graphics init failed — fatal, and there is nothing useful to do but say so.
#[derive(Debug, thiserror::Error)]
#[error("graphics: {0}")]
pub struct GfxError(pub String);
