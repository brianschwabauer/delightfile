//! The palette, resolved once: [`df_core::config::Theme`]'s named colours as
//! `egui::Color32`s the painter can use without a lookup per row.
//!
//! df-core stores colours as sRGB bytes in a `Vec<(String, Color)>` — the shape
//! `theme.toml` writes, and the shape a theme browser wants to list. That is the
//! wrong shape for a paint loop: a hundred rows × several colours each × sixty
//! frames is a lot of string comparisons for an answer that cannot change
//! between frames. So the names are looked up once at startup and everything
//! after that is a field.
//!
//! Every field is a *named catppuccin-mocha value*, never an invented one. When
//! a state needs to be lighter, it moves up the palette's own ramp
//! (`base → surface0 → surface1 → surface2`) rather than being tinted by a
//! number somebody eyeballed — which is also what makes a user's `[palette]`
//! override in `theme.toml` do something coherent instead of half of one.

use df_core::config::{Color, Theme};

use crate::icons::to_color32;

/// The colours the panes are painted from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    /// The window ground, behind and between the panes.
    pub crust: egui::Color32,
    /// The recessed panes: parent and preview.
    pub mantle: egui::Color32,
    /// The pane you live in: the list.
    pub base: egui::Color32,
    /// A row under the pointer.
    pub surface0: egui::Color32,
    /// The row the cursor is on.
    pub surface1: egui::Color32,
    /// Separators and rules.
    pub surface2: egui::Color32,
    pub overlay0: egui::Color32,
    pub overlay1: egui::Color32,
    pub overlay2: egui::Color32,
    pub subtext0: egui::Color32,
    pub subtext1: egui::Color32,
    pub text: egui::Color32,
    /// The focus accent, and the colour of a directory's name.
    pub blue: egui::Color32,
    pub sky: egui::Color32,
    pub red: egui::Color32,
    pub yellow: egui::Color32,
    // The rest of the accent ramp. Added for the preview pane's syntax
    // highlighting (PLAN §6), which needs a colour per token kind and takes
    // each of them from the flavour's own names — the mapping is catppuccin's
    // published one (keywords mauve, strings green, numbers peach, types
    // yellow, functions blue), so a `[palette]` override in `theme.toml`
    // re-tints the code the same way it re-tints everything else.
    pub mauve: egui::Color32,
    pub green: egui::Color32,
    pub peach: egui::Color32,
    pub teal: egui::Color32,
    pub lavender: egui::Color32,
    pub maroon: egui::Color32,
    pub pink: egui::Color32,
}

impl Palette {
    pub fn from_theme(theme: &Theme) -> Palette {
        // Every one of these names is in the shipped table, so the fallback is
        // unreachable in practice; it exists because a user's `theme.toml` can
        // rename nothing but can, in principle, be loaded against a future
        // flavour that is missing a name. Falling back to a mid grey makes that
        // visible without crashing.
        let pick = |name: &str| {
            to_color32(theme.color(name).unwrap_or(Color {
                r: 0x7f,
                g: 0x84,
                b: 0x9c,
            }))
        };
        Palette {
            crust: pick("crust"),
            mantle: pick("mantle"),
            base: pick("base"),
            surface0: pick("surface0"),
            surface1: pick("surface1"),
            surface2: pick("surface2"),
            overlay0: pick("overlay0"),
            overlay1: pick("overlay1"),
            overlay2: pick("overlay2"),
            subtext0: pick("subtext0"),
            subtext1: pick("subtext1"),
            text: pick("text"),
            blue: pick("blue"),
            sky: pick("sky"),
            red: pick("red"),
            yellow: pick("yellow"),
            mauve: pick("mauve"),
            green: pick("green"),
            peach: pick("peach"),
            teal: pick("teal"),
            lavender: pick("lavender"),
            maroon: pick("maroon"),
            pink: pick("pink"),
        }
    }
}

impl Default for Palette {
    fn default() -> Palette {
        Palette::from_theme(&Theme::default())
    }
}

/// Blend two opaque colours.
///
/// Straight per-channel interpolation in sRGB. Every use of this mixes two
/// neighbouring values on one catppuccin ramp, which share a hue — so there is
/// no hue to be lost on the way and a gamma-correct mix would land in visibly
/// the same place.
pub fn mix(a: egui::Color32, b: egui::Color32, t: f32) -> egui::Color32 {
    let t = t.clamp(0.0, 1.0);
    let ch = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    egui::Color32::from_rgb(ch(a.r(), b.r()), ch(a.g(), b.g()), ch(a.b(), b.b()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_palette_resolves_every_name() {
        let p = Palette::default();
        assert_eq!(p.base, egui::Color32::from_rgb(0x1e, 0x1e, 0x2e));
        assert_eq!(p.crust, egui::Color32::from_rgb(0x11, 0x11, 0x1b));
        assert_eq!(p.blue, egui::Color32::from_rgb(0x89, 0xb4, 0xfa));
        assert_eq!(p.red, egui::Color32::from_rgb(0xf3, 0x8b, 0xa8));
    }

    /// A `[palette]` override in `theme.toml` reaches the painter.
    #[test]
    fn a_user_override_changes_the_pane_colour() {
        let (theme, warnings) = Theme::parse(
            "[palette]\nbase = \"#000000\"\n",
            std::path::Path::new("theme.toml"),
        );
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(Palette::from_theme(&theme).base, egui::Color32::BLACK);
    }

    #[test]
    fn mixing_ends_at_both_ends() {
        let (a, b) = (egui::Color32::BLACK, egui::Color32::WHITE);
        assert_eq!(mix(a, b, 0.0), a);
        assert_eq!(mix(a, b, 1.0), b);
        assert_eq!(mix(a, b, -1.0), a);
        assert_eq!(mix(a, b, 2.0), b);
        assert_eq!(mix(a, b, 0.5), egui::Color32::from_rgb(128, 128, 128));
    }
}
