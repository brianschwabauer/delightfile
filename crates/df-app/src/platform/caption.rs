//! Which part of the window's top is the title bar and which is the
//! program's own, when the program draws into the title bar
//! (`plans/other-platforms/04-windows.md` W4.39).
//!
//! On Windows the top row — or, with two tabs or more, the tab strip — is
//! drawn where the system's title bar was, and the system keeps the rest of
//! the title bar's work: dragging the window, a double click that maximizes,
//! the system menu on a right click, the resize edge along the top. The
//! three caption buttons are the window's own, drawn by it at the band's
//! right end, and the system is told they are caption buttons so that Snap
//! Layouts still comes up under Maximize. The system learns which is which
//! by asking the window, point by point (`WM_NCHITTEST`); this module is the
//! answer, and the buttons' reading of the pointer the system then reports,
//! without a line of Win32 in it, so both are tested on every target.
//!
//! The answer comes from two things. The *frame*: how wide the window is,
//! how thick its resize edge, whether it is maximized (a maximized window
//! has no edge to take hold of). The *regions*: the strip across the top
//! that is the title bar's, the three buttons at its right end, and the
//! rects in it that are the window's own controls — ☰, a crumb, the counter,
//! a tab chip, its `×`, the `+` — all as the window last laid them out. A
//! point in the band on none of them is the title bar's; everything below
//! the band is the window's.
//!
//! Everything here is physical pixels in the window's client coordinates,
//! which is what the system hands over. The layout is in logical points, so
//! a rect is taken out to the whole pixels it touches ([`Px::around`]): a
//! control whose edge falls inside a pixel owns that pixel.

use crate::ui::{CaptionButton, CaptionPointer};

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
    /// One of the three caption buttons.
    Button(CaptionButton),
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
}

/// What the window last said about its band: where it is, where its three
/// caption buttons are, and which rects in it are the window's own
/// controls.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Regions {
    pub band: Px,
    /// Minimize, maximize, close, left to right.
    pub buttons: Option<[Px; 3]>,
    pub controls: Vec<Px>,
}

impl Regions {
    /// No band: every point is the window's until the first layout says
    /// otherwise.
    pub const NONE: Regions = Regions {
        band: Px::EMPTY,
        buttons: None,
        controls: Vec::new(),
    };
}

/// What `at` is. In this order: a caption button, which answers wherever it
/// is drawn; the resize edge along the top, unless the window is maximized;
/// below the band, the window's; on one of the window's controls, the
/// window's; and anywhere else in the band, the title bar.
pub fn classify(at: (i32, i32), frame: &Frame, regions: &Regions) -> Hit {
    if let Some(buttons) = regions.buttons {
        if let Some(index) = buttons.iter().position(|button| button.contains(at)) {
            return Hit::Button(CaptionButton::ALL[index]);
        }
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

/// A mouse message the system sends while the pointer is the title bar's,
/// as far as the caption buttons care: which button, if any, the system
/// says it is over (the hit test's own answer, handed back).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mouse {
    /// `WM_NCMOUSEMOVE`.
    Move(Option<CaptionButton>),
    /// `WM_NCMOUSELEAVE`: out of the title bar, into the window or away.
    Leave,
    /// `WM_NCLBUTTONDOWN`, and its double click.
    Down(Option<CaptionButton>),
    /// `WM_NCLBUTTONUP`.
    Up(Option<CaptionButton>),
}

/// What a [`Mouse`] message comes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Heard {
    /// The buttons look different now: the window is to be drawn again.
    pub changed: bool,
    /// Letting go on the button that was pressed: its command.
    pub click: Option<CaptionButton>,
    /// The message was the buttons' and goes no further. A press or a
    /// release on a button is: the system's own handling of one would track
    /// buttons of its own metrics, which are not the ones drawn. Anything
    /// else goes on to the system, which moves, maximizes and opens the
    /// system menu for the title bar, and brings Snap Layouts up over
    /// Maximize.
    pub ours: bool,
}

/// Follow `mouse` on the buttons: hovered while over, pressed on a press,
/// clicked on a release over the one pressed. Leaving the title bar lets go
/// of the press as well, since the release will land in the window, where
/// these messages do not go.
pub fn track(pointer: &mut CaptionPointer, mouse: Mouse) -> Heard {
    let before = *pointer;
    let mut heard = Heard::default();
    match mouse {
        Mouse::Move(over) => pointer.hover = over,
        Mouse::Leave => *pointer = CaptionPointer::default(),
        Mouse::Down(on) => {
            pointer.hover = on;
            pointer.pressed = on;
            heard.ours = on.is_some();
        }
        Mouse::Up(on) => {
            heard.click = on.filter(|button| before.pressed == Some(*button));
            pointer.hover = on;
            pointer.pressed = None;
            heard.ours = on.is_some();
        }
    }
    heard.changed = *pointer != before;
    heard
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 1400 px wide client area at 100 %, with an 8 px resize edge.
    fn frame() -> Frame {
        Frame {
            width: 1400,
            resize: 8,
            maximized: false,
        }
    }

    /// One tab: the band is the top row's block, 46 px, with ☰, two crumbs
    /// and the counter on it, and the three buttons, 46 px each, at its
    /// right end.
    fn regions() -> Regions {
        let px = |left, top, right, bottom| Px {
            left,
            top,
            right,
            bottom,
        };
        Regions {
            band: px(0, 0, 1400, 46),
            buttons: Some([
                px(1262, 0, 1308, 46),
                px(1308, 0, 1354, 46),
                px(1354, 0, 1400, 46),
            ]),
            controls: vec![
                // ☰
                px(11, 11, 43, 43),
                // Two crumbs
                px(49, 11, 120, 43),
                px(133, 11, 210, 43),
                // The counter
                px(1180, 8, 1238, 46),
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
        // gap above the row, beside it, and between the row and the buttons.
        assert_eq!(at(125, 30), Hit::Caption);
        assert_eq!(at(600, 30), Hit::Caption);
        assert_eq!(at(600, 9), Hit::Caption);
        assert_eq!(at(4, 30), Hit::Caption);
        assert_eq!(at(1250, 30), Hit::Caption);
        // Below the band everything is the window's, controls or not.
        assert_eq!(at(600, 46), Hit::Client);
        assert_eq!(at(1300, 400), Hit::Client);
    }

    /// Each button answers over the whole of its rect, the band's full
    /// depth, and the buttons win over the resize edge that runs under
    /// them: they are what is drawn there.
    #[test]
    fn the_caption_buttons_answer_where_they_are_drawn() {
        let (frame, regions) = (frame(), regions());
        let at = |x, y| classify((x, y), &frame, &regions);
        assert_eq!(at(1262, 16), Hit::Button(CaptionButton::Minimize));
        assert_eq!(at(1307, 45), Hit::Button(CaptionButton::Minimize));
        assert_eq!(at(1308, 16), Hit::Button(CaptionButton::Maximize));
        assert_eq!(at(1353, 16), Hit::Button(CaptionButton::Maximize));
        assert_eq!(at(1354, 16), Hit::Button(CaptionButton::Close));
        assert_eq!(
            at(1399, 0),
            Hit::Button(CaptionButton::Close),
            "the corner, over the edge"
        );
        assert_eq!(at(1261, 16), Hit::Caption, "just left of them");
        assert_eq!(at(1330, 46), Hit::Client, "just under them");
        // No buttons: the band is the title bar there too.
        let bare = Regions {
            buttons: None,
            ..regions
        };
        assert_eq!(classify((1330, 16), &frame, &bare), Hit::Caption);
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
        let bare = Regions {
            buttons: None,
            ..regions.clone()
        };
        assert_eq!(classify((1396, 3), &frame, &bare), Hit::TopRight);
        let maximized = Frame {
            maximized: true,
            ..frame
        };
        assert_eq!(classify((600, 0), &maximized, &regions), Hit::Caption);
        assert_eq!(classify((3, 3), &maximized, &regions), Hit::Caption);
        assert_eq!(
            classify((1330, 3), &maximized, &regions),
            Hit::Button(CaptionButton::Maximize)
        );
    }

    /// Before the window has laid anything out there is no band and there
    /// are no buttons, and every point but the edge is the window's.
    #[test]
    fn with_no_band_reported_the_window_is_all_client() {
        let frame = frame();
        let at = |x, y| classify((x, y), &frame, &Regions::NONE);
        assert_eq!(at(600, 30), Hit::Client);
        assert_eq!(at(1330, 16), Hit::Client);
        assert_eq!(at(600, 3), Hit::Top);
    }

    /// Two tabs: the band is the strip's block, and the top row below it is
    /// the window's even where it is empty.
    #[test]
    fn with_a_strip_the_row_below_it_is_all_client() {
        let frame = frame();
        let px = |left, top, right, bottom| Px {
            left,
            top,
            right,
            bottom,
        };
        let regions = Regions {
            band: px(0, 0, 1400, 38),
            buttons: Some([
                px(1262, 0, 1308, 38),
                px(1308, 0, 1354, 38),
                px(1354, 0, 1400, 38),
            ]),
            controls: vec![
                px(8, 8, 120, 38),
                px(128, 8, 250, 38),
                // The `+`
                px(258, 8, 288, 38),
            ],
        };
        let at = |x, y| classify((x, y), &frame, &regions);
        assert_eq!(at(60, 20), Hit::Client, "a chip");
        assert_eq!(at(270, 20), Hit::Client, "the +");
        assert_eq!(at(124, 20), Hit::Caption, "between two chips");
        assert_eq!(at(700, 20), Hit::Caption, "after the +");
        assert_eq!(at(700, 50), Hit::Client, "the top row's empty middle");
        assert_eq!(at(1330, 50), Hit::Client, "under the buttons");
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

    const MIN: Option<CaptionButton> = Some(CaptionButton::Minimize);
    const MAX: Option<CaptionButton> = Some(CaptionButton::Maximize);
    const CLOSE: Option<CaptionButton> = Some(CaptionButton::Close);

    /// Over a button it is lit; off it, not; a move that changes nothing
    /// asks for no frame. Moves are never the buttons' alone: the system
    /// brings Snap Layouts up from them.
    #[test]
    fn the_pointer_lights_the_button_it_is_over() {
        let mut pointer = CaptionPointer::default();
        let heard = track(&mut pointer, Mouse::Move(MAX));
        assert_eq!(pointer.hover, MAX);
        assert!(heard.changed && !heard.ours && heard.click.is_none());
        assert!(!track(&mut pointer, Mouse::Move(MAX)).changed);
        assert!(track(&mut pointer, Mouse::Move(CLOSE)).changed);
        track(&mut pointer, Mouse::Move(None));
        assert_eq!(pointer, CaptionPointer::default());
        track(&mut pointer, Mouse::Move(MIN));
        let heard = track(&mut pointer, Mouse::Leave);
        assert!(heard.changed && !heard.ours);
        assert_eq!(pointer, CaptionPointer::default());
    }

    /// A press and a release on one button is its click, and both are the
    /// buttons' own; a release on another button, or after the pointer
    /// left the title bar, is no click.
    #[test]
    fn a_click_is_a_press_and_a_release_on_the_same_button() {
        let mut pointer = CaptionPointer::default();
        let down = track(&mut pointer, Mouse::Down(CLOSE));
        assert!(down.ours && down.changed && down.click.is_none());
        assert_eq!(pointer.pressed, CLOSE);
        let up = track(&mut pointer, Mouse::Up(CLOSE));
        assert_eq!(up.click, CLOSE);
        assert!(up.ours);
        assert_eq!(pointer.pressed, None);

        track(&mut pointer, Mouse::Down(CLOSE));
        track(&mut pointer, Mouse::Move(MAX));
        assert_eq!(pointer.pressed, CLOSE, "held while it crosses");
        assert_eq!(track(&mut pointer, Mouse::Up(MAX)).click, None);

        track(&mut pointer, Mouse::Down(MIN));
        track(&mut pointer, Mouse::Leave);
        assert_eq!(pointer.pressed, None);
        assert_eq!(track(&mut pointer, Mouse::Up(MIN)).click, None);
    }

    /// A press or a release on the title bar, not a button, is the
    /// system's: it moves and maximizes the window.
    #[test]
    fn the_title_bars_own_presses_go_on_to_the_system() {
        let mut pointer = CaptionPointer::default();
        assert!(!track(&mut pointer, Mouse::Down(None)).ours);
        assert!(!track(&mut pointer, Mouse::Up(None)).ours);
        // …and a release there lets go of a button pressed and left.
        track(&mut pointer, Mouse::Down(MAX));
        track(&mut pointer, Mouse::Up(None));
        assert_eq!(pointer.pressed, None);
    }
}
