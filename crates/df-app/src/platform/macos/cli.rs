//! The lines of `--help` that differ by platform ([`crate::cli::usage`]).
//! There is no `--portal` here, so nothing names it: the two flags whose
//! Linux lines say the portal is what uses them are described without it,
//! and there is no `--portal` paragraph.

/// The `--reveal` entry.
pub const REVEAL_USAGE: &str =
    "  --reveal               show the path rather than open it: its folder opens
                         with the cursor on it, a folder included (needs a
                         path, and cannot be a --chooser-* dialog)
";

/// The `--chooser-request` entry.
pub const REQUEST_USAGE: &str = "  --chooser-request=<path>
                         read the whole dialog from this file instead: its
                         kind, title, button label, suggested name, starting
                         folder and file-type filters (needs --chooser-file,
                         and outranks the switches)
";

/// The flags only this platform has, after the chooser's: none.
pub const EXTRA_USAGE: &str = "";
