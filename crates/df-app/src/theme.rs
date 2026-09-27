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
//!   *as text* on a light ground is taken most of the way to the text, to a
//!   floor ([`ink`]).
//! - **Where a ramp step is the wrong tool** — latte's `surface1` as the
//!   cursor row is the darkest slab in a near-white column. On the light side
//!   the cursor and the hover are washes of the accent over whatever they
//!   stand on ([`cursor_fill`], [`hover_fill`], [`cursor_on_selection`],
//!   [`lift`]).
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
    /// The accents as the ink of text on this palette ([`ink`]), worked out
    /// once when the palette is resolved: on a dark palette each accent is its
    /// own ink.
    inks: [(egui::Color32, egui::Color32); 11],
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
    /// Every name [`Palette::from_theme`] reads, in the order of the fields —
    /// what the flavour tests hold every table to.
    #[cfg(test)]
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
        // The accents, and what each is as type on this side ([`ink`]).
        let accents = [
            "blue", "sky", "red", "yellow", "mauve", "green", "peach", "teal", "lavender",
            "maroon", "pink",
        ]
        .map(pick);
        let inks = accents.map(|accent| {
            if light {
                (accent, inked(accent, text, base))
            } else {
                (accent, accent)
            }
        });
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
            inks,
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
/// selection. On the dark side only: the light side's cursor is a wash of the
/// accent itself ([`CURSOR_TINT_LIGHT`]), which on a near-white pane reads as
/// where you are rather than as a selection, the selection being yellow.
const ROW_TINT: f32 = 0.18;

/// How far the light side's cursor row is tinted from the pane towards
/// `blue`.
///
/// On a light palette the cursor is not a step along the ramp — latte's
/// `surface1` is a grey slab on a near-white pane, the darkest thing in the
/// column, and it reads as a hole rather than as where you are. It is a wash
/// of the accent instead, as a light desktop's own lists draw theirs.
/// Fourteen percent: 12.5 in CIE lightness and hue away from `base`, the
/// row's text on it at 5.9:1, quiet text 3.7:1 and a folder's name 4.9:1 —
/// and on a *selected* row ([`cursor_on_selection`]) 5.4:1, 3.4:1 and 4.5:1,
/// every one above its floor (4.5, 3 and 4.5), where latte's `surface1` step
/// left the text at 4.0:1. Sixteen percent reads a shade stronger and puts a
/// folder's name on a selected cursor row under its floor.
const CURSOR_TINT_LIGHT: f32 = 0.14;

/// …and a row under the pointer: seven percent, a whisper — 6.5 from `base`
/// in CIE lightness and hue, enough to say where the hand is and too little to
/// be mistaken for the cursor.
const HOVER_TINT_LIGHT: f32 = 0.07;

/// What the cursor row is lifted towards.
///
/// One function for the list and the grid so a row and its tile cannot drift
/// apart — the grid's promise is that toggling the view changes the geometry
/// and nothing else.
///
/// On a dark palette, the palette's `surface1`, warmed ([`ROW_TINT`]). On a
/// light one, the pane washed towards `blue` ([`CURSOR_TINT_LIGHT`]).
pub fn cursor_fill(palette: &Palette) -> egui::Color32 {
    if palette.light {
        mix(palette.base, palette.blue, CURSOR_TINT_LIGHT)
    } else {
        mix(palette.surface1, palette.lavender, ROW_TINT)
    }
}

/// What a row under the pointer is lifted towards: on a dark palette the step
/// below [`cursor_fill`] on the same ramp, warmed by the same amount; on a
/// light one the pane's whisper of `blue` ([`HOVER_TINT_LIGHT`]).
pub fn hover_fill(palette: &Palette) -> egui::Color32 {
    if palette.light {
        mix(palette.base, palette.blue, HOVER_TINT_LIGHT)
    } else {
        mix(palette.surface0, palette.lavender, ROW_TINT)
    }
}

/// The cursor standing on a *selected* row: what `cursor` — the fill it takes
/// on the plain pane — becomes over the selection's wash, `selected`.
///
/// On a dark palette the cursor's fill replaces the ground, as it always has,
/// and the selection is kept by its bar. On a light one the cursor is a tint,
/// so it tints whatever it stands on: the selection's cream turned towards
/// blue, which is a third colour — neither the cursor's nor the selection's —
/// so a cursor on a selected row reads as both at once.
pub fn cursor_on_selection(
    palette: &Palette,
    selected: egui::Color32,
    cursor: egui::Color32,
) -> egui::Color32 {
    if palette.light {
        mix(selected, palette.blue, CURSOR_TINT_LIGHT)
    } else {
        cursor
    }
}

/// `under` — a row's fill so far — lifted by the pointer's hover, `amount`
/// 0–1.
///
/// On a dark palette, towards [`hover_fill`], as it always was. On a light one
/// the hover is a tint like the cursor, so it deepens whatever it lies on by
/// the same whisper of `blue` — the pane, a selection, the cursor itself —
/// rather than pulling a selected row or the cursor back towards the pane.
pub fn lift(palette: &Palette, under: egui::Color32, amount: f32) -> egui::Color32 {
    if palette.light {
        mix(
            under,
            palette.blue,
            HOVER_TINT_LIGHT * amount.clamp(0.0, 1.0),
        )
    } else {
        mix(under, hover_fill(palette), amount)
    }
}

/// How far a *selected* row's ground is tinted towards the selection accent,
/// on either side.
///
/// A tenth of the way. Enough to say which files `d` is about to trash, and
/// still a tint and not a fill — at much above this the file names start
/// fighting the ground they are on, and a selection of forty rows would turn
/// the column into a yellow block.
///
/// On latte a tenth is a cream 7.5 from `base` in CIE lightness and hue, where
/// mocha's is 10.9 from its own: quieter, and held there by the cursor that
/// stands on it. Any more and a folder's name on a selected cursor row
/// ([`cursor_on_selection`]) falls under 4.5:1; at a tenth it is 4.5:1, the
/// row's text 5.4:1, and the three states stay apart — the cursor on a
/// selected row 6.3 from the plain cursor and 13.4 from the plain selection.
/// The selection's bar says the rest, as it does on the dark side.
const SELECT_TINT: f32 = 0.10;

/// A selected row's ground: `ground`, tinted towards the selection's yellow.
/// One function for the list and the grid, as [`cursor_fill`] is.
pub fn select_fill(palette: &Palette, ground: egui::Color32) -> egui::Color32 {
    mix(ground, palette.yellow, SELECT_TINT)
}

/// The outline the grid draws round the cursor's tile when it is not
/// selected — the cursor's own colour.
///
/// On a dark palette the cursor's fill, as it always was. On a light one the
/// fill is a pale wash that an outline one point wide cannot carry (1.2:1 on
/// `base`), so the outline is the accent as ink ([`ink`]) — the navy a
/// folder's name is, 5.9:1 — which reads as the same blue family as the row
/// the list draws for it.
pub fn cursor_ring(palette: &Palette) -> egui::Color32 {
    if palette.light {
        ink(palette, palette.blue)
    } else {
        cursor_fill(palette)
    }
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
/// a light palette, at the least.
///
/// A little over half. catppuccin's accents are ground colours on a dark
/// flavour and mid-tones on latte — its blue as a folder's name is a
/// saturated royal blue that shouts down the rest of the column, and its
/// yellow as a key legend is 2.3:1 and not read at all. Fifty-five percent of
/// the way to `text` makes blue a quiet navy, `#3759a8`: text with a hint of
/// hue, 5.9:1 on `base`.
const LIGHT_INK: f32 = 0.55;

/// The floor an accent set as type is held to on a light palette's `base`:
/// 4.5:1, the line body text is held to. Blue, red, mauve, green, peach,
/// teal, lavender and maroon clear it at [`LIGHT_INK`]; sky, yellow and pink,
/// catppuccin's palest, are taken on towards `text` until they do.
pub const INK_CONTRAST: f32 = 4.5;

/// `accent` as type on a light ground: taken [`LIGHT_INK`] of the way to
/// `text`, and further, a percent at a time, until it reads at
/// [`INK_CONTRAST`] on `base`.
fn inked(accent: egui::Color32, text: egui::Color32, base: egui::Color32) -> egui::Color32 {
    let mut t = LIGHT_INK;
    loop {
        let ink = mix(accent, text, t);
        if t >= 1.0 || contrast(ink, base) >= INK_CONTRAST {
            return ink;
        }
        t = (t + 0.01).min(1.0);
    }
}

/// An accent colour as the ink of text — the one rule for every place an
/// accent is set as type: a folder's name, an executable's, a broken link's,
/// a chip's label, a filter's matched letters, a card's heading, an error.
///
/// The accent itself on a dark palette. On a light one the accent taken
/// towards `text` ([`LIGHT_INK`], and past it to [`INK_CONTRAST`]), looked up
/// from the palette's own table for its accents and worked out on the spot
/// for any other colour.
///
/// Only for type. A bar, a dot, a plate or an icon in the accent is a mark,
/// and a mark in a saturated hue reads on latte's grounds as it is. And not
/// for the preview's content — a file's syntax colours, a document's links —
/// which is the flavour's own colouring of somebody else's text.
pub fn ink(palette: &Palette, accent: egui::Color32) -> egui::Color32 {
    if !palette.light {
        return accent;
    }
    palette
        .inks
        .iter()
        .find(|(from, _)| *from == accent)
        .map_or_else(|| inked(accent, palette.text, palette.base), |(_, to)| *to)
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
        // The rows: a step up the ramp, warmed, and the cursor on a selected
        // row is the cursor's own fill; a hover mixes towards its fill.
        assert_eq!(cursor_fill(&p), mix(p.surface1, p.lavender, 0.18));
        assert_eq!(hover_fill(&p), mix(p.surface0, p.lavender, 0.18));
        let selected = select_fill(&p, p.base);
        assert_eq!(
            cursor_on_selection(&p, selected, cursor_fill(&p)),
            cursor_fill(&p)
        );
        assert_eq!(lift(&p, selected, 0.5), mix(selected, hover_fill(&p), 0.5));
        assert_eq!(ink(&p, p.blue), p.blue);
        assert_eq!(cursor_ring(&p), cursor_fill(&p));
    }

    /// Two decimals, as the docs quote a contrast.
    fn ratio(a: egui::Color32, b: egui::Color32) -> f32 {
        (contrast(a, b) * 100.0).round() / 100.0
    }

    /// On the light side the cursor row is a wash of blue, not latte's grey
    /// `surface1`: the row's text reads on it at 4.5:1 or better and quiet
    /// text at 3:1; a hover is a lighter wash of the same; and the cursor on
    /// a selected row is a third colour, neither the cursor's nor the
    /// selection's, that the text still reads on.
    #[test]
    fn the_light_cursor_is_a_wash_the_row_still_reads_on() {
        let l = latte();
        let cursor = cursor_fill(&l);
        assert_eq!(cursor, mix(l.base, l.blue, 0.14));
        let navy = ink(&l, l.blue);
        assert!(ratio(l.text, cursor) >= 4.5, "{}", ratio(l.text, cursor));
        assert!(ratio(l.quiet, cursor) >= 3.0, "{}", ratio(l.quiet, cursor));
        assert!(
            ratio(navy, cursor) >= INK_CONTRAST,
            "{}",
            ratio(navy, cursor)
        );
        let hover = hover_fill(&l);
        assert_eq!(hover, mix(l.base, l.blue, 0.07));
        assert!(contrast(hover, l.base) < contrast(cursor, l.base));

        let selected = select_fill(&l, l.base);
        let both = cursor_on_selection(&l, selected, cursor);
        assert_eq!(both, mix(selected, l.blue, 0.14));
        assert_ne!(both, cursor);
        assert_ne!(both, selected);
        // Every number on every row state clears its floor: the row's text
        // 4.5:1, quiet text 3:1, a folder's name 4.5:1.
        for (state, ground) in [("selected", selected), ("cursor and selected", both)] {
            assert!(
                ratio(l.text, ground) >= 4.5,
                "{state}: {}",
                ratio(l.text, ground)
            );
            assert!(
                ratio(l.quiet, ground) >= 3.0,
                "{state}: {}",
                ratio(l.quiet, ground)
            );
            assert!(
                ratio(navy, ground) >= INK_CONTRAST,
                "{state}: {}",
                ratio(navy, ground)
            );
        }
        println!(
            "light rows — cursor: text {} quiet {} folder {}; selected: text {} quiet {} folder {}; both: text {} quiet {} folder {}; hover: text {}",
            ratio(l.text, cursor), ratio(l.quiet, cursor), ratio(navy, cursor),
            ratio(l.text, selected), ratio(l.quiet, selected), ratio(navy, selected),
            ratio(l.text, both), ratio(l.quiet, both), ratio(navy, both),
            ratio(l.text, hover),
        );
        // The grid's outline round the cursor is the navy, which reads.
        assert_eq!(cursor_ring(&l), navy);
        assert!(ratio(cursor_ring(&l), l.base) >= 4.5);
        // A hover deepens whatever it lies on, the cursor included, rather
        // than pulling it back towards the pane.
        assert!(luminance(lift(&l, cursor, 1.0)) < luminance(cursor));
        assert!(luminance(lift(&l, selected, 1.0)) < luminance(selected));
        assert_eq!(lift(&l, l.base, 1.0), hover);
    }

    /// Every accent set as type on the light side reads at 4.5:1 on `base`;
    /// blue — a folder's name — is the quiet navy fifty-five percent of the
    /// way to `text`, 5.9:1, and reads on the cursor row too.
    #[test]
    fn accents_as_type_reach_their_floor_on_the_light_side() {
        let l = latte();
        let navy = ink(&l, l.blue);
        assert_eq!(navy, mix(l.blue, l.text, 0.55));
        assert_eq!(navy, egui::Color32::from_rgb(0x37, 0x59, 0xa8));
        assert_eq!(ratio(navy, l.base), 5.88);
        assert!(ratio(navy, cursor_fill(&l)) >= INK_CONTRAST);
        for accent in [
            l.blue, l.sky, l.red, l.yellow, l.mauve, l.green, l.peach, l.teal, l.lavender,
            l.maroon, l.pink,
        ] {
            let inked = ink(&l, accent);
            assert!(
                contrast(inked, l.base) >= INK_CONTRAST,
                "{accent:?} → {inked:?} at {}",
                ratio(inked, l.base)
            );
            // The table and the rule agree, for a colour the table does not
            // hold as much as for one it does.
            assert_eq!(inked, inked_by_rule(&l, accent));
        }
        // A colour the palette never handed out is inked by the same rule.
        let custom = egui::Color32::from_rgb(0xff, 0x80, 0x00);
        assert!(contrast(ink(&l, custom), l.base) >= INK_CONTRAST);
    }

    fn inked_by_rule(palette: &Palette, accent: egui::Color32) -> egui::Color32 {
        inked(accent, palette.text, palette.base)
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
        assert_eq!(select_fill(&l, l.base), mix(l.base, l.yellow, 0.10));
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
