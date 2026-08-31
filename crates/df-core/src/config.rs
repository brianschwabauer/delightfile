//! Configuration: `~/.config/delightfile/{delightfile,keymap,theme,vfs}.toml`.
//!
//! Hand-rolled TOML (PLAN §1, §3) — ported from delightviewer's
//! `parse_toml_tables` rather than pulling in the `toml` crate, because what is
//! actually needed is tables of scalars and arrays, and a parser that can point
//! at the offending line is worth more here than one that handles every corner
//! of the spec. The rules that matter: a missing file is silence, not an error;
//! an invalid line warns human-readably and the valid lines around it still
//! apply; the shipped defaults are Brian's current yazi config, so a fresh
//! install is already home.
