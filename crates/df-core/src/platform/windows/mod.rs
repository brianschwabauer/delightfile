//! Windows: bodies where one `std` call or one CRT function does the job, and
//! stubs everywhere else (the native bodies are Phase 4,
//! `plans/other-platforms/04-windows.md`).

pub mod errno;
