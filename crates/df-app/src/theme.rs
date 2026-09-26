//! The palette, resolved once per side: [`df_core::config::Theme`]'s named
//! colours as `egui::Color32`s the painter can use without a lookup per row.
//!
//! df-core stores colours as sRGB bytes in a `Vec<(String, Color)>` per side —
//! the shape `theme.toml` writes, and the shape a theme browser wants to list.
//! That is the wrong shape for a paint loop: a hundred rows × several colours
//! each × sixty frames is a lot of string comparisons for an answer that
//! cannot change between frames. So the names are looked up once, when the
//! side on screen is decided — at startup, and again the one frame the window
//! turns light or dark — and everything after that is a field.
//!
//! Every field is a *named catppuccin value* of the flavour that side is,
//! never an invented one. When a state needs to be lighter, it moves up the
//! palette's own ramp (`base → surface0 → surface1 → surface2`) rather than
//! being tinted by a number somebody eyeballed — which is also what makes a
//! user's `[palette]` override in `theme.toml` do something coherent instead
//! of half of one.
//!
//! ## Two sides
//!
//! The window is dark or light (`[flavor] mode`, the desktop, or the session's
//! own `theme-*` command), and the two sides are two flavours: mocha and latte
//! unless `theme.toml` says otherwise. Catppuccin's ramps run from the ground
//! towards the text in both, so almost everything reads the same way on
//! either — a hover one step up the ramp is a step *darker* on latte and
//! nothing had to know. What does have to know is below, as helpers that
//! answer by [`Palette::light`]:
//!
//! - **What was white or black and meant "brighter" or "darker"** — a
//!   ripple's splash, the scrim under a modal card, the dark edge a drag's
//!   ghost wears. On a light ground a splash darkens and the scrim washes
//!   towards the ground rather than towards black; a shadow stays dark but
//!   carries less weight ([`splash`], [`scrim`], [`shadow`]).
//! - **Where latte's ramp is too close together to read** — its whole range,
//!   crust to text, is 6:1 where mocha's is 13:1, so the few places that sat
//!   on the ramp's quiet end take a step further along it on the light side
//!   ([`Palette::faint`], [`hairline`], [`thumb`]), and an accent that has to be read
//!   *as text* on a light ground is taken halfway to the text ([`ink`]).
//!
//! Every number those helpers use for the light side is measured against what
//! the dark side already does — the same step in lightness, the same residual
//! contrast — and each constant says which measurement. The dark side is
//! exactly what it was.

use df_core::config::{Appearance, Color, Theme};

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
    /// Whether this is a light palette: its ground brighter than its text.
    ///
    /// Read off the colours rather than off which side asked for them, so a
    /// `light = "catppuccin-mocha"` in `theme.toml` — a dark flavour on the
    /// light side — is painted as the dark flavour it is, splashes and scrims
    /// included.
    pub light: bool,
    /// Secondary text: a tooltip's second line, a hint strip's words, a
    /// card's detail column, a count, an empty state, a greyed menu row and
    /// its keys, a line number — everything that is read, but read second.
    ///
    /// `overlay0` on a dark palette, exactly as it always was (3.4:1 on
    /// mocha's `base`, 3.8:1 on a card's `crust`). On a light one it is the
    /// first step of the ramp from `overlay0` towards `text` that reaches
    /// [`FAINT_CONTRAST`] on the palette's `base` — on latte that is
    /// `overlay2`, 3.5:1, where latte's `overlay0` is 2.3:1 and `overlay1`
    /// 2.8:1. Measured when the palette is resolved rather than named here,
    /// so a `[palette.light]` override that moves `base` or the ramp moves
    /// this with it. Not for a line: a rule is [`hairline`]'s, and a mark is
    /// its own colour.
    pub faint: egui::Color32,
    /// Quiet text, a step louder than [`Palette::faint`]: a hint with an
    /// instruction in it, a card's detail line, a status, a code comment, a
    /// row's size column, an inactive tab's title — read second, but read
    /// through.
    ///
    /// `overlay1` on a dark palette, exactly as it always was (4.4:1 on
    /// mocha's `base`). On a light one it is the first step of the ramp from
    /// `overlay1` towards `text` that is louder than `faint` and reaches
    /// [`QUIET_CONTRAST`] on `base` — on latte that is `subtext0`, 4.4:1,
    /// where latte's `overlay1` is 2.8:1. Measured when the palette is
    /// resolved, as `faint` is.
    pub quiet: egui::Color32,
}

/// The contrast a light palette's [`Palette::faint`] has to reach on its
/// `base`: 3:1, WCAG's floor for text that is not body copy and for the
/// parts of a control that have to be seen — which is what secondary text in
/// a file manager is, and what mocha's `overlay0` already gives (3.4:1).
pub const FAINT_CONTRAST: f32 = 3.0;

/// The contrast a light palette's [`Palette::quiet`] has to reach on its
/// `base`: 4:1, what mocha's `overlay1` gives on its own `base` (4.4:1) —
/// quiet text is read through, a sentence at a time, and has to sit near the
/// line body copy is held to rather than at the floor faint text is.
pub const QUIET_CONTRAST: f32 = 4.0;

impl Palette {
    /// Every name [`Palette::from_theme`] reads, in the order of the fields.
    pub const NAMES: [&'static str; 23] = [
        "crust", "mantle", "base", "surface0", "surface1", "surface2", "overlay0", "overlay1",
        "overlay2", "subtext0", "subtext1", "text", "blue", "sky", "red", "yellow", "mauve",
        "green", "peach", "teal", "lavender", "maroon", "pink",
    ];

    /// One side of `theme`, resolved.
    pub fn from_theme(theme: &Theme, side: Appearance) -> Palette {
        // Every one of these names is in all four shipped tables, so the
        // fallback is unreachable in practice; it exists because a user's
        // `theme.toml` can rename nothing but can, in principle, be loaded
        // against a future flavour that is missing a name. Falling back to a
        // mid grey makes that visible without crashing.
        let pick = |name: &str| {
            to_color32(theme.color(side, name).unwrap_or(Color {
                r: 0x7f,
                g: 0x84,
                b: 0x9c,
            }))
        };
        let (base, text) = (pick("base"), pick("text"));
        let light = luminance(base) > luminance(text);
        // The dark side's faint and quiet text are `overlay0` and `overlay1`,
        // untouched. The light side's are measured along the ramp towards
        // `text`: faint is the first step that reads on `base`, quiet the
        // first after it that reads through (see [`Palette::faint`] and
        // [`Palette::quiet`]), and `text` itself when the ramp runs out.
        let (faint, quiet) = if light {
            let ramp = [
                "overlay0", "overlay1", "overlay2", "subtext0", "subtext1", "text",
            ]
            .map(|name| if name == "text" { text } else { pick(name) });
            let reads = |from: usize, floor: f32| {
                (from..ramp.len())
                    .find(|&i| contrast(ramp[i], base) >= floor)
                    .unwrap_or(ramp.len() - 1)
            };
            let faint = reads(0, FAINT_CONTRAST);
            let quiet = reads((faint + 1).clamp(1, ramp.len() - 1), QUIET_CONTRAST);
            (ramp[faint], ramp[quiet])
        } else {
            (pick("overlay0"), pick("overlay1"))
        };
        Palette {
            crust: pick("crust"),
            mantle: pick("mantle"),
            base,
            surface0: pick("surface0"),
            surface1: pick("surface1"),
            surface2: pick("surface2"),
            overlay0: pick("overlay0"),
            overlay1: pick("overlay1"),
            overlay2: pick("overlay2"),
            subtext0: pick("subtext0"),
            subtext1: pick("subtext1"),
            text,
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
            light,
            faint,
            quiet,
        }
    }

    /// The palette for what is drawn *over a picture* — the transport strip
    /// on its scrim across a playing video.
    ///
    /// Always a dark one, whichever side the window is on: a frame of video is
    /// its own ground, the scrim under the controls is black on either side
    /// (it is darkening a picture, not a pane), and the controls on it are
    /// light-on-dark for the same reason a subtitle is. The theme's dark side
    /// when that is dark, and mocha when somebody has put a light flavour
    /// there.
    pub fn for_media(theme: &Theme) -> Palette {
        let dark = Palette::from_theme(theme, Appearance::Dark);
        if dark.light {
            Palette::default()
        } else {
            dark
        }
    }

    /// `dark` on a dark palette, `light` on a light one: a choice between two
    /// named values, never a number in between.
    pub fn sided<T>(&self, dark: T, light: T) -> T {
        if self.light {
            light
        } else {
            dark
        }
    }

    /// One of the named colours, by its catppuccin name — for a colour chosen
    /// by name somewhere that is not a paint loop, such as the shipped
    /// directory icons' light-side accents.
    pub fn named(&self, name: &str) -> Option<egui::Color32> {
        Some(match name {
            "crust" => self.crust,
            "mantle" => self.mantle,
            "base" => self.base,
            "surface0" => self.surface0,
            "surface1" => self.surface1,
            "surface2" => self.surface2,
            "overlay0" => self.overlay0,
            "overlay1" => self.overlay1,
            "overlay2" => self.overlay2,
            "subtext0" => self.subtext0,
            "subtext1" => self.subtext1,
            "text" => self.text,
            "blue" => self.blue,
            "sky" => self.sky,
            "red" => self.red,
            "yellow" => self.yellow,
            "mauve" => self.mauve,
            "green" => self.green,
            "peach" => self.peach,
            "teal" => self.teal,
            "lavender" => self.lavender,
            "maroon" => self.maroon,
            "pink" => self.pink,
            _ => return None,
        })
    }

    /// `color`, which this palette handed out under some name, as the same
    /// name in `to`; unchanged when it is none of this palette's.
    ///
    /// For the few things that hold a colour across frames rather than asking
    /// for one each frame — a drag's ghost keeps the icon of the row it was
    /// picked up from — so the one frame the window turns light or dark turns
    /// them too.
    pub fn translate(&self, color: egui::Color32, to: &Palette) -> egui::Color32 {
        Palette::NAMES
            .iter()
            .find(|name| self.named(name) == Some(color))
            .and_then(|name| to.named(name))
            .unwrap_or(color)
    }
}

impl Default for Palette {
    /// The shipped dark side: catppuccin-mocha.
    fn default() -> Palette {
        Palette::from_theme(&Theme::default(), Appearance::Dark)
    }
}

/// Relative luminance, for telling a light ground from a dark one. The WCAG
/// formula, on the one question it is never wrong about.
fn luminance(color: egui::Color32) -> f32 {
    let channel = |c: u8| {
        let c = f32::from(c) / 255.0;
        if c <= 0.040_45 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(color.r()) + 0.7152 * channel(color.g()) + 0.0722 * channel(color.b())
}

/// The WCAG contrast ratio of two opaque colours, 1 to 21.
pub fn contrast(a: egui::Color32, b: egui::Color32) -> f32 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

/// The same colour at `alpha` of its own opacity.
fn at(color: egui::Color32, alpha: f32) -> egui::Color32 {
    let a = (alpha.clamp(0.0, 1.0) * f32::from(color.a())).round() as u8;
    egui::Color32::from_rgba_unmultiplied(color.r(), color.g(), color.b(), a)
}

/// How far the two lit row surfaces are pulled off the grey ramp, towards the
/// palette's `lavender`.
///
/// The ramp's `surface0`/`surface1` are a true neutral, and a neutral grey
/// sitting between catppuccin's tinted `base` and its coloured file names is
/// the one surface in the window that looks like it came from a different
/// palette. Eighteen percent of the way to `lavender` is still read as grey —
/// it is a step up the ramp, not a highlight colour — but it belongs to the
/// same family as everything around it. Lavender rather than `blue` because
/// blue is the *directory* colour and the accent this program marks live
/// things with; a cursor bar the same hue as the accent would look like a
/// selection.
const ROW_TINT: f32 = 0.18;

/// What the cursor row is lifted towards: the palette's `surface1`, warmed.
///
/// One function for the list and the grid so a row and its tile cannot drift
/// apart — the grid's promise is that toggling the view changes the geometry
/// and nothing else.
///
/// The same step on both sides. On latte it is 1.8:1 against `base` where
/// mocha's is 2.5:1, and a step further up the ramp would close that — at the
/// price of the name on the row, which falls from 4.0:1 to 3.4:1 on
/// `surface2`. The cursor's job is to be found and its row's to be read, and
/// latte's lavender tint is strong enough for the first.
pub fn cursor_fill(palette: &Palette) -> egui::Color32 {
    mix(palette.surface1, palette.lavender, ROW_TINT)
}

/// What a row under the pointer is lifted towards: the step below
/// [`cursor_fill`] on the same ramp, warmed by the same amount, so a hover
/// that lands on the cursor row composes with it instead of greying it out.
pub fn hover_fill(palette: &Palette) -> egui::Color32 {
    mix(palette.surface0, palette.lavender, ROW_TINT)
}

/// How far a *selected* row's ground is tinted towards the selection accent,
/// on a dark palette.
///
/// A tenth of the way. Enough to say which files `d` is about to trash, and
/// still a tint and not a fill — at much above this the file names start
/// fighting the ground they are on, and a selection of forty rows would turn
/// the column into a yellow block.
const SELECT_TINT: f32 = 0.10;

/// …and on a light one: a fifth.
///
/// latte's yellow is a deep amber on a near-white ground, and a tenth of it is
/// 1.09:1 against `base` — a wash nobody sees, which leaves the selection to
/// its bar alone. A fifth is a visible cream (1.18:1, as near mocha's 1.30:1 as
/// it gets before the names on it start to lose: 6.0:1 on it, from 6.5:1).
const SELECT_TINT_LIGHT: f32 = 0.20;

/// A selected row's ground: `ground`, tinted towards the selection's yellow.
/// One function for the list and the grid, as [`cursor_fill`] is.
pub fn select_fill(palette: &Palette, ground: egui::Color32) -> egui::Color32 {
    mix(
        ground,
        palette.yellow,
        palette.sided(SELECT_TINT, SELECT_TINT_LIGHT),
    )
}

/// How much more of the text a ripple is splashed with on a light palette
/// than white is on a dark one.
///
/// Twice. A white splash at the ripple's peak (0.08) lifts mocha's `surface0`
/// by 7 in CIE lightness; latte's `text` at the same 0.08 darkens latte's
/// `surface0` by 3.6, and at 0.16 by 7.4 — the same step, the other way.
const LIGHT_SPLASH: f32 = 2.0;

/// A click's ripple at `alpha` (see [`crate::ripple`]).
///
/// On a dark palette the surface brightens under the finger, in white, as it
/// always has. On a light one a white splash is a brightening nobody can see —
/// 1.04:1 on latte's `surface0` — so the surface *darkens* instead, in the
/// palette's own text colour, by the same step in lightness ([`LIGHT_SPLASH`]).
pub fn splash(palette: &Palette, alpha: f32) -> egui::Color32 {
    if palette.light {
        at(palette.text, alpha * LIGHT_SPLASH)
    } else {
        egui::Color32::from_white_alpha((alpha.clamp(0.0, 1.0) * 255.0).round() as u8)
    }
}

/// The light side's scrim: how opaque the wash of `base` is, 0–255.
///
/// A hundred. Under mocha's scrim ([`crate::chrome::HELP_SCRIM`], black at
/// 150) the panes' text is left at 2.7:1 against their ground — pushed back,
/// still legible. latte's `base` at 100 over latte's panes leaves them at
/// 2.8:1: the same distance back, reached by fading towards the ground rather
/// than towards black.
const LIGHT_SCRIM: u8 = 100;

/// The wash laid over the whole window under a card that dims it.
///
/// A flat wash, not a gradient: it covers the window uniformly, so there is no
/// fade-to-transparent to ease. On a dark palette it is black; on a light one
/// it is the palette's own `base`, so the window is pushed back by going
/// paler — a black veil over a light window reads as the lights going out,
/// not as a card coming forward.
pub fn scrim(palette: &Palette) -> egui::Color32 {
    if palette.light {
        at(palette.base, f32::from(LIGHT_SCRIM) / 255.0)
    } else {
        egui::Color32::from_black_alpha(crate::chrome::HELP_SCRIM)
    }
}

/// How much of a dark palette's shadow weight a light palette's shadow keeps.
///
/// A little over half. The ghost card's edge is `crust` at 0.6 over mocha's
/// `surface1`, 14.6 darker in CIE lightness; latte's `text` over latte's
/// `surface1` makes the same step at 0.35 — seven twelfths of 0.6.
const LIGHT_SHADOW: f32 = 7.0 / 12.0;

/// A dark edge or shadow at `alpha`.
///
/// Dark on either side — a shadow is the absence of light, and a light
/// ground does not change that — but lighter in alpha on a light palette,
/// where the same weight of dark is a stronger mark ([`LIGHT_SHADOW`]). On a
/// dark palette it is `crust`, the darkest thing there; on a light one `crust`
/// is paler than the surfaces it would edge, so it is the palette's `text`.
pub fn shadow(palette: &Palette, alpha: f32) -> egui::Color32 {
    if palette.light {
        at(palette.text, alpha * LIGHT_SHADOW)
    } else {
        at(palette.crust, alpha)
    }
}

/// How far an accent that is read as text is taken towards the text colour on
/// a light palette.
///
/// Halfway. catppuccin's accents are ground colours on a dark flavour and
/// mid-tones on latte: latte's yellow on its crust is 2.0:1, and a key legend
/// in it is a legend nobody reads. Halfway to `text` puts every accent this
/// program sets as type — the which-key card's keys, the chips' counts, a
/// filter's matched letters — at 3:1 or better on the grounds they sit on,
/// with the hue still the accent's.
const LIGHT_INK: f32 = 0.5;

/// An accent colour as the ink of text: the accent itself on a dark palette,
/// and on a light one the accent taken halfway to `text` ([`LIGHT_INK`]).
///
/// Only for type. A bar, a dot or a plate in the accent is a mark, and a mark
/// in a saturated hue reads on latte's grounds as it is.
pub fn ink(palette: &Palette, accent: egui::Color32) -> egui::Color32 {
    if palette.light {
        mix(accent, palette.text, LIGHT_INK)
    } else {
        accent
    }
}

/// The grey one step louder than [`Palette::quiet`], for where the two stand
/// side by side as two states of one thing — an archive's folders and its
/// files, a bulk rename's old name changed and unchanged, a chip at rest and
/// under the pointer — and have to stay two.
///
/// `subtext0` on a dark palette, as those places always had it. On latte
/// `quiet` *is* `subtext0`, so the louder of the pair is the next step,
/// `subtext1` (5.5:1 on `base`).
pub fn louder(palette: &Palette) -> egui::Color32 {
    if palette.quiet == palette.subtext0 {
        palette.subtext1
    } else if palette.quiet == palette.subtext1 {
        palette.text
    } else {
        palette.subtext0
    }
}

/// A one-point rule: a card's edge, a menu's separators, the hairline between
/// two quiet tabs.
///
/// `surface1` on a dark palette; one step on, `surface2`, on a light one,
/// where `surface1` on `crust` is 1.4:1 — a hairline that disappears on a
/// laptop panel tilted a few degrees.
pub fn hairline(palette: &Palette) -> egui::Color32 {
    palette.sided(palette.surface1, palette.surface2)
}

/// A scrollbar's thumb: at rest, and lit by the pointer over its band (`lit`
/// 0–1).
///
/// `overlay0` towards `overlay1` on a dark palette; one step on, `overlay1`
/// towards `overlay2`, on a light one — latte's `overlay0` on its `base` is
/// 2.3:1 where mocha's is 3.4:1, and `overlay1` is 2.8:1.
pub fn thumb(palette: &Palette, lit: f32) -> egui::Color32 {
    if palette.light {
        mix(palette.overlay1, palette.overlay2, lit)
    } else {
        mix(palette.overlay0, palette.overlay1, lit)
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

    fn latte() -> Palette {
        Palette::from_theme(&Theme::default(), Appearance::Light)
    }

    #[test]
    fn the_shipped_palette_resolves_every_name() {
        let p = Palette::default();
        assert_eq!(p.base, egui::Color32::from_rgb(0x1e, 0x1e, 0x2e));
        assert_eq!(p.crust, egui::Color32::from_rgb(0x11, 0x11, 0x1b));
        assert_eq!(p.blue, egui::Color32::from_rgb(0x89, 0xb4, 0xfa));
        assert_eq!(p.red, egui::Color32::from_rgb(0xf3, 0x8b, 0xa8));
        assert!(!p.light);
        let l = latte();
        assert_eq!(l.base, egui::Color32::from_rgb(0xef, 0xf1, 0xf5));
        assert_eq!(l.text, egui::Color32::from_rgb(0x4c, 0x4f, 0x69));
        assert!(l.light);
    }

    /// All four flavours, on either side, build a palette in which every name
    /// the painter reads is the flavour's own — none of them the fallback grey.
    #[test]
    fn every_flavour_builds_a_whole_palette() {
        for flavour in df_core::config::flavor_names() {
            for side in [Appearance::Dark, Appearance::Light] {
                let (theme, warnings) = Theme::parse(
                    &format!("[flavor]\n{} = \"{flavour}\"\n", side.name()),
                    std::path::Path::new("theme.toml"),
                );
                assert!(warnings.is_empty(), "{warnings:?}");
                let palette = Palette::from_theme(&theme, side);
                for name in Palette::NAMES {
                    let wanted = theme.color(side, name).expect("a shipped name");
                    assert_eq!(
                        palette.named(name),
                        Some(to_color32(wanted)),
                        "{flavour} {name}"
                    );
                }
                // Only latte is light, and it is light on whichever side it
                // was put.
                assert_eq!(palette.light, flavour == "catppuccin-latte", "{flavour}");
            }
        }
    }

    /// Faint text is `overlay0` on every dark flavour — so the dark side paints
    /// every place it is used exactly as before — and on latte the first step
    /// of the ramp that reads at 3:1 on `base`, which is `overlay2`.
    #[test]
    fn faint_text_is_overlay0_in_the_dark_and_reads_in_the_light() {
        for flavour in [
            "catppuccin-mocha",
            "catppuccin-macchiato",
            "catppuccin-frappe",
        ] {
            let (theme, _) = Theme::parse(
                &format!("[flavor]\ndark = \"{flavour}\"\n"),
                std::path::Path::new("theme.toml"),
            );
            let dark = Palette::from_theme(&theme, Appearance::Dark);
            assert_eq!(dark.faint, dark.overlay0, "{flavour}");
        }
        let l = latte();
        assert_eq!(l.faint, l.overlay2);
        assert!(contrast(l.faint, l.base) >= FAINT_CONTRAST);
        // …and it is the *first* that does: the two quieter steps do not.
        assert!(contrast(l.overlay0, l.base) < FAINT_CONTRAST);
        assert!(contrast(l.overlay1, l.base) < FAINT_CONTRAST);
        // The measurement, as the docs quote it.
        let at = |a, b| (contrast(a, b) * 100.0).round() / 100.0;
        assert_eq!(at(Palette::default().faint, Palette::default().base), 3.36);
        assert_eq!(at(l.faint, l.base), 3.49);

        // Measured, not named: a light side whose ground is darker needs a
        // stronger step to read on it, and gets one.
        let (theme, warnings) = Theme::parse(
            "[palette.light]\nbase = \"#c0c4d0\"\n",
            std::path::Path::new("theme.toml"),
        );
        assert!(warnings.is_empty(), "{warnings:?}");
        let grey = Palette::from_theme(&theme, Appearance::Light);
        assert!(grey.light);
        assert!(contrast(grey.faint, grey.base) >= FAINT_CONTRAST);
        assert_ne!(grey.faint, l.faint);
    }

    /// Quiet text is `overlay1` on every dark flavour, and on latte the first
    /// step louder than faint that reads at 4:1 on `base`, which is
    /// `subtext0`; the step above it keeps a pair of states apart on both.
    #[test]
    fn quiet_text_is_overlay1_in_the_dark_and_reads_in_the_light() {
        for flavour in [
            "catppuccin-mocha",
            "catppuccin-macchiato",
            "catppuccin-frappe",
        ] {
            let (theme, _) = Theme::parse(
                &format!("[flavor]\ndark = \"{flavour}\"\n"),
                std::path::Path::new("theme.toml"),
            );
            let dark = Palette::from_theme(&theme, Appearance::Dark);
            assert_eq!(dark.quiet, dark.overlay1, "{flavour}");
            assert_eq!(louder(&dark), dark.subtext0, "{flavour}");
        }
        let l = latte();
        assert_eq!(l.quiet, l.subtext0);
        assert!(contrast(l.quiet, l.base) >= QUIET_CONTRAST);
        assert!(contrast(l.quiet, l.base) > contrast(l.faint, l.base));
        // overlay2 is louder than faint's step only by being faint's step.
        assert!(contrast(l.overlay2, l.base) < QUIET_CONTRAST);
        assert_eq!(louder(&l), l.subtext1);
        let at = |a, b| (contrast(a, b) * 100.0).round() / 100.0;
        assert_eq!(at(Palette::default().quiet, Palette::default().base), 4.44);
        assert_eq!(at(l.quiet, l.base), 4.37);

        // A light ground dark enough that faint is already `subtext1`
        // leaves quiet the only louder step there is.
        let (theme, _) = Theme::parse(
            "[palette.light]\nbase = \"#c0c4d0\"\n",
            std::path::Path::new("theme.toml"),
        );
        let grey = Palette::from_theme(&theme, Appearance::Light);
        assert_eq!(grey.faint, grey.subtext1);
        assert_eq!(grey.quiet, grey.text);
    }

    /// A `[palette]` override in `theme.toml` reaches the painter, on both
    /// sides; a side's own table reaches only its side.
    #[test]
    fn a_user_override_changes_the_pane_colour() {
        let (theme, warnings) = Theme::parse(
            "[palette]\nbase = \"#000000\"\n\n[palette.light]\nblue = \"#123456\"\n",
            std::path::Path::new("theme.toml"),
        );
        assert!(warnings.is_empty(), "{warnings:?}");
        let dark = Palette::from_theme(&theme, Appearance::Dark);
        let light = Palette::from_theme(&theme, Appearance::Light);
        assert_eq!(dark.base, egui::Color32::BLACK);
        assert_eq!(light.base, egui::Color32::BLACK);
        assert_eq!(light.blue, egui::Color32::from_rgb(0x12, 0x34, 0x56));
        assert_eq!(dark.blue, Palette::default().blue);
        // A black ground under latte's dark text is a dark palette now, and
        // is painted as one.
        assert!(!light.light);
    }

    /// Media chrome is dark whichever side is up, and whatever flavour the
    /// dark side was given.
    #[test]
    fn what_is_drawn_over_a_picture_is_always_dark() {
        assert_eq!(Palette::for_media(&Theme::default()), Palette::default());
        let (theme, _) = Theme::parse(
            "[flavor]\ndark = \"catppuccin-latte\"\n",
            std::path::Path::new("theme.toml"),
        );
        assert!(!Palette::for_media(&theme).light);
    }

    /// The dark side's helpers are exactly what the painters used before
    /// there was a light side.
    #[test]
    fn the_dark_side_is_unchanged() {
        let p = Palette::default();
        assert_eq!(
            splash(&p, 0.08),
            egui::Color32::from_white_alpha((0.08f32 * 255.0).round() as u8)
        );
        assert_eq!(
            scrim(&p),
            egui::Color32::from_black_alpha(crate::chrome::HELP_SCRIM)
        );
        assert_eq!(ink(&p, p.yellow), p.yellow);
        assert_eq!(p.faint, p.overlay0);
        assert_eq!(hairline(&p), p.surface1);
        assert_eq!(thumb(&p, 0.0), p.overlay0);
        assert_eq!(thumb(&p, 1.0), p.overlay1);
        assert_eq!(select_fill(&p, p.base), mix(p.base, p.yellow, 0.10));
        // What the ghost's edge was drawn with: `crust` at its alpha.
        assert_eq!(shadow(&p, 0.6), crate::chrome::fade(p.crust, 0.6));
    }

    /// On the light side the helpers go the other way: a splash darkens, a
    /// scrim goes pale, a shadow stays dark with less weight, accents set as
    /// type deepen, and the quiet end of the ramp moves along.
    #[test]
    fn the_light_side_turns_the_right_way() {
        let l = latte();
        // Colours are held premultiplied; the hue is the unmultiplied one.
        let lum = |c: egui::Color32| {
            let [r, g, b, _] = c.to_srgba_unmultiplied();
            luminance(egui::Color32::from_rgb(r, g, b))
        };
        // Premultiplied: a splash in the text colour is dark ink.
        let s = splash(&l, 0.08);
        assert!(
            s.a() > (0.08f32 * 255.0) as u8 && s.r() < s.a() / 2,
            "{s:?}"
        );
        let veil = scrim(&l);
        assert!(lum(veil) > 0.5, "{veil:?}");
        let edge = shadow(&l, 0.6);
        assert!(edge.a() < egui::Color32::from_black_alpha(153).a());
        assert!(lum(edge) < lum(l.surface1), "{edge:?}");
        assert!(lum(ink(&l, l.yellow)) < lum(l.yellow));
        assert_eq!(l.faint, l.overlay2);
        assert_eq!(hairline(&l), l.surface2);
        assert_eq!(thumb(&l, 0.0), l.overlay1);
        assert_ne!(select_fill(&l, l.base), mix(l.base, l.yellow, 0.10));
    }

    /// A colour held across the switch comes out as the same name on the
    /// other side, and one that was never a palette colour is left alone.
    #[test]
    fn a_held_colour_is_translated_by_name() {
        let (dark, light) = (Palette::default(), latte());
        assert_eq!(dark.translate(dark.blue, &light), light.blue);
        assert_eq!(light.translate(light.text, &dark), dark.text);
        let odd = egui::Color32::from_rgb(1, 2, 3);
        assert_eq!(dark.translate(odd, &light), odd);
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
