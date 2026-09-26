//! What a tag looks like: its colour, and the little overlapping dots a row
//! wears for its coloured tags (the tags themselves are df-core's,
//! [`df_core::fs::tags`]).
//!
//! **Seven colours come with the program.** `red orange yellow green blue
//! purple grey` — Finder's set — each painted with the palette's colour of
//! that name, so a `[palette]` override in `theme.toml` re-tints them like
//! everything else: `orange` is `peach`, `purple` is `mauve` and `grey` is
//! `overlay1` in catppuccin's words, and the rest are their own names.
//! `[tags]` in `delightfile.toml` gives any other tag a colour — a palette
//! name or a hex — and a tag with none is text only: it shows in the `m t`
//! column and on the spot panel, and wears no dot.
//!
//! **Dots, not chips.** A row is one line in a column of hundreds, and a
//! coloured word per tag would push the names apart; a 6-point dot per colour,
//! overlapping the one before it as Finder's do, says "this is tagged red and
//! blue" in the width of two letters. Three at most, because a fourth
//! overlapping dot is a smudge rather than a colour.

use df_core::config::{TagColor, Theme};

use crate::icons::to_color32;
use crate::theme::Palette;

/// One dot's diameter, in points.
pub const DOT: f32 = 6.0;

/// How far each dot sits from the one before it, centre to centre, in points:
/// two thirds of a dot, so each overlaps the last by a third — enough that
/// the colours read as a stack and not as a row of beads.
pub const DOT_STEP: f32 = 4.0;

/// The most dots a row draws. The rest of a file's tags are in the `m t`
/// column and on the spot panel.
pub const MAX_DOTS: usize = 3;

/// The ring each dot wears in the colour behind it, in points, so where two
/// overlap the later one is cut out of the earlier rather than merged into it.
const RING: f32 = 1.0;

/// The gap between a name and its dots, in points.
pub const DOT_GAP: f32 = 5.0;

/// The tag colours this window paints with: the seven built in, and whatever
/// `[tags]` adds, resolved once against the theme.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TagColors {
    /// `[tags]`, each tag lowercased, with the colour its line resolved to.
    custom: Vec<(String, egui::Color32)>,
}

/// No `[tags]` at all — the seven built-in colours only: what a test paints
/// with, having no config to hand.
#[cfg(test)]
pub static BUILT_IN: TagColors = TagColors { custom: Vec::new() };

impl TagColors {
    /// Resolve `[tags]` against the theme. A line whose palette name the
    /// theme does not have is left out, with a warning: a tag painted in a
    /// made-up grey would be a colour nobody asked for.
    pub fn new(tags: &[(String, TagColor)], theme: &Theme) -> TagColors {
        let mut custom = Vec::new();
        for (tag, color) in tags {
            let resolved = match color {
                TagColor::Hex(color) => Some(to_color32(*color)),
                TagColor::Named(name) => {
                    let name = name.trim();
                    let lower = name.to_lowercase();
                    theme
                        .color(palette_name(&lower).unwrap_or(&lower))
                        .or_else(|| theme.color(name))
                        .map(to_color32)
                }
            };
            match resolved {
                Some(color) => custom.push((tag.to_lowercase(), color)),
                None => log::warn!("[tags] {tag}: no colour called that in the palette"),
            }
        }
        TagColors { custom }
    }

    /// The colour `tag` is painted in, or `None` for a tag that is text only.
    /// A `[tags]` line wins over the built-in colour of the same name.
    pub fn color(&self, tag: &str, palette: &Palette) -> Option<egui::Color32> {
        let lower = tag.to_lowercase();
        if let Some((_, color)) = self.custom.iter().find(|(name, _)| *name == lower) {
            return Some(*color);
        }
        Some(match lower.as_str() {
            "red" => palette.red,
            "orange" => palette.peach,
            "yellow" => palette.yellow,
            "green" => palette.green,
            "blue" => palette.blue,
            "purple" => palette.mauve,
            "grey" => palette.overlay1,
            _ => return None,
        })
    }

    /// The colours of a row's dots: its coloured tags, in order, at most
    /// [`MAX_DOTS`] of them.
    pub fn dots(&self, tags: &[String], palette: &Palette) -> Vec<egui::Color32> {
        tags.iter()
            .filter_map(|tag| self.color(tag, palette))
            .take(MAX_DOTS)
            .collect()
    }
}

/// The palette's name for a colour tag's word, where the two differ.
fn palette_name(word: &str) -> Option<&'static str> {
    Some(match word {
        "orange" => "peach",
        "purple" => "mauve",
        "grey" => "overlay1",
        "red" => "red",
        "yellow" => "yellow",
        "green" => "green",
        "blue" => "blue",
        _ => return None,
    })
}

/// How wide `count` overlapping dots are, in points.
pub fn dots_width(count: usize) -> f32 {
    match count {
        0 => 0.0,
        n => DOT + DOT_STEP * (n - 1) as f32,
    }
}

/// Paint `colours` as overlapping dots, the first with its left edge at
/// `left`, all centred on `y`. Each wears a ring of `behind` — the colour
/// the dots sit on — so the one on top is cut out of the one under it.
pub fn paint_dots(
    painter: &egui::Painter,
    left: f32,
    y: f32,
    colours: &[egui::Color32],
    behind: egui::Color32,
) {
    for (index, colour) in colours.iter().enumerate() {
        let centre = egui::pos2(left + DOT / 2.0 + DOT_STEP * index as f32, y);
        if index > 0 {
            painter.circle_filled(centre, DOT / 2.0 + RING, behind);
        }
        painter.circle_filled(centre, DOT / 2.0, *colour);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use df_core::config::Color;

    /// The seven colour tags are the palette's own colours, in any case, and
    /// every other tag is text until `[tags]` says otherwise.
    #[test]
    fn the_seven_colours_are_the_palette_s() {
        let palette = Palette::default();
        let colors = TagColors::default();
        assert_eq!(colors.color("red", &palette), Some(palette.red));
        assert_eq!(colors.color("Orange", &palette), Some(palette.peach));
        assert_eq!(colors.color("YELLOW", &palette), Some(palette.yellow));
        assert_eq!(colors.color("green", &palette), Some(palette.green));
        assert_eq!(colors.color("blue", &palette), Some(palette.blue));
        assert_eq!(colors.color("purple", &palette), Some(palette.mauve));
        assert_eq!(colors.color("grey", &palette), Some(palette.overlay1));
        assert_eq!(colors.color("work", &palette), None);
        assert_eq!(
            colors.color("gray", &palette),
            None,
            "the seven, as spelled"
        );
        for word in df_core::fs::tags::COLOURS {
            assert!(colors.color(word, &palette).is_some(), "{word}");
        }
    }

    /// `[tags]` colours a tag by palette name, by one of the colour tags'
    /// own words, or by hex — and overrides a built-in one.
    #[test]
    fn a_tags_table_adds_colours() {
        let theme = Theme::default();
        let palette = Palette::from_theme(&theme);
        let lines = vec![
            ("Work".to_string(), TagColor::Named("blue".to_string())),
            ("later".to_string(), TagColor::Named("Orange".to_string())),
            (
                "urgent".to_string(),
                TagColor::Hex(Color { r: 255, g: 0, b: 0 }),
            ),
            ("red".to_string(), TagColor::Named("teal".to_string())),
            (
                "nope".to_string(),
                TagColor::Named("ultraviolet".to_string()),
            ),
        ];
        let colors = TagColors::new(&lines, &theme);
        assert_eq!(colors.color("work", &palette), Some(palette.blue));
        assert_eq!(colors.color("LATER", &palette), Some(palette.peach));
        assert_eq!(
            colors.color("urgent", &palette),
            Some(egui::Color32::from_rgb(255, 0, 0))
        );
        assert_eq!(colors.color("red", &palette), Some(palette.teal));
        assert_eq!(
            colors.color("nope", &palette),
            None,
            "an unknown name is left out"
        );
    }

    /// A row shows its coloured tags' dots, in order, three at most, and
    /// three dots are a dot and two steps wide.
    #[test]
    fn a_row_wears_at_most_three_dots() {
        let palette = Palette::default();
        let tags: Vec<String> = ["work", "red", "blue", "green", "yellow"]
            .iter()
            .map(|t| t.to_string())
            .collect();
        assert_eq!(
            BUILT_IN.dots(&tags, &palette),
            vec![palette.red, palette.blue, palette.green]
        );
        assert!(BUILT_IN.dots(&tags[..1], &palette).is_empty());
        assert_eq!(dots_width(0), 0.0);
        assert_eq!(dots_width(1), DOT);
        assert_eq!(dots_width(3), DOT + 2.0 * DOT_STEP);
    }
}
