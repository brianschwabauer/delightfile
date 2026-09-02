//! The `wl_data_device` client: dragging files out, taking a drop in
//! (PLAN §7.1, the plan's own "biggest technical risk"), and owning the
//! clipboard selection (PLAN §7.4).
//!
//! ## Why there is no hand-rolled wire client here
//!
//! [`crate::clipboard`] shells out to `wl-copy` and delightviewer talks D-Bus
//! down the socket by hand, so the house answer to "we need a protocol" is
//! normally *write the protocol*. It cannot be the answer here, and the reason
//! is one line of the spec:
//!
//! > `wl_data_device.start_drag(source, origin, icon, serial)` — "The `serial`
//! > parameter is the serial of the implicit grab on the origin that is
//! > currently active."
//!
//! A drag may only be started with the serial of a pointer button press *this
//! client* received, on a surface *this client* owns. That rules out both of
//! the independent options at a stroke:
//!
//! - **A second `wl_display` connection** is a second `wl_client` as far as the
//!   compositor is concerned. Its seat has its own serials, its surfaces are
//!   other people's surfaces, and `start_drag` from it is a protocol error, not
//!   a drag. No amount of care makes this work — it is refused by design, and
//!   the design is right: the alternative would be any program on the machine
//!   being able to start a drag out of any other.
//! - **Hand-rolling the wire on the existing socket** is refused by the same
//!   fact from the other side. winit is built on libwayland (`wayland-backend`
//!   with `client_system`), so the socket is not ours to read: libwayland owns
//!   the fd, the object-id space and the read/dispatch protocol between
//!   threads. A second reader writing raw messages onto that fd would be
//!   interleaving with a library that is also allocating ids on it.
//!
//! What is left is the thing libwayland was designed for and the thing this
//! file does: **share winit's connection and take a second event queue on it**.
//! That is a supported, documented pattern — it is exactly what
//! `smithay-clipboard` does for every winit application on the desktop
//! (Alacritty included), down to binding its own `wl_seat` to harvest serials.
//!
//! So the dependency is [`wayland_client`], and *only* it: not
//! `smithay-client-toolkit`. Every interface a data-device drag needs —
//! `wl_registry`, `wl_seat`, `wl_pointer`, `wl_data_device_manager`,
//! `wl_data_device`, `wl_data_source`, `wl_data_offer`, `wl_compositor`,
//! `wl_shm` — is in the *core* protocol, which `wayland-client` generates
//! itself; sctk would add a seat/registry/shm framework on top of interfaces we
//! use directly anyway. **Neither crate is new to the build**: winit already
//! compiles `wayland-client` 0.31 with `wayland-backend/client_system`, so
//! naming it costs no new compilation, and this file is the protocol-faithful
//! half of what sctk would have wrapped.
//!
//! ## Where the serial comes from
//!
//! winit does not expose input serials, so this queue binds **its own
//! `wl_pointer`** on the same seat — and, since `c c` is a *keyboard* gesture,
//! its own `wl_keyboard` too. A client that has two pointer resources gets
//! every button event on both, with the *same* serial — serials are the
//! compositor's, not the object's — so the number recorded here is the number
//! the implicit grab is keyed by. Again, `smithay-clipboard` does the same
//! thing for `set_selection`, keyboard included; the keyboard here does nothing
//! else, and its keymap fd is dropped (and so closed) on arrival, because winit
//! is the one translating keys.
//!
//! The two serials are *not* interchangeable, so [`Serials`] keeps them apart:
//! `start_drag` may only name the serial of a button press that is still held
//! (a release ends the implicit grab), while `set_selection` wants the serial
//! of whatever input event triggered it — the key press that spelled `c c`,
//! most of the time.
//!
//! ## The selection
//!
//! A copy is a `wl_data_source` offering [`crate::clipboard::offer_mimes`]'s
//! types, handed to `wl_data_device.set_selection`; a `send` is answered from
//! this thread with the same [`hand_over`] and the same [`SEND_TIMEOUT`] that
//! serves a drag, and `cancelled` — another client took the clipboard — drops
//! the source and its bytes. Because the compositor only calls back while we
//! are alive, quitting hands the clipboard back; [`crate::clipboard`]'s header
//! argues that trade.
//!
//! Paste is the same machinery pointed the other way. Incoming
//! `wl_data_device.selection` offers used to be destroyed on arrival; now the
//! current one is kept with its mime list and mirrored to the window as
//! [`Event::Selection`], so `p` can decide what to ask for without a round
//! trip. The `receive` and the pipe read happen **here**, with
//! [`RECEIVE_TIMEOUT`], and the bytes go back over the event channel — a paste
//! must never park the paint loop on a pipe some other application is filling.
//! Pasting our *own* selection is served straight out of memory: asking the
//! compositor would have this thread waiting for bytes only this thread can
//! write.
//!
//! ## Sharing a socket with winit, safely
//!
//! One thread parks on the connection, and the obvious worry is that it steals
//! reads from under winit: libwayland distributes a read into *every* queue at
//! once, so whoever calls `wl_display_read_events` consumes the other's events
//! too. It cannot go wrong, and the reason is libwayland's reader protocol
//! rather than anything either side does:
//!
//! - `prepare_read` raises a **reader count**, and `read_events` only performs
//!   the actual socket read when that count falls to zero — every other reader
//!   *waits* for the last one to arrive. So a thread that has prepared a read
//!   is never bypassed; it is waited for.
//! - A reader that has not prepared cannot be bypassed either: its next
//!   `prepare_read` fails while its queue has anything pending, which is
//!   exactly the "somebody already queued events for me" signal.
//!   `calloop-wayland-source` — winit's — handles that in `before_sleep` by
//!   dispatching immediately instead of sleeping.
//!
//! So this thread must **not** ring [`Waker`] on every read. It did once, and
//! the result was a 60 Hz feedback loop: presenting a frame produces Wayland
//! traffic, this thread read it, woke the paint loop, and the paint loop
//! presented another frame. `DF_FRAME_LOG` names that as a wake per frame at
//! rest, which is PLAN §1's idle-cost rule broken by the thread that was meant
//! to be helping. The doorbell rings for exactly one reason: an [`Event`] the
//! app has to draw.
//!
//! ## What this deliberately does *not* do
//!
//! **An external target may not move our files.** The drag offers
//! `DndAction::Copy` and nothing else, so a target that asks for a move gets
//! copy — and no source of ours is ever deleted because some other program said
//! `dnd_finished` with `move`. A file manager that let an arbitrary application
//! silently unlink the file it was just handed would be a file manager you
//! could not trust with a drag; the sources stay, and the user deletes them
//! with `d` if that is what they meant (PLAN §5's "every destructive act is
//! undoable" reads the same way from this side).

// The three unsafe operations in this file are all FFI boundary crossings that
// have no safe spelling: adopting winit's `wl_display` and `wl_surface`
// pointers, and the three libc calls (`pipe2`, `poll`, `memfd_create`) that a
// thread parked on two file descriptors needs. Each one carries its own safety
// note. Allowed here rather than at the workspace level so this stays the one
// file in df-app where the compiler is not checking.
#![allow(unsafe_code)]

mod icon;

use std::ffi::c_void;
use std::io::{Read, Write};
use std::os::fd::{AsFd, AsRawFd, FromRawFd, OwnedFd};
use std::path::PathBuf;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crossbeam_channel::{Receiver, Sender};
use wayland_client::backend::{Backend, ObjectId};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::{
    wl_buffer::WlBuffer,
    wl_compositor::WlCompositor,
    wl_data_device::{self, WlDataDevice},
    wl_data_device_manager::{DndAction, WlDataDeviceManager},
    wl_data_offer::{self, WlDataOffer},
    wl_data_source::{self, WlDataSource},
    wl_keyboard::{self, WlKeyboard},
    wl_pointer::{self, WlPointer},
    wl_registry::WlRegistry,
    wl_seat::{self, WlSeat},
    wl_shm::{Format, WlShm},
    wl_shm_pool::WlShmPool,
    wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle, WEnum};

pub use icon::Rgba;

use crate::app::Waker;

/// How long a target has to take the bytes we are handing it before we give up.
///
/// Five seconds. A `send` writes into a pipe the *other* application is reading
/// from, and an application that asked for a drop and then stopped reading must
/// not be able to park this thread for the rest of the session — the pointer
/// would keep working and drags would silently stop.
const SEND_TIMEOUT: Duration = Duration::from_secs(5);

/// …and how long we will wait for a drop's bytes to arrive from *their* side.
const RECEIVE_TIMEOUT: Duration = Duration::from_secs(5);

/// What the pointer's own drag machinery learns from the compositor.
#[derive(Debug, Clone)]
pub enum Event {
    /// A drag — somebody else's, or ours come back — is over our window, at
    /// this surface-local point.
    Enter {
        at: (f32, f32),
        ours: bool,
    },
    Motion {
        at: (f32, f32),
    },
    /// It left without dropping.
    Leave,
    /// It was dropped, and here is what it was carrying. Empty when the offer
    /// held nothing this program can paste.
    Drop {
        paths: Vec<PathBuf>,
        ours: bool,
    },
    /// **Our** outgoing drag is over, whatever became of it.
    DragEnded,
    /// The clipboard changed hands: this is what it now offers, in the order
    /// the owner announced it. Empty when the selection was cleared.
    Selection {
        mimes: Vec<String>,
    },
    /// The compositor's answer to a [`Command::SetSelection`]. `false` means
    /// the copy did *not* happen and the window must say so.
    Copied {
        ok: bool,
    },
    /// The bytes a [`Command::Receive`] asked for. `None` when there was no
    /// offer left to ask, which is a clipboard that changed under the paste.
    ///
    /// `seq` is the number the request carried, echoed back. `p` then `P`
    /// inside one round trip asks twice, and the window is only waiting for the
    /// second one: without the number the first answer to arrive is applied
    /// with the *second* request's meaning, so a `p` that should have asked
    /// before overwriting overwrites.
    Pasted {
        seq: u64,
        bytes: Option<Vec<u8>>,
    },
}

/// What the paint thread asks the wayland thread to do.
enum Command {
    Drag {
        offers: Vec<(String, Vec<u8>)>,
        count: usize,
        card: Rgba,
        ink: Rgba,
        scale: i32,
    },
    /// Take the clipboard, offering one payload under `mimes`.
    ///
    /// One `bytes` for all of them rather than a list of pairs like a drag's:
    /// every copy this program makes is one thing under one or two names, and
    /// a `Vec<(String, Vec<u8>)>` would mean a second 50 MB in the channel to
    /// say so.
    SetSelection {
        mimes: Vec<String>,
        bytes: Vec<u8>,
    },
    /// Ask the current selection for `mime` and read the pipe. `seq` comes back
    /// on the [`Event::Pasted`] that answers it — see that variant.
    Receive {
        seq: u64,
        mime: String,
    },
    Exit,
}

/// The handle the app holds: a doorbell, two channels and a thread.
pub struct DataDevice {
    commands: Sender<Command>,
    events: Receiver<Event>,
    /// The write end of the self-pipe. The thread is asleep in `poll(2)` on the
    /// wayland fd *and* this one, so a command sent from the paint thread wakes
    /// it without a timeout to poll on (PLAN §1: no polling loops).
    bell: OwnedFd,
    /// Set once the thread has a seat, a manager and a data device — i.e. once
    /// there is a clipboard to take. The handle exists as soon as the thread
    /// spawns, but the thread bails out on a session that has none of that, and
    /// a command sent into a dead thread would be a copy that never happened
    /// and never said so. [`crate::clipboard`]'s `wl-copy` path is what the
    /// window falls back to while this is false.
    ready: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl DataDevice {
    /// Adopt winit's connection and start the thread.
    ///
    /// `None` — never an error — when there is no data device to be had: an X11
    /// session, a compositor without `wl_data_device_manager`, a registry that
    /// did not answer. PLAN §6's rule for missing libraries applies to missing
    /// protocols too: a machine without it loses drag-out and nothing else.
    ///
    /// # Safety
    ///
    /// `display` must be a live `*mut wl_display` and `surface` a live
    /// `*mut wl_proxy` for a `wl_surface`, both of which must outlive the
    /// returned handle. They come straight from winit's `raw-window-handle`
    /// implementation for the window this program owns, and the handle is
    /// dropped in `App::finish`, before the window is.
    pub unsafe fn start(
        display: NonNull<c_void>,
        surface: NonNull<c_void>,
        waker: Waker,
    ) -> Option<DataDevice> {
        // SAFETY: the caller's contract, one line up.
        let backend = unsafe { Backend::from_foreign_display(display.as_ptr().cast()) };
        let connection = Connection::from_backend(backend);
        // A pointer is not `Send`, and this one has to cross to the thread. It
        // travels as an integer with the same contract attached rather than
        // through a newtype whose `unsafe impl Send` would be the same promise
        // written twice.
        let origin = surface.as_ptr() as usize;

        let (bell_read, bell) = pipe()?;
        let (commands, rx) = crossbeam_channel::unbounded();
        let (tx, events) = crossbeam_channel::unbounded();
        let ready = Arc::new(AtomicBool::new(false));
        let mine = ready.clone();
        let thread = std::thread::Builder::new()
            .name("df-wayland".to_string())
            .spawn(move || run(connection, origin, bell_read, rx, tx, waker, mine))
            .ok()?;
        Some(DataDevice {
            commands,
            events,
            bell,
            ready,
            thread: Some(thread),
        })
    }

    /// Is there a data device to talk to? See the `ready` field above for what
    /// the window does when there is not.
    pub fn ready(&self) -> bool {
        self.ready.load(Ordering::Relaxed)
    }

    /// Take the clipboard, offering `bytes` under every name in `mimes`.
    ///
    /// Answered later with [`Event::Copied`] — the toast waits for it, because
    /// "Copied" before the compositor has accepted the selection is a claim
    /// this program cannot make yet. `false` means the command was not even
    /// accepted, and no `Copied` is coming.
    #[must_use]
    pub fn set_selection(&self, mimes: Vec<String>, bytes: Vec<u8>) -> bool {
        self.send(Command::SetSelection { mimes, bytes })
    }

    /// Ask the clipboard for `mime`. Answered with [`Event::Pasted`] carrying
    /// `seq`. `false` means no answer is coming.
    #[must_use]
    pub fn receive(&self, seq: u64, mime: String) -> bool {
        self.send(Command::Receive { seq, mime })
    }

    /// Start a drag out of the window, offering `offers` and carrying an icon
    /// drawn for `count` files. `false` means the thread is gone and no drag
    /// will happen — the caller has a ghost to spring home.
    #[must_use]
    pub fn drag(
        &self,
        offers: Vec<(String, Vec<u8>)>,
        count: usize,
        card: Rgba,
        ink: Rgba,
        scale: i32,
    ) -> bool {
        self.send(Command::Drag {
            offers,
            count,
            card,
            ink,
            scale,
        })
    }

    /// Everything that has happened since the last frame.
    pub fn poll(&self) -> Vec<Event> {
        self.events.try_iter().collect()
    }

    /// Hand a command to the thread. `false` means the receiving end is gone —
    /// the thread has exited — and the command will never be acted on, which is
    /// a fact the caller has to be told rather than a thing to swallow: every
    /// one of these is answered by an event the window is waiting for.
    fn send(&self, command: Command) -> bool {
        if self.commands.send(command).is_err() {
            return false;
        }
        ring(&self.bell);
        true
    }
}

impl Drop for DataDevice {
    fn drop(&mut self) {
        let _ = self.send(Command::Exit);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// A close-on-exec, non-blocking pipe: `(read, write)`.
fn pipe() -> Option<(OwnedFd, OwnedFd)> {
    let mut fds = [0i32; 2];
    // SAFETY: `fds` is two `i32`s, which is what `pipe2` writes.
    let ok = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } == 0;
    if !ok {
        return None;
    }
    // SAFETY: both descriptors were just created by `pipe2` and are owned here.
    Some(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

/// Wake whoever is polling the read end.
fn ring(bell: &OwnedFd) {
    let byte = [0u8; 1];
    // SAFETY: a one-byte write to a descriptor this process owns. A full pipe
    // (`EAGAIN`) is not a failure — the reader has not drained the last ring
    // yet, so it is already about to wake.
    unsafe {
        libc::write(bell.as_raw_fd(), byte.as_ptr().cast(), 1);
    }
}

/// Wait for either descriptor, or for `timeout`. Returns `(a, b)` readiness.
fn wait(a: i32, b: i32, timeout: Option<Duration>) -> (bool, bool) {
    let mut fds = [
        libc::pollfd {
            fd: a,
            events: libc::POLLIN,
            revents: 0,
        },
        libc::pollfd {
            fd: b,
            events: libc::POLLIN,
            revents: 0,
        },
    ];
    let ms = timeout.map_or(-1, |d| d.as_millis().min(i32::MAX as u128) as i32);
    // SAFETY: `fds` is two initialised `pollfd`s and the count matches.
    let n = unsafe { libc::poll(fds.as_mut_ptr(), 2, ms) };
    if n <= 0 {
        return (false, false);
    }
    (fds[0].revents != 0, fds[1].revents != 0)
}

/// Drain the doorbell so one ring is one wake-up.
fn drain(bell: i32) {
    let mut buf = [0u8; 64];
    loop {
        // SAFETY: a read into a stack buffer of its own length, from a
        // non-blocking descriptor this process owns.
        let n = unsafe { libc::read(bell, buf.as_mut_ptr().cast(), buf.len()) };
        if n <= 0 {
            return;
        }
    }
}

// ── The thread ──────────────────────────────────────────────────────────────

/// The thread. `ready` is *its* flag, not the handle's: it is true only while
/// this function is running with a data device in hand, and the guard below
/// puts it back to false on every way out — including the ones in the middle of
/// the loop, where a dispatch error ends the thread with the window still
/// holding a handle that used to say "the clipboard is native".
fn run(
    connection: Connection,
    origin: usize,
    bell: OwnedFd,
    commands: Receiver<Command>,
    events: Sender<Event>,
    waker: Waker,
    ready: Arc<AtomicBool>,
) {
    /// Clears `ready` however this thread leaves — an early bail, a broken
    /// socket, `Exit`, or a panic. A handle that goes on claiming a live data
    /// device sends copies into a channel nobody reads, and the window's
    /// `wl-copy` fallback never gets its turn.
    struct NotReadyOnExit(Arc<AtomicBool>);
    impl Drop for NotReadyOnExit {
        fn drop(&mut self) {
            self.0.store(false, Ordering::Relaxed);
        }
    }
    let guard = NotReadyOnExit(ready);
    let ready = &guard.0;

    let Ok((globals, mut queue)) = registry_queue_init::<State>(&connection) else {
        log::info!("wayland: no registry — drag and drop and the clipboard are off");
        return;
    };
    let qh = queue.handle();
    // SAFETY: the pointer came from winit's window handle and the window
    // outlives this thread (see `DataDevice::start`).
    let origin = unsafe { ObjectId::from_ptr(WlSurface::interface(), origin as *mut _) }
        .ok()
        .and_then(|id| WlSurface::from_id(&connection, id).ok());
    if origin.is_none() {
        // Only *drag-out* names a surface; the clipboard does not, so this is
        // one feature lost rather than the thread's reason to exist.
        log::info!("wayland: the window is not a wl_surface — drag-out is off");
    }

    let manager = match globals.bind::<WlDataDeviceManager, _, _>(&qh, 1..=3, ()) {
        Ok(manager) => manager,
        Err(e) => {
            log::info!("wayland: no data device manager ({e}) — drag, drop and clipboard are off");
            return;
        }
    };
    let seat = match globals.bind::<WlSeat, _, _>(&qh, 1..=7, ()) {
        Ok(seat) => seat,
        Err(e) => {
            log::info!("wayland: no seat ({e}) — drag, drop and clipboard are off");
            return;
        }
    };
    let device = manager.get_data_device(&seat, &qh, ());

    let mut state = State {
        connection: connection.clone(),
        origin,
        // Both are optional: without them the drag has no icon, which is the
        // documented degradation, not a failure.
        compositor: globals.bind::<WlCompositor, _, _>(&qh, 1..=4, ()).ok(),
        shm: globals.bind::<WlShm, _, _>(&qh, 1..=1, ()).ok(),
        manager,
        device,
        events,
        serials: Serials::default(),
        offers: Vec::new(),
        incoming: None,
        source: None,
        payload: Vec::new(),
        selection: None,
        selection_mimes: Vec::new(),
        selection_bytes: Vec::new(),
        selection_offer: None,
        icon: None,
        pending: false,
        exit: false,
    };
    // The seat's capabilities arrive on the first round trip; the pointer and
    // keyboard this thread's serials come from are created when they do. The
    // compositor also hands a focused client its current selection here, which
    // is what makes the mirror right before the first `p`.
    let _ = queue.roundtrip(&mut state);
    ready.store(true, Ordering::Relaxed);
    log::info!(
        "wayland: data device ready (manager v{}, icon {}) — clipboard is native",
        state.manager.version(),
        if state.shm.is_some() && state.compositor.is_some() {
            "on"
        } else {
            "off"
        }
    );

    let wayland_fd = connection.as_fd().as_raw_fd();
    let bell_fd = bell.as_raw_fd();
    loop {
        for command in commands.try_iter() {
            // **`Exit` ends the drain, it does not merely note it.** A
            // `Receive` queued behind it goes through `take_paste`, which can
            // sit on a pipe for `RECEIVE_TIMEOUT` waiting for an application
            // that is also shutting down — so a quit with two pastes still in
            // the channel took ten seconds to be honoured, with the window
            // already gone. Nothing queued after a quit has anywhere to go: the
            // window that asked for it is not there to be told the answer.
            if state.exit {
                break;
            }
            match command {
                Command::Exit => state.exit = true,
                Command::Drag {
                    offers,
                    count,
                    card,
                    ink,
                    scale,
                } => state.start_drag(&qh, offers, count, card, ink, scale),
                Command::SetSelection { mimes, bytes } => {
                    // The toast waits on this answer, so the round trip is
                    // here rather than a flush: it is the compositor saying it
                    // has the selection, and a `cancelled` that arrives inside
                    // it (somebody else was faster) clears the source again.
                    let asked = state.set_selection(&qh, mimes, bytes);
                    let ok = asked && queue.roundtrip(&mut state).is_ok();
                    state.tell(Event::Copied {
                        ok: ok && state.selection.is_some(),
                    });
                }
                Command::Receive { seq, mime } => state.take_paste(seq, mime),
            }
        }
        if state.exit {
            break;
        }
        if queue.flush().is_err() || queue.dispatch_pending(&mut state).is_err() {
            break;
        }
        if state.pending {
            state.pending = false;
            waker.wake();
        }
        // No guard means libwayland already has events queued for *somebody* —
        // possibly for us — so go round and dispatch them rather than sleeping
        // on a socket that has already been read.
        let Some(guard) = connection.prepare_read() else {
            continue;
        };
        let (wayland, doorbell) = wait(wayland_fd, bell_fd, None);
        if doorbell {
            drain(bell_fd);
        }
        if wayland {
            if guard.read().is_err() {
                break;
            }
        } else {
            drop(guard);
        }
        // …and only if that read turned into something the window has to draw.
        // Never for traffic alone: see the module header's last paragraph.
        if state.pending {
            state.pending = false;
            waker.wake();
        }
    }
    // **Before the tail, not with the guard at the end of it.** The tail below
    // destroys the selection and flushes, and the window is running frames the
    // whole time — so for as long as it took, `ready()` went on saying "the
    // clipboard is native" about a thread that had already stopped reading its
    // channel, and a `y` landing in that window sent a copy into a queue with
    // nobody behind it. The guard stays for every other way out of this
    // function; this is the ordinary one, made honest at the first instruction
    // after the loop.
    ready.store(false, Ordering::Relaxed);
    // Whatever is still in flight is cancelled by the objects going away, and
    // the compositor treats a destroyed source as a cancelled drag. The
    // selection goes the same way, and deliberately: a source whose thread has
    // stopped dispatching would leave the next application to paste waiting on
    // a pipe nobody is going to write into.
    if let Some(selection) = state.selection.take() {
        selection.destroy();
    }
    state.clear_icon();
    let _ = connection.flush();
}

/// The two serials this thread harvests, kept apart because the compositor
/// keeps them apart.
///
/// `start_drag` may only name the serial of a pointer press whose implicit
/// grab is *still held* — a release ends the grab, so remembering its serial
/// would mean sometimes starting a drag the compositor refuses. `set_selection`
/// wants something else entirely: the serial of whatever input event the user
/// meant by it, which for `c c` is a key press and never a button at all.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Serials {
    grab: Option<u32>,
    latest: Option<u32>,
}

impl Serials {
    fn button(&mut self, serial: u32, pressed: bool) {
        if pressed {
            self.grab = Some(serial);
        }
        self.latest = Some(serial);
    }

    /// A key press or release, or the keyboard focus arriving. All three are
    /// serials `set_selection` may be made with, and the focus one matters:
    /// it is the only serial a window that has been clicked into but not typed
    /// in yet has.
    fn key(&mut self, serial: u32) {
        self.latest = Some(serial);
    }

    /// The implicit grab a `start_drag` names.
    fn grab(&self) -> Option<u32> {
        self.grab
    }

    /// The last input serial of any kind — what `set_selection` names.
    fn latest(&self) -> Option<u32> {
        self.latest
    }
}

/// Everything the thread holds between events.
struct State {
    connection: Connection,
    /// winit's window surface — the drag's origin, and the only surface a
    /// `start_drag` of ours may name. `None` only in the odd session where the
    /// handle did not resolve, which costs drag-out and not the clipboard.
    origin: Option<WlSurface>,
    compositor: Option<WlCompositor>,
    shm: Option<WlShm>,
    manager: WlDataDeviceManager,
    device: WlDataDevice,
    events: Sender<Event>,
    /// The input serials a `start_drag` and a `set_selection` are made with.
    serials: Serials,
    /// What each live offer says it can provide.
    offers: Vec<(ObjectId, Vec<String>)>,
    /// The drag currently over our window, and the mime we accepted from it.
    incoming: Option<(WlDataOffer, Option<String>, bool)>,
    source: Option<WlDataSource>,
    /// The bytes our own source will hand over, by mime.
    payload: Vec<(String, Vec<u8>)>,
    /// The clipboard source, while this program owns the selection.
    selection: Option<WlDataSource>,
    /// What that source announced, and the one payload all of it names.
    selection_mimes: Vec<String>,
    selection_bytes: Vec<u8>,
    /// The selection somebody *else* is offering — kept alive, because an
    /// offer that has been destroyed is an offer nothing can be received from.
    selection_offer: Option<WlDataOffer>,
    icon: Option<(WlSurface, WlBuffer, WlShmPool)>,
    /// Something has been put on the channel that the window has not been told
    /// about yet.
    pending: bool,
    exit: bool,
}

impl State {
    fn tell(&mut self, event: Event) {
        let _ = self.events.send(event);
        self.pending = true;
    }

    /// The mimes an offer has announced.
    fn mimes(&self, offer: &WlDataOffer) -> Vec<String> {
        self.offers
            .iter()
            .find(|(id, _)| *id == offer.id())
            .map(|(_, mimes)| mimes.clone())
            .unwrap_or_default()
    }

    fn start_drag(
        &mut self,
        qh: &QueueHandle<State>,
        offers: Vec<(String, Vec<u8>)>,
        count: usize,
        card: Rgba,
        ink: Rgba,
        scale: i32,
    ) {
        let (Some(serial), Some(origin)) = (self.serials.grab(), self.origin.clone()) else {
            // No button press has been seen on this queue, so there is no
            // implicit grab to name and the compositor would refuse.
            log::debug!("wayland: no grab serial — drag-out skipped");
            self.tell(Event::DragEnded);
            return;
        };
        // VERIFY-LIVE: drag a row out of the window and drop it on another
        // application — a Chrome upload box, Zed's editor pane, a GIMP canvas.
        // The target should see `text/uri-list`; a terminal should see the
        // plain path. Nothing about the serial, the origin surface or the
        // compositor's acceptance of this request can be exercised without a
        // real compositor and a real second application. `RUST_LOG=debug`
        // reports a refused drag as "no grab serial".
        self.clear_icon();
        let source = self.manager.create_data_source(qh, ());
        for (mime, _) in &offers {
            source.offer(mime.clone());
        }
        if self.manager.version() >= 3 {
            // Copy and nothing else: see the module header's last section.
            source.set_actions(DndAction::Copy);
        }
        self.payload = offers;
        let icon = self.make_icon(qh, count, card, ink, scale);
        self.device
            .start_drag(Some(&source), &origin, icon.as_ref(), serial);
        if let Some(surface) = icon {
            // The role is set by `start_drag`, so the buffer goes on after it.
            if let Some((buffer, _)) = self.icon.as_ref().map(|(_, b, p)| (b, p)) {
                // The offset puts the card's grab point under the cursor
                // rather than its top-left corner, so the icon does not jump as
                // the drag crosses the window's edge and changes hands.
                surface.attach(Some(buffer), -icon::HOTSPOT.0, -icon::HOTSPOT.1);
                surface.damage_buffer(0, 0, i32::MAX, i32::MAX);
            }
            surface.commit();
        }
        self.source = Some(source);
        let _ = self.connection.flush();
    }

    /// Paint the drag icon into a shm buffer and hand back its surface.
    ///
    /// `None` at the first sign of trouble: an icon is a courtesy, and a drag
    /// with the plain cursor is a working drag.
    fn make_icon(
        &mut self,
        qh: &QueueHandle<State>,
        count: usize,
        card: Rgba,
        ink: Rgba,
        scale: i32,
    ) -> Option<WlSurface> {
        let compositor = self.compositor.as_ref()?;
        let shm = self.shm.as_ref()?;
        let scale = scale.clamp(1, 4);
        let image = icon::draw(count, card, ink);
        let stride = image.width * 4;
        let size = (stride * image.height) as i32;

        // SAFETY: a `memfd` with a static, NUL-terminated name.
        let fd = unsafe { libc::memfd_create(c"df-drag-icon".as_ptr(), libc::MFD_CLOEXEC) };
        if fd < 0 {
            return None;
        }
        // SAFETY: `fd` was just created and is owned here from now on.
        let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
        file.write_all(&image.pixels).ok()?;
        file.flush().ok()?;

        let pool = shm.create_pool(file.as_fd(), size, qh, ());
        let buffer = pool.create_buffer(
            0,
            image.width as i32,
            image.height as i32,
            stride as i32,
            Format::Argb8888,
            qh,
            (),
        );
        let surface = compositor.create_surface(qh, ());
        if surface.version() >= 3 {
            // The buffer is drawn at device resolution, so a scaled output has
            // to be told that — otherwise the icon is a quarter of the size it
            // should be on a 2× display, which is the sort of thing that only
            // shows up on somebody else's machine.
            surface.set_buffer_scale(scale);
        }
        self.icon = Some((surface.clone(), buffer, pool));
        Some(surface)
    }

    fn clear_icon(&mut self) {
        if let Some((surface, buffer, pool)) = self.icon.take() {
            buffer.destroy();
            pool.destroy();
            surface.destroy();
        }
    }

    /// Our drag is over. Tidy up and say so once.
    fn drag_ended(&mut self) {
        if let Some(source) = self.source.take() {
            source.destroy();
        }
        self.payload.clear();
        self.clear_icon();
        self.tell(Event::DragEnded);
    }

    /// Read what a dropped offer is carrying, on this thread, and hand the app
    /// paths rather than bytes.
    fn take_drop(&mut self) {
        let Some((offer, mime, ours)) = self.incoming.take() else {
            return;
        };
        let paths = match &mime {
            Some(mime) => match receive(&self.connection, &offer, mime) {
                Some(bytes) => crate::dnd::paths_from(mime, &bytes),
                None => Vec::new(),
            },
            None => Vec::new(),
        };
        if offer.version() >= 3 {
            offer.finish();
        }
        offer.destroy();
        self.offers.retain(|(id, _)| *id != offer.id());
        let _ = self.connection.flush();
        self.tell(Event::Drop { paths, ours });
    }

    // ── The selection (PLAN §7.4) ───────────────────────────────────────────

    /// Take the clipboard. `false` when there was no serial to take it with,
    /// which is the one refusal we can see coming.
    ///
    // VERIFY-LIVE: `c c` in delightfile, then paste into a terminal, a browser
    // address bar and a GTK app; `Y` on an image and paste into GIMP; `Y` on a
    // multi-file selection and paste into Nautilus. None of the compositor's
    // half of this — the serial being accepted, another client's paste
    // arriving as a `send` — exists without a real compositor and a real
    // second application. `RUST_LOG=info` names the path each copy took.
    fn set_selection(
        &mut self,
        qh: &QueueHandle<State>,
        mimes: Vec<String>,
        bytes: Vec<u8>,
    ) -> bool {
        let Some(serial) = self.serials.latest() else {
            log::warn!("wayland: no input serial — the compositor would refuse set_selection");
            return false;
        };
        // Out first, so the `cancelled` the compositor sends the old source
        // when the new one takes over arrives for an object this thread has
        // already stopped believing in.
        let previous = self.selection.take();
        let source = self.manager.create_data_source(qh, ());
        for mime in &mimes {
            source.offer(mime.clone());
        }
        self.device.set_selection(Some(&source), serial);
        self.selection = Some(source);
        self.selection_mimes = mimes;
        self.selection_bytes = bytes;
        if let Some(previous) = previous {
            previous.destroy();
        }
        true
    }

    /// Our own selection is the one being pasted, and these mimes are ours.
    ///
    /// Both halves matter. Owning a source is not enough on its own — for the
    /// instant between another client taking the clipboard and our `cancelled`
    /// arriving, both could look true — so the offer's own mime list has to be
    /// the list we announced as well.
    fn owns_selection(&self, mime: &str) -> bool {
        self.selection.is_some()
            && self.selection_mimes.iter().any(|known| known == mime)
            && self
                .selection_offer
                .as_ref()
                .is_some_and(|offer| self.mimes(offer) == self.selection_mimes)
    }

    /// The clipboard changed hands. Keep the offer and tell the window what is
    /// on it.
    fn take_selection(&mut self, offer: Option<WlDataOffer>) {
        if let Some(previous) = self.selection_offer.take() {
            self.offers.retain(|(id, _)| *id != previous.id());
            previous.destroy();
        }
        let mimes = offer.as_ref().map(|o| self.mimes(o)).unwrap_or_default();
        self.selection_offer = offer;
        self.tell(Event::Selection { mimes });
    }

    /// Read the clipboard, here, and send the bytes back to the window.
    fn take_paste(&mut self, seq: u64, mime: String) {
        if self.owns_selection(&mime) {
            // Our own copy. Going through the compositor would have this
            // thread blocked on a pipe that only this thread can write into —
            // the `send` arrives as an event nobody is left to dispatch.
            let bytes = self.selection_bytes.clone();
            self.tell(Event::Pasted {
                seq,
                bytes: Some(bytes),
            });
            return;
        }
        let bytes = self
            .selection_offer
            .as_ref()
            .and_then(|offer| receive(&self.connection, offer, &mime));
        self.tell(Event::Pasted { seq, bytes });
    }
}

/// Ask an offer for one mime and read the pipe it writes into.
fn receive(connection: &Connection, offer: &WlDataOffer, mime: &str) -> Option<Vec<u8>> {
    let (read, write) = pipe()?;
    offer.receive(mime.to_string(), write.as_fd());
    connection.flush().ok()?;
    // Our copy of the write end has to go, or the read below never sees EOF:
    // the pipe stays open as long as *anybody* holds a writer, and that would
    // be us.
    drop(write);

    let fd = read.as_raw_fd();
    let mut file = std::fs::File::from(read);
    let mut out = Vec::new();
    let deadline = std::time::Instant::now() + RECEIVE_TIMEOUT;
    let mut chunk = [0u8; 4096];
    loop {
        match file.read(&mut chunk) {
            Ok(0) => return Some(out),
            Ok(n) => out.extend_from_slice(&chunk[..n]),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                let left = deadline.saturating_duration_since(std::time::Instant::now());
                if left.is_zero() {
                    log::warn!("wayland: a drop's data never arrived");
                    return Some(out);
                }
                wait(fd, fd, Some(left));
            }
            Err(_) => return Some(out),
        }
    }
}

/// Write our payload into the pipe a target handed us.
fn hand_over(fd: OwnedFd, bytes: &[u8]) {
    let mut file = std::fs::File::from(fd);
    let mut sent = 0;
    let deadline = std::time::Instant::now() + SEND_TIMEOUT;
    while sent < bytes.len() {
        match file.write(&bytes[sent..]) {
            Ok(0) => return,
            Ok(n) => sent += n,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                let left = deadline.saturating_duration_since(std::time::Instant::now());
                if left.is_zero() {
                    log::warn!("wayland: a drop target stopped reading");
                    return;
                }
                // `poll` for writability is the same call with the other flag;
                // waiting for readability on a pipe we are writing to would
                // never fire, so this waits the remaining time in slices.
                std::thread::sleep(Duration::from_millis(2).min(left));
            }
            // A target that went away mid-transfer is not an error worth a word
            // to the user: the drag simply did not happen.
            Err(_) => return,
        }
    }
    let _ = file.flush();
}

// ── Dispatch ────────────────────────────────────────────────────────────────

impl Dispatch<WlRegistry, GlobalListContents> for State {
    fn event(
        _: &mut Self,
        _: &WlRegistry,
        _: <WlRegistry as Proxy>::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // Globals coming and going do not change what a drag needs: the seat
        // and the manager are bound once, and a compositor that withdrew them
        // mid-session is taking the window with them.
    }
}

impl Dispatch<WlSeat, ()> for State {
    fn event(
        state: &mut Self,
        seat: &WlSeat,
        event: wl_seat::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        if let wl_seat::Event::Capabilities {
            capabilities: WEnum::Value(capabilities),
        } = event
        {
            // The whole reason this thread has input objects at all: serials.
            // The pointer's press serial is the drag's implicit grab; the
            // keyboard's is what a `set_selection` from `c c` is made with.
            let _ = state;
            if capabilities.contains(wl_seat::Capability::Pointer) {
                seat.get_pointer(qh, ());
            }
            if capabilities.contains(wl_seat::Capability::Keyboard) {
                seat.get_keyboard(qh, ());
            }
        }
    }
}

impl Dispatch<WlKeyboard, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlKeyboard,
        event: wl_keyboard::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            // Presses *and* releases: unlike a drag's grab there is nothing to
            // still be holding, so the last input serial is simply the last
            // one, and a `c c` released before the frame runs must not lose it.
            wl_keyboard::Event::Key { serial, .. } => state.serials.key(serial),
            wl_keyboard::Event::Enter { serial, .. } => state.serials.key(serial),
            // The keymap is winit's business, not ours — this keyboard exists
            // for its serials. The fd arrives owned and is dropped here, which
            // closes it; leaking one per focus change would be a descriptor
            // leak in the longest-running thread in the program.
            wl_keyboard::Event::Keymap { fd, .. } => drop(fd),
            _ => {}
        }
    }
}

impl Dispatch<WlPointer, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlPointer,
        event: wl_pointer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // Both ends of a click, and [`Serials`] files them differently: only a
        // press is an implicit grab a `start_drag` may name, while either is
        // an input serial a `set_selection` may be made with.
        if let wl_pointer::Event::Button {
            serial,
            state: WEnum::Value(button),
            ..
        } = event
        {
            state
                .serials
                .button(serial, button == wl_pointer::ButtonState::Pressed);
        }
    }
}

impl Dispatch<WlDataDeviceManager, ()> for State {
    fn event(
        _: &mut Self,
        _: &WlDataDeviceManager,
        _: <WlDataDeviceManager as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<WlDataDevice, ()> for State {
    fn event(
        state: &mut Self,
        _: &WlDataDevice,
        event: wl_data_device::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            // The offer arrives *before* the enter that uses it, with its mime
            // list following on its own events — so it is filed by id and read
            // when the enter says which drag it belongs to.
            wl_data_device::Event::DataOffer { id } => {
                state.offers.push((id.id(), Vec::new()));
            }
            // VERIFY-LIVE: drag files *in* from another file manager, from
            // Firefox's downloads shelf, or from a terminal that supports it.
            // The window's edge should ring, the row or pane under the pointer
            // should light up as the pointer moves, and letting go should
            // paste — conflict dialog, undo toast and all.
            wl_data_device::Event::Enter {
                serial, x, y, id, ..
            } => {
                let Some(offer) = id else { return };
                let mimes = state.mimes(&offer);
                // *This* window's own drag, come back through the compositor —
                // not merely some delightfile's, which with one process per
                // window is an ordinary external drag (see
                // [`crate::dnd::is_ours`]).
                let ours = crate::dnd::is_ours(&mimes);
                let wanted = crate::dnd::wanted_mime(&mimes);
                // Accepting is what turns the cursor into a "yes" in the
                // source application; a `None` accept is the honest answer for
                // a drag carrying nothing we can paste.
                offer.accept(serial, wanted.clone());
                if offer.version() >= 3 {
                    offer.set_actions(DndAction::Copy, DndAction::Copy);
                }
                state.incoming = Some((offer, wanted, ours));
                state.tell(Event::Enter {
                    at: (x as f32, y as f32),
                    ours,
                });
            }
            wl_data_device::Event::Motion { x, y, .. } => {
                state.tell(Event::Motion {
                    at: (x as f32, y as f32),
                });
            }
            wl_data_device::Event::Leave => {
                if let Some((offer, _, _)) = state.incoming.take() {
                    state.offers.retain(|(id, _)| *id != offer.id());
                    offer.destroy();
                }
                state.tell(Event::Leave);
            }
            wl_data_device::Event::Drop => state.take_drop(),
            // The clipboard, ours or somebody else's. This used to be
            // destroyed on sight and pasting shelled out to `wl-paste`; the
            // offer is now kept, because it is the only thing a `receive` can
            // be asked of (see the module header's "The selection").
            //
            // VERIFY-LIVE: copy in another application, then `p` here with
            // nothing yanked — a `text/uri-list` from a file manager should
            // paste files, an image from a screenshot tool should land a PNG,
            // text should land a `.txt`. `RUST_LOG=info` shows the path taken.
            wl_data_device::Event::Selection { id } => state.take_selection(id),
            _ => {}
        }
    }

    wayland_client::event_created_child!(State, WlDataDevice, [
        wl_data_device::EVT_DATA_OFFER_OPCODE => (WlDataOffer, ()),
    ]);
}

impl Dispatch<WlDataOffer, ()> for State {
    fn event(
        state: &mut Self,
        offer: &WlDataOffer,
        event: wl_data_offer::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_data_offer::Event::Offer { mime_type } = event {
            if let Some((_, mimes)) = state.offers.iter_mut().find(|(id, _)| *id == offer.id()) {
                mimes.push(mime_type);
            }
        }
    }
}

impl Dispatch<WlDataSource, ()> for State {
    fn event(
        state: &mut Self,
        source: &WlDataSource,
        event: wl_data_source::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // The clipboard's source, which outlives every drag and answers a
        // `send` for as long as this program owns the selection.
        if state.selection.as_ref() == Some(source) {
            match event {
                wl_data_source::Event::Send { mime_type, fd } => {
                    if state.selection_mimes.contains(&mime_type) {
                        // Taken out and put back rather than cloned: this is a
                        // file's contents, up to `clipboard::SIZE_CAP` of it,
                        // and every paste from every application lands here.
                        let bytes = std::mem::take(&mut state.selection_bytes);
                        hand_over(fd, &bytes);
                        state.selection_bytes = bytes;
                    } else {
                        // Asked for something we never offered: close the pipe
                        // empty rather than leave the other side waiting.
                        drop(fd);
                    }
                }
                // Another client took the clipboard. Ours is over — the source
                // may not be used again, and the bytes it was holding are no
                // longer anybody's business.
                wl_data_source::Event::Cancelled => {
                    if let Some(source) = state.selection.take() {
                        source.destroy();
                    }
                    state.selection_mimes.clear();
                    state.selection_bytes = Vec::new();
                    log::info!("wayland: another client took the clipboard");
                }
                _ => {}
            }
            return;
        }
        // A source that is not the live one is a drag that has already ended;
        // its events are noise from an object on its way out.
        if state.source.as_ref() != Some(source) {
            return;
        }
        match event {
            wl_data_source::Event::Send { mime_type, fd } => {
                let bytes = state
                    .payload
                    .iter()
                    .find(|(mime, _)| *mime == mime_type)
                    .map(|(_, bytes)| bytes.clone())
                    .unwrap_or_default();
                hand_over(fd, &bytes);
            }
            // Either ending. `Cancelled` is a drop on nothing (or a second drag
            // starting); `DndFinished` is a target that took it.
            wl_data_source::Event::Cancelled | wl_data_source::Event::DndFinished => {
                state.drag_ended();
            }
            _ => {}
        }
    }
}

wayland_client::delegate_noop!(State: ignore WlCompositor);
wayland_client::delegate_noop!(State: ignore WlSurface);
wayland_client::delegate_noop!(State: ignore WlShm);
wayland_client::delegate_noop!(State: ignore WlShmPool);
wayland_client::delegate_noop!(State: ignore WlBuffer);

#[cfg(test)]
mod tests {
    use super::*;

    /// The two serials, and the rule that keeps them apart.
    #[test]
    fn a_release_ends_the_grab_but_is_still_an_input_serial() {
        let mut serials = Serials::default();
        assert_eq!(serials.grab(), None);
        assert_eq!(serials.latest(), None);

        serials.button(7, true);
        assert_eq!(serials.grab(), Some(7));
        assert_eq!(serials.latest(), Some(7));

        // The release is not a grab — a `start_drag` naming it would be
        // refused — but it *is* the most recent input event on the seat.
        serials.button(8, false);
        assert_eq!(serials.grab(), Some(7));
        assert_eq!(serials.latest(), Some(8));
    }

    /// A keyboard-triggered copy has a serial even though nothing was clicked.
    #[test]
    fn a_key_press_gives_a_selection_serial_and_no_grab() {
        let mut serials = Serials::default();
        serials.key(12);
        assert_eq!(serials.latest(), Some(12));
        assert_eq!(serials.grab(), None, "a key is not an implicit grab");

        // …and typing after a click does not make the old press look fresh.
        let mut serials = Serials::default();
        serials.button(3, true);
        serials.key(4);
        assert_eq!(serials.grab(), Some(3));
        assert_eq!(serials.latest(), Some(4));
    }
}
