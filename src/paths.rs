//! Per-user locations. No external crate: only environment variables.
//!
//! Windows: `%LOCALAPPDATA%\FocusIsland`. Elsewhere (development only): `$XDG_DATA_HOME` or
//! `~/.local/share`. `FOCUS_ISLAND_DATA_DIR` overrides everything (used by tests and
//! performance measurements).

use std::env;
use std::path::PathBuf;

pub const APP_DIR: &str = "FocusIsland";
pub const DB_FILE: &str = "focus-island.db";

fn non_empty(var: &str) -> Option<PathBuf> {
    env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

pub fn data_dir() -> PathBuf {
    if let Some(p) = non_empty("FOCUS_ISLAND_DATA_DIR") {
        return p;
    }
    #[cfg(windows)]
    {
        if let Some(p) = non_empty("LOCALAPPDATA") {
            return p.join(APP_DIR);
        }
        if let Some(p) = non_empty("APPDATA") {
            return p.join(APP_DIR);
        }
    }
    #[cfg(not(windows))]
    {
        if let Some(p) = non_empty("XDG_DATA_HOME") {
            return p.join(APP_DIR);
        }
        if let Some(h) = non_empty("HOME") {
            return h.join(".local").join("share").join(APP_DIR);
        }
    }
    env::temp_dir().join(APP_DIR).join("fallback")
}

pub fn db_path() -> PathBuf {
    data_dir().join(DB_FILE)
}

/// Default destination for "Export": `Documents\Focus Island`.
pub fn default_export_dir() -> PathBuf {
    let home = non_empty("USERPROFILE").or_else(|| non_empty("HOME"));
    match home {
        Some(h) => h.join("Documents").join("Focus Island"),
        None => data_dir().join("exports"),
    }
}
