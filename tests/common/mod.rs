//! Shared helpers for integration tests (no external crates).
#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use chrono::FixedOffset;
use focus_island::models::{Priority, Task, TaskInput, Ts};
use focus_island::timeutil::parse_due;

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// A unique temp directory, removed on drop.
pub struct TestDir(PathBuf);

impl TestDir {
    pub fn new(label: &str) -> TestDir {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let p = std::env::temp_dir().join(format!(
            "focus-island-test-{}-{label}-{n}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        TestDir(p)
    }
    pub fn path(&self) -> &Path {
        &self.0
    }
    pub fn db(&self) -> PathBuf {
        self.0.join("focus-island.db")
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// UTC+7, a fixed zone so day-boundary tests do not depend on the machine's time zone.
pub fn tz() -> FixedOffset {
    FixedOffset::east_opt(7 * 3600).unwrap()
}

/// Local timestamp for `YYYY-MM-DD` `HH:MM` in the fixed test zone.
pub fn at(date: &str, time: &str) -> Ts {
    parse_due(&tz(), date, time).unwrap().unwrap()
}

pub fn input(title: &str) -> TaskInput {
    TaskInput {
        title: title.into(),
        ..Default::default()
    }
}

pub fn timed(title: &str, due: Ts, reminder: bool, lead_min: i64) -> TaskInput {
    TaskInput {
        title: title.into(),
        due_at: Some(due),
        reminder_enabled: reminder,
        remind_lead_min: lead_min,
        ..Default::default()
    }
}

pub fn with_priority(mut i: TaskInput, p: Priority) -> TaskInput {
    i.priority = p;
    i
}

pub fn titles(tasks: &[Task]) -> Vec<&str> {
    tasks.iter().map(|t| t.title.as_str()).collect()
}
