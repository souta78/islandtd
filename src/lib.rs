//! Focus Island core: everything except the window.
//!
//! The UI (`app.rs`, Slint) and Windows integration (`platform/`) live in the binary; this
//! library has no UI dependency, which keeps it fast to test and easy to reason about.

pub mod database;
pub mod export;
pub mod models;
pub mod paths;
pub mod quickadd;
pub mod reminders;
pub mod settings;
pub mod timer;
pub mod timeutil;
pub mod wake;
pub mod worker;
