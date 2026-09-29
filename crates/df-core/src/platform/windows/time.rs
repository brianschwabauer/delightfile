//! The local wall clock, from the C runtime: `localtime_s`, the MSVC CRT's
//! reentrant local-time conversion (the argument order is the reverse of
//! POSIX's `localtime_r`, and it returns an error number rather than a
//! pointer).

use crate::rename::facts::Civil;

/// Seconds since the epoch (negative before it) as a civil date-time on this
/// machine's local clock. `None` when the CRT refuses — a time before 1970 is
/// one it refuses.
#[allow(unsafe_code)]
pub fn local_civil(secs: i64) -> Option<Civil> {
    let t = libc::time_t::try_from(secs).ok()?;
    let mut tm = libc::tm {
        tm_sec: 0,
        tm_min: 0,
        tm_hour: 0,
        tm_mday: 0,
        tm_mon: 0,
        tm_year: 0,
        tm_wday: 0,
        tm_yday: 0,
        tm_isdst: 0,
    };
    // SAFETY: `t` and `tm` are owned locals of the right types, and
    // `localtime_s` writes only into `tm`.
    let rc = unsafe { libc::localtime_s(&mut tm, &t) };
    if rc != 0 {
        return None;
    }
    Some(Civil {
        year: tm.tm_year.checked_add(1900)?,
        month: u32::try_from(tm.tm_mon).ok()?.checked_add(1)?,
        day: u32::try_from(tm.tm_mday).ok()?,
        hour: u32::try_from(tm.tm_hour).ok()?,
        minute: u32::try_from(tm.tm_min).ok()?,
        second: u32::try_from(tm.tm_sec).ok()?.min(59),
    })
}
