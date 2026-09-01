//! "Where was I?", remembered per path for as long as the session lasts.
//!
//! Two things in the app want the same shape (PLAN §2's per-directory cursor,
//! PLAN §6's preview scroll): a small map from a path to whatever the view was
//! showing there, which must not grow without bound over a session that walks a
//! whole filesystem. So it is one type, generic over the value, rather than two
//! nearly-identical maps with two nearly-identical eviction bugs.
//!
//! The eviction is least-recently-*remembered*, kept as a plain `Vec` of keys
//! in touch order beside the map. At a few hundred entries the linear search a
//! touch costs is a memcmp over a handful of cache lines — cheaper than the
//! allocation a linked structure would need to avoid it, and this is touched
//! once per directory change, not once per frame.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// How many directories the cursor memory holds.
///
/// 512 is far past the point where anyone remembers having been somewhere, and
/// exists only so a session that walks a large tree cannot grow the map without
/// bound. At a path plus a name apiece this is tens of kilobytes.
pub const CURSOR_MEMORY: usize = 512;

/// The last thing seen at each of a bounded number of paths.
#[derive(Debug, Clone)]
pub struct Recent<V> {
    limit: usize,
    seen: HashMap<PathBuf, V>,
    /// The keys, least recently touched first — the eviction order.
    order: Vec<PathBuf>,
}

impl<V> Recent<V> {
    /// A memory holding at most `limit` paths. A limit of zero remembers
    /// nothing, which is a legal way to turn the feature off rather than a
    /// state that panics later.
    pub fn new(limit: usize) -> Recent<V> {
        Recent {
            limit,
            seen: HashMap::new(),
            order: Vec::new(),
        }
    }

    /// Record what was on screen at `path`, evicting the oldest entry if that
    /// takes the map over its limit.
    pub fn remember(&mut self, path: impl Into<PathBuf>, value: V) {
        if self.limit == 0 {
            return;
        }
        let path = path.into();
        if let Some(at) = self.order.iter().position(|p| *p == path) {
            // Touched, so it goes to the young end: a directory somebody keeps
            // coming back to must not be evicted by one they passed through.
            let key = self.order.remove(at);
            self.order.push(key);
        } else {
            self.order.push(path.clone());
        }
        self.seen.insert(path, value);
        while self.order.len() > self.limit {
            let oldest = self.order.remove(0);
            self.seen.remove(&oldest);
        }
    }

    /// What was on screen at `path`, if it is still remembered.
    pub fn recall(&self, path: &Path) -> Option<&V> {
        self.seen.get(path)
    }

    /// Forget one path — what a caller does when the value turned out to be
    /// stale (a file that changed under the preview, a directory that is gone).
    pub fn forget(&mut self, path: &Path) {
        if self.seen.remove(path).is_some() {
            self.order.retain(|p| p != path);
        }
    }

    pub fn len(&self) -> usize {
        self.seen.len()
    }

    pub fn is_empty(&self) -> bool {
        self.seen.is_empty()
    }
}

/// Which entry the cursor was on in each directory the tab has left.
pub type CursorMemory = Recent<String>;

impl Default for CursorMemory {
    fn default() -> CursorMemory {
        Recent::new(CURSOR_MEMORY)
    }
}
