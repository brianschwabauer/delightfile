//! The picture the pointer carries out of the window.
//!
//! Once a drag is handed to the compositor the pointer stops being ours, and
//! with it every pixel of [`crate::dnd`]'s ghost: the card that was following
//! the cursor is drawn *inside* our window, and our window is no longer where
//! the cursor is. A `wl_data_device` drag can carry a surface of its own for
//! exactly this reason, and this file draws it.
//!
//! It is the same object as the internal ghost — a small stack of cards with a
//! count on it — drawn a second time with a different tool, because there is no
//! shared one: inside the window it is an [`egui::Painter`] over the GPU, and
//! out here it is bytes in a `wl_shm` buffer that the compositor composites
//! itself. Duplication is the honest answer (a painter that could target both
//! would be a rendering abstraction written for one 200-pixel image).
//!
//! ## The format
//!
//! `wl_shm`'s `Argb8888` is one 32-bit word per pixel in **native byte order**
//! — so on this machine the bytes come out `B G R A` — with the colour channels
//! **premultiplied** by alpha. Getting either wrong is not a crash: it is a
//! drag icon with a black halo, which is exactly the sort of bug that survives
//! to a release because it looks like a shadow.

/// The card's size in the icon, in pixels at scale 1.
///
/// Smaller than [`crate::dnd::GHOST_WIDTH`]: a cursor-attached surface travels
/// over other applications' windows, and something the width of a list row
/// would be a placard rather than a token.
const CARD_W: u32 = 128;
const CARD_H: u32 = 30;

/// The corner radius, and the offset between stacked cards. Both are the
/// internal ghost's numbers scaled to this card, so the two read as one object.
const RADIUS: f32 = 7.0;
const STEP: u32 = 5;

/// How many cards behind the top one — [`crate::dnd::GHOST_STACK`], and for the
/// same reason.
const STACK: u32 = 3;

/// The margin the stack sits in, so the offsets and the badge have somewhere to
/// go and the surface's own edge never clips a card.
const PAD: u32 = 3;

/// Where the pointer sits on the icon: the top card's grab point, matching
/// [`crate::dnd::GHOST_GRAB`] so the card does not jump the moment the drag
/// crosses the window's edge.
pub const HOTSPOT: (i32, i32) = (PAD as i32 + 16, PAD as i32 + CARD_H as i32 / 2);

/// A drawn icon, ready to be copied into a shm pool.
pub struct Icon {
    pub width: u32,
    pub height: u32,
    /// `Argb8888`, premultiplied, native byte order.
    pub pixels: Vec<u8>,
}

/// One straight-alpha colour on the way in. Premultiplication happens once, at
/// the point a pixel is written, so no caller can forget it.
#[derive(Clone, Copy)]
pub struct Rgba(pub u8, pub u8, pub u8, pub u8);

/// Draw the stack for a drag of `count` items.
///
/// `card` is the card's own colour and `ink` is what the count is written in —
/// handed in rather than hard-coded so the icon is the running theme's colours
/// and not a second palette that drifts from it.
// VERIFY-LIVE: what this draws is only visible *outside* the window, so no
// test in this file can say whether the card is the right size on screen, or
// whether the hotspot puts it under the cursor rather than beside it. Drag a
// selection out and look at what the pointer is carrying.
pub fn draw(count: usize, card: Rgba, ink: Rgba) -> Icon {
    let behind = (count.saturating_sub(1) as u32).min(STACK);
    let width = CARD_W + PAD * 2 + STACK * STEP / 2;
    let height = CARD_H + PAD * 2 + STACK * STEP;
    let mut buf = Canvas::new(width, height);

    // Back to front, so the top card lands over the ones behind it.
    for depth in (1..=behind).rev() {
        let alpha = 1.0 - 0.2 * depth as f32;
        buf.rounded_rect(
            (PAD + depth * STEP / 2) as f32,
            (PAD + depth * STEP) as f32,
            CARD_W as f32,
            CARD_H as f32,
            RADIUS,
            card,
            alpha,
        );
    }
    buf.rounded_rect(
        PAD as f32,
        PAD as f32,
        CARD_W as f32,
        CARD_H as f32,
        RADIUS,
        card,
        1.0,
    );
    if count > 1 {
        // The number goes on the *top* card's trailing edge, where nothing else
        // is: inside the window the badge can float beside the stack, but out
        // here every pixel outside the card is somebody else's window.
        let text = count.to_string();
        let w = digits_width(&text);
        buf.digits(
            (PAD + CARD_W) as f32 - w - 9.0,
            PAD as f32 + (CARD_H as f32 - DIGIT_H as f32 * SCALE as f32) / 2.0,
            &text,
            ink,
        );
    }
    Icon {
        width,
        height,
        pixels: buf.pixels,
    }
}

/// A premultiplied `Argb8888` surface, and the two shapes this file draws on it.
struct Canvas {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl Canvas {
    fn new(width: u32, height: u32) -> Canvas {
        Canvas {
            width,
            height,
            // Fully transparent, which in premultiplied form is all zeroes —
            // the one colour that is the same in both conventions.
            pixels: vec![0; (width * height * 4) as usize],
        }
    }

    /// Source-over one pixel at `coverage` (0…1 antialiasing) times the colour's
    /// own alpha.
    fn blend(&mut self, x: u32, y: u32, color: Rgba, coverage: f32) {
        if x >= self.width || y >= self.height || coverage <= 0.0 {
            return;
        }
        let a = (color.3 as f32 / 255.0) * coverage.clamp(0.0, 1.0);
        if a <= 0.0 {
            return;
        }
        let i = ((y * self.width + x) * 4) as usize;
        let src = [color.2, color.1, color.0]; // B, G, R — native little-endian
        for (channel, value) in src.iter().enumerate() {
            let under = self.pixels[i + channel] as f32;
            let over = *value as f32 * a;
            self.pixels[i + channel] = (over + under * (1.0 - a)).round().min(255.0) as u8;
        }
        let under = self.pixels[i + 3] as f32;
        self.pixels[i + 3] = (a * 255.0 + under * (1.0 - a)).round().min(255.0) as u8;
    }

    /// A rounded rectangle, antialiased by signed distance over one pixel.
    ///
    /// A pixel's coverage is `0.5 - distance`, clamped: at the shape's edge the
    /// distance is zero and the pixel is half covered, which is what makes a
    /// 7-pixel radius read as a curve rather than as four staircases.
    #[allow(clippy::too_many_arguments)]
    fn rounded_rect(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        radius: f32,
        color: Rgba,
        alpha: f32,
    ) {
        let color = Rgba(color.0, color.1, color.2, (color.3 as f32 * alpha) as u8);
        let radius = radius.min(w / 2.0).min(h / 2.0);
        let (x0, y0) = (x.floor() as i32 - 1, y.floor() as i32 - 1);
        let (x1, y1) = ((x + w).ceil() as i32 + 1, (y + h).ceil() as i32 + 1);
        for py in y0.max(0)..y1.max(0) {
            for px in x0.max(0)..x1.max(0) {
                let cx = px as f32 + 0.5;
                let cy = py as f32 + 0.5;
                // Distance to the rounded box, positive outside.
                let dx = (x + radius - cx).max(cx - (x + w - radius)).max(0.0);
                let dy = (y + radius - cy).max(cy - (y + h - radius)).max(0.0);
                let outside = if dx > 0.0 && dy > 0.0 {
                    (dx * dx + dy * dy).sqrt() - radius
                } else {
                    let flat_x = (x - cx).max(cx - (x + w));
                    let flat_y = (y - cy).max(cy - (y + h));
                    flat_x.max(flat_y)
                };
                self.blend(px as u32, py as u32, color, 0.5 - outside);
            }
        }
    }

    /// A run of digits in the 3×5 face below.
    fn digits(&mut self, x: f32, y: f32, text: &str, color: Rgba) {
        let mut pen = x;
        for character in text.chars() {
            let Some(glyph) = digit(character) else { continue };
            for (row, bits) in glyph.iter().enumerate() {
                for column in 0..DIGIT_W {
                    if bits & (1 << (DIGIT_W - 1 - column)) == 0 {
                        continue;
                    }
                    // One "pixel" of the face is a SCALE×SCALE block. No
                    // antialiasing: a bitmap face blurred is a smudge, and at
                    // this size the staircase *is* the letterform.
                    for sy in 0..SCALE {
                        for sx in 0..SCALE {
                            let px = pen as u32 + (column as u32 * SCALE) + sx;
                            let py = y as u32 + (row as u32 * SCALE) + sy;
                            self.blend(px, py, color, 1.0);
                        }
                    }
                }
            }
            pen += (DIGIT_W as u32 * SCALE + DIGIT_GAP) as f32;
        }
    }
}

/// The count face: 3 px wide, 5 px tall, one `u8` per row with the low three
/// bits set.
///
/// Hand-drawn rather than laid out with a real font, because the alternative is
/// linking a rasteriser into a 200-pixel image whose entire vocabulary is ten
/// glyphs. Every digit is legible at [`SCALE`], which is the only requirement.
const DIGIT_W: usize = 3;
const DIGIT_H: usize = 5;

/// How many device pixels one cell of the face is. Three: a 9×15 digit, which
/// is about the size of the row text the drag came from.
const SCALE: u32 = 3;

/// The space between two digits, in device pixels.
const DIGIT_GAP: u32 = 3;

fn digit(character: char) -> Option<[u8; DIGIT_H]> {
    Some(match character {
        '0' => [0b111, 0b101, 0b101, 0b101, 0b111],
        '1' => [0b010, 0b110, 0b010, 0b010, 0b111],
        '2' => [0b111, 0b001, 0b111, 0b100, 0b111],
        '3' => [0b111, 0b001, 0b111, 0b001, 0b111],
        '4' => [0b101, 0b101, 0b111, 0b001, 0b001],
        '5' => [0b111, 0b100, 0b111, 0b001, 0b111],
        '6' => [0b111, 0b100, 0b111, 0b101, 0b111],
        '7' => [0b111, 0b001, 0b010, 0b010, 0b010],
        '8' => [0b111, 0b101, 0b111, 0b101, 0b111],
        '9' => [0b111, 0b101, 0b111, 0b001, 0b111],
        _ => return None,
    })
}

/// How wide a run of digits will be, so it can be right-aligned on the card.
fn digits_width(text: &str) -> f32 {
    let count = text.chars().filter(|c| digit(*c).is_some()).count() as u32;
    if count == 0 {
        return 0.0;
    }
    (count * (DIGIT_W as u32 * SCALE) + (count - 1) * DIGIT_GAP) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    const CARD: Rgba = Rgba(60, 70, 90, 255);
    const INK: Rgba = Rgba(240, 240, 250, 255);

    fn alpha_at(icon: &Icon, x: u32, y: u32) -> u8 {
        icon.pixels[((y * icon.width + x) * 4 + 3) as usize]
    }

    fn word_at(icon: &Icon, x: u32, y: u32) -> [u8; 4] {
        let i = ((y * icon.width + x) * 4) as usize;
        [
            icon.pixels[i],
            icon.pixels[i + 1],
            icon.pixels[i + 2],
            icon.pixels[i + 3],
        ]
    }

    /// The buffer is the size it says it is, and every pixel of it exists.
    #[test]
    fn the_icon_is_a_whole_argb_buffer() {
        let icon = draw(1, CARD, INK);
        assert_eq!(icon.pixels.len(), (icon.width * icon.height * 4) as usize);
        assert!(icon.width > CARD_W && icon.height > CARD_H);
        // The size does not change with the count: one surface, one hotspot.
        let many = draw(97, CARD, INK);
        assert_eq!((many.width, many.height), (icon.width, icon.height));
    }

    /// Transparent outside the card, opaque inside it, and premultiplied.
    #[test]
    fn the_card_is_drawn_and_the_margin_is_not() {
        let icon = draw(1, CARD, INK);
        // The very corner of the surface is outside the rounded card.
        assert_eq!(alpha_at(&icon, 0, 0), 0);
        // The middle of the card is solid, and in B G R A order.
        let mid = word_at(&icon, PAD + CARD_W / 2, PAD + CARD_H / 2);
        assert_eq!(mid, [CARD.2, CARD.1, CARD.0, 255]);
        // The corner *of the card* is rounded away, and the edge beside it is
        // partly covered — that is the antialiasing doing its job.
        assert_eq!(alpha_at(&icon, PAD, PAD), 0);
        // Somewhere along that curve a pixel is partly covered — that is the
        // antialiasing, and without it the corner is a staircase.
        let soft = (0..RADIUS as u32 + 2)
            .flat_map(|y| (0..RADIUS as u32 + 2).map(move |x| (PAD + x, PAD + y)))
            .map(|(x, y)| alpha_at(&icon, x, y))
            .any(|a| a > 0 && a < 255);
        assert!(soft, "the rounded corner is not antialiased");
    }

    /// No premultiplied pixel may have more colour than it has alpha — that is
    /// the halo bug, and it is invisible until it is on somebody's wallpaper.
    #[test]
    fn no_pixel_carries_more_colour_than_alpha() {
        for count in [1, 2, 4, 128] {
            let icon = draw(count, CARD, INK);
            for pixel in icon.pixels.chunks_exact(4) {
                let a = pixel[3];
                for channel in &pixel[..3] {
                    assert!(
                        *channel <= a.saturating_add(1),
                        "channel {channel} > alpha {a} at count {count}"
                    );
                }
            }
        }
    }

    /// A stack of several is bigger than one, and the count is written on it.
    #[test]
    fn several_files_stack_and_are_counted() {
        let one = draw(1, CARD, INK);
        let four = draw(4, CARD, INK);
        let lit = |icon: &Icon| icon.pixels.chunks_exact(4).filter(|p| p[3] > 0).count();
        assert!(lit(&four) > lit(&one), "a stack must cover more ground");
        // A single file has no number on it; two do.
        let two = draw(2, CARD, INK);
        let ink_pixels = |icon: &Icon| {
            icon.pixels
                .chunks_exact(4)
                .filter(|p| p[0] == INK.2 && p[1] == INK.1 && p[2] == INK.0)
                .count()
        };
        assert_eq!(ink_pixels(&one), 0);
        assert!(ink_pixels(&two) > 0);
        // …and a wider number is wider ink.
        assert!(ink_pixels(&draw(128, CARD, INK)) > ink_pixels(&two));
    }

    /// The face's metrics, which the right-alignment depends on.
    #[test]
    fn the_count_face_measures_itself() {
        assert_eq!(digits_width(""), 0.0);
        assert_eq!(digits_width("7"), (DIGIT_W as u32 * SCALE) as f32);
        assert_eq!(
            digits_width("42"),
            (2 * DIGIT_W as u32 * SCALE + DIGIT_GAP) as f32
        );
        // Every digit is drawn; nothing else is.
        for c in "0123456789".chars() {
            assert!(digit(c).is_some(), "{c}");
        }
        assert!(digit('x').is_none());
        // No glyph sets a bit outside its three columns.
        for c in "0123456789".chars() {
            for row in digit(c).expect("a digit") {
                assert_eq!(row & !0b111, 0, "{c} spills out of its cell");
            }
        }
    }

    /// The hotspot is on the card, not in the margin — the pointer must not
    /// appear to hold the icon by thin air.
    #[test]
    fn the_pointer_holds_the_top_card() {
        let icon = draw(3, CARD, INK);
        let (x, y) = (HOTSPOT.0 as u32, HOTSPOT.1 as u32);
        assert!(x < icon.width && y < icon.height);
        assert_eq!(alpha_at(&icon, x, y), 255);
    }
}
