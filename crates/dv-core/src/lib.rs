//! Project model, commands/undo, timeline math, persistence (§11).
//! Pure logic: no ffmpeg, no GPU, no system dependencies beyond bundled SQLite.

pub mod autosave;
pub mod command;
pub mod db;
pub mod edit;
pub mod framing;
pub mod graphic;
pub mod hash;
pub mod model;
pub mod names;
pub mod snapshot;
pub mod transcript;

/// Version of the serialized model (snapshots, §8.1). Bumped whenever the
/// model's serde shape changes incompatibly; snapshot decode refuses newer.
pub const MODEL_VERSION: u32 = 1;
