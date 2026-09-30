//! No menu bar of the system's to put the app menu in: Linux's and
//! Windows'. The app menu is the ☰ button's on the top row, as it always
//! has been, and nothing is chosen anywhere else. macOS's body, the app menu
//! in the system's menu bar, is `platform/macos/menubar.rs`
//! (`plans/other-platforms/02-macos.md` M2.37).

use df_core::keymap::Registry;

use crate::app::Waker;
use crate::menu::{Action, Item};

/// Whether the top row carries the ☰ button: it does, the app menu having
/// nowhere else to be.
pub const MENU_BUTTON: bool = true;

/// Nothing to start.
pub fn start(_waker: Waker) {}

/// Never called: the window publishes the app menu only where the top row
/// has no ☰ button.
pub fn publish(_items: &[Item], _keymap: &Registry) {}

/// Nothing is chosen outside the window.
pub fn take() -> Vec<Action> {
    Vec::new()
}
