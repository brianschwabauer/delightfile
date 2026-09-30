//! Which part of the window's top is the title bar and which is the
//! program's own, when the program draws into the title bar
//! (`plans/other-platforms/04-windows.md` W4.39).
//!
//! On Windows the top row — or, with two tabs or more, the tab strip — is
//! drawn where the system's title bar was, and the system keeps the rest of
//! the title bar's work: dragging the window, a double click that maximizes,
//! the system menu on a right click, the resize edge along the top, and the
//! three caption buttons with Snap Layouts under the middle one. The system
//! learns which is which by asking the window, point by point
//! (`WM_NCHITTEST`); this module is the answer, without a line of Win32 in
//! it, so it is tested on every target.
//!
//! The answer comes from three things. The *frame*: how wide the window is,
//! how thick its resize edge, whether it is maximized (a maximized window
//! has no edge to take hold of) and where the caption buttons are. The
//! *band*: the strip across the top that is the title bar's, as the window
//! last laid it out. The *controls*: the rects in the band that are the
//! window's own — ☰, a crumb, the counter, a tab chip, its `×`, the `+` —
//! reported after every layout. A point in the band on none of them is the
//! title bar's; everything below the band is the window's.
//!
//! Everything here is physical pixels in the window's client coordinates,
//! which is what the system hands over. The layout is in logical points, so
//! a rect is taken out to the whole pixels it touches ([`Px::around`]): a
//! control whose edge falls inside a pixel owns that pixel.

use crate::ui::TitleBand;

/// A rectangle in physical pixels, client coordinates: left and top inside
/// it, right and bottom just outside, as a Win32 `RECT` is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Px {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Px {
    /// The empty rectangle, which contains nothing.
    pub const EMPTY: Px = Px {
        left: 0,
        top: 0,
        right: 0,
        bottom: 0,
    };

    pub fn contains(&self, (x, y): (i32, i32)) -> bool {
        x >= self.left && x < self.right && y >= self.top && y < self.bottom
    }

    pub fn width(&self) -> i32 {
        self.right - self.left
    }

    /// `rect`, in logical points at `scale`, out to every pixel it touches.
    pub fn around(rect: egui::Rect, scale: f64) -> Px {
        let at = |value: f32| f64::from(value) * scale;
        Px {
            left: at(rect.min.x).floor() as i32,
            top: at(rect.min.y).floor() as i32,
            right: at(rect.max.x).ceil() as i32,
            bottom: at(rect.max.y).ceil() as i32,
        }
    }

    /// The same rectangle moved by `(dx, dy)`.
    pub fn moved(&self, (dx, dy): (i32, i32)) -> Px {
        Px {
            left: self.left + dx,
            top: self.top + dy,
            right: self.right + dx,
            bottom: self.bottom + dy,
        }
    }
}

/// What a point in the window is, as the system asks it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    /// The window's own: a control in the band, or anything below it.
    Client,
    /// The title bar: a drag moves the window, a double click maximizes it,
    /// a right click opens the system menu.
    Caption,
    /// The resize edge along the top, and its two corners.
    Top,
    TopLeft,
    TopRight,
    /// The three caption buttons.
    Minimize,
    Maximize,
    Close,
}

/// The window, as the hit test needs it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    /// The client area's width.
    pub width: i32,
    /// How deep the resize edge along the top is.
    pub resize: i32,
    /// A maximized window has no edge to resize by: the band runs to the
    /// top of the screen, where a drag from the top is a drag of the window.
    pub maximized: bool,
    /// Where the caption buttons are, when the system says.
    pub buttons: Option<Px>,
}

/// What the window last said about its band: where it is, and which rects in
/// it are the window's own controls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Regions {
    pub band: Px,
    pub controls: Vec<Px>,
}

impl Regions {
    /// No band: every point is the window's until the first layout says
    /// otherwise.
    pub const NONE: Regions = Regions {
        band: Px::EMPTY,
        controls: Vec::new(),
    };
}

/// What `at` is. In this order: a caption button, which the system draws and
/// which must answer wherever it is drawn; the resize edge along the top,
/// unless the window is maximized; below the band, the window's; on one of
/// the window's controls, the window's; and anywhere else in the band, the
/// title bar.
pub fn classify(at: (i32, i32), frame: &Frame, regions: &Regions) -> Hit {
    if let Some(buttons) = frame.buttons.filter(|buttons| buttons.contains(at)) {
        return button_at(buttons, at.0);
    }
    if !frame.maximized && at.1 < frame.resize {
        return if at.0 < frame.resize {
            Hit::TopLeft
        } else if at.0 >= frame.width - frame.resize {
            Hit::TopRight
        } else {
            Hit::Top
        };
    }
    if !regions.band.contains(at) {
        return Hit::Client;
    }
    if regions.controls.iter().any(|control| control.contains(at)) {
        return Hit::Client;
    }
    Hit::Caption
}

/// Which of the three buttons `x` is over: the band the system reports is
/// all three side by side, the same width each.
fn button_at(buttons: Px, x: i32) -> Hit {
    let width = buttons.width().max(1);
    match ((x - buttons.left) * 3 / width).clamp(0, 2) {
        0 => Hit::Minimize,
        1 => Hit::Maximize,
        _ => Hit::Close,
    }
}

/// Where the caption buttons are when the system will not say: three
/// buttons of the size the system names, in the band's top right corner.
pub fn fallback_buttons(width: i32, button: (i32, i32)) -> Px {
    Px {
        left: width - button.0 * 3,
        top: 0,
        right: width,
        bottom: button.1,
    }
}

/// The band the layout is given, from where the caption buttons are and how
/// wide the client area is, at `scale` physical pixels to a point: at least
/// as tall as the buttons reach, and everything from their left edge to the
/// window's right kept clear.
pub fn band_of(buttons: Px, width: i32, scale: f64) -> TitleBand {
    let points = |pixels: i32| (f64::from(pixels.max(0)) / scale) as f32;
    TitleBand {
        height: points(buttons.bottom),
        left_inset: 0.0,
        right_inset: points(width - buttons.left),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Windows 11 at 100 %: a 1400 px wide client area, the three buttons
    /// 46 × 32 each in its top right corner, an 8 px resize edge.
    fn frame() -> Frame {
        Frame {
            width: 1400,
            resize: 8,
            maximized: false,
            buttons: Some(Px {
                left: 1400 - 138,
                top: 0,
                right: 1400,
                bottom: 32,
            }),
        }
    }

    /// One tab: the band is the top row's block, 46 px, with ☰, two crumbs
    /// and the counter on it.
    fn regions() -> Regions {
        Regions {
            band: Px {
                left: 0,
                top: 0,
                right: 1400,
                bottom: 46,
            },
            controls: vec![
                // ☰
                Px {
                    left: 11,
                    top: 11,
                    right: 43,
                    bottom: 43,
                },
                // Two crumbs
                Px {
                    left: 49,
                    top: 11,
                    right: 120,
                    bottom: 43,
                },
                Px {
                    left: 133,
                    top: 11,
                    right: 210,
                    bottom: 43,
                },
                // The counter
                Px {
                    left: 1180,
                    top: 8,
                    right: 1238,
                    bottom: 46,
                },
            ],
        }
    }

    #[test]
    fn the_windows_controls_are_its_own_and_the_rest_of_the_band_is_the_title_bar() {
        let (frame, regions) = (frame(), regions());
        let at = |x, y| classify((x, y), &frame, &regions);
        assert_eq!(at(20, 20), Hit::Client, "☰");
        assert_eq!(at(60, 30), Hit::Client, "a crumb");
        assert_eq!(at(1200, 20), Hit::Client, "the counter");
        // Between the crumbs, between the crumbs and the counter, in the
        // gap above the row and beside it, and under the buttons.
        assert_eq!(at(125, 30), Hit::Caption);
        assert_eq!(at(600, 30), Hit::Caption);
        assert_eq!(at(600, 9), Hit::Caption);
        assert_eq!(at(4, 30), Hit::Caption);
        assert_eq!(at(1300, 40), Hit::Caption);
        // Below the band everything is the window's, controls or not.
        assert_eq!(at(600, 46), Hit::Client);
        assert_eq!(at(1300, 400), Hit::Client);
    }

    /// Each third of the buttons' band is its own button, and the buttons
    /// win over the resize edge that runs under them: they are what is
    /// drawn there.
    #[test]
    fn the_caption_buttons_answer_where_they_are_drawn() {
        let (frame, regions) = (frame(), regions());
        let at = |x, y| classify((x, y), &frame, &regions);
        assert_eq!(at(1262, 16), Hit::Minimize);
        assert_eq!(at(1307, 16), Hit::Minimize);
        assert_eq!(at(1308, 16), Hit::Maximize);
        assert_eq!(at(1353, 16), Hit::Maximize);
        assert_eq!(at(1354, 16), Hit::Close);
        assert_eq!(at(1399, 0), Hit::Close, "the corner, over the edge");
        assert_eq!(at(1261, 16), Hit::Caption, "just left of them");
        assert_eq!(at(1330, 32), Hit::Caption, "just under them");
        // No buttons reported: the band is the title bar there too.
        let bare = Frame {
            buttons: None,
            ..frame
        };
        assert_eq!(classify((1330, 16), &bare, &regions), Hit::Caption);
    }

    #[test]
    fn the_top_edge_resizes_unless_the_window_is_maximized() {
        let (frame, regions) = (frame(), regions());
        let at = |x, y| classify((x, y), &frame, &regions);
        assert_eq!(at(600, 0), Hit::Top);
        assert_eq!(at(600, 7), Hit::Top);
        assert_eq!(at(600, 8), Hit::Caption);
        assert_eq!(at(3, 3), Hit::TopLeft);
        assert_eq!(at(20, 3), Hit::Top, "over ☰'s column, but above it");
        let bare = Frame {
            buttons: None,
            ..frame
        };
        assert_eq!(classify((1396, 3), &bare, &regions), Hit::TopRight);
        let maximized = Frame {
            maximized: true,
            ..frame
        };
        assert_eq!(classify((600, 0), &maximized, &regions), Hit::Caption);
        assert_eq!(classify((3, 3), &maximized, &regions), Hit::Caption);
        assert_eq!(classify((1330, 3), &maximized, &regions), Hit::Maximize);
    }

    /// Before the window has laid anything out there is no band, and every
    /// point but the buttons and the edge is the window's.
    #[test]
    fn with_no_band_reported_the_window_is_all_client() {
        let frame = frame();
        let at = |x, y| classify((x, y), &frame, &Regions::NONE);
        assert_eq!(at(600, 30), Hit::Client);
        assert_eq!(at(600, 3), Hit::Top);
        assert_eq!(at(1330, 16), Hit::Maximize);
    }

    /// Two tabs: the band is the strip's block, and the top row below it is
    /// the window's even where it is empty.
    #[test]
    fn with_a_strip_the_row_below_it_is_all_client() {
        let frame = frame();
        let regions = Regions {
            band: Px {
                left: 0,
                top: 0,
                right: 1400,
                bottom: 38,
            },
            controls: vec![
                Px {
                    left: 8,
                    top: 8,
                    right: 120,
                    bottom: 38,
                },
                Px {
                    left: 128,
                    top: 8,
                    right: 250,
                    bottom: 38,
                },
                // The `+`
                Px {
                    left: 258,
                    top: 8,
                    right: 288,
                    bottom: 38,
                },
            ],
        };
        let at = |x, y| classify((x, y), &frame, &regions);
        assert_eq!(at(60, 20), Hit::Client, "a chip");
        assert_eq!(at(270, 20), Hit::Client, "the +");
        assert_eq!(at(124, 20), Hit::Caption, "between two chips");
        assert_eq!(at(700, 20), Hit::Caption, "after the +");
        assert_eq!(at(700, 50), Hit::Client, "the top row's empty middle");
    }

    #[test]
    fn a_rect_in_points_owns_every_pixel_it_touches() {
        let rect = egui::Rect::from_min_max(egui::pos2(10.3, 8.0), egui::pos2(40.2, 46.0));
        assert_eq!(
            Px::around(rect, 1.0),
            Px {
                left: 10,
                top: 8,
                right: 41,
                bottom: 46,
            }
        );
        assert_eq!(
            Px::around(rect, 1.5),
            Px {
                left: 15,
                top: 12,
                right: 61,
                bottom: 69,
            }
        );
    }

    /// The layout keeps clear everything from the buttons' left edge to the
    /// window's right, in points, and the band reaches at least as far down
    /// as they do.
    #[test]
    fn the_band_keeps_the_buttons_clear() {
        let band = band_of(frame().buttons.expect("buttons"), 1400, 1.0);
        assert_eq!(band.right_inset, 138.0);
        assert_eq!(band.left_inset, 0.0);
        assert_eq!(band.height, 32.0);
        // At 150 % the same buttons are 1.5 times the pixels and the same
        // points.
        let scaled = Px {
            left: 2100 - 207,
            top: 0,
            right: 2100,
            bottom: 48,
        };
        let band = band_of(scaled, 2100, 1.5);
        assert_eq!(band.right_inset, 138.0);
        assert_eq!(band.height, 32.0);
    }

    #[test]
    fn with_no_word_from_the_system_the_buttons_are_three_of_its_size_in_the_corner() {
        let buttons = fallback_buttons(1400, (46, 32));
        assert_eq!(
            buttons,
            Px {
                left: 1262,
                top: 0,
                right: 1400,
                bottom: 32,
            }
        );
        assert_eq!(band_of(buttons, 1400, 1.0).right_inset, 138.0);
    }

    /// The system reports the buttons from the window's corner, which is
    /// outside the client area by the invisible resize border at the side,
    /// and above it by the frame when maximized.
    #[test]
    fn window_relative_bounds_move_into_the_client_area() {
        let window = Px {
            left: 1269,
            top: 1,
            right: 1407,
            bottom: 33,
        };
        assert_eq!(
            window.moved((-7, 0)),
            Px {
                left: 1262,
                top: 1,
                right: 1400,
                bottom: 33,
            }
        );
    }
}
