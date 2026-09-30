//! The app menu in macOS's menu bar (`plans/other-platforms/02-macos.md`
//! M2.37), where a Mac keeps a program's menu, in place of the ☰ button on
//! the top row.
//!
//! **One menu, two places to show it.** The rows are the ☰ menu's: the
//! window builds them as the button would open them now, every frame, and
//! [`publish`]es them; `platform::mirror` says which of the bar's menus each
//! goes under. The bar is AppKit's and opens when the user opens it, so the
//! rows are kept, and a menu takes the latest of them as it opens: its
//! delegate's `menuNeedsUpdate:` builds it again when the rows have changed
//! since it was last built, and never while it is open. The first rows are
//! put in at once, and the bar handed to the application then, so no menu
//! is ever empty and winit's own menu is on screen only until the first
//! frame.
//!
//! **A choice is the window's to carry out.** Choosing a row sends
//! `delightfileChoose:` to [`Target`], which looks up what the row does by
//! its tag, queues it, and rings the window's bell; the window [`take`]s the
//! queue in its next frame and does each through the door the ☰ menu's rows
//! go through (`App::menu_action`). Nothing runs inside AppKit's callback,
//! where the window is not to be borrowed. A row's tag says which menu it is
//! in and where in that menu's list of actions, and a menu's list is
//! replaced only when the menu is built again, so a tag always means the
//! row that was on screen.
//!
//! **Keys are shown and not answered.** Each row shows the key the ☰ menu
//! shows beside it, in AppKit's notation, but its menu's delegate answers
//! `menuHasKeyEquivalent:forEvent:target:action:` with NO, so AppKit never
//! matches a keystroke against those rows: the key goes to the window, as
//! it always has, and through the keymap, which is the one door. Were the
//! rows to answer, `⌘v` in a prompt would paste files where it pastes
//! text, and a bare `y` typed into one would copy the selection. The
//! application menu and Window are AppKit's own items (About, Hide, Quit;
//! Minimize, Zoom, Bring All to Front), sent up the responder chain as the
//! system's are, and their keys answer as the system's do: `⌘Q` is
//! `terminate:`, M2.26's path, and so is the Quit row.
//!
//! Window tabbing is turned off for the process
//! (`NSWindow.allowsAutomaticWindowTabbing`): the window has tabs of its
//! own, and a window per process would make AppKit's Show Tab Bar and Merge
//! All Windows items show one tab or do nothing.
//!
//! **Unsafe.** The two classes here are Objective-C classes of our own
//! (`declare_class!`), and most of the menu calls are ones objc2-app-kit 0.2
//! marks `unsafe` because it cannot check them. Each is sound for the reason
//! written at it. Menus hold their delegate and their items' target weakly,
//! so both are held here, in [`Bar`], for as long as the menus are.
#![allow(unsafe_code)] // The menu bar's NSMenus, their delegate and their items' target, through objc2; see the essay.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::sync::{Mutex, OnceLock};

use df_core::keymap::Registry;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject, Sel};
use objc2::{declare_class, msg_send_id, mutability, sel, ClassType, DeclaredClass};
use objc2_app_kit::{
    NSApplication, NSControlStateValueOff, NSControlStateValueOn, NSEvent, NSEventModifierFlags,
    NSMenu, NSMenuDelegate, NSMenuItem, NSWindow,
};
use objc2_foundation::{MainThreadMarker, NSInteger, NSString};

use crate::app::Waker;
use crate::menu::{Action, Item};
use crate::platform::mirror::{self, KeyEquivalent, Row, System, Title};

/// Whether the top row carries the ☰ button: not here, where the app menu
/// is the menu bar's.
pub const MENU_BUTTON: bool = false;

/// The rows the window last published, as the bar's menus, and a count
/// that moves each time they change.
struct Published {
    generation: u64,
    menus: Vec<(Title, Vec<Row>)>,
}

static PUBLISHED: Mutex<Option<Published>> = Mutex::new(None);

/// What was chosen in the bar and not yet taken by the window.
static CHOSEN: Mutex<Vec<Action>> = Mutex::new(Vec::new());

/// The window's bell, rung when something is chosen.
static WAKER: OnceLock<Waker> = OnceLock::new();

thread_local! {
    /// The bar, on the main thread, from [`start`] on.
    static BAR: RefCell<Option<Bar>> = const { RefCell::new(None) };
}

/// Every tag at or above this is a row of ours: the rows AppKit puts into a
/// menu itself (Help's search, a View menu's Enter Full Screen) are left
/// where they are when a menu is built again.
const OURS: NSInteger = 0x4446_0000;

/// How many rows one menu may have, as the tag counts them.
const PER_MENU: NSInteger = 0x1000;

/// A chosen row's tag: which mirrored menu (`slot`, its place in
/// [`Title::MIRRORED`]) and where in that menu's list of actions.
fn tag_of(slot: usize, index: usize) -> NSInteger {
    OURS + 1 + slot as NSInteger * PER_MENU + index as NSInteger
}

/// [`tag_of`]'s inverse: the slot and the index, or `None` for a tag that
/// is no chosen row of ours.
fn untag(tag: NSInteger) -> Option<(usize, usize)> {
    let n = tag.checked_sub(OURS + 1).filter(|n| *n >= 0)?;
    Some(((n / PER_MENU) as usize, (n % PER_MENU) as usize))
}

/// The bar's menus and the two objects their rows answer to.
struct Bar {
    main: Retained<NSMenu>,
    services: Retained<NSMenu>,
    window: Retained<NSMenu>,
    help: Retained<NSMenu>,
    /// The menus built from the app menu's rows, in [`Title::MIRRORED`]'s
    /// order, and the generation of rows each was last built from (`0`:
    /// never).
    mirrored: Vec<(Title, Retained<NSMenu>, Cell<u64>)>,
    target: Retained<Target>,
    delegate: Retained<Delegate>,
}

declare_class!(
    /// What every chosen row is sent to.
    struct Target;

    // SAFETY: `NSObject` has no subclassing requirements, the class is only
    // ever touched on the main thread, and it is named for this program.
    unsafe impl ClassType for Target {
        type Super = NSObject;
        type Mutability = mutability::MainThreadOnly;
        const NAME: &'static str = "DelightfileMenuTarget";
    }

    // Each mirrored menu's actions, by the index in its rows' tags.
    impl DeclaredClass for Target {
        type Ivars = RefCell<Vec<Vec<Action>>>;
    }

    unsafe impl NSObjectProtocol for Target {}

    // SAFETY: an action method: one argument, the item that sent it.
    unsafe impl Target {
        #[method(delightfileChoose:)]
        fn choose(&self, sender: Option<&NSMenuItem>) {
            let Some(sender) = sender else {
                return;
            };
            // SAFETY: a read of a live item's tag.
            let tag = unsafe { sender.tag() };
            let chosen = untag(tag).and_then(|(slot, index)| {
                self.ivars()
                    .borrow()
                    .get(slot)
                    .and_then(|actions| actions.get(index))
                    .copied()
            });
            let Some(action) = chosen else {
                return;
            };
            if let Ok(mut queue) = CHOSEN.lock() {
                queue.push(action);
            }
            if let Some(waker) = WAKER.get() {
                waker.wake();
            }
        }
    }
);

declare_class!(
    /// The mirrored menus' delegate: builds a menu as it opens, and keeps
    /// AppKit from answering a key with one of its rows.
    struct Delegate;

    // SAFETY: as for `Target`.
    unsafe impl ClassType for Delegate {
        type Super = NSObject;
        type Mutability = mutability::MainThreadOnly;
        const NAME: &'static str = "DelightfileMenuDelegate";
    }

    impl DeclaredClass for Delegate {
        type Ivars = ();
    }

    unsafe impl NSObjectProtocol for Delegate {}

    // SAFETY: the signature is `NSMenuDelegate`'s own.
    unsafe impl NSMenuDelegate for Delegate {
        #[method(menuNeedsUpdate:)]
        fn menu_needs_update(&self, menu: &NSMenu) {
            refresh(MainThreadMarker::from(self), menu);
        }
    }

    // SAFETY: `NSMenuDelegate`'s own signature, which objc2-app-kit 0.2
    // leaves out for its two out-pointers (`id *`, `SEL *`): they are
    // written only when the answer is YES, and it never is.
    unsafe impl Delegate {
        #[method(menuHasKeyEquivalent:forEvent:target:action:)]
        fn has_key_equivalent(
            &self,
            _menu: &NSMenu,
            _event: &NSEvent,
            _target: *mut *mut AnyObject,
            _action: *mut c_void,
        ) -> bool {
            false
        }
    }
);

impl Target {
    fn new(mtm: MainThreadMarker) -> Retained<Target> {
        let this = mtm
            .alloc::<Target>()
            .set_ivars(RefCell::new(vec![Vec::new(); Title::MIRRORED.len()]));
        // SAFETY: `init` is `NSObject`'s, on a freshly allocated object.
        unsafe { msg_send_id![super(this), init] }
    }
}

impl Delegate {
    fn new(mtm: MainThreadMarker) -> Retained<Delegate> {
        let this = mtm.alloc::<Delegate>().set_ivars(());
        // SAFETY: as for `Target::new`.
        unsafe { msg_send_id![super(this), init] }
    }
}

/// Make the bar, once, after the window is made. It is handed to the
/// application with the first rows [`publish`] brings; until then winit's
/// own menu stands.
pub fn start(waker: Waker) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let _ = WAKER.set(waker);
    NSWindow::setAllowsAutomaticWindowTabbing(false, mtm);
    BAR.with(|bar| {
        let Ok(mut bar) = bar.try_borrow_mut() else {
            return;
        };
        if bar.is_none() {
            *bar = Some(Bar::new(mtm));
        }
    });
}

/// The app menu's rows as the ☰ button would open them now. Kept for the
/// bar's menus to be built from as each opens; the first rows are put in at
/// once, and the bar handed to the application.
pub fn publish(items: &[Item], keymap: &Registry) {
    let menus = mirror::bar(items, keymap);
    let first = {
        let Ok(mut published) = PUBLISHED.lock() else {
            return;
        };
        match published.as_mut() {
            Some(published) if published.menus == menus => return,
            Some(published) => {
                published.menus = menus;
                published.generation += 1;
                false
            }
            None => {
                *published = Some(Published {
                    generation: 1,
                    menus,
                });
                true
            }
        }
    };
    if first {
        install();
    }
}

/// What was chosen in the bar since the last call, oldest first.
pub fn take() -> Vec<Action> {
    CHOSEN
        .lock()
        .map(|mut queue| std::mem::take(&mut *queue))
        .unwrap_or_default()
}

/// The latest rows for the menu `title`, and their generation.
fn latest(title: Title) -> Option<(u64, Vec<Row>)> {
    let published = PUBLISHED.lock().ok()?;
    let published = published.as_ref()?;
    let rows = published
        .menus
        .iter()
        .find(|(t, _)| *t == title)
        .map(|(_, rows)| rows.clone())?;
    Some((published.generation, rows))
}

/// Build every mirrored menu from the first rows and hand the bar to the
/// application.
fn install() {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    BAR.with(|bar| {
        let Ok(bar) = bar.try_borrow() else {
            return;
        };
        let Some(bar) = bar.as_ref() else {
            return;
        };
        for slot in 0..bar.mirrored.len() {
            bar.build(mtm, slot);
        }
        let app = NSApplication::sharedApplication(mtm);
        app.setMainMenu(Some(&bar.main));
        // SAFETY: three menus of the main menu's, held by `Bar` for the
        // process; AppKit keeps the Services list, the window list and
        // Help's search in them.
        unsafe {
            app.setServicesMenu(Some(&bar.services));
            app.setWindowsMenu(Some(&bar.window));
            app.setHelpMenu(Some(&bar.help));
        }
    });
}

/// `menu` is about to open: build it again if the rows have changed since
/// it was built. A list flown out of one of its rows is built with it, and
/// is not built again on its own.
fn refresh(mtm: MainThreadMarker, menu: &NSMenu) {
    BAR.with(|bar| {
        let Ok(bar) = bar.try_borrow() else {
            return;
        };
        let Some(bar) = bar.as_ref() else {
            return;
        };
        let Some(slot) = bar
            .mirrored
            .iter()
            .position(|(_, mirrored, _)| std::ptr::eq(&**mirrored, menu))
        else {
            return;
        };
        bar.build(mtm, slot);
    });
}

impl Bar {
    /// The main menu, its seven menus, and the application menu's and
    /// Window's items, which are AppKit's and never change.
    fn new(mtm: MainThreadMarker) -> Bar {
        let target = Target::new(mtm);
        let delegate = Delegate::new(mtm);
        let main = NSMenu::new(mtm);
        let services = menu(mtm, "Services");
        let mut window = None;
        let mut help = None;
        let mut mirrored = Vec::new();
        for title in Title::ALL {
            let list = menu(mtm, title.label());
            // SAFETY: a fresh item, with no action and no key.
            let item = unsafe {
                NSMenuItem::initWithTitle_action_keyEquivalent(
                    mtm.alloc(),
                    &NSString::from_str(title.label()),
                    None,
                    &NSString::new(),
                )
            };
            item.setSubmenu(Some(&list));
            main.addItem(&item);
            match title {
                Title::App => {
                    for row in mirror::app_menu() {
                        let item = static_item(mtm, &row);
                        if row == Row::System(System::Services) {
                            item.setSubmenu(Some(&services));
                        }
                        list.addItem(&item);
                    }
                }
                Title::Window => {
                    for row in mirror::window_menu() {
                        list.addItem(&static_item(mtm, &row));
                    }
                    window = Some(list);
                }
                _ => {
                    quiet(&list, &delegate);
                    if title == Title::Help {
                        help = Some(list.retain());
                    }
                    mirrored.push((title, list, Cell::new(0)));
                }
            }
        }
        Bar {
            main,
            services,
            window: window.unwrap_or_else(|| menu(mtm, "Window")),
            help: help.unwrap_or_else(|| menu(mtm, "Help")),
            mirrored,
            target,
            delegate,
        }
    }

    /// Build the mirrored menu at `slot` from the latest rows, unless it was
    /// built from them already: its rows of ours taken out and the new ones
    /// put in their place, anything AppKit put in the menu left as it was.
    fn build(&self, mtm: MainThreadMarker, slot: usize) {
        let Some((title, list, built)) = self.mirrored.get(slot) else {
            return;
        };
        let Some((generation, rows)) = latest(*title) else {
            return;
        };
        if built.get() == generation {
            return;
        }
        // Marked first, so a menu AppKit asked about again while it is
        // being built is not built twice over.
        built.set(generation);
        let actions = fill(mtm, list, &rows, slot, &self.target, &self.delegate);
        if let Some(slots) = self.target.ivars().borrow_mut().get_mut(slot) {
            *slots = actions;
        }
    }
}

/// A menu of our own: its items' greys are the rows' (no validation), and
/// its keys are shown and not answered ([`Delegate`]).
fn quiet(list: &NSMenu, delegate: &Delegate) {
    // SAFETY: a live menu told how to treat its own items, and given a
    // delegate that `Bar` holds for as long as the menu.
    unsafe {
        list.setAutoenablesItems(false);
        list.setDelegate(Some(ProtocolObject::from_ref(delegate)));
    }
}

fn menu(mtm: MainThreadMarker, title: &str) -> Retained<NSMenu> {
    // SAFETY: a fresh, empty menu.
    unsafe { NSMenu::initWithTitle(mtm.alloc(), &NSString::from_str(title)) }
}

/// Put `rows` into `list` where its rows of ours were, or at its end when it
/// has none yet; the actions the rows' tags index, in order.
fn fill(
    mtm: MainThreadMarker,
    list: &NSMenu,
    rows: &[Row],
    slot: usize,
    target: &Target,
    delegate: &Delegate,
) -> Vec<Action> {
    // SAFETY: reads and removals of a live menu's own items, by index, from
    // the end, so every index read is still the item it was.
    let mut at = unsafe {
        let count = list.numberOfItems();
        let mut first = None;
        for index in (0..count).rev() {
            if list
                .itemAtIndex(index)
                .is_some_and(|item| item.tag() >= OURS)
            {
                list.removeItemAtIndex(index);
                first = Some(index);
            }
        }
        first.unwrap_or_else(|| list.numberOfItems())
    };
    let mut actions = Vec::new();
    for row in rows {
        let item = row_item(mtm, row, slot, &mut actions, target, delegate);
        // SAFETY: an index no further than the menu's end.
        unsafe { list.insertItem_atIndex(&item, at) };
        at += 1;
    }
    actions
}

/// One row as a menu item: a leaf sent to `target` under a tag that indexes
/// `actions`, a parent with its list built in, or a separator.
fn row_item(
    mtm: MainThreadMarker,
    row: &Row,
    slot: usize,
    actions: &mut Vec<Action>,
    target: &Target,
    delegate: &Delegate,
) -> Retained<NSMenuItem> {
    let entry = match row {
        Row::Separator => {
            let item = NSMenuItem::separatorItem(mtm);
            // SAFETY: a fresh item's tag.
            unsafe { item.setTag(OURS) };
            return item;
        }
        Row::System(system) => return static_item(mtm, &Row::System(*system)),
        Row::Item(entry) => entry,
    };
    let (key, mask) = key_equivalent(entry.key);
    let action = entry.submenu.is_none().then_some(sel!(delightfileChoose:));
    // SAFETY: a fresh item with our selector, whose target is set below.
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            mtm.alloc(),
            &NSString::from_str(&entry.title),
            action,
            &NSString::from_str(&key),
        )
    };
    item.setKeyEquivalentModifierMask(mask);
    // SAFETY: setters on a fresh item; the target is held by `Bar` for as
    // long as the menus are, which is as long as the item can send to it.
    unsafe {
        item.setEnabled(entry.enabled);
        if let Some(checked) = entry.checked {
            item.setState(if checked {
                NSControlStateValueOn
            } else {
                NSControlStateValueOff
            });
        }
        match &entry.submenu {
            Some(rows) => {
                let list = menu(mtm, &entry.title);
                quiet(&list, delegate);
                for row in rows {
                    list.addItem(&row_item(mtm, row, slot, actions, target, delegate));
                }
                item.setSubmenu(Some(&list));
                item.setTag(OURS);
            }
            None => {
                let object: &AnyObject = target;
                item.setTarget(Some(object));
                item.setTag(tag_of(slot, actions.len()));
                actions.push(entry.action);
            }
        }
    }
    item
}

/// One of AppKit's own items, or a separator, for the menus that are only
/// those. Sent to no target, so up the responder chain — the window for
/// Minimize and Zoom, the application for the rest — as the system's are.
fn static_item(mtm: MainThreadMarker, row: &Row) -> Retained<NSMenuItem> {
    let Row::System(system) = row else {
        return NSMenuItem::separatorItem(mtm);
    };
    let action = selector(*system);
    let (key, mask) = key_equivalent(system.key());
    // SAFETY: a fresh item with one of AppKit's own selectors.
    let item = unsafe {
        NSMenuItem::initWithTitle_action_keyEquivalent(
            mtm.alloc(),
            &NSString::from_str(&system.title()),
            action,
            &NSString::from_str(&key),
        )
    };
    item.setKeyEquivalentModifierMask(mask);
    item
}

/// What the system's own item sends.
fn selector(system: System) -> Option<Sel> {
    Some(match system {
        System::About => sel!(orderFrontStandardAboutPanel:),
        System::Services => return None,
        System::Hide => sel!(hide:),
        System::HideOthers => sel!(hideOtherApplications:),
        System::ShowAll => sel!(unhideAllApplications:),
        System::Quit => sel!(terminate:),
        System::Minimize => sel!(performMiniaturize:),
        System::Zoom => sel!(performZoom:),
        System::BringAllToFront => sel!(arrangeInFront:),
    })
}

/// A key equivalent as the item's `keyEquivalent` and modifier mask; none
/// is an empty key and no modifiers (an item's mask is ⌘ until it is set).
fn key_equivalent(key: Option<KeyEquivalent>) -> (String, NSEventModifierFlags) {
    let Some(key) = key else {
        return (String::new(), NSEventModifierFlags::empty());
    };
    let mut mask = NSEventModifierFlags::empty();
    for (held, flag) in [
        (
            key.command,
            NSEventModifierFlags::NSEventModifierFlagCommand,
        ),
        (key.option, NSEventModifierFlags::NSEventModifierFlagOption),
        (key.shift, NSEventModifierFlags::NSEventModifierFlagShift),
        (
            key.control,
            NSEventModifierFlags::NSEventModifierFlagControl,
        ),
    ] {
        if held {
            mask |= flag;
        }
    }
    (key.key.to_string(), mask)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tag_says_which_menu_and_which_row() {
        for (slot, index) in [(0, 0), (0, 41), (4, 0), (2, 4094)] {
            let tag = tag_of(slot, index);
            assert!(tag > OURS);
            assert_eq!(untag(tag), Some((slot, index)));
        }
        assert_eq!(untag(OURS), None, "a separator or a parent");
        assert_eq!(untag(0), None, "AppKit's own");
        assert_eq!(untag(-1), None);
    }

    #[test]
    fn a_key_equivalent_is_its_key_and_its_modifiers() {
        let (key, mask) = key_equivalent(Some(KeyEquivalent {
            key: 'n',
            command: true,
            option: false,
            shift: true,
            control: false,
        }));
        assert_eq!(key, "n");
        assert_eq!(
            mask,
            NSEventModifierFlags::NSEventModifierFlagCommand
                | NSEventModifierFlags::NSEventModifierFlagShift
        );
        let (key, mask) = key_equivalent(None);
        assert_eq!(key, "");
        assert_eq!(mask, NSEventModifierFlags::empty(), "not ⌘, the default");
    }

    /// The ☰ menu's rows, built into real menus and read back: how many
    /// items each has, their titles, their keys and masks, their greys and
    /// ticks, and the tags that say what a choice of each does.
    ///
    /// Not on the main thread — libtest runs every test on one of its own —
    /// so the marker is asserted rather than proved. The menus are made,
    /// read and dropped on this one thread and never reach the application
    /// or the screen; AppKit's main-thread rule is about the menus it
    /// tracks and draws.
    #[test]
    fn the_rows_build_into_menus_that_read_back_as_they_were_given() {
        // SAFETY: see the doc comment above.
        let mtm = unsafe { MainThreadMarker::new_unchecked() };
        let keymap = Registry::defaults();
        let mut items = crate::menu::app_items(
            crate::menu::AppFacts {
                picker: false,
                local: true,
                targets: 0,
                clipboard: false,
                scale: df_core::config::ViewScale::Compact,
                hidden: true,
                linemode: df_core::config::LineMode::Size,
                sort: df_core::config::SortBy::Natural,
                reverse: false,
                parent_open: true,
                preview_open: false,
                appearance: df_core::config::ThemeMode::Dark,
            },
            Vec::new(),
            &keymap,
            |_| false,
        );
        crate::menu::insert_go(
            &mut items,
            crate::menu::place_rows(&[], false, &keymap, |_| false),
        );
        let bar = mirror::bar(&items, &keymap);
        let target = Target::new(mtm);
        let delegate = Delegate::new(mtm);
        for (slot, (title, rows)) in bar.iter().enumerate() {
            let list = menu(mtm, title.label());
            quiet(&list, &delegate);
            let actions = fill(mtm, &list, rows, slot, &target, &delegate);
            // SAFETY: reads of the live menu and items made above.
            unsafe {
                assert_eq!(list.numberOfItems() as usize, rows.len(), "{title:?}");
                for (index, row) in rows.iter().enumerate() {
                    let item = list.itemAtIndex(index as NSInteger).expect("an item");
                    match row {
                        Row::Separator => assert!(item.isSeparatorItem()),
                        Row::System(_) => unreachable!("no system rows in a mirrored menu"),
                        Row::Item(entry) => {
                            assert_eq!(item.title().to_string(), entry.title);
                            let (key, mask) = key_equivalent(entry.key);
                            assert_eq!(item.keyEquivalent().to_string(), key, "{}", entry.title);
                            assert_eq!(item.keyEquivalentModifierMask(), mask, "{}", entry.title);
                            assert_eq!(item.isEnabled(), entry.enabled, "{}", entry.title);
                            assert_eq!(
                                item.state() == NSControlStateValueOn,
                                entry.checked == Some(true),
                                "{}",
                                entry.title
                            );
                            match &entry.submenu {
                                Some(rows) => assert_eq!(
                                    item.submenu().map(|list| list.numberOfItems() as usize),
                                    Some(rows.len())
                                ),
                                None => {
                                    let (at, index) =
                                        untag(item.tag()).expect("a chosen row's tag");
                                    assert_eq!(at, slot);
                                    assert_eq!(actions.get(index), Some(&entry.action));
                                }
                            }
                        }
                    }
                }
            }
            // Built again, it has the same items, in the same place.
            fill(mtm, &list, rows, slot, &target, &delegate);
            // SAFETY: a read of the live menu.
            assert_eq!(unsafe { list.numberOfItems() } as usize, rows.len());
        }
        // The shipped keys, as a Mac shows them.
        let file = &bar[0].1;
        let new_window = file
            .iter()
            .find_map(|row| match row {
                Row::Item(entry) if entry.title == "New window" => Some(entry),
                _ => None,
            })
            .expect("New window");
        assert_eq!(
            key_equivalent(new_window.key),
            (
                "n".to_string(),
                NSEventModifierFlags::NSEventModifierFlagCommand
            )
        );
    }

    /// AppKit's own items: the titles, the keys, and the selectors that
    /// go up the responder chain.
    #[test]
    fn the_systems_items_are_built_as_the_systems_are() {
        // SAFETY: as in the test above: made, read and dropped here.
        let mtm = unsafe { MainThreadMarker::new_unchecked() };
        let quit = static_item(mtm, &Row::System(System::Quit));
        // SAFETY: reads of the live item made above.
        unsafe {
            assert_eq!(quit.title().to_string(), "Quit delightfile");
            assert_eq!(quit.keyEquivalent().to_string(), "q");
            assert_eq!(
                quit.keyEquivalentModifierMask(),
                NSEventModifierFlags::NSEventModifierFlagCommand
            );
            assert_eq!(quit.action(), Some(sel!(terminate:)));
            assert!(quit.target().is_none());
        }
        assert_eq!(selector(System::Minimize), Some(sel!(performMiniaturize:)));
        assert_eq!(selector(System::Services), None);
    }
}
