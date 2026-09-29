//! Which wgpu backend is tried first on Windows: DX12, the one every Windows
//! 10 and 11 machine has a driver for. Vulkan is there on most gaming GPUs
//! and missing on plenty of others, and a first instance that finds no
//! adapter costs the retry with every backend on every launch.

use egui_wgpu::wgpu;

/// The first instance's backends ([`crate::graphics::Gfx::new`]).
pub const PREFERRED_BACKENDS: wgpu::Backends = wgpu::Backends::DX12;

/// What the log calls them when they find no adapter.
pub const PREFERRED_NAME: &str = "DX12";
