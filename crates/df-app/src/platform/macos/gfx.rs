//! Which wgpu backend is tried first on macOS: Metal, the only one Apple
//! ships a driver for. wgpu builds no Vulkan backend here without
//! `vulkan-portability`, which nothing enables, so the Linux choice would
//! find no adapter and fall through to the retry every launch.

use egui_wgpu::wgpu;

/// The first instance's backends ([`crate::graphics::Gfx::new`]).
pub const PREFERRED_BACKENDS: wgpu::Backends = wgpu::Backends::METAL;

/// What the log calls them when they find no adapter.
pub const PREFERRED_NAME: &str = "Metal";
