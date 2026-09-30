//! Which wgpu backend is tried first on Windows: DX12, the one every Windows
//! 10 and 11 machine has a driver for. Vulkan is there on most gaming GPUs
//! and missing on plenty of others, and a first instance that finds no
//! adapter costs the retry with every backend on every launch.
//!
//! A machine with no GPU driver at all — a VM without passthrough, the
//! first place this port was run — still has DX12, through WARP, Microsoft's
//! software rasteriser (the "Microsoft Basic Render Driver"). It is slow and
//! it draws correctly, which is enough to use and to test the program, so a
//! failed hardware request asks for it by name before anything else
//! (`plans/other-platforms/04-windows.md` W4.23).

use egui_wgpu::wgpu;

/// The first instance's backends ([`crate::graphics::Gfx::new`]).
pub const PREFERRED_BACKENDS: wgpu::Backends = wgpu::Backends::DX12;

/// What the log calls them when they find no adapter.
pub const PREFERRED_NAME: &str = "DX12";

/// WARP is asked for when no hardware adapter answers.
pub const ALLOW_FALLBACK_ADAPTER: bool = true;
