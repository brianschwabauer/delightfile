//! The bulk-rename card's headless half (PLAN §5).
//!
//! Four pure pieces that the app's `bulk.rs` assembles into the two-column
//! card, none of which knows about a window:
//!
//! - [`template`] — the `{name}-{date}{ext}` language: a parser, the per-file
//!   [`facts::Facts`] it resolves against, and the counter, date and text
//!   transforms.
//! - [`exif`] — the smallest reader that answers the four questions the
//!   template asks of a photo: when it was taken, how big it is and what took
//!   it. JPEG and TIFF-container raws; nothing else, on purpose.
//! - [`editor`] — the right column as one document: rows are lines that cannot
//!   be joined or split, and there can be more than one caret.
//! - [`complete`] — the catalogue behind the `{` popover.

pub mod complete;
pub mod editor;
pub mod exif;
pub mod facts;
pub mod template;
