//! SQLite persistence: schema + migrations, corruption recovery, task CRUD and list views.
//! All SQL is parameterised; multi-step changes run inside transactions.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension, Row};

use crate::models::*;

const SCHEMA_V1: &str = r#"
CREATE TABLE tasks (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    title            TEXT    NOT NULL CHECK (length(trim(title)) > 0 AND length(title) <= 200),
    description      TEXT    CHECK (description IS NULL OR length(description) <= 2000),
    created_at       INTEGER NOT NULL,
    updated_at       INTEGER NOT NULL,
    due_at           INTEGER,
    completed_at     INTEGER,
    priority         INTEGER NOT NULL DEFAULT 0 CHECK (priority BETWEEN 0 AND 3),
    reminder_enabled INTEGER NOT NULL DEFAULT 0 CHECK (reminder_enabled IN (0, 1)),
    remind_lead_min  INTEGER NOT NULL DEFAULT 0 CHECK (remind_lead_min BETWEEN 0 AND 10080),
    remind_at        INTEGER,
    snooze_until     INTEGER,
    CHECK (reminder_enabled = 0 OR remind_at IS NOT NULL)
);
CREATE INDEX idx_tasks_open_due ON tasks (due_at) WHERE completed_at IS NULL;
CREATE INDEX idx_tasks_reminder ON tasks (COALESCE(snooze_until, remind_at))
    WHERE reminder_enabled = 1 AND completed_at IS NULL;

-- One row per (task, scheduled fire time). The UNIQUE key is what prevents duplicate alerts.
CREATE TABLE notifications (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id       INTEGER NOT NULL REFERENCES tasks (id) ON DELETE CASCADE,
    scheduled_for INTEGER NOT NULL,
    fired_at      INTEGER NOT NULL,
    outcome       TEXT    NOT NULL DEFAULT 'shown'
                  CHECK (outcome IN ('shown', 'missed', 'completed', 'snoozed', 'dismissed')),
    UNIQUE (task_id, scheduled_for)
);

CREATE TABLE focus_sessions (
    id           INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id      INTEGER REFERENCES tasks (id) ON DELETE SET NULL,
    started_at   INTEGER NOT NULL,
    ended_at     INTEGER NOT NULL,
    planned_secs INTEGER NOT NULL CHECK (planned_secs > 0)
);

CREATE TABLE settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
) WITHOUT ROWID;
"#;

/// Ordered list of migrations. `PRAGMA user_version` is the number applied. Append only.
pub const MIGRATIONS: &[&str] = &[SCHEMA_V1];

#[derive(Debug)]
pub enum DbError {
    Sqlite(rusqlite::Error),
    Validation(ValidationError),
    NotFound(i64),
    /// The file was written by a newer version of the app. It is left untouched.
    NewerSchema {
        found: i64,
        supported: i64,
    },
    Io(std::io::Error),
    Message(String),
}

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DbError::Sqlite(e) => write!(f, "Database error: {e}"),
            DbError::Validation(e) => write!(f, "{e}"),
            DbError::NotFound(id) => write!(f, "Task {id} no longer exists."),
            DbError::NewerSchema { found, supported } => write!(
                f,
                "The database was created by a newer version (schema {found}, this build supports {supported})."
            ),
            DbError::Io(e) => write!(f, "File error: {e}"),
            DbError::Message(m) => write!(f, "{m}"),
        }
    }
}

impl std::error::Error for DbError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DbError::Sqlite(e) => Some(e),
            DbError::Validation(e) => Some(e),
            DbError::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<rusqlite::Error> for DbError {
    fn from(e: rusqlite::Error) -> Self {
        DbError::Sqlite(e)
    }
}
impl From<ValidationError> for DbError {
    fn from(e: ValidationError) -> Self {
        DbError::Validation(e)
    }
}
impl From<std::io::Error> for DbError {
    fn from(e: std::io::Error) -> Self {
        DbError::Io(e)
    }
}

pub type DbResult<T> = Result<T, DbError>;

pub struct Db {
    pub(crate) conn: Connection,
    path: Option<PathBuf>,
}

pub struct Opened {
    pub db: Db,
    /// Shown to the user when something unusual happened while opening.
    pub notice: Option<String>,
}

const TASK_COLS: &str = "id, title, description, created_at, updated_at, due_at, completed_at, \
                         priority, reminder_enabled, remind_lead_min, remind_at, snooze_until";

fn row_to_task(r: &Row<'_>) -> rusqlite::Result<Task> {
    Ok(Task {
        id: r.get(0)?,
        title: r.get(1)?,
        description: r.get(2)?,
        created_at: r.get(3)?,
        updated_at: r.get(4)?,
        due_at: r.get(5)?,
        completed_at: r.get(6)?,
        priority: Priority::from_i64(r.get(7)?),
        reminder_enabled: r.get::<_, i64>(8)? != 0,
        remind_lead_min: r.get(9)?,
        remind_at: r.get(10)?,
        snooze_until: r.get(11)?,
    })
}

pub(crate) fn fetch_task(conn: &Connection, id: i64) -> DbResult<Option<Task>> {
    let sql = format!("SELECT {TASK_COLS} FROM tasks WHERE id = ?1");
    Ok(conn
        .prepare_cached(&sql)?
        .query_row([id], row_to_task)
        .optional()?)
}

fn query_tasks(conn: &Connection, sql: &str, p: &[&dyn rusqlite::ToSql]) -> DbResult<Vec<Task>> {
    let mut st = conn.prepare_cached(sql)?;
    let rows = st.query_map(p, row_to_task)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

fn configure(conn: &Connection) -> DbResult<()> {
    conn.busy_timeout(std::time::Duration::from_secs(2))?;
    // journal_mode returns a row, so it cannot go through `pragma_update`.
    let _mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
    conn.execute_batch(
        "PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;
         PRAGMA cache_size = -1024;
         PRAGMA mmap_size = 0;",
    )?;
    conn.set_prepared_statement_cache_capacity(32);
    Ok(())
}

fn user_version(conn: &Connection) -> DbResult<i64> {
    Ok(conn.query_row("PRAGMA user_version", [], |r| r.get(0))?)
}

/// Apply any migrations newer than the file's `user_version`, each in its own transaction.
/// Before upgrading an existing, non-empty database, a consistent copy is written next to it
/// (`<name>.pre-v<N>.bak`) so an update can never lose data.
pub fn apply_migrations(
    conn: &mut Connection,
    migrations: &[&str],
    path: Option<&Path>,
) -> DbResult<()> {
    let current = user_version(conn)?;
    let supported = migrations.len() as i64;
    if current > supported {
        return Err(DbError::NewerSchema {
            found: current,
            supported,
        });
    }
    if current == supported {
        return Ok(());
    }
    if current > 0 {
        if let Some(p) = path {
            let bak = backup_name(p, current);
            if !bak.exists() {
                conn.execute("VACUUM INTO ?1", [bak.to_string_lossy().as_ref()])?;
            }
        }
    }
    for (i, sql) in migrations.iter().enumerate().skip(current as usize) {
        let tx = conn.transaction()?;
        tx.execute_batch(sql)?;
        tx.pragma_update(None, "user_version", i as i64 + 1)?;
        tx.commit()?;
    }
    Ok(())
}

fn backup_name(path: &Path, version: i64) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(format!(".pre-v{version}.bak"));
    path.with_file_name(name)
}

impl Db {
    pub fn open_in_memory() -> DbResult<Db> {
        let mut conn = Connection::open_in_memory()?;
        configure(&conn)?;
        apply_migrations(&mut conn, MIGRATIONS, None)?;
        Ok(Db { conn, path: None })
    }

    /// Open (creating if needed) and migrate. Fails on corruption or a newer schema; use
    /// [`Db::open_or_recover`] for the forgiving startup path.
    pub fn open(path: &Path) -> DbResult<Db> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut conn = Connection::open(path)?;
        configure(&conn)?;
        let check: String = conn.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
        if check != "ok" {
            return Err(DbError::Message(format!("integrity check failed: {check}")));
        }
        apply_migrations(&mut conn, MIGRATIONS, Some(path))?;
        Ok(Db {
            conn,
            path: Some(path.to_path_buf()),
        })
    }

    /// Startup path: never panics and never deletes data.
    /// * corrupt file  -> moved aside to `*.corrupt-<unix>`, fresh database created
    /// * newer schema  -> file untouched, in-memory database used (changes are not saved)
    /// * unwritable    -> in-memory database used
    pub fn open_or_recover(path: &Path, now: Ts) -> Opened {
        match Db::open(path) {
            Ok(db) => Opened { db, notice: None },
            Err(DbError::NewerSchema { found, supported }) => Opened {
                db: Db::open_in_memory().expect("in-memory SQLite must open"),
                notice: Some(format!(
                    "Your data file is from a newer Focus Island (schema {found}, this version \
                     understands {supported}). It was left untouched; changes in this session \
                     will not be saved."
                )),
            },
            Err(first) => {
                let aside = corrupt_name(path, now);
                let moved = move_aside(path, &aside);
                if moved.is_ok() {
                    if let Ok(db) = Db::open(path) {
                        return Opened {
                            db,
                            notice: Some(format!(
                                "Your task database could not be read ({first}). It was kept as \
                                 {} and a new one was created.",
                                aside.display()
                            )),
                        };
                    }
                }
                Opened {
                    db: Db::open_in_memory().expect("in-memory SQLite must open"),
                    notice: Some(format!(
                        "Task storage is unavailable ({first}). Changes in this session will \
                         not be saved."
                    )),
                }
            }
        }
    }

    pub fn is_persistent(&self) -> bool {
        self.path.is_some()
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }

    // ---- tasks -----------------------------------------------------------------------

    pub fn get_task(&self, id: i64) -> DbResult<Option<Task>> {
        fetch_task(&self.conn, id)
    }

    pub fn create_task(&mut self, input: &TaskInput, now: Ts) -> DbResult<Task> {
        let v = input.validate(now, None)?;
        let id = {
            let mut st = self.conn.prepare_cached(
                "INSERT INTO tasks (title, description, created_at, updated_at, due_at, priority, \
                 reminder_enabled, remind_lead_min, remind_at) \
                 VALUES (?1, ?2, ?3, ?3, ?4, ?5, ?6, ?7, ?8)",
            )?;
            st.insert(params![
                v.title,
                v.description,
                now,
                v.due_at,
                v.priority.as_i64(),
                v.reminder_enabled as i64,
                v.remind_lead_min,
                v.remind_at
            ])?
        };
        fetch_task(&self.conn, id)?.ok_or(DbError::NotFound(id))
    }

    pub fn update_task(&mut self, id: i64, input: &TaskInput, now: Ts) -> DbResult<Task> {
        let tx = self.conn.transaction()?;
        let existing = fetch_task(&tx, id)?.ok_or(DbError::NotFound(id))?;
        let v = input.validate(now, existing.remind_at)?;
        // A changed reminder time starts a fresh reminder; an unchanged one keeps its snooze.
        let snooze = if v.reminder_enabled && v.remind_at == existing.remind_at {
            existing.snooze_until
        } else {
            None
        };
        tx.execute(
            "UPDATE tasks SET title = ?1, description = ?2, updated_at = ?3, due_at = ?4, \
             priority = ?5, reminder_enabled = ?6, remind_lead_min = ?7, remind_at = ?8, \
             snooze_until = ?9 WHERE id = ?10",
            params![
                v.title,
                v.description,
                now,
                v.due_at,
                v.priority.as_i64(),
                v.reminder_enabled as i64,
                v.remind_lead_min,
                v.remind_at,
                snooze,
                id
            ],
        )?;
        tx.commit()?;
        fetch_task(&self.conn, id)?.ok_or(DbError::NotFound(id))
    }

    pub fn set_completed(&mut self, id: i64, completed: bool, now: Ts) -> DbResult<Task> {
        let tx = self.conn.transaction()?;
        let n = tx.execute(
            "UPDATE tasks SET completed_at = ?1, updated_at = ?2 WHERE id = ?3",
            params![completed.then_some(now), now, id],
        )?;
        if n == 0 {
            return Err(DbError::NotFound(id));
        }
        if completed {
            tx.execute(
                "UPDATE notifications SET outcome = 'completed' \
                 WHERE task_id = ?1 AND outcome = 'shown'",
                [id],
            )?;
        }
        tx.commit()?;
        fetch_task(&self.conn, id)?.ok_or(DbError::NotFound(id))
    }

    /// Deletes the task. Its notification rows go with it (ON DELETE CASCADE), so no reminder
    /// can outlive it; focus sessions keep their history with `task_id` cleared.
    pub fn delete_task(&mut self, id: i64) -> DbResult<bool> {
        let n = self.conn.execute("DELETE FROM tasks WHERE id = ?1", [id])?;
        Ok(n > 0)
    }

    pub fn list_tasks(
        &self,
        view: View,
        now: Ts,
        day: (Ts, Ts),
        limit: usize,
    ) -> DbResult<Vec<Task>> {
        let (start, end) = day;
        let limit = limit as i64;
        match view {
            View::Overdue => {
                let sql = format!(
                    "SELECT {TASK_COLS} FROM tasks \
                     WHERE completed_at IS NULL AND due_at IS NOT NULL AND due_at < ?1 \
                     ORDER BY due_at ASC, priority DESC, id ASC LIMIT ?2"
                );
                query_tasks(&self.conn, &sql, &[&now, &limit])
            }
            View::Upcoming => {
                let sql = format!(
                    "SELECT {TASK_COLS} FROM tasks \
                     WHERE completed_at IS NULL AND due_at >= ?1 \
                     ORDER BY due_at ASC, priority DESC, id ASC LIMIT ?2"
                );
                query_tasks(&self.conn, &sql, &[&end, &limit])
            }
            View::Today => {
                let sql = format!(
                    "SELECT {TASK_COLS} FROM tasks \
                     WHERE (completed_at IS NULL AND (due_at IS NULL OR (due_at >= ?1 AND due_at < ?2))) \
                        OR (completed_at IS NOT NULL AND completed_at >= ?3 AND completed_at < ?2) \
                     ORDER BY (completed_at IS NOT NULL) ASC, (due_at IS NULL) ASC, due_at ASC, \
                              priority DESC, id ASC LIMIT ?4"
                );
                query_tasks(&self.conn, &sql, &[&now, &end, &start, &limit])
            }
        }
    }

    pub fn summary(&self, now: Ts, day: (Ts, Ts)) -> DbResult<Summary> {
        let (_, end) = day;
        let (overdue, today_open, upcoming): (i64, i64, i64) = self.conn.prepare_cached(
            "SELECT \
               COALESCE(SUM(CASE WHEN due_at IS NOT NULL AND due_at < ?1 THEN 1 END), 0), \
               COALESCE(SUM(CASE WHEN due_at IS NULL OR (due_at >= ?1 AND due_at < ?2) THEN 1 END), 0), \
               COALESCE(SUM(CASE WHEN due_at >= ?2 THEN 1 END), 0) \
             FROM tasks WHERE completed_at IS NULL",
        )?
        .query_row(params![now, end], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;

        let sql = format!(
            "SELECT {TASK_COLS} FROM tasks \
             WHERE completed_at IS NULL AND (due_at IS NULL OR (due_at >= ?1 AND due_at < ?2)) \
             ORDER BY (due_at IS NULL) ASC, due_at ASC, priority DESC, id ASC LIMIT 1"
        );
        let next = self
            .conn
            .prepare_cached(&sql)?
            .query_row(params![now, end], row_to_task)
            .optional()?;
        Ok(Summary {
            overdue,
            today_open,
            upcoming,
            next,
        })
    }

    // ---- settings key/value ------------------------------------------------------------

    pub fn all_settings(&self) -> DbResult<Vec<(String, String)>> {
        let mut st = self
            .conn
            .prepare_cached("SELECT key, value FROM settings")?;
        let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    pub fn put_settings(&mut self, pairs: &[(&str, String)]) -> DbResult<()> {
        let tx = self.conn.transaction()?;
        {
            let mut st = tx.prepare_cached(
                "INSERT INTO settings (key, value) VALUES (?1, ?2) \
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            )?;
            for (k, v) in pairs {
                st.execute(params![k, v])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    // ---- focus sessions ------------------------------------------------------------------

    pub fn record_focus_session(
        &mut self,
        task_id: Option<i64>,
        started_at: Ts,
        ended_at: Ts,
        planned_secs: i64,
    ) -> DbResult<()> {
        // A task deleted while the timer ran must not turn into a foreign-key failure.
        let task_id = match task_id {
            Some(id) if fetch_task(&self.conn, id)?.is_some() => Some(id),
            _ => None,
        };
        self.conn.execute(
            "INSERT INTO focus_sessions (task_id, started_at, ended_at, planned_secs) \
             VALUES (?1, ?2, ?3, ?4)",
            params![task_id, started_at, ended_at, planned_secs],
        )?;
        Ok(())
    }

    pub fn focus_session_count(&self) -> DbResult<i64> {
        Ok(self
            .conn
            .query_row("SELECT COUNT(*) FROM focus_sessions", [], |r| r.get(0))?)
    }

    // ---- housekeeping ----------------------------------------------------------------------

    /// Keep history bounded: notification rows older than 60 days (unless they still guard a
    /// live reminder, which would otherwise be able to fire again) and focus sessions older
    /// than a year.
    pub fn prune(&mut self, now: Ts) -> DbResult<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM notifications WHERE fired_at < ?1 AND NOT EXISTS ( \
               SELECT 1 FROM tasks t WHERE t.id = notifications.task_id \
                 AND t.reminder_enabled = 1 AND t.completed_at IS NULL \
                 AND COALESCE(t.snooze_until, t.remind_at) = notifications.scheduled_for)",
            [now - 60 * 86_400],
        )?;
        tx.execute(
            "DELETE FROM focus_sessions WHERE ended_at < ?1",
            [now - 365 * 86_400],
        )?;
        tx.commit()?;
        Ok(())
    }
}

fn corrupt_name(path: &Path, now: Ts) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(format!(".corrupt-{now}"));
    path.with_file_name(name)
}

/// Move the database and its WAL/SHM sidecars out of the way.
fn move_aside(path: &Path, aside: &Path) -> std::io::Result<()> {
    if path.exists() {
        fs::rename(path, aside)?;
    }
    for ext in ["-wal", "-shm"] {
        let mut from = path.as_os_str().to_os_string();
        from.push(ext);
        let from = PathBuf::from(from);
        if from.exists() {
            let mut to = aside.as_os_str().to_os_string();
            to.push(ext);
            // Sidecars of a corrupt file are useless without it; failing to move them is
            // not fatal because SQLite recreates them.
            let _ = fs::rename(&from, PathBuf::from(to));
        }
    }
    Ok(())
}
