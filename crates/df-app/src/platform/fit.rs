//! The opening size, cut down to a screen too small for it.
//!
//! The window asks for [`crate::app::WINDOW_SIZE`], and on a screen with room
//! for it that is what it gets, placed where the system puts a new window.
//! On a smaller one — the Windows VM this port was first run on is 1014×771 —
//! a window of that size hangs off the bottom of the screen, its lower edge
//! and its resize handle under the taskbar, and nothing about it says so. So
//! where the system can say which part of the screen a window may cover (its
//! *work area*: the screen less the taskbar, the menu bar, the Dock), the
//! opening size is cut to fit inside it, frame and all, and the window is
//! centred there.
//!
//! winit has no work area on any platform, so the platform asks the system
//! (`SystemParametersInfoW(SPI_GETWORKAREA)` on Windows, `NSScreen`'s
//! `visibleFrame` on macOS). Wayland has no such question — a client is not
//! told the size of any screen — so Linux opens as it always has, and the
//! compositor's own placement is what keeps a window on screen there.

/// A window that did not fit, fitted: its inner size and the top-left corner
/// of its frame, in whatever units the work area was measured in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fitted {
    pub size: (f64, f64),
    pub at: (f64, f64),
}

/// A rectangle on the screen: left, top, width, height.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Area {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

/// What the system draws around a window's inner size, on each side.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Frame {
    pub left: f64,
    pub top: f64,
    pub right: f64,
    pub bottom: f64,
}

/// `wanted`, fitted into `work` with `frame` around it; `None` when it fits
/// as it is, so a screen with room for the window places it where it always
/// did. Each side is cut on its own — a wide, short screen keeps the width —
/// and never below one unit.
pub fn fit(wanted: (f64, f64), work: Area, frame: Frame) -> Option<Fitted> {
    let room = (
        work.width - frame.left - frame.right,
        work.height - frame.top - frame.bottom,
    );
    if wanted.0 <= room.0 && wanted.1 <= room.1 {
        return None;
    }
    let size = (wanted.0.min(room.0).max(1.0), wanted.1.min(room.1).max(1.0));
    let outer = (
        size.0 + frame.left + frame.right,
        size.1 + frame.top + frame.bottom,
    );
    let at = (
        work.x + ((work.width - outer.0) / 2.0).max(0.0),
        work.y + ((work.height - outer.1) / 2.0).max(0.0),
    );
    Some(Fitted { size, at })
}

#[cfg(test)]
mod tests {
    use super::*;

    const WANTED: (f64, f64) = (1400.0, 900.0);

    /// Windows 11's frame at 100 %: 7 px of invisible resize border at the
    /// sides and bottom, and the caption.
    const FRAME: Frame = Frame {
        left: 7.0,
        top: 31.0,
        right: 7.0,
        bottom: 7.0,
    };

    #[test]
    fn a_screen_with_room_leaves_the_window_alone() {
        let work = Area {
            x: 0.0,
            y: 0.0,
            width: 1920.0,
            height: 1032.0,
        };
        assert_eq!(fit(WANTED, work, FRAME), None);
    }

    /// The VM the port was first run on: 1014×771 with a 48 px taskbar.
    #[test]
    fn a_small_screen_gets_a_window_it_can_see_all_of() {
        let work = Area {
            x: 0.0,
            y: 0.0,
            width: 1014.0,
            height: 723.0,
        };
        let fitted = fit(WANTED, work, FRAME).expect("too big for 1014×723");
        assert_eq!(fitted.size, (1000.0, 685.0));
        assert_eq!(fitted.at, (0.0, 0.0), "frame and all, inside the work area");
    }

    /// Only the side that does not fit is cut, and the window is centred
    /// in a work area that does not start at the corner (a taskbar on the
    /// left or at the top).
    #[test]
    fn each_side_is_cut_on_its_own_and_the_window_is_centred() {
        let work = Area {
            x: 60.0,
            y: 0.0,
            width: 1860.0,
            height: 800.0,
        };
        let fitted = fit(WANTED, work, FRAME).expect("too short");
        assert_eq!(fitted.size, (1400.0, 762.0), "the width fits and is kept");
        assert_eq!(fitted.at, (60.0 + (1860.0 - 1414.0) / 2.0, 0.0));
    }

    #[test]
    fn a_work_area_smaller_than_the_frame_still_opens_a_window() {
        let work = Area {
            x: 0.0,
            y: 0.0,
            width: 10.0,
            height: 10.0,
        };
        let fitted = fit(WANTED, work, FRAME).expect("far too small");
        assert_eq!(fitted.size, (1.0, 1.0));
    }
}
