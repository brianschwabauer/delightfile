//! The desktop's light or dark, on Windows: the registry value Settings →
//! Personalisation → Colours writes, read and then heard
//! (`plans/other-platforms/04-windows.md` W4.33).
//!
//! **Why the registry and not the window.** winit reports the system theme
//! on Windows too (`Window::theme`, `WindowEvent::ThemeChanged`), with the
//! two catches it has on macOS: the question goes out in `App::new`, before
//! there is a window to ask, and once `App::window_theme` sets the window's
//! own theme to the program's choice, the window stops saying what the
//! system's is. The value behind both is `AppsUseLightTheme` under
//! `HKCU\Software\Microsoft\Windows\CurrentVersion\Themes\Personalize` — `0`
//! dark, `1` light — which needs no window, is read in a call, and is heard
//! by `RegNotifyChangeKeyValue` on a thread of the watcher's own.
//!
//! So [`Desktop::watch_over`] reads the value before it returns (the first
//! answer is in at once, as on macOS) and starts a thread that waits on the
//! key: on a change it reads the value again and, when the side moved, keeps
//! it for the next [`Desktop::drain`] and rings the window's bell. A second
//! event wakes the thread when the watcher is dropped. A machine with no
//! such key — a server core, a stripped image — has no preference to follow:
//! the watcher has already gone ([`Link::Gone`]), and `auto` is dark, as on a
//! Linux session with no portal.
#![allow(unsafe_code)] // the registry key, its change notification and two events; each call says why it holds

use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use df_core::fs::Notifier;
use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows_sys::Win32::System::Registry::{
    RegCloseKey, RegGetValueW, RegNotifyChangeKeyValue, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER,
    KEY_NOTIFY, KEY_QUERY_VALUE, REG_NOTIFY_CHANGE_LAST_SET, RRF_RT_REG_DWORD,
};
use windows_sys::Win32::System::Threading::{
    CreateEventW, SetEvent, WaitForMultipleObjects, INFINITE,
};

use crate::appearance::{Link, Scheme};

/// The key the value is under, below `HKEY_CURRENT_USER`.
const PERSONALIZE: &str = r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize";

/// The value: `0` dark, `1` light, for applications.
const APPS_USE_LIGHT_THEME: &str = "AppsUseLightTheme";

/// How a watcher reaches the desktop: the current user's registry, which is
/// always there to be asked.
#[derive(Debug, Clone)]
pub struct Connect;

/// The registry, as every watcher asks it.
pub fn session() -> Connect {
    Connect
}

/// What the value means: `0` dark, anything else light, no value no
/// preference.
pub fn scheme_of(value: Option<u32>) -> Scheme {
    match value {
        Some(0) => Scheme::Dark,
        Some(_) => Scheme::Light,
        None => Scheme::NoPreference,
    }
}

/// `text` as a NUL-terminated wide string.
fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(Some(0)).collect()
}

/// The value as it is now, or `None` when there is none.
fn read() -> Option<u32> {
    let (key, name) = (wide(PERSONALIZE), wide(APPS_USE_LIGHT_THEME));
    let mut value = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    // SAFETY: both names are NUL-terminated and outlive the call; the value
    // is a live u32 and `size` says so; the type is not asked for.
    let error = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            key.as_ptr(),
            name.as_ptr(),
            RRF_RT_REG_DWORD,
            std::ptr::null_mut(),
            (&mut value as *mut u32).cast(),
            &mut size,
        )
    };
    (error == 0).then_some(value)
}

/// What the thread has heard, read by the [`Desktop`] that owns it.
#[derive(Default)]
struct Heard {
    latest: Option<Scheme>,
    ever: bool,
    gone: bool,
}

/// An event this program made, closed with it.
struct Event(HANDLE);

// SAFETY: an event handle may be signalled and waited on from any thread.
unsafe impl Send for Event {}
unsafe impl Sync for Event {}

impl Event {
    /// A fresh auto-reset event, unsignalled.
    fn new() -> Option<Event> {
        // SAFETY: default security, auto-reset, unsignalled, unnamed.
        let handle = unsafe { CreateEventW(std::ptr::null(), 0, 0, std::ptr::null()) };
        (handle != 0).then_some(Event(handle))
    }
}

impl Drop for Event {
    fn drop(&mut self) {
        // SAFETY: the handle is this program's and closed once.
        unsafe { CloseHandle(self.0) };
    }
}

/// The key, open for its change notifications, closed with it.
struct Key(HKEY);

// SAFETY: a registry key handle may be used from any thread.
unsafe impl Send for Key {}

impl Key {
    fn open() -> Option<Key> {
        let path = wide(PERSONALIZE);
        let mut key: HKEY = 0;
        // SAFETY: the path outlives the call and `key` is a live HKEY.
        let error = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                path.as_ptr(),
                0,
                KEY_NOTIFY | KEY_QUERY_VALUE,
                &mut key,
            )
        };
        (error == 0).then_some(Key(key))
    }
}

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: the key is this program's and closed once.
        unsafe { RegCloseKey(self.0) };
    }
}

/// A watcher of the registry value.
pub struct Desktop {
    started: Instant,
    heard: Arc<Mutex<Heard>>,
    /// The thread and the event that stops it, while it is listening.
    listening: Option<(JoinHandle<()>, Arc<Event>)>,
}

impl Desktop {
    /// Read the value and start listening for it to change. The first answer
    /// is in before this returns; `notify` rings for every change after it.
    pub fn watch_over(_connect: Connect, notify: Notifier) -> Desktop {
        let started = Instant::now();
        let heard = Arc::new(Mutex::new(Heard::default()));
        let first = read();
        if let Ok(mut heard) = heard.lock() {
            heard.latest = Some(scheme_of(first));
            heard.ever = first.is_some();
        }
        let listening = match (first, Key::open(), Event::new(), Event::new()) {
            (Some(first), Some(key), Some(changed), Some(stop)) => {
                let stop = Arc::new(stop);
                let spawned = std::thread::Builder::new()
                    .name("df-appearance".to_string())
                    .spawn({
                        let (heard, stop) = (Arc::clone(&heard), Arc::clone(&stop));
                        move || listen(key, changed, &stop, first, &heard, &notify)
                    });
                match spawned {
                    Ok(thread) => Some((thread, stop)),
                    Err(error) => {
                        log::warn!("appearance: no thread to listen on: {error}");
                        None
                    }
                }
            }
            _ => {
                log::info!("appearance: no {APPS_USE_LIGHT_THEME} to follow");
                None
            }
        };
        if listening.is_none() {
            if let Ok(mut heard) = heard.lock() {
                heard.gone = true;
            }
        }
        Desktop {
            started,
            heard,
            listening,
        }
    }

    /// The latest side, once, if it has changed since the last drain.
    pub fn drain(&mut self) -> Option<Scheme> {
        self.heard.lock().ok()?.latest.take()
    }

    /// Whether any answer has arrived.
    pub fn heard(&self) -> bool {
        self.heard.lock().is_ok_and(|heard| heard.ever)
    }

    /// Listening while the thread waits on the key; gone when there was no
    /// key, or the notification failed.
    pub fn link(&self) -> Link {
        match self.heard.lock() {
            Ok(heard) if !heard.gone => Link::Listening,
            _ => Link::Gone,
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
        let Some((thread, stop)) = self.listening.take() else {
            return;
        };
        // SAFETY: the event is alive (`stop` holds it) and signalled once.
        unsafe { SetEvent(stop.0) };
        let _ = thread.join();
    }
}

/// The thread: arm the notification, wait for it or for `stop`, read the
/// value on a change and keep it when the side moved.
fn listen(
    key: Key,
    changed: Event,
    stop: &Event,
    mut last: u32,
    heard: &Mutex<Heard>,
    notify: &Notifier,
) {
    loop {
        // SAFETY: the key and the event are alive for the thread's length,
        // and this thread stays alive while the notification is pending, as
        // an asynchronous one requires.
        let armed =
            unsafe { RegNotifyChangeKeyValue(key.0, 0, REG_NOTIFY_CHANGE_LAST_SET, changed.0, 1) };
        if armed != 0 {
            log::warn!("appearance: the registry would not say when it changes ({armed})");
            break;
        }
        let events = [changed.0, stop.0];
        // SAFETY: both handles are alive for the length of the wait.
        let woke = unsafe { WaitForMultipleObjects(2, events.as_ptr(), 0, INFINITE) };
        if woke != WAIT_OBJECT_0 {
            // The stop event, or a failed wait: either way, done.
            return;
        }
        let Some(now) = read() else {
            continue;
        };
        if now == last {
            continue;
        }
        last = now;
        if let Ok(mut heard) = heard.lock() {
            heard.latest = Some(scheme_of(Some(now)));
            heard.ever = true;
        }
        notify();
    }
    if let Ok(mut heard) = heard.lock() {
        heard.gone = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_value_is_the_side() {
        assert_eq!(scheme_of(Some(0)), Scheme::Dark);
        assert_eq!(scheme_of(Some(1)), Scheme::Light);
        assert_eq!(scheme_of(None), Scheme::NoPreference);
    }

    /// The first answer is in before the watcher is back, and it is the
    /// value the registry holds now; the watcher is listening where there is
    /// a value to hear, and has already gone where there is none. Dropping it
    /// stops its thread.
    #[test]
    fn the_first_answer_is_there_at_once() {
        let now = read();
        let mut desktop = Desktop::watch_over(session(), Arc::new(|| {}));
        assert_eq!(desktop.heard(), now.is_some());
        assert_eq!(
            desktop.wait_first(Duration::from_millis(1)),
            Some(scheme_of(now))
        );
        assert_eq!(
            desktop.link(),
            if now.is_some() {
                Link::Listening
            } else {
                Link::Gone
            }
        );
        drop(desktop);
    }
}
