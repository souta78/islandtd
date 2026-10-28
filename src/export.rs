//! Backup and export. Writes only into the folder the user chose in Settings.
//!
//! * `focus-island-backup-<time>.db`   a consistent SQLite copy (`VACUUM INTO`), restorable by
//!   copying it over `focus-island.db` while the app is closed.
//! * `focus-island-tasks-<time>.json`  human-readable export of tasks and focus sessions.

use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Local, Utc};

use crate::database::{Db, DbError, DbResult};
use crate::models::{Task, Ts, ValidationError};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportPaths {
    pub db_path: PathBuf,
    pub json_path: PathBuf,
}

fn json_escape(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

fn opt_ts(ts: Option<Ts>) -> String {
    match ts.and_then(|t| DateTime::<Utc>::from_timestamp(t, 0)) {
        Some(d) => format!("\"{}\"", d.to_rfc3339()),
        None => "null".to_string(),
    }
}

fn opt_num(v: Option<i64>) -> String {
    v.map(|n| n.to_string()).unwrap_or_else(|| "null".into())
}

pub fn tasks_to_json(tasks: &[Task], sessions: &[(Option<i64>, Ts, Ts, i64)], now: Ts) -> String {
    let mut o = String::with_capacity(256 + tasks.len() * 256);
    o.push_str("{\n  \"app\": \"Focus Island\",\n  \"format_version\": 1,\n  \"exported_at\": ");
    o.push_str(&opt_ts(Some(now)));
    o.push_str(",\n  \"times\": \"UTC, RFC 3339\",\n  \"tasks\": [");
    for (i, t) in tasks.iter().enumerate() {
        o.push_str(if i == 0 { "\n    {" } else { ",\n    {" });
        let _ = write!(o, "\"id\": {}, \"title\": ", t.id);
        json_escape(&t.title, &mut o);
        o.push_str(", \"description\": ");
        match &t.description {
            Some(d) => json_escape(d, &mut o),
            None => o.push_str("null"),
        }
        let _ = write!(
            o,
            ", \"created_at\": {}, \"updated_at\": {}, \"due_at\": {}, \"completed_at\": {}, \
             \"priority\": \"{}\", \"reminder_enabled\": {}, \"remind_at\": {}",
            opt_ts(Some(t.created_at)),
            opt_ts(Some(t.updated_at)),
            opt_ts(t.due_at),
            opt_ts(t.completed_at),
            t.priority.label().to_lowercase(),
            t.reminder_enabled,
            opt_ts(t.remind_at),
        );
        o.push('}');
    }
    o.push_str(if tasks.is_empty() { "],\n" } else { "\n  ],\n" });
    o.push_str("  \"focus_sessions\": [");
    for (i, (task, start, end, planned)) in sessions.iter().enumerate() {
        o.push_str(if i == 0 { "\n    {" } else { ",\n    {" });
        let _ = write!(
            o,
            "\"task_id\": {}, \"started_at\": {}, \"ended_at\": {}, \"planned_seconds\": {}}}",
            opt_num(*task),
            opt_ts(Some(*start)),
            opt_ts(Some(*end)),
            planned
        );
    }
    o.push_str(if sessions.is_empty() {
        "]\n}\n"
    } else {
        "\n  ]\n}\n"
    });
    o
}

pub fn export_all(db: &Db, dir: &Path, now: Ts) -> DbResult<ExportPaths> {
    if dir.as_os_str().is_empty() || !dir.is_absolute() {
        return Err(ValidationError::InvalidPath(
            "Choose an absolute export folder in Settings.".into(),
        )
        .into());
    }
    if dir.exists() && !dir.is_dir() {
        return Err(ValidationError::InvalidPath(
            "The export path is a file, not a folder.".into(),
        )
        .into());
    }
    fs::create_dir_all(dir)?;

    let stamp = DateTime::<Utc>::from_timestamp(now, 0)
        .unwrap_or_default()
        .with_timezone(&Local)
        .format("%Y%m%d-%H%M%S");
    let db_path = dir.join(format!("focus-island-backup-{stamp}.db"));
    let json_path = dir.join(format!("focus-island-tasks-{stamp}.json"));
    if db_path.exists() || json_path.exists() {
        return Err(DbError::Message(
            "An export from this second already exists; try again.".into(),
        ));
    }

    db.conn()
        .execute("VACUUM INTO ?1", [db_path.to_string_lossy().as_ref()])?;

    let tasks: Vec<Task> = {
        let mut st = db.conn().prepare("SELECT id FROM tasks ORDER BY id")?;
        let ids: Vec<i64> = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
        let mut v = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(t) = db.get_task(id)? {
                v.push(t);
            }
        }
        v
    };
    let sessions: Vec<(Option<i64>, Ts, Ts, i64)> = {
        let mut st = db.conn().prepare(
            "SELECT task_id, started_at, ended_at, planned_secs FROM focus_sessions ORDER BY id",
        )?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        rows.collect::<Result<_, _>>()?
    };
    fs::write(&json_path, tasks_to_json(&tasks, &sessions, now))?;
    Ok(ExportPaths { db_path, json_path })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Priority, Task};

    fn task(title: &str) -> Task {
        Task {
            id: 1,
            title: title.into(),
            description: Some("line1\nline2 \"quoted\" \\ back\u{1}".into()),
            created_at: 1_800_000_000,
            updated_at: 1_800_000_000,
            due_at: None,
            completed_at: None,
            priority: Priority::High,
            reminder_enabled: false,
            remind_lead_min: 0,
            remind_at: None,
            snooze_until: None,
        }
    }

    #[test]
    fn json_escapes_special_characters() {
        let j = tasks_to_json(&[task("a \"b\" \\ c")], &[], 1_800_000_000);
        assert!(j.contains(r#""title": "a \"b\" \\ c""#));
        assert!(j.contains(r#"line1\nline2 \"quoted\" \\ back\u0001"#));
        assert!(j.contains("\"priority\": \"high\""));
        assert!(j.contains("\"due_at\": null"));
    }

    #[test]
    fn empty_export_is_valid_shape() {
        let j = tasks_to_json(&[], &[], 1_800_000_000);
        assert!(j.contains("\"tasks\": [],"));
        assert!(j.contains("\"focus_sessions\": []"));
    }
}
