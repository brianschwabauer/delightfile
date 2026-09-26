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
    /// What each frame is cleared to before egui draws: the palette's `base`
    /// ([`Gfx::new`]), and so the window's side, light or dark.
    clear: egui::Color32,
}

/// What one call to [`Gfx::present`] did.
pub enum Presented {
    /// A frame went to the compositor.
    Shown,
    /// The surface was not available and the swapchain has been reconfigured.
    /// The caller backs off and asks for another frame.
    Retry,
    /// The window is not visible.
    ///
    /// **Dead on this platform, and kept anyway.** wgpu only ever returns
    /// `Occluded` from the Metal backend, and winit does not emit
    /// `WindowEvent::Occluded` on Wayland at all — so on the platform this
    /// program targets neither half of the pair fires, and a covered window
    /// simply goes on presenting frames the compositor throws away. The arm
    /// stays because the code is correct where it *is* reached (an X11 or macOS
    /// build, a future wgpu that reports it here), and deleting it would mean
    /// re-deriving it the day it starts arriving.
    ///
    /// Almost nothing is retried: a covered window does not start answering
    /// because it was asked again, and probing it costs an acquire — which can
    /// block for up to a second — for as long as it stays covered. The next
    /// frame normally comes from an event: `WindowEvent::Occluded(false)`, a
    /// key, or anything else that asks for a redraw. But because the *uncover*
    /// event is the half that is missing on Wayland, the caller also schedules
    /// one slow probe (see [`crate::app::OCCLUDED_PROBE`]) rather than none at
    /// all — a platform that reports occlusion and never reports the end of it
    /// would otherwise leave the window deaf for the rest of the session, and
    /// one acquire every couple of seconds is a price worth paying to make that
    /// impossible.
    Occluded,
}

impl Gfx {
    /// `clear` is what the surface is cleared to under every frame: the
    /// palette's `base`, the pane ground, so a resize never flashes a
    /// different surface behind the panes before egui has repainted them.
    pub fn new(window: Arc<Window>, clear: egui::Color32) -> Result<Gfx, GfxError> {
        // **Vulkan only, unless `WGPU_BACKEND` says otherwise.** With every
        // backend enabled, wgpu brings GL up beside Vulkan and `request_adapter`
        // enumerates both, and GL is never the one picked while Vulkan works.
        // Measured on Hyprland + RTX 2070, release build: creating the EGL
        // instance took 44 ms and probing the GL adapter's extensions another
        // 45 ms, for a backend nothing draws with. Without it, exec to "window
        // mapped" went from 235 ms to 172 ms (medians of eight interleaved
        // runs). What is left is the Vulkan driver loading (about 60 ms) and
        // `request_device` (about 40 ms), which no descriptor makes cheaper.
        //
        // The override goes in *before* `with_env`, so `WGPU_BACKEND=gl` still
        // gets GL, and the flag variables (`WGPU_VALIDATION` and the rest) read
        // the environment exactly as they did.
        let pinned = wgpu::Backends::from_env().is_some();
        let vulkan = wgpu::InstanceDescriptor {
            backends: wgpu::Backends::VULKAN,
            ..wgpu::InstanceDescriptor::new_without_display_handle()
        }
        .with_env();
        let (surface, adapter) = match find_adapter(&window, vulkan) {
            Ok(found) => found,
            // No Vulkan: one more attempt with every backend, which is what
            // every launch used to do, so a machine without Vulkan ends up
            // where it did before. On Wayland that is still an error — "gl not
            // compatible with provided surface", because this instance has no
            // display handle to give EGL — and it was the same error before
            // the narrowing (both checked with `VK_DRIVER_FILES=/nonexistent`).
            // Not when `WGPU_BACKEND` chose the set: that was the user's
            // choice, and the error should say it failed.
            Err(e) if !pinned => {
                log::warn!("Vulkan only: {e}; retrying with every backend");
                find_adapter(
                    &window,
                    wgpu::InstanceDescriptor::new_without_display_handle_from_env(),
                )?
            }
            Err(e) => return Err(e),
        };
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
            clear,
        })
    }

    /// The clear colour, for the frame the window turns light or dark.
    pub fn set_clear(&mut self, clear: egui::Color32) {
        self.clear = clear;
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
    ///
    /// ## Why the texture deltas come first
    ///
    /// `textures_delta` is **one-shot**: egui hands over the glyphs and images
    /// that changed *this* frame and then forgets them, on the understanding
    /// that the backend has taken them. Returning early on a failed acquire —
    /// a lost swapchain, a compositor that did not answer in time — used to
    /// drop that set on the floor, so the next frame drew from an atlas the
    /// renderer had never been given: blank glyphs and missing thumbnails until
    /// something forced a full atlas rebuild. The matching `free` list leaked
    /// the same way, in the other direction.
    ///
    /// Uploads do not need a swapchain image; only the render pass does. So the
    /// deltas are applied unconditionally and it is the *drawing* that is
    /// skipped.
    pub fn present(&mut self, full_output: egui::FullOutput) -> Presented {
        use wgpu::CurrentSurfaceTexture as Cst;

        for (id, delta) in &full_output.textures_delta.set {
            self.renderer
                .update_texture(&self.device, &self.queue, *id, delta);
        }
        // The acquire is folded into a `Result` rather than returning from
        // each arm, so all five failures leave by the one path that still frees
        // what egui has stopped believing in.
        let acquired = match self.surface.get_current_texture() {
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
                Ok(f)
            }
            Cst::Lost | Cst::Outdated => {
                log::warn!("surface lost or outdated; reconfiguring");
                self.surface.configure(&self.device, &self.surface_config);
                Err(Presented::Retry)
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
                Err(Presented::Retry)
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
                Err(Presented::Occluded)
            }
            Cst::Validation => {
                log::warn!("surface frame unavailable (validation); skipping");
                Err(Presented::Retry)
            }
        };
        let frame = match acquired {
            Ok(frame) => frame,
            Err(outcome) => {
                // The uploads above stand; the shapes are dropped, because the
                // caller is about to ask for a frame that will build them again.
                for id in &full_output.textures_delta.free {
                    self.renderer.free_texture(id);
                }
                // …and "stand" has to mean *submitted*. `write_texture` stages
                // into the queue's own buffer and nothing reaches the GPU until
                // a submit, which on the success path is the render pass below.
                // On this path there is no pass — so through a run of failed
                // acquires the deltas were applied, egui forgot them (they are
                // one-shot), and every one of them sat unsubmitted. An empty
                // submit is the flush: it costs no command buffer and it is the
                // documented way to push staged writes through.
                self.queue.submit(std::iter::empty());
                return outcome;
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
                            r: self.clear.r() as f64 / 255.0,
                            g: self.clear.g() as f64 / 255.0,
                            b: self.clear.b() as f64 / 255.0,
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

/// An instance built from `desc`, the window's surface on it, and an adapter
/// that can present to that surface.
///
/// The instance itself is dropped here: the surface and the adapter keep what
/// they need of it alive, and a failed attempt must take its surface down with
/// it before the next attempt makes another one on the same window.
fn find_adapter(
    window: &Arc<Window>,
    desc: wgpu::InstanceDescriptor,
) -> Result<(wgpu::Surface<'static>, wgpu::Adapter), GfxError> {
    let instance = wgpu::Instance::new(desc);
    let surface = instance
        .create_surface(window.clone())
        .map_err(|e| GfxError(format!("create surface: {e}")))?;
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::default(),
        compatible_surface: Some(&surface),
        force_fallback_adapter: false,
    }))
    .map_err(|e| GfxError(format!("no suitable adapter: {e}")))?;
    Ok((surface, adapter))
}

/// Graphics init failed — fatal, and there is nothing useful to do but say so.
#[derive(Debug, thiserror::Error)]
#[error("graphics: {0}")]
pub struct GfxError(pub String);
