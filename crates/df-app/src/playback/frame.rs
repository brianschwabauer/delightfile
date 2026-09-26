//! Turning a `dv-playback` frame into something the preview pane can draw
//! (PLAN §6, §10's playback checkbox).
//!
//! **The integration choice**, ported from delightviewer's `dlv-app/src/video.rs`
//! and for the same reason. dv-playback publishes each decoded frame as two
//! wgpu textures (Y in R8, UV in Rg8) plus the metadata its NV12 shader needs.
//! delightvideo draws those straight to the screen through an `egui_wgpu` paint
//! callback and lets the callback rect letterbox them. delightfile cannot: the
//! preview pane is painted with [`egui::Painter`] like everything else in this
//! program (see [`crate::ui`]), the frame has to sit *under* the position strip
//! and *over* the cached thumbnail, and a paint callback would be a second
//! coordinate system for exactly one widget.
//!
//! So dv-playback's own frame shader runs **into an offscreen RGBA texture** at
//! source resolution, once per newly decoded frame, and that texture is
//! registered with egui (`Renderer::register_native_texture`). From there a
//! video frame is a `TextureId` of known pixel size — precisely what a decoded
//! still already is in [`crate::preview`] — so it is drawn with the same
//! `fit_rect` + mesh the JPEG path uses, and the fit, the HiDPI division and
//! the crossfade are all one implementation instead of two.
//!
//! The shader is dv-playback's own `FRAME_WGSL` with dv-playback's own
//! `FrameUniforms` packing, so colour handling — limited range, BT.601/709 —
//! cannot drift from delightvideo's or delightviewer's. The grade block those
//! two programs fill is left at its identity here: a file manager previews a
//! file, it does not colour-correct one.

use egui_wgpu::wgpu;

use dv_playback::frame_shader::{FrameUniforms, FRAME_WGSL};
use dv_playback::VideoFrameTex;

/// The offscreen target, plus its registration with egui.
struct Target {
    /// Held only to keep the texture alive behind `view`, which is what egui
    /// samples.
    _texture: wgpu::Texture,
    view: wgpu::TextureView,
    id: egui::TextureId,
    width: u32,
    height: u32,
}

/// A converted frame: an egui texture and the size it was decoded at, in
/// physical pixels.
#[derive(Clone, Copy)]
pub struct FrameTex {
    pub id: egui::TextureId,
    pub width: u32,
    pub height: u32,
}

pub struct FrameConverter {
    pipeline: wgpu::RenderPipeline,
    layout: wgpu::BindGroupLayout,
    /// Two, chosen per frame by the picture's own size. Smooth for a video,
    /// crisp for the tiny ones — see [`sampler_for`].
    sampler_linear: wgpu::Sampler,
    sampler_nearest: wgpu::Sampler,
    /// One 96-byte uniform buffer for the life of the converter — the packing
    /// is fixed size, so a new frame is a `write_buffer`, never an allocation.
    uniforms: wgpu::Buffer,
    /// The bind group and the frame it names. dv-playback uploads each decoded
    /// frame into *new* Y/UV textures, so the plane views really do change per
    /// frame and the group has to be rebuilt then.
    bind: Option<((i64, u64), wgpu::BindGroup)>,
    target: Option<Target>,
    /// pts + serial of the frame already in `target`, so a redraw the decoder
    /// has not overtaken — every repaint of a paused video — costs nothing.
    current: Option<(i64, u64)>,
}

/// egui samples user textures as sRGB-encoded ("gamma") values, and the frame
/// shader's last step is exactly that encode — so the target is `Unorm`, not
/// `UnormSrgb`, and no conversion happens twice.
const TARGET_FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Unorm;

impl FrameConverter {
    pub fn new(device: &wgpu::Device) -> FrameConverter {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("df-frame-nv12"),
            source: wgpu::ShaderSource::Wgsl(FRAME_WGSL.into()),
        });
        let tex_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("df-frame-bind"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: None,
                    },
                    count: None,
                },
                tex_entry(1),
                tex_entry(2),
                wgpu::BindGroupLayoutEntry {
                    binding: 3,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("df-frame-pipeline"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("df-frame-nv12"),
            layout: Some(&pipeline_layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[],
                compilation_options: Default::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: TARGET_FORMAT,
                    blend: None,
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: Default::default(),
            }),
            primitive: wgpu::PrimitiveState::default(),
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let sampler = |label, filter| {
            device.create_sampler(&wgpu::SamplerDescriptor {
                label: Some(label),
                mag_filter: filter,
                min_filter: filter,
                ..Default::default()
            })
        };
        let sampler_linear = sampler("df-frame-sampler-linear", wgpu::FilterMode::Linear);
        // The chroma planes are half resolution, so this sampler is doing an
        // upsample even at 1:1 — and for the pixel-art sources
        // [`crate::preview::paint::nearest_for`] is about, a smooth chroma
        // upsample is exactly the bleed between two flat colours that the
        // nearest-neighbour magnification downstream was chosen to avoid.
        let sampler_nearest = sampler("df-frame-sampler-nearest", wgpu::FilterMode::Nearest);
        let uniforms = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("df-frame-uniforms"),
            // Asked of the packing itself, so the size cannot drift from it.
            size: FrameUniforms::new(false, 1.0, false).to_bytes().len() as u64,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        FrameConverter {
            pipeline,
            layout,
            sampler_linear,
            sampler_nearest,
            uniforms,
            bind: None,
            target: None,
            current: None,
        }
    }

    /// Which sampler a source of `size` physical pixels is read through.
    ///
    /// The still-image rule, applied to moving pictures: below
    /// the still path's 128-pixel line a picture is an icon or pixel art
    /// and "crisp" is what it means, above it "smooth" is. Asked of the plain
    /// size rather than `preview::oriented_size`: a quarter turn swaps the two
    /// axes, which cannot change which of them is longer.
    fn sampler_for(&self, size: (u32, u32)) -> &wgpu::Sampler {
        if crate::preview::nearest_for(size) {
            &self.sampler_nearest
        } else {
            &self.sampler_linear
        }
    }

    /// The frame already converted, if any.
    pub fn current(&self) -> Option<FrameTex> {
        let target = self.target.as_ref()?;
        self.current?;
        Some(FrameTex {
            id: target.id,
            width: target.width,
            height: target.height,
        })
    }

    /// Convert `frame` unless it is the one already converted — a repaint the
    /// decoder has not overtaken costs nothing, which is what keeps a *paused*
    /// video at the idle cost of a still picture (PLAN §1).
    pub fn convert(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        renderer: &mut egui_wgpu::Renderer,
        frame: &VideoFrameTex,
    ) -> FrameTex {
        let key = (frame.pts_us, frame.source_serial);
        if self.current == Some(key) {
            if let Some(t) = self.current() {
                return t;
            }
        }
        self.ensure_target(device, renderer, frame.width.max(1), frame.height.max(1));
        let Some(target) = &self.target else {
            // Cannot happen — `ensure_target` either sets it or the device is
            // gone — but the fallback in a file manager must not be a panic.
            return FrameTex {
                id: egui::TextureId::default(),
                width: 1,
                height: 1,
            };
        };

        // Identity placement: the target IS the source frame, at source size,
        // and the pane's `fit_rect` does the framing — the same one the still
        // images use. The container's **display matrix** is framing too, and
        // it is applied the same way: `preview::oriented_size` turns the
        // footprint and `preview::oriented_mesh` turns the four UVs, so a
        // portrait clip costs four different floats per frame rather than a
        // rotated blit (`app::redraw_inner`).
        let uniforms = FrameUniforms::new(
            frame.limited_range,
            1.0,
            matches!(frame.matrix, dv_playback::ColorMatrix::Bt709),
        );
        queue.write_buffer(&self.uniforms, 0, &uniforms.to_bytes());
        if self.bind.as_ref().is_none_or(|(k, _)| *k != key) {
            let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("df-frame-bind"),
                layout: &self.layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: self.uniforms.as_entire_binding(),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: wgpu::BindingResource::TextureView(&frame.y),
                    },
                    wgpu::BindGroupEntry {
                        binding: 2,
                        resource: wgpu::BindingResource::TextureView(&frame.uv),
                    },
                    wgpu::BindGroupEntry {
                        binding: 3,
                        resource: wgpu::BindingResource::Sampler(
                            self.sampler_for((frame.width, frame.height)),
                        ),
                    },
                ],
            });
            self.bind = Some((key, bind_group));
        }
        let Some((_, bind_group)) = &self.bind else {
            return FrameTex {
                id: target.id,
                width: target.width,
                height: target.height,
            };
        };

        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("df-frame-convert"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("df-frame-convert"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &target.view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        // The frame's own texture, which the conversion covers
                        // edge to edge: black is a video's ground, not a pane's.
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                    depth_slice: None,
                })],
                depth_stencil_attachment: None,
                ..Default::default()
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
        queue.submit([encoder.finish()]);

        self.current = Some(key);
        FrameTex {
            id: target.id,
            width: target.width,
            height: target.height,
        }
    }

    /// Drop the target and its egui registration — on teardown, and on quit.
    ///
    /// Not optional bookkeeping: the registration holds a GPU texture the size
    /// of the video, and a session that arrowed past forty clips would hold
    /// forty of them.
    pub fn clear(&mut self, renderer: &mut egui_wgpu::Renderer) {
        if let Some(t) = self.target.take() {
            renderer.free_texture(&t.id);
        }
        self.current = None;
        // Holding the group would hold the last frame's Y/UV textures with it.
        self.bind = None;
    }

    fn ensure_target(
        &mut self,
        device: &wgpu::Device,
        renderer: &mut egui_wgpu::Renderer,
        width: u32,
        height: u32,
    ) {
        if let Some(t) = &self.target {
            if t.width == width && t.height == height {
                return;
            }
        }
        self.clear(renderer);
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some("df-frame-rgba"),
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: TARGET_FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        // …and the same rule again for the magnification egui itself does when
        // the fitted rectangle is bigger than the source (`paint::fit_rect`,
        // up to its cap). A 32-pixel animation blown up eight times through a
        // bilinear filter is a blur where the file's own pixels are the point.
        let filter = if crate::preview::nearest_for((width, height)) {
            wgpu::FilterMode::Nearest
        } else {
            wgpu::FilterMode::Linear
        };
        let id = renderer.register_native_texture(device, &view, filter);
        self.target = Some(Target {
            _texture: texture,
            view,
            id,
            width,
            height,
        });
    }
}
