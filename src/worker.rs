//! The single background thread.
//!
//! It owns the SQLite connection (so the UI thread never blocks on disk) and doubles as the
//! one centralized reminder scheduler: it blocks on its command channel with a timeout equal
//! to the time until the next deadline ([`plan_wait`]). No polling loop, no busy waiting, and
//! with nothing pending it sleeps until local midnight.
//!
//! Results go back through the [`Sink`], which the app wires to Slint's event loop. Tests use a
//! plain channel instead, so the whole scheduler runs without any UI.

use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};

use chrono::Local;

use crate::database::{Db, DbError, DbResult};
use crate::export::{export_all, ExportPaths};
use crate::models::*;
use crate::reminders::{collect_due, plan_wait, ReminderPolicy};
use crate::settings::Settings;
use crate::timeutil::{day_bounds, now_ts};

/// Rows loaded for the visible tab. The panel scrolls; nothing else is kept in memory.
pub const LIST_LIMIT: usize = 100;

pub enum Cmd {
    SetView(View),
    Refresh,
    Create(TaskInput),
    Update(i64, TaskInput),
    SetCompleted(i64, bool),
    Delete(i64),
    LoadTask(i64),
    Snooze {
        task_id: i64,
        scheduled_for: Ts,
        minutes: u32,
    },
    Dismiss {
        task_id: i64,
        scheduled_for: Ts,
    },
    RecordFocus {
        task_id: Option<i64>,
        started_at: Ts,
        ended_at: Ts,
        planned_secs: i64,
    },
    SaveSettings(Settings),
    Export(PathBuf),
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Snapshot(Snapshot),
    /// Reminders to present now, plus how many older ones were only recorded as missed.
    Reminders {
        show: Vec<Task>,
        missed: usize,
    },
    TaskLoaded(Task),
    /// A create/update succeeded.
    Saved(i64),
    Deleted(i64),
    /// The input was refused (shown next to the field being edited).
    Rejected(String),
    /// Something failed that the user did not cause (shown in the error banner).
    Error(String),
    Exported(ExportPaths),
}

pub type Sink = Box<dyn Fn(Event) + Send + 'static>;

/// What the optional Windows Task Scheduler integration should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeAction {
    Register(Ts),
    Unregister,
}

pub type WakeHook = Box<dyn FnMut(WakeAction) -> Result<(), String> + Send + 'static>;

pub struct Worker {
    tx: Sender<Cmd>,
    handle: Option<JoinHandle<()>>,
}

impl Worker {
    pub fn spawn(db: Db, settings: Settings, sink: Sink, wake_hook: Option<WakeHook>) -> Worker {
        let (tx, rx) = mpsc::channel();
        let handle = thread::Builder::new()
            .name("focus-island-worker".into())
            .spawn(move || Core::new(db, settings, sink, wake_hook).run(rx))
            .ok();
        Worker { tx, handle }
    }

    pub fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }

    /// Stop the thread and wait for it. Pending writes are already committed (each command
    /// is a transaction), so this never loses data.
    pub fn shutdown(mut self) {
        let _ = self.tx.send(Cmd::Shutdown);
        if let Some(h) = self.handle.take() {
            let _ = h.join();
        }
    }
}

struct Core {
    db: Db,
    settings: Settings,
    sink: Sink,
    wake_hook: Option<WakeHook>,
    /// What the scheduled task was last told: (enabled, deadline).
    wake_state: (bool, Option<Ts>),
    view: View,
    next_deadline: Option<Ts>,
    day_end: Ts,
    /// After a failed fire, do not retry before this time (prevents a hot loop).
    backoff_until: Ts,
}

impl Core {
    fn new(db: Db, settings: Settings, sink: Sink, wake_hook: Option<WakeHook>) -> Core {
        Core {
            db,
            settings,
            sink,
            wake_hook,
            // Assume no scheduled task exists yet; the first sync registers one if enabled.
            wake_state: (false, None),
            view: View::Today,
            next_deadline: None,
            day_end: 0,
            backoff_until: 0,
        }
    }

    fn emit(&self, e: Event) {
        (self.sink)(e);
    }

    fn report(&self, e: DbError) {
        match e {
            DbError::Validation(v) => self.emit(Event::Rejected(v.to_string())),
            other => self.emit(Event::Error(other.to_string())),
        }
    }

    fn policy(&self) -> ReminderPolicy {
        ReminderPolicy {
            missed_window_secs: i64::from(self.settings.missed_window_min) * 60,
        }
    }

    fn run(mut self, rx: Receiver<Cmd>) {
        let now = now_ts();
        self.day_end = day_bounds(&Local, now).1;
        if let Err(e) = self.db.prune(now) {
            self.report(e);
        }
        // Startup reconciliation: same code path as a normal fire, so reminders that came due
        // while the app was closed are either shown (recent) or summarised (old), once.
        self.fire(now);
        self.push_snapshot(now);

        loop {
            let now = now_ts();
            if now >= self.day_end {
                self.day_end = day_bounds(&Local, now).1;
                self.push_snapshot(now); // a new local day began
            }
            let deadline = self.next_deadline.map(|d| d.max(self.backoff_until));
            if deadline.is_some_and(|d| d <= now) {
                self.fire(now);
                continue;
            }
            match rx.recv_timeout(plan_wait(now, deadline, self.day_end)) {
                Ok(Cmd::Shutdown) | Err(RecvTimeoutError::Disconnected) => break,
                Ok(cmd) => self.handle(cmd),
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }

    /// Claim and announce everything due, then refresh the deadline.
    fn fire(&mut self, now: Ts) {
        let policy = self.policy();
        match collect_due(&mut self.db, now, &policy) {
            Ok(batch) => {
                if !batch.show.is_empty() || !batch.missed.is_empty() {
                    self.emit(Event::Reminders {
                        show: batch.show,
                        missed: batch.missed.len(),
                    });
                    self.push_snapshot(now);
                }
                self.backoff_until = 0;
            }
            Err(e) => {
                self.backoff_until = now + 30;
                self.report(e);
            }
        }
        self.refresh_deadline();
    }

    fn refresh_deadline(&mut self) {
        match self.db.next_reminder_deadline() {
            Ok(d) => self.next_deadline = d,
            Err(e) => {
                self.next_deadline = None;
                self.report(e);
            }
        }
        self.sync_wake(false);
    }

    fn snapshot(&self, now: Ts) -> DbResult<Snapshot> {
        let day = day_bounds(&Local, now);
        Ok(Snapshot {
            view: self.view,
            tasks: self.db.list_tasks(self.view, now, day, LIST_LIMIT)?,
            summary: self.db.summary(now, day)?,
            now,
            persistent: self.db.is_persistent(),
        })
    }

    fn push_snapshot(&self, now: Ts) {
        match self.snapshot(now) {
            Ok(s) => self.emit(Event::Snapshot(s)),
            Err(e) => self.report(e),
        }
    }

    /// Keep the optional Windows scheduled task in step with the next reminder.
    fn sync_wake(&mut self, force: bool) {
        let Some(hook) = self.wake_hook.as_mut() else {
            return;
        };
        let enabled = self.settings.scheduled_wake;
        let want = (enabled, if enabled { self.next_deadline } else { None });
        if !force && self.wake_state == want {
            return;
        }
        let action = match want {
            (true, Some(t)) => WakeAction::Register(t),
            _ => WakeAction::Unregister,
        };
        match hook(action) {
            Ok(()) => self.wake_state = want,
            Err(msg) => {
                let m = format!("Windows scheduled reminder: {msg}");
                (self.sink)(Event::Error(m));
            }
        }
    }

    fn handle(&mut self, cmd: Cmd) {
        let now = now_ts();
        let result: DbResult<()> = match cmd {
            Cmd::Shutdown => Ok(()),
            Cmd::SetView(v) => {
                self.view = v;
                self.push_snapshot(now);
                Ok(())
            }
            Cmd::Refresh => {
                self.push_snapshot(now);
                Ok(())
            }
            Cmd::Create(input) => self.db.create_task(&input, now).map(|t| {
                self.emit(Event::Saved(t.id));
                self.after_mutation(now);
            }),
            Cmd::Update(id, input) => self.db.update_task(id, &input, now).map(|t| {
                self.emit(Event::Saved(t.id));
                self.after_mutation(now);
            }),
            Cmd::SetCompleted(id, done) => self.db.set_completed(id, done, now).map(|_| {
                self.after_mutation(now);
            }),
            Cmd::Delete(id) => self.db.delete_task(id).map(|_| {
                self.emit(Event::Deleted(id));
                self.after_mutation(now);
            }),
            Cmd::LoadTask(id) => match self.db.get_task(id) {
                Ok(Some(t)) => {
                    self.emit(Event::TaskLoaded(t));
                    Ok(())
                }
                Ok(None) => Err(DbError::NotFound(id)),
                Err(e) => Err(e),
            },
            Cmd::Snooze {
                task_id,
                scheduled_for,
                minutes,
            } => self
                .db
                .snooze_reminder(task_id, scheduled_for, now, minutes)
                .map(|_| {
                    self.after_mutation(now);
                }),
            Cmd::Dismiss {
                task_id,
                scheduled_for,
            } => self.db.dismiss_reminder(task_id, scheduled_for),
            Cmd::RecordFocus {
                task_id,
                started_at,
                ended_at,
                planned_secs,
            } => self
                .db
                .record_focus_session(task_id, started_at, ended_at, planned_secs),
            Cmd::SaveSettings(s) => {
                let s = s.sanitized();
                s.save(&mut self.db).map(|_| {
                    self.settings = s;
                    self.sync_wake(true);
                })
            }
            Cmd::Export(dir) => {
                export_all(&self.db, &dir, now).map(|p| self.emit(Event::Exported(p)))
            }
        };
        if let Err(e) = result {
            self.report(e);
            // The list on screen may now be stale (e.g. the task was deleted elsewhere).
            self.push_snapshot(now);
        }
    }

    fn after_mutation(&mut self, now: Ts) {
        self.refresh_deadline();
        self.push_snapshot(now);
    }
}
