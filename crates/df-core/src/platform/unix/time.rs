//! The local wall clock, from the C library: `localtime_r`, which knows the
//! zone's rules for every date through the system's zoneinfo.

use crate::rename::facts::Civil;

/// Seconds since the epoch (negative before it) as a civil date-time on this
/// machine's local clock. `None` only if the C library refuses, which in
/// practice means a time so far out that the year no longer fits its `int`.
#[allow(unsafe_code)]
pub fn local_civil(secs: i64) -> Option<Civil> {
    let t = libc::time_t::try_from(secs).ok()?;
    // SAFETY: `libc::tm` is a plain C struct of integers (and, on glibc, a
    // zone-name pointer), for which all-zero bits are a valid value.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `t` and `tm` are owned locals of the right types, and
    // `localtime_r` writes only into `tm`. The `_r` form is the reentrant
    // one, so there is no shared static to race the photo workers over.
    let ok = unsafe { !libc::localtime_r(&t, &mut tm).is_null() };
    if !ok {
        return None;
    }
    Some(Civil {
        year: tm.tm_year.checked_add(1900)?,
        month: u32::try_from(tm.tm_mon).ok()?.checked_add(1)?,
        day: u32::try_from(tm.tm_mday).ok()?,
        hour: u32::try_from(tm.tm_hour).ok()?,
        minute: u32::try_from(tm.tm_min).ok()?,
        // A leap second reads as :60 from some C libraries. A filename has
        // no use for it, and every formatter downstream assumes 0–59.
        second: u32::try_from(tm.tm_sec).ok()?.min(59),
    })
}
