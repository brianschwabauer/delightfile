//! The desktop's light or dark, on macOS: the application's
//! `effectiveAppearance`, observed.
//!
//! macOS says which side it is on through `NSApp.effectiveAppearance`, and
//! says when that changes through key-value observing on the same property.
//! The application is asked rather than the window for two reasons. The
//! question goes out in `App::new`, before there is a window to ask. And
//! the window follows this program's own choice (`App::window_theme` sets
//! its appearance), after which its appearance is ours and says nothing
//! about the system's; the application's is never set, so it keeps
//! following System Settings.
//!
//! The observation is registered with `NSKeyValueObservingOptionInitial`, so
//! the first answer arrives before [`Desktop::watch_over`] returns: there is
//! no thread and nothing to wait for, and [`Desktop::wait_first`] only takes
//! what is already there. A change afterwards arrives on the main thread,
//! where AppKit makes it, is kept for the next [`Desktop::drain`], and rings
//! the window's bell so an idle window turns at once.
//!
//! Everything AppKit is main-thread-only, and asking from any other thread
//! (a test's) gets a watcher that has already gone ([`Link::Gone`]), which
//! the window takes as dark, as it takes a Linux session with no portal.
//!
//! **Unsafe.** The observer is an Objective-C class of our own
//! (`declare_class!`), and registering it, reading the appearance and the
//! two appearance names are calls objc2-app-kit 0.2 marks `unsafe` because
//! it cannot check them. Each is sound for the reason written at it; the one
//! rule that spans them is that an observer is removed before it is freed,
//! which [`Desktop`]'s `Drop` keeps.

#![allow(unsafe_code)] // Key-value observing of NSApp's appearance through objc2; see the essay.

use std::cell::Cell;
use std::ffi::c_void;
use std::time::{Duration, Instant};

use df_core::fs::Notifier;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol};
use objc2::{declare_class, msg_send_id, mutability, ClassType, DeclaredClass};
use objc2_app_kit::{NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSApplication};
use objc2_foundation::{
    ns_string, MainThreadMarker, NSArray, NSCopying, NSKeyValueObservingOptions,
    NSObjectNSKeyValueObserverRegistration, NSString,
};

use crate::appearance::{Link, Scheme};

/// The property observed on the application.
fn key_path() -> &'static NSString {
    ns_string!("effectiveAppearance")
}

/// How a watcher reaches the desktop: the application object, which is
/// always there to be asked (on the main thread).
#[derive(Debug, Clone)]
pub struct Connect;

/// The application, as every watcher asks it.
pub fn session() -> Connect {
    Connect
}

/// What an appearance name means here. `bestMatchFromAppearancesWithNames`
/// folds every variant (the high-contrast ones included) into one of the two
/// it is offered, so these are the only names that arrive.
fn scheme_of(name: &str) -> Scheme {
    match name {
        "NSAppearanceNameDarkAqua" => Scheme::Dark,
        "NSAppearanceNameAqua" => Scheme::Light,
        _ => Scheme::NoPreference,
    }
}

/// The side the application is on now.
fn current(mtm: MainThreadMarker) -> Scheme {
    let app = NSApplication::sharedApplication(mtm);
    let appearance = app.effectiveAppearance();
    // SAFETY: two constant strings AppKit exports, alive for the process.
    let names = unsafe {
        NSArray::from_vec(vec![
            NSAppearanceNameAqua.copy(),
            NSAppearanceNameDarkAqua.copy(),
        ])
    };
    appearance
        .bestMatchFromAppearancesWithNames(&names)
        .map_or(Scheme::NoPreference, |name| scheme_of(&name.to_string()))
}

/// What the observer has heard, read by the [`Desktop`] that owns it.
struct Heard {
    latest: Cell<Option<Scheme>>,
    ever: Cell<bool>,
    notify: Notifier,
}

declare_class!(
    struct Observer;

    // SAFETY: `NSObject` has no subclassing requirements, the class is only
    // ever touched on the main thread, and it is named for this program.
    unsafe impl ClassType for Observer {
        type Super = NSObject;
        type Mutability = mutability::MainThreadOnly;
        const NAME: &'static str = "DelightfileAppearanceObserver";
    }

    impl DeclaredClass for Observer {
        type Ivars = Heard;
    }

    unsafe impl NSObjectProtocol for Observer {}

    // SAFETY: the signature is `NSKeyValueObserving`'s own.
    unsafe impl Observer {
        #[method(observeValueForKeyPath:ofObject:change:context:)]
        fn observe_value(
            &self,
            _key_path: Option<&NSString>,
            _object: Option<&AnyObject>,
            _change: Option<&AnyObject>,
            _context: *mut c_void,
        ) {
            let heard = self.ivars();
            heard.latest.set(Some(current(MainThreadMarker::from(self))));
            heard.ever.set(true);
            (heard.notify)();
        }
    }
);

/// A watcher of the application's appearance.
pub struct Desktop {
    started: Instant,
    /// The registered observer, or `None` off the main thread, where nothing
    /// can be observed.
    observer: Option<Retained<Observer>>,
}

impl Desktop {
    /// Start observing. The first answer is in before this returns; `notify`
    /// rings for it and for every change after it.
    pub fn watch_over(_connect: Connect, notify: Notifier) -> Desktop {
        let started = Instant::now();
        let Some(mtm) = MainThreadMarker::new() else {
            log::info!("the appearance can only be observed on the main thread");
            return Desktop {
                started,
                observer: None,
            };
        };
        let observer = mtm.alloc::<Observer>().set_ivars(Heard {
            latest: Cell::new(None),
            ever: Cell::new(false),
            notify,
        });
        // SAFETY: `init` is `NSObject`'s, on a freshly allocated object.
        let observer: Retained<Observer> = unsafe { msg_send_id![super(observer), init] };
        let app = NSApplication::sharedApplication(mtm);
        // SAFETY: the observer is alive for as long as it is registered —
        // `Desktop` holds it and removes the registration in `Drop` before
        // letting go — and the key path is a constant string.
        unsafe {
            app.addObserver_forKeyPath_options_context(
                &observer,
                key_path(),
                NSKeyValueObservingOptions::NSKeyValueObservingOptionInitial
                    | NSKeyValueObservingOptions::NSKeyValueObservingOptionNew,
                std::ptr::null_mut(),
            );
        }
        Desktop {
            started,
            observer: Some(observer),
        }
    }

    /// The latest side the application has been on, once, if it has changed
    /// since the last drain.
    pub fn drain(&mut self) -> Option<Scheme> {
        self.observer.as_ref()?.ivars().latest.take()
    }

    /// Whether any answer has arrived.
    pub fn heard(&self) -> bool {
        self.observer
            .as_ref()
            .is_some_and(|observer| observer.ivars().ever.get())
    }

    /// Listening for as long as the observer is registered; gone when there
    /// was none to register.
    pub fn link(&self) -> Link {
        match self.observer {
            Some(_) => Link::Listening,
            None => Link::Gone,
        }
    }

    pub fn started(&self) -> Instant {
        self.started
    }

    /// The first answer, which is already here: no wait.
    pub fn wait_first(&mut self, _within: Duration) -> Option<Scheme> {
        self.drain()
    }
}

impl Drop for Desktop {
    fn drop(&mut self) {
        let Some(observer) = self.observer.take() else {
            return;
        };
        let app = NSApplication::sharedApplication(MainThreadMarker::from(&*observer));
        // SAFETY: the observer was registered for exactly this key path in
        // `watch_over`, and is still alive here.
        unsafe { app.removeObserver_forKeyPath(&observer, key_path()) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_appearance_names_are_the_two_sides() {
        assert_eq!(scheme_of("NSAppearanceNameDarkAqua"), Scheme::Dark);
        assert_eq!(scheme_of("NSAppearanceNameAqua"), Scheme::Light);
        assert_eq!(
            scheme_of("NSAppearanceNameVibrantDark"),
            Scheme::NoPreference
        );
        assert_eq!(scheme_of(""), Scheme::NoPreference);
    }

    /// The constants AppKit exports are spelled as [`scheme_of`] expects.
    #[test]
    fn appkit_spells_the_names_as_they_are_matched() {
        // SAFETY: two constant strings AppKit exports, alive for the process.
        let (aqua, dark) = unsafe { (NSAppearanceNameAqua, NSAppearanceNameDarkAqua) };
        assert_eq!(scheme_of(&aqua.to_string()), Scheme::Light);
        assert_eq!(scheme_of(&dark.to_string()), Scheme::Dark);
    }

    /// Off the main thread — where every test runs — there is nothing to
    /// observe, and the watcher says so rather than pretending to listen.
    #[test]
    fn off_the_main_thread_the_watcher_has_already_gone() {
        let mut desktop = Desktop::watch_over(session(), std::sync::Arc::new(|| {}));
        assert_eq!(desktop.link(), Link::Gone);
        assert!(!desktop.heard());
        assert_eq!(desktop.wait_first(Duration::from_millis(1)), None);
    }
}
