//! Which wgpu backend is tried first on Linux: Vulkan, for the start-up cost
//! measured in [`crate::graphics::Gfx::new`].

use egui_wgpu::wgpu;

/// The first instance's backends ([`crate::graphics::Gfx::new`]).
pub const PREFERRED_BACKENDS: wgpu::Backends = wgpu::Backends::VULKAN;

/// What the log calls them when they find no adapter.
pub const PREFERRED_NAME: &str = "Vulkan";

/// No software adapter is asked for by name: the retry with every backend
/// is what a machine without Vulkan gets, as it always has.
pub const ALLOW_FALLBACK_ADAPTER: bool = false;
