//! `--portal` is the xdg-desktop-portal backend, and there is no such portal
//! here: the flag is not accepted ([`super::HAS_PORTAL`]), so this is never
//! reached. It exists so `main` can name `platform::portal::run` on every
//! target, and answers as `main` answers any other refusal.

/// Say that there is no portal to serve, and fail.
pub fn run() -> i32 {
    eprintln!("error: --portal is not available on this platform");
    2
}
