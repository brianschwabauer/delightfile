//! The desktop device on macOS: the pasteboard, answered at once.
//!
//! The window's clipboard is written against the Wayland data device's
//! shape — a copy is offered and answered later, a paste asked for and
//! answered later, a mirror of what the clipboard holds kept up to date
//! (`platform::desktop`'s essay). The pasteboard is synchronous, so this
//! device does each thing when it is asked, through the same functions the
//! window's fallback calls ([`super::clipboard`]), and queues the answer for
//! the next [`Desktop::poll`]: the window sees the events it sees on Linux,
//! one frame later, and cannot tell the difference.
//!
//! Nothing tells a program when the pasteboard changes hands; its
//! `changeCount` moves, and that is all. So [`Desktop::poll`] reads the count
//! (at most every [`MIRROR_EVERY`]) and, when it has moved, answers
//! [`Event::Selection`] with the types now on offer, which is the mirror a
//! paste chooses its type from.
//!
//! It also says where the pointer is when winit reports a drop without a
//! position ([`pointer_position`]), which takes two AppKit calls on the
//! window's own view.
//!
//! ## Dragging out
//!
//! A drag leaves the window as an `NSDraggingSession` begun on winit's own
//! view ([`Desktop::drag`]). AppKit wants that begun inside the mouse event
//! that is dragging, so the window hands a drag off from its `CursorMoved`
//! arm on macOS ([`HANDS_OFF_ON_CURSOR_MOVED`]), where the application's
//! current event is the `mouseDragged` that moved the pointer out, rather
//! than from the frame after it. What the drag carries — a pasteboard item
//! per file — and the picture that follows the pointer are
//! `crate::platform::dragout`'s. The session's source is an object of this
//! file's own class ([`DragSource`]): it offers a copy, whatever the hand
//! holds, since nothing leaves the window as a move, and it queues
//! [`Event::DragEnded`] when the drag is over, dropped or not, so the window
//! lets go of its files as it does when a Wayland drag ends.
//!
//! The picture is drawn at one pixel to the point, as it is for Wayland at
//! scale 1; on a Retina screen AppKit scales it up.
//!
//! **Unsafe.** The AppKit calls are `unsafe` in objc2-app-kit 0.2's
//! generated bindings, the drag source is an Objective-C class of our own
//! (`declare_class!`), and the picture's pixels are copied into a buffer
//! AppKit allocates. Each is sound for the reason written at it; the view
//! is winit's and outlives the device, which the window drops first.

#![allow(unsafe_code)] // winit's NSView and NSWindow, the drag session and its source, through objc2; each call says why it holds.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::{Duration, Instant};

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{declare_class, msg_send_id, mutability, ClassType, DeclaredClass};
use objc2_app_kit::{
    NSApplication, NSBitmapFormat, NSBitmapImageRep, NSDeviceRGBColorSpace, NSDragOperation,
    NSDraggingContext, NSDraggingItem, NSDraggingSession, NSDraggingSource, NSImage,
    NSPasteboardItem, NSView,
};
use objc2_foundation::{
    MainThreadMarker, NSArray, NSData, NSInteger, NSPoint, NSRect, NSSize, NSString, NSURL,
};
use winit::event_loop::ActiveEventLoop;
use winit::window::Window;

use crate::app::Waker;
use crate::platform::desktop::{Event, PasteFailure};
use crate::platform::dragout;
use crate::platform::icon::Rgba;
use crate::platform::pasteboard;

/// Whether the window hands a drag off from its `CursorMoved` arm rather
/// than from the frame: AppKit begins a drag only inside the mouse event
/// that is dragging.
pub const HANDS_OFF_ON_CURSOR_MOVED: bool = true;

/// How often the pasteboard's change count is read. Each read is a round
/// trip to the pasteboard server, and a frame can come every few
/// milliseconds; a clipboard changed in another program and pasted here
/// within a fifth of a second is still read right, because the paste asks
/// for the bytes themselves.
const MIRROR_EVERY: Duration = Duration::from_millis(200);

/// The answers waiting for the next poll: the pasteboard's, and the drag
/// source's when a drag ends.
type Answers = Rc<RefCell<Vec<Event>>>;

/// What the drag source holds: where to say a drag has ended, and the bell.
struct Ending {
    answers: Answers,
    waker: Waker,
}

declare_class!(
    /// The source of every drag out of the window.
    struct DragSource;

    // SAFETY: `NSObject` has no subclassing requirements, the class is only
    // ever touched on the main thread, and it is named for this program.
    unsafe impl ClassType for DragSource {
        type Super = NSObject;
        type Mutability = mutability::MainThreadOnly;
        const NAME: &'static str = "DelightfileDragSource";
    }

    impl DeclaredClass for DragSource {
        type Ivars = Ending;
    }

    unsafe impl NSObjectProtocol for DragSource {}

    // SAFETY: the signatures are `NSDraggingSource`'s own.
    unsafe impl NSDraggingSource for DragSource {
        #[method(draggingSession:sourceOperationMaskForDraggingContext:)]
        fn operation_mask(
            &self,
            _session: &NSDraggingSession,
            _context: NSDraggingContext,
        ) -> NSDragOperation {
            NSDragOperation::Copy
        }

        #[method(draggingSession:endedAtPoint:operation:)]
        fn ended(&self, _session: &NSDraggingSession, _at: NSPoint, _operation: NSDragOperation) {
            let ending = self.ivars();
            ending.answers.borrow_mut().push(Event::DragEnded);
            ending.waker.wake();
        }
    }
);

/// The device: the pasteboard, the view a drag begins on, and the answers
/// waiting for the next poll.
pub struct Desktop {
    waker: Waker,
    answers: Answers,
    /// The change count the mirror was last taken at; `None` before the
    /// first poll, so the first poll always mirrors.
    seen: Cell<Option<isize>>,
    looked: Cell<Option<Instant>>,
    /// winit's view and the drag source, or `None` off the main thread,
    /// where there is no drag to begin.
    dragging: Option<(Retained<NSView>, Retained<DragSource>)>,
}

impl Desktop {
    fn new(waker: Waker, view: Option<Retained<NSView>>) -> Desktop {
        let answers: Answers = Rc::new(RefCell::new(Vec::new()));
        let dragging = view.map(|view| {
            let mtm = MainThreadMarker::from(&*view);
            let source = mtm.alloc::<DragSource>().set_ivars(Ending {
                answers: Rc::clone(&answers),
                waker: waker.named("drag"),
            });
            // SAFETY: `init` is `NSObject`'s, on a freshly allocated object.
            let source: Retained<DragSource> = unsafe { msg_send_id![super(source), init] };
            (view, source)
        });
        Desktop {
            waker,
            answers,
            seen: Cell::new(None),
            looked: Cell::new(None),
            dragging,
        }
    }

    /// Always: there is no thread here to have stopped.
    pub fn ready(&self) -> bool {
        true
    }

    /// Copy `bytes` onto the pasteboard now, and answer [`Event::Copied`] on
    /// the next poll. `mimes` is the offer `crate::clipboard::offer_mimes`
    /// makes: text is its `text/plain` names, anything else its one type.
    #[must_use]
    pub fn set_selection(&self, mimes: Vec<String>, bytes: Vec<u8>) -> bool {
        let ok = match super::clipboard::copy(copied_as(&mimes), &bytes) {
            Ok(_) => true,
            Err(error) => {
                log::info!("clipboard: the pasteboard refused the copy: {error}");
                false
            }
        };
        self.answer(Event::Copied { ok });
        true
    }

    /// Read the pasteboard as `mime` now, and answer [`Event::Pasted`] with
    /// `seq` on the next poll.
    #[must_use]
    pub fn receive(&self, seq: u64, mime: String) -> bool {
        let bytes = super::clipboard::paste(&mime).map_err(|error| {
            log::info!("clipboard: nothing came off the pasteboard as {mime}: {error}");
            PasteFailure::Gone
        });
        self.answer(Event::Pasted { seq, bytes });
        true
    }

    /// Begin a drag of what `offers` names, carrying the picture of a stack
    /// of `count` cards. Called from the window's `CursorMoved`, inside the
    /// mouse event that is dragging; `false` when there is no such event,
    /// no file to drag or no view to drag from, and the window springs the
    /// ghost home. Once it has begun, [`Event::DragEnded`] says it is over.
    #[must_use]
    pub fn drag(
        &self,
        offers: Vec<(String, Vec<u8>)>,
        count: usize,
        card: Rgba,
        ink: Rgba,
        _scale: i32,
    ) -> bool {
        let Some((view, source)) = &self.dragging else {
            return false;
        };
        let items = dragout::items(&offers);
        if items.is_empty() {
            return false;
        }
        let mtm = MainThreadMarker::from(&**view);
        let Some(event) = NSApplication::sharedApplication(mtm).currentEvent() else {
            log::info!("drag: no mouse event to begin the drag in");
            return false;
        };
        let icon = crate::platform::icon::draw(count, card, ink);
        let Some(image) = picture(&icon) else {
            return false;
        };
        // SAFETY: a read of a point from the live current event.
        let at = view.convertPoint_fromView(unsafe { event.locationInWindow() }, None);
        let (grab_x, grab_y) = crate::platform::icon::HOTSPOT;
        // winit's view is flipped, so the frame's origin is its top-left
        // corner, which puts the picture's grab point under the pointer.
        let frame = NSRect::new(
            NSPoint::new(at.x - f64::from(grab_x), at.y - f64::from(grab_y)),
            NSSize::new(f64::from(icon.width), f64::from(icon.height)),
        );
        let mut dragged = Vec::new();
        for (at, item) in items.iter().enumerate() {
            let Some(writer) = pasteboard_item(item) else {
                continue;
            };
            // SAFETY: a fresh dragging item, made from a live pasteboard
            // item, which conforms to `NSPasteboardWriting`.
            let dragging = unsafe {
                NSDraggingItem::initWithPasteboardWriter(
                    NSDraggingItem::alloc(),
                    ProtocolObject::from_ref(&*writer),
                )
            };
            // One picture for the whole stack: the first item carries it,
            // and the rest move with it undrawn.
            let contents: Option<&AnyObject> = (at == 0).then_some(&**image);
            // SAFETY: a frame in the view's coordinates and a live image.
            unsafe { dragging.setDraggingFrame_contents(frame, contents) };
            dragged.push(dragging);
        }
        if dragged.is_empty() {
            return false;
        }
        let dragged = NSArray::from_vec(dragged);
        // SAFETY: winit's live view, the live current event, and a source
        // this device keeps for as long as it can be asked about the drag.
        unsafe {
            view.beginDraggingSessionWithItems_event_source(
                &dragged,
                &event,
                ProtocolObject::from_ref(&**source),
            );
        }
        true
    }

    /// The answers since the last poll, after a fresh mirror of the
    /// pasteboard if it has changed hands.
    pub fn poll(&self) -> Vec<Event> {
        let mut events = Vec::new();
        let now = Instant::now();
        let due = self
            .looked
            .get()
            .is_none_or(|looked| now.saturating_duration_since(looked) >= MIRROR_EVERY);
        if due {
            self.looked.set(Some(now));
            let count = super::clipboard::change_count();
            if self.seen.get() != Some(count) {
                self.seen.set(Some(count));
                events.push(Event::Selection {
                    mimes: super::clipboard::offered_types().unwrap_or_default(),
                });
            }
        }
        events.append(&mut self.answers.borrow_mut());
        events
    }

    fn answer(&self, event: Event) {
        self.answers.borrow_mut().push(event);
        self.waker.wake();
    }
}

/// The mime a copy offered as `mimes` goes onto the pasteboard as: `None`
/// for text, which is the pasteboard's string whichever `text/plain`
/// spelling it was offered under, and the one type otherwise.
fn copied_as(mimes: &[String]) -> Option<&str> {
    mimes
        .first()
        .map(String::as_str)
        .filter(|mime| !mime.starts_with("text/plain"))
}

/// The device, for any window: the pasteboard is the application's, and a
/// drag begins on the window's view.
pub fn start(_event_loop: &ActiveEventLoop, window: &Window, waker: Waker) -> Option<Desktop> {
    Some(Desktop::new(waker, view_of(window)))
}

/// winit's view for `window`, held: `None` for a window that is not an
/// AppKit one, or off the main thread.
fn view_of(window: &Window) -> Option<Retained<NSView>> {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    MainThreadMarker::new()?;
    let RawWindowHandle::AppKit(handle) = window.window_handle().ok()?.as_raw() else {
        return None;
    };
    // SAFETY: the handle is winit's `NSView`, alive as long as the window,
    // and retained here so it outlives this borrow.
    let view: &NSView = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
    Some(view.retain())
}

/// One file of a drag as a pasteboard item: its URL, its path as text, and
/// the window's mark when it carries it.
fn pasteboard_item(item: &dragout::Item) -> Option<Retained<NSPasteboardItem>> {
    let path = item.path.to_str()?;
    // SAFETY: class methods and setters on live objects made here; each
    // answers `None` or `false` rather than failing.
    unsafe {
        let url = NSURL::fileURLWithPath(&NSString::from_str(path)).absoluteString()?;
        let writer = NSPasteboardItem::new();
        writer.setString_forType(&url, &NSString::from_str(pasteboard::FILE_URL));
        writer.setString_forType(
            &NSString::from_str(&item.text),
            &NSString::from_str(pasteboard::TEXT),
        );
        if let Some(mark) = &item.mark {
            writer.setData_forType(&NSData::with_bytes(&[]), &NSString::from_str(mark));
        }
        Some(writer)
    }
}

/// The drag icon as an image AppKit can draw.
fn picture(icon: &crate::platform::icon::Icon) -> Option<Retained<NSImage>> {
    let pixels = dragout::rgba(&icon.pixels);
    let (width, height) = (icon.width as NSInteger, icon.height as NSInteger);
    if pixels.len() != (width * height * 4) as usize {
        return None;
    }
    // SAFETY: no planes, so the representation allocates its own buffer of
    // `height` rows of `width * 4` bytes, premultiplied RGBA (no format
    // flags), which is what is copied into it below.
    let bitmap = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bitmapFormat_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut(),
            width,
            height,
            8,
            4,
            true,
            false,
            NSDeviceRGBColorSpace,
            NSBitmapFormat(0),
            width * 4,
            32,
        )
    }?;
    // SAFETY: the representation's own buffer, exactly `pixels.len()` bytes
    // as it was asked for, and not otherwise touched while this writes it.
    unsafe {
        let buffer = bitmap.bitmapData();
        if buffer.is_null() {
            return None;
        }
        std::ptr::copy_nonoverlapping(pixels.as_ptr(), buffer, pixels.len());
    }
    let size = NSSize::new(f64::from(icon.width), f64::from(icon.height));
    // SAFETY: a fresh image of the icon's size, given its one representation.
    unsafe {
        let image = NSImage::initWithSize(NSImage::alloc(), size);
        image.addRepresentation(&bitmap);
        Some(image)
    }
}

/// Where the pointer is over `window`, in logical points from its top-left
/// corner, for a drop winit reports without a position (`App::winit_drop`).
///
/// AppKit keeps the pointer's place in the window whatever events have been
/// delivered (`mouseLocationOutsideOfEventStream`), in the window's own
/// bottom-up coordinates; winit's view is flipped, so converting the point
/// into the view turns it top-down, in the logical points egui draws in.
/// `None` only for a window that is not an AppKit one.
pub fn pointer_position(window: &Window) -> Option<(f32, f32)> {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let RawWindowHandle::AppKit(handle) = window.window_handle().ok()?.as_raw() else {
        return None;
    };
    // SAFETY: the handle is winit's `NSView`, which lives as long as the
    // window this borrow came from, and this runs on the main thread, in the
    // window's own event.
    let view: &NSView = unsafe { handle.ns_view.cast::<NSView>().as_ref() };
    let ns_window = view.window()?;
    // SAFETY: a read of a point from a live window.
    let at = unsafe { ns_window.mouseLocationOutsideOfEventStream() };
    let local = view.convertPoint_fromView(at, None);
    Some((local.x as f32, local.y as f32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_goes_as_the_string_and_anything_else_as_its_type() {
        let offered = |mime: Option<&str>| crate::clipboard::offer_mimes(mime);
        assert_eq!(copied_as(&offered(None)), None);
        assert_eq!(
            copied_as(&offered(Some("text/uri-list"))),
            Some("text/uri-list")
        );
        assert_eq!(copied_as(&offered(Some("image/png"))), Some("image/png"));
        assert_eq!(copied_as(&[]), None);
    }
}
