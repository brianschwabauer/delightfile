//! The right-click context menu (PLAN §7.5): "mirroring opener rules +
//! operations, with shortcuts rendered inline".
//!
//! It is the same floating card the which-key hint and the opener picker are
//! ([`crate::chrome::card`]) at a third size, for the reason that file's header
//! gives: one card style is what makes six surfaces read as one program.
//!
//! ## What it is *not*
//!
//! It is not a second command system. Every row here is a key that already
//! exists, and the key is drawn on the row — right-aligned and dim, the way a
//! menu has taught its own shortcuts since 1984. A menu item with no keyboard
//! equivalent would be a feature only the mouse could reach, which is the
//! opposite of what this program is.
//!
//! ## Motion
//!
//! Instant in, faded out (`delightful-ui` §3, PLAN §8). A menu that grew or
//! slid on the way in would put an animation between the click and the answer;
//! on the way out there is nothing left to wait for, so it fades over
//! [`FADE`] and the app drops it when the fade is spent.

use std::time::{Duration, Instant};

use crate::chrome::{card, fade as fade_color, key_font, CARD_PAD, CARD_ROW_RADIUS, FONT, PAD_X};
use crate::hover::{pressed_rect, Hovers};
use crate::ripple::Ripples;
use crate::theme::mix;
use crate::ui::{Control, Painting};

/// One menu row's height, in logical points.
///
/// Two points taller than a file row: a menu is read once and aimed at, not
/// scanned in bulk, and 24 is also `delightful-ui` §1's hit-target floor — a
/// row that is exactly big enough to click without care.
pub const ROW: f32 = 24.0;

/// The space a separator opens between two groups, in logical points.
///
/// A gap, plus a hairline drawn in the middle of it. Ten points is enough that
/// the groups read as groups at a glance; a rule alone (with no space) reads as
/// a row with a line through it.
const SEPARATOR: f32 = 9.0;

/// The narrowest the card gets. Wide enough that "Copy" plus its `y` do not sit
/// on top of each other, so a menu of short verbs still looks like a menu.
const MIN_WIDTH: f32 = 168.0;

/// The gap between a label and its key column. Generous: they are two different
/// kinds of information and the eye has to be able to ignore one of them.
const KEY_GAP: f32 = 28.0;

/// How far the card stays off the window's edge, in logical points. The same
/// margin every other floating surface keeps.
const MARGIN: f32 = 6.0;

/// How long the menu takes to fade once it is dismissed.
///
/// 120 ms — PLAN §8's state-fade duration. The menu is *gone* the instant it is
/// dismissed (the click that dismissed it has already been acted on); this is
/// only the pixels catching up, so it must be shorter than the eye's patience
/// and it must never be waited on.
pub const FADE: Duration = Duration::from_millis(120);

/// How far a submenu overlaps its parent row's card, in logical points.
///
/// A small overlap rather than a gap: the pointer travels diagonally from the
/// parent row to the submenu, and a gap between the two cards is a corridor the
/// pointer falls out of — the classic menu bug.
const SUBMENU_OVERLAP: f32 = 4.0;

/// What a menu row does. The identity only — the doing is [`crate::app`]'s,
/// exactly as with [`df_core::keymap::Command`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Open,
    /// The `n`th opener rule that matched the hovered file.
    OpenWith(usize),
    /// The parent row of the opener submenu; activating it flies the submenu
    /// out rather than doing anything itself.
    OpenWithMenu,
    Yank,
    Cut,
    Paste,
    Rename,
    Trash,
    /// Unpack the hovered archive into this directory (PLAN §7.3).
    ExtractHere,
    /// …or into a new folder named after it.
    ExtractSubfolder,
    CopyPath,
    CopyName,
    Properties,
    /// Put a trashed item back where it came from (PLAN §7.4).
    Restore,
    /// Destroy trashed items for good.
    Purge,
    /// …and destroy all of them.
    EmptyTrash,
}

/// One row.
#[derive(Debug, Clone)]
pub struct Item {
    pub label: String,
    /// The keyboard equivalent, drawn right-aligned and dim. Never empty — a
    /// row with no key does not belong in this menu (see the header).
    pub keys: &'static str,
    pub action: Action,
    /// Dimmed and inert. A disabled row is *shown*, not hidden: a menu whose
    /// rows move about depending on what is selected is a menu you cannot aim
    /// at from memory (`delightful-ui` §8).
    pub enabled: bool,
    /// Draw a separator above this row.
    pub gap_before: bool,
}

impl Item {
    fn new(label: &str, keys: &'static str, action: Action, enabled: bool) -> Item {
        Item {
            label: label.to_string(),
            keys,
            action,
            enabled,
            gap_before: false,
        }
    }

    fn after_gap(mut self) -> Item {
        self.gap_before = true;
        self
    }

    /// Does this row fly a submenu out?
    pub fn submenu(&self) -> bool {
        self.action == Action::OpenWithMenu
    }
}

/// What the menu needs to know about the world to decide its rows.
///
/// A plain struct of answers rather than a borrow of the app, so the enablement
/// rules are testable without a window — which matters, because "why is Paste
/// grey" is a question that has to have one answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Facts {
    /// Is there a row under the cursor at all? An empty directory has none.
    pub has_row: bool,
    /// What the cursor row is, when there is one.
    pub is_dir: bool,
    /// How many files an operation would act on: the selection, or the cursor
    /// row.
    pub targets: usize,
    /// Is there anything on the internal clipboard?
    ///
    /// **Only the internal one.** `p` also falls back to the *system*
    /// clipboard (PLAN §7.4), but finding out what that is holding means
    /// shelling out to `wl-paste`, and doing that on every right-click would
    /// put a process spawn in the path of opening a menu. So the row reports
    /// what this program is carrying, and the fallback is a thing `p` does.
    pub clipboard: bool,
    /// How many opener rules match the hovered file.
    pub openers: usize,
    /// Whether the cursor row is an archive this build can read (PLAN §7.3).
    ///
    /// The two extract rows are **hidden**, not disabled, when it is not — the
    /// menu's general rule is that a row stays put and greys out, so the shape
    /// is aimable from memory, and that rule is about rows that *sometimes*
    /// apply to the thing under the pointer. "Extract" never applies to a text
    /// file, and two permanently grey rows on every right-click in a source
    /// directory would be two rows of noise to read past.
    pub archive: bool,
    /// Whether the list pane is showing the trash (PLAN §7.4).
    ///
    /// A *different menu*, not the ordinary one with rows greyed out. The
    /// menu's usual rule — rows stay put so the shape is aimable from memory —
    /// is about one directory's rows differing from another's; the trash is a
    /// different place with three verbs of its own, and eight permanently grey
    /// rows above them would be eight rows to read past every time.
    pub trash: bool,
    /// How many items the trash holds, for the "Empty trash" row.
    pub trashed: usize,
}

/// The rows, in order, with their enablement.
pub fn items(facts: Facts) -> Vec<Item> {
    let acts = facts.targets > 0;
    if facts.trash {
        return vec![
            Item::new("Restore", "Enter", Action::Restore, acts),
            Item::new("Destroy permanently", "D", Action::Purge, acts),
            Item::new("Copy original path", "c c", Action::CopyPath, facts.has_row).after_gap(),
            Item::new("Properties", "Tab", Action::Properties, facts.has_row),
            // Last, after a gap, and the only row that acts on things the
            // pointer is not on: it is the one gesture in this menu that cannot
            // be taken back, so it is the hardest one to hit by accident.
            Item::new("Empty trash", "", Action::EmptyTrash, facts.trashed > 0).after_gap(),
        ];
    }
    let mut items = vec![
        Item::new(
            if facts.is_dir { "Open folder" } else { "Open" },
            "Enter",
            Action::Open,
            facts.has_row,
        ),
        Item::new(
            "Open with",
            "O",
            Action::OpenWithMenu,
            facts.has_row && facts.openers > 0,
        ),
        Item::new("Copy", "y", Action::Yank, acts).after_gap(),
        Item::new("Cut", "x", Action::Cut, acts),
        Item::new("Paste", "p", Action::Paste, facts.clipboard),
        Item::new("Rename", "r", Action::Rename, facts.has_row),
        Item::new("Move to trash", "d", Action::Trash, acts),
        Item::new("Copy path", "c c", Action::CopyPath, facts.has_row).after_gap(),
        Item::new("Copy name", "c f", Action::CopyName, facts.has_row),
        Item::new("Properties", "Tab", Action::Properties, facts.has_row).after_gap(),
    ];
    if facts.archive {
        // Directly under "Open with", where the eye already is when the
        // question is "what else can I do with this file".
        let at = items
            .iter()
            .position(|item| item.action == Action::OpenWithMenu)
            .map(|i| i + 1)
            .unwrap_or(1);
        items.insert(at, Item::new("Extract here", "e", Action::ExtractHere, true));
        items.insert(
            at + 1,
            Item::new("Extract to subfolder", "E", Action::ExtractSubfolder, true),
        );
    }
    // A menu that opened on empty pane space with everything grey would be a
    // menu about nothing; Paste is the one row that still makes sense there,
    // and `items` already says so.
    items.retain(|item| item.action != Action::OpenWithMenu || facts.openers > 0);
    items
}

/// The menu, while it is up.
pub struct Menu {
    /// Where the pointer was. The card is placed *from* this, never centred on
    /// it: a menu whose first row is under the pointer is a menu you can
    /// activate by twitching.
    pub anchor: egui::Pos2,
    pub items: Vec<Item>,
    /// The opener submenu's rows, by name.
    pub openers: Vec<String>,
    /// The keyboard's row, once `↑`/`↓` has been pressed. `None` until then —
    /// a menu opened by the pointer must not pre-select anything, or `Enter`
    /// would do something nobody aimed at.
    pub cursor: Option<usize>,
    /// Whether the opener submenu is out, and where its own cursor is.
    pub submenu: bool,
    pub sub_cursor: Option<usize>,
    /// Set when the menu is dismissed; it is drawn fading until [`FADE`] is up.
    pub closing: Option<Instant>,
}

impl Menu {
    pub fn new(anchor: egui::Pos2, items: Vec<Item>, openers: Vec<String>) -> Menu {
        Menu {
            anchor,
            items,
            openers,
            cursor: None,
            submenu: false,
            sub_cursor: None,
            closing: None,
        }
    }

    /// Is this menu still taking input? A closing one is pixels, not a surface.
    pub fn live(&self) -> bool {
        self.closing.is_none()
    }

    pub fn alpha(&self, now: Instant) -> f32 {
        match self.closing {
            None => 1.0,
            Some(at) => {
                let elapsed = now.saturating_duration_since(at).as_secs_f32();
                (1.0 - elapsed / FADE.as_secs_f32()).clamp(0.0, 1.0)
            }
        }
    }

    /// Has the fade finished, so the app can drop it?
    pub fn spent(&self, now: Instant) -> bool {
        self.closing
            .is_some_and(|at| now.saturating_duration_since(at) >= FADE)
    }

    /// `↑`/`↓`, in whichever list is live. Skips disabled rows: a keyboard that
    /// stopped on a grey row would be offering something it cannot do.
    pub fn move_cursor(&mut self, delta: isize) {
        if self.submenu {
            let len = self.openers.len();
            if len == 0 {
                return;
            }
            let next = match self.sub_cursor {
                Some(at) => wrap(at as isize + delta, len),
                None if delta < 0 => len - 1,
                None => 0,
            };
            self.sub_cursor = Some(next);
            return;
        }
        let len = self.items.len();
        if len == 0 {
            return;
        }
        let mut at = match self.cursor {
            Some(at) => at as isize,
            None if delta < 0 => len as isize,
            None => -1,
        };
        for _ in 0..len {
            at = wrap(at + delta, len) as isize;
            if self.items[at as usize].enabled {
                self.cursor = Some(at as usize);
                return;
            }
        }
    }

    /// What `Enter` would do, or `None` when nothing is picked.
    pub fn activate(&self) -> Option<Action> {
        if self.submenu {
            return self.sub_cursor.map(Action::OpenWith);
        }
        let item = self.items.get(self.cursor?)?;
        item.enabled.then_some(item.action)
    }

    /// `→`, or hovering the parent row: fly the submenu out.
    pub fn open_submenu(&mut self) -> bool {
        let Some(index) = self.items.iter().position(Item::submenu) else {
            return false;
        };
        if self.openers.is_empty() || !self.items[index].enabled {
            return false;
        }
        self.cursor = Some(index);
        self.submenu = true;
        true
    }

    /// `←`: back to the parent list, keeping the row the submenu belongs to.
    pub fn close_submenu(&mut self) -> bool {
        if !self.submenu {
            return false;
        }
        self.submenu = false;
        self.sub_cursor = None;
        true
    }
}

fn wrap(at: isize, len: usize) -> usize {
    let len = len as isize;
    (((at % len) + len) % len) as usize
}

// ── Geometry ────────────────────────────────────────────────────────────────

/// Where the card and its rows are.
pub struct Geometry {
    pub card: egui::Rect,
    /// One rect per item, in `items` order.
    pub rows: Vec<egui::Rect>,
    /// The submenu's card and rows, when it is out.
    pub sub: Option<(egui::Rect, Vec<egui::Rect>)>,
}

impl Geometry {
    /// What the pointer is over.
    pub fn hit(&self, pos: egui::Pos2) -> Option<Control> {
        if let Some((_, rows)) = &self.sub {
            if let Some(index) = rows.iter().position(|r| r.contains(pos)) {
                return Some(Control::SubmenuItem(index));
            }
        }
        self.rows
            .iter()
            .position(|r| r.contains(pos))
            .map(Control::MenuItem)
    }

    /// Is the pointer anywhere on the menu at all — including the card's
    /// padding and its separators? A press there must dismiss nothing.
    pub fn contains(&self, pos: egui::Pos2) -> bool {
        self.card.contains(pos) || self.sub.as_ref().is_some_and(|(c, _)| c.contains(pos))
    }

    pub fn rect_of(&self, control: Control) -> Option<egui::Rect> {
        match control {
            Control::MenuItem(index) => self.rows.get(index).copied(),
            Control::SubmenuItem(index) => self
                .sub
                .as_ref()
                .and_then(|(_, rows)| rows.get(index).copied()),
            _ => None,
        }
    }
}

/// A card of `size` placed at `anchor`, kept inside `area`.
///
/// The rule every menu on every platform uses, and the reason it is a named
/// function with a test: the card grows down and to the right of the pointer,
/// **flips** to the other side when there is not room, and only slides as a
/// last resort. Flipping is what keeps the pointer on a corner of the card
/// rather than in the middle of it, which is what makes the first row still be
/// one flick away near an edge.
pub fn place(area: egui::Rect, anchor: egui::Pos2, size: egui::Vec2) -> egui::Rect {
    let fits_right = anchor.x + size.x <= area.right() - MARGIN;
    let fits_below = anchor.y + size.y <= area.bottom() - MARGIN;
    let left = if fits_right {
        anchor.x
    } else {
        anchor.x - size.x
    };
    let top = if fits_below {
        anchor.y
    } else {
        anchor.y - size.y
    };
    // The slide: a window too small for the card either way. Clamped rather
    // than allowed off screen, because a menu with rows past the edge is a menu
    // with unreachable rows.
    let left = left.clamp(
        area.left() + MARGIN,
        (area.right() - MARGIN - size.x).max(area.left() + MARGIN),
    );
    let top = top.clamp(
        area.top() + MARGIN,
        (area.bottom() - MARGIN - size.y).max(area.top() + MARGIN),
    );
    egui::Rect::from_min_size(egui::pos2(left, top), size)
}

/// How tall a list of items is, separators included.
pub fn height(items: &[Item]) -> f32 {
    let gaps = items.iter().filter(|i| i.gap_before).count() as f32;
    items.len() as f32 * ROW + gaps * SEPARATOR + CARD_PAD * 2.0
}

/// Lay the menu out. Needs a painter because the width is measured from the
/// text — a menu sized by a guess is a menu with a ragged key column.
pub fn geometry(area: egui::Rect, menu: &Menu, painter: &egui::Painter) -> Geometry {
    let label_font = egui::FontId::proportional(FONT);
    let width = menu
        .items
        .iter()
        .map(|item| {
            let label = crate::chrome::text_width(painter, &item.label, label_font.clone());
            let keys = crate::chrome::text_width(painter, item.keys, key_font(FONT - 1.0));
            // The chevron's column is reserved on *every* row, so the key
            // column does not step sideways on the one row that has one.
            label + KEY_GAP + keys + CHEVRON_COLUMN
        })
        .fold(MIN_WIDTH, f32::max)
        + CARD_PAD * 2.0;
    let size = egui::vec2(
        width.min((area.width() - MARGIN * 2.0).max(MIN_WIDTH)),
        height(&menu.items),
    );
    let card = place(area, menu.anchor, size);

    let mut rows = Vec::with_capacity(menu.items.len());
    let mut y = card.top() + CARD_PAD;
    for item in &menu.items {
        if item.gap_before {
            y += SEPARATOR;
        }
        rows.push(egui::Rect::from_min_size(
            egui::pos2(card.left() + CARD_PAD, y),
            egui::vec2(card.width() - CARD_PAD * 2.0, ROW),
        ));
        y += ROW;
    }

    let sub = menu.submenu.then(|| {
        let parent = menu
            .items
            .iter()
            .position(Item::submenu)
            .and_then(|i| rows.get(i).copied())
            .unwrap_or(card);
        let sub_width = menu
            .openers
            .iter()
            .map(|name| crate::chrome::text_width(painter, name, label_font.clone()))
            .fold(MIN_WIDTH, f32::max)
            + CARD_PAD * 2.0;
        let sub_size = egui::vec2(
            sub_width.min((area.width() - MARGIN * 2.0).max(MIN_WIDTH)),
            menu.openers.len() as f32 * ROW + CARD_PAD * 2.0,
        );
        // Anchored at the parent row's outer corner, so `place` flips it to the
        // *left* of the card near the right edge of the window — which is where
        // every submenu on every platform goes.
        let anchor = egui::pos2(
            card.right() - SUBMENU_OVERLAP,
            parent.top() - CARD_PAD,
        );
        let sub_card = if anchor.x + sub_size.x <= area.right() - MARGIN {
            place(area, anchor, sub_size)
        } else {
            place(
                area,
                egui::pos2(card.left() + SUBMENU_OVERLAP, anchor.y),
                sub_size,
            )
        };
        let sub_rows = (0..menu.openers.len())
            .map(|i| {
                egui::Rect::from_min_size(
                    egui::pos2(
                        sub_card.left() + CARD_PAD,
                        sub_card.top() + CARD_PAD + i as f32 * ROW,
                    ),
                    egui::vec2(sub_card.width() - CARD_PAD * 2.0, ROW),
                )
            })
            .collect();
        (sub_card, sub_rows)
    });

    Geometry { card, rows, sub }
}

/// The room the `▸` gets on every row.
const CHEVRON_COLUMN: f32 = 12.0;

// ── Paint ───────────────────────────────────────────────────────────────────

/// Draw the menu and its submenu.
pub fn paint(
    paint: &Painting<'_>,
    menu: &Menu,
    geometry: &Geometry,
    hovers: &Hovers<Control>,
    ripples: &Ripples<Control>,
    now: Instant,
) {
    let alpha = menu.alpha(now);
    if alpha <= 0.0 {
        return;
    }
    card(paint, geometry.card, alpha);
    for (index, (item, rect)) in menu.items.iter().zip(&geometry.rows).enumerate() {
        if item.gap_before {
            // The hairline in the middle of the gap it opened. Inset to the
            // card's text column so it reads as separating rows rather than as
            // cutting the card in half.
            let y = rect.top() - SEPARATOR / 2.0;
            paint.painter.rect_filled(
                egui::Rect::from_min_max(
                    egui::pos2(rect.left() + PAD_X, y),
                    egui::pos2(rect.right() - PAD_X, y + 1.0),
                ),
                0,
                fade_color(paint.palette.surface1, alpha),
            );
        }
        let key = Control::MenuItem(index);
        let selected = menu.cursor == Some(index) && !menu.submenu;
        row(
            paint,
            *rect,
            &item.label,
            item.keys,
            item.enabled,
            selected,
            item.submenu(),
            key,
            hovers,
            ripples,
            alpha,
            now,
        );
    }

    let Some((sub_card, sub_rows)) = &geometry.sub else {
        return;
    };
    card(paint, *sub_card, alpha);
    for (index, (name, rect)) in menu.openers.iter().zip(sub_rows).enumerate() {
        row(
            paint,
            *rect,
            name,
            "",
            true,
            menu.sub_cursor == Some(index),
            false,
            Control::SubmenuItem(index),
            hovers,
            ripples,
            alpha,
            now,
        );
    }
}

/// One row of either list.
#[allow(clippy::too_many_arguments)]
fn row(
    paint: &Painting<'_>,
    rect: egui::Rect,
    label: &str,
    keys: &str,
    enabled: bool,
    selected: bool,
    chevron: bool,
    key: Control,
    hovers: &Hovers<Control>,
    ripples: &Ripples<Control>,
    alpha: f32,
    now: Instant,
) {
    let palette = paint.palette;
    // A disabled row takes no hover and no press: the pointer must not be able
    // to make something inert look live.
    let hover = if enabled { hovers.hover(key) } else { 0.0 };
    let press = if enabled { hovers.press(key) } else { 0.0 };
    let rect = pressed_rect(rect, press);
    let lit = hover.max(f32::from(selected));
    if lit > 0.0 {
        paint.painter.rect_filled(
            rect,
            CARD_ROW_RADIUS,
            fade_color(mix(palette.crust, palette.surface1, lit), alpha),
        );
    }
    let inside = paint.painter.with_clip_rect(rect);
    for splash in ripples.splashes(key, now) {
        inside.circle_filled(
            splash.center,
            splash.radius,
            egui::Color32::from_white_alpha((splash.alpha * alpha * 255.0).round() as u8),
        );
    }

    let text_color = if enabled {
        fade_color(palette.text, alpha)
    } else {
        // Dim, not hidden — see [`Item::enabled`].
        fade_color(palette.overlay0, alpha)
    };
    let key_color = fade_color(palette.overlay0, alpha);
    let keys_width = if keys.is_empty() {
        0.0
    } else {
        let galley =
            inside.layout_no_wrap(keys.to_string(), key_font(FONT - 1.0), key_color);
        let width = galley.size().x;
        inside.galley(
            egui::pos2(
                rect.right() - PAD_X - width,
                rect.center().y - galley.size().y / 2.0,
            ),
            galley,
            key_color,
        );
        width + PAD_X
    };
    if chevron {
        // The submenu's promise, in the place every menu puts it.
        inside.text(
            egui::pos2(rect.right() - PAD_X, rect.center().y),
            egui::Align2::RIGHT_CENTER,
            "▸",
            egui::FontId::proportional(FONT),
            text_color,
        );
    }
    crate::chrome::truncated(
        &inside,
        egui::pos2(rect.left() + PAD_X, rect.center().y),
        label,
        text_color,
        (rect.width() - PAD_X * 2.0 - keys_width - if chevron { CHEVRON_COLUMN } else { 0.0 })
            .max(0.0),
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> Facts {
        Facts {
            has_row: true,
            is_dir: false,
            targets: 1,
            clipboard: true,
            openers: 2,
            archive: false,
            trash: false,
            trashed: 0,
        }
    }

    /// Every row carries a key, because every row *is* one.
    #[test]
    fn every_menu_row_teaches_its_own_shortcut() {
        for item in items(facts()) {
            assert!(!item.keys.is_empty(), "{} has no key", item.label);
        }
    }

    /// Enablement, one clause at a time.
    #[test]
    fn the_rows_are_enabled_by_what_is_actually_there() {
        let enabled = |facts: Facts, action: Action| {
            items(facts)
                .into_iter()
                .find(|i| i.action == action)
                .map(|i| i.enabled)
        };
        // An empty directory: nothing to open, nothing to act on — but a
        // clipboard is still pastable, which is the whole reason the menu
        // opens on empty pane space at all.
        let empty = Facts {
            has_row: false,
            is_dir: false,
            targets: 0,
            clipboard: true,
            openers: 0,
            archive: false,
            trash: false,
            trashed: 0,
        };
        assert_eq!(enabled(empty, Action::Open), Some(false));
        assert_eq!(enabled(empty, Action::Yank), Some(false));
        assert_eq!(enabled(empty, Action::Trash), Some(false));
        assert_eq!(enabled(empty, Action::Paste), Some(true));
        assert_eq!(enabled(empty, Action::Properties), Some(false));
        // An empty clipboard greys exactly one row.
        let nothing_yanked = Facts {
            clipboard: false,
            ..facts()
        };
        assert_eq!(enabled(nothing_yanked, Action::Paste), Some(false));
        assert_eq!(enabled(nothing_yanked, Action::Yank), Some(true));
        // A file no opener rule matches loses the submenu row entirely: a
        // "Open with ▸" that flew out an empty card would be a dead end.
        let unopenable = Facts {
            openers: 0,
            ..facts()
        };
        assert_eq!(enabled(unopenable, Action::OpenWithMenu), None);
        assert_eq!(enabled(facts(), Action::OpenWithMenu), Some(true));
    }

    /// A folder's first row says so — the same distinction `Enter` makes.
    #[test]
    fn a_directory_row_is_labelled_as_one() {
        let dir = Facts {
            is_dir: true,
            ..facts()
        };
        assert_eq!(items(dir)[0].label, "Open folder");
        assert_eq!(items(facts())[0].label, "Open");
    }

    fn area() -> egui::Rect {
        egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(1400.0, 900.0))
    }

    /// The anchor flip: the card grows away from whichever edge it is near, and
    /// never leaves the window.
    #[test]
    fn the_card_flips_rather_than_hanging_off_the_edge() {
        let size = egui::vec2(200.0, 300.0);
        // Room both ways: down and to the right of the pointer.
        let a = place(area(), egui::pos2(100.0, 100.0), size);
        assert_eq!(a.min, egui::pos2(100.0, 100.0));
        // Near the right edge: flipped left, so the pointer is on its right
        // corner rather than past its edge.
        let b = place(area(), egui::pos2(1380.0, 100.0), size);
        assert!((b.right() - 1380.0).abs() < 1e-3, "{b:?}");
        // Near the bottom: flipped up.
        let c = place(area(), egui::pos2(100.0, 880.0), size);
        assert!((c.bottom() - 880.0).abs() < 1e-3, "{c:?}");
        // Both at once.
        let d = place(area(), egui::pos2(1380.0, 880.0), size);
        assert!((d.right() - 1380.0).abs() < 1e-3);
        assert!((d.bottom() - 880.0).abs() < 1e-3);
        for rect in [a, b, c, d] {
            assert!(rect.left() >= area().left() && rect.right() <= area().right() + 1e-3);
            assert!(rect.top() >= area().top() && rect.bottom() <= area().bottom() + 1e-3);
        }
    }

    /// A window too small for the card at all slides it in rather than letting
    /// rows fall off the edge.
    #[test]
    fn a_card_too_big_for_the_window_is_slid_in() {
        let tiny = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(120.0, 100.0));
        let rect = place(tiny, egui::pos2(110.0, 90.0), egui::vec2(200.0, 300.0));
        assert!((rect.left() - (tiny.left() + MARGIN)).abs() < 1e-3);
        assert!((rect.top() - (tiny.top() + MARGIN)).abs() < 1e-3);
    }

    /// The two extract rows appear only for an archive, and they land where
    /// the eye already is — next to "Open with", not at the bottom.
    #[test]
    fn extract_rows_appear_only_on_an_archive() {
        let plain = items(facts());
        assert!(plain.iter().all(|i| i.action != Action::ExtractHere));

        let rows = items(Facts {
            archive: true,
            ..facts()
        });
        let at = |action: Action| rows.iter().position(|i| i.action == action);
        let here = at(Action::ExtractHere).expect("extract here");
        let sub = at(Action::ExtractSubfolder).expect("extract to subfolder");
        assert_eq!(sub, here + 1, "the two extract rows are adjacent");
        assert!(here > at(Action::Open).expect("open"));
        assert!(here < at(Action::Yank).expect("copy"));
        assert!(rows[here].enabled && rows[sub].enabled);
        assert_eq!(rows[here].keys, "e");
        assert_eq!(rows[sub].keys, "E");
    }

    /// The trash gets a menu of its own — three verbs, not the ordinary ten
    /// with seven of them grey (PLAN §7.4).
    #[test]
    fn the_trash_gets_its_own_menu() {
        let in_trash = Facts {
            trash: true,
            trashed: 4,
            ..facts()
        };
        let rows = items(in_trash);
        let actions: Vec<Action> = rows.iter().map(|item| item.action).collect();
        assert_eq!(
            actions,
            vec![
                Action::Restore,
                Action::Purge,
                Action::CopyPath,
                Action::Properties,
                Action::EmptyTrash,
            ]
        );
        // Nothing that would act on a trashed file where it lies.
        assert!(!actions.contains(&Action::Trash));
        assert!(!actions.contains(&Action::Paste));
        assert!(!actions.contains(&Action::Rename));
        // …and the irreversible row is last, after a gap, so it is the hardest
        // one in the menu to hit by accident.
        let last = rows.last().expect("a row");
        assert_eq!(last.action, Action::EmptyTrash);
        assert!(last.gap_before);

        // An empty trash still shows the row, greyed: a menu whose shape
        // changes with its contents is a menu you cannot aim at from memory.
        let empty = Facts {
            trash: true,
            trashed: 0,
            targets: 0,
            has_row: false,
            ..facts()
        };
        let enabled = |action: Action| {
            items(empty)
                .into_iter()
                .find(|i| i.action == action)
                .map(|i| i.enabled)
        };
        assert_eq!(enabled(Action::EmptyTrash), Some(false));
        assert_eq!(enabled(Action::Restore), Some(false));
    }

    /// The keyboard skips grey rows and wraps at both ends.
    #[test]
    fn the_keyboard_walks_only_the_rows_it_can_use() {
        let sparse = Facts {
            has_row: false,
            is_dir: false,
            targets: 0,
            clipboard: true,
            openers: 0,
            archive: false,
            trash: false,
            trashed: 0,
        };
        let mut menu = Menu::new(egui::pos2(0.0, 0.0), items(sparse), Vec::new());
        assert_eq!(menu.cursor, None, "an unaimed menu picks nothing");
        assert_eq!(menu.activate(), None);
        menu.move_cursor(1);
        assert_eq!(menu.activate(), Some(Action::Paste));
        // Only one row is live, so every further move stays on it.
        menu.move_cursor(1);
        assert_eq!(menu.activate(), Some(Action::Paste));
        menu.move_cursor(-1);
        assert_eq!(menu.activate(), Some(Action::Paste));

        // …and with everything live, `↑` from nothing lands on the last row.
        let mut menu = Menu::new(egui::pos2(0.0, 0.0), items(facts()), Vec::new());
        menu.move_cursor(-1);
        assert_eq!(menu.activate(), Some(Action::Properties));
        menu.move_cursor(1);
        assert_eq!(menu.activate(), Some(Action::Open));
    }

    /// The submenu takes the arrows while it is out, and gives them back.
    #[test]
    fn the_submenu_owns_the_keyboard_while_it_is_out() {
        let openers = vec!["Zed".to_string(), "Firefox".to_string()];
        let mut menu = Menu::new(egui::pos2(0.0, 0.0), items(facts()), openers);
        assert!(menu.open_submenu());
        menu.move_cursor(1);
        assert_eq!(menu.activate(), Some(Action::OpenWith(0)));
        menu.move_cursor(1);
        assert_eq!(menu.activate(), Some(Action::OpenWith(1)));
        assert!(menu.close_submenu());
        assert!(!menu.close_submenu());
        // The parent row kept the cursor, so `←` does not lose your place.
        assert_eq!(menu.activate(), Some(Action::OpenWithMenu));
    }

    /// The fade is a fade, and it ends.
    #[test]
    fn a_dismissed_menu_fades_and_then_is_spent() {
        let t0 = Instant::now();
        let mut menu = Menu::new(egui::pos2(0.0, 0.0), items(facts()), Vec::new());
        assert_eq!(menu.alpha(t0), 1.0);
        assert!(menu.live() && !menu.spent(t0));
        menu.closing = Some(t0);
        assert!(!menu.live());
        let mid = menu.alpha(t0 + FADE / 2);
        assert!(mid > 0.2 && mid < 0.8, "got {mid}");
        assert_eq!(menu.alpha(t0 + FADE), 0.0);
        assert!(menu.spent(t0 + FADE));
    }

    /// The layout: rows stack, separators open the gaps, and the hit test finds
    /// what was drawn.
    #[test]
    fn the_rows_are_laid_out_and_hit_tested_the_same_way() {
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(Default::default(), |ui| {
            let menu = Menu::new(egui::pos2(200.0, 200.0), items(facts()), Vec::new());
            let g = geometry(area(), &menu, ui.painter());
            assert_eq!(g.rows.len(), menu.items.len());
            assert!((g.card.height() - height(&menu.items)).abs() < 1e-3);
            for (index, rect) in g.rows.iter().enumerate() {
                assert_eq!(g.hit(rect.center()), Some(Control::MenuItem(index)));
                assert!(g.contains(rect.center()));
                assert!(g.card.contains(rect.center()));
            }
            assert_eq!(g.hit(egui::pos2(0.0, 0.0)), None);
            assert!(!g.contains(egui::pos2(0.0, 0.0)));

            // The submenu is beside the card, not on top of it.
            let mut menu = Menu::new(
                egui::pos2(200.0, 200.0),
                items(facts()),
                vec!["Zed".to_string(), "mpv".to_string()],
            );
            menu.open_submenu();
            let g = geometry(area(), &menu, ui.painter());
            let (sub, rows) = g.sub.as_ref().expect("the submenu is out");
            assert_eq!(rows.len(), 2);
            assert!(sub.left() > g.card.left());
            assert_eq!(g.hit(rows[1].center()), Some(Control::SubmenuItem(1)));
        });
    }
}
