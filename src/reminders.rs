//! Reminder scheduling logic: which reminders are due, de-duplication, snooze, dismissal and
//! the startup policy for reminders that were missed while the app was not running.
//!
//! Nothing here sleeps or polls. [`plan_wait`] turns "next deadline" into a bounded sleep
//! that the worker thread uses; everything else is plain database logic and fully testable.

use std::time::Duration;

use rusqlite::params;

use crate::database::{fetch_task, Db, DbError, DbResult};
use crate::models::{Task, Ts};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Shown,
    Missed,
    Completed,
    Snoozed,
    Dismissed,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Shown => "shown",
            Outcome::Missed => "missed",
            Outcome::Completed => "completed",
            Outcome::Snoozed => "snoozed",
            Outcome::Dismissed => "dismissed",
        }
    }
}

/// How late a reminder may be and still pop up. Older ones are recorded as "missed" and
/// summarised (one badge) instead of flooding the screen after a long absence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReminderPolicy {
    pub missed_window_secs: i64,
}

impl Default for ReminderPolicy {
    fn default() -> Self {
        ReminderPolicy {
            missed_window_secs: 15 * 60,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DueBatch {
    /// Reminders to present now (earliest first). Each was newly recorded, so it will not be
    /// returned again.
    pub show: Vec<Task>,
    /// Reminders that were too old to pop up; now recorded as missed.
    pub missed: Vec<Task>,
}

/// Reminders whose fire time has passed and that have no notification record yet.
const DUE_FILTER: &str = "t.reminder_enabled = 1 AND t.completed_at IS NULL \
     AND NOT EXISTS (SELECT 1 FROM notifications n WHERE n.task_id = t.id \
                     AND n.scheduled_for = COALESCE(t.snooze_until, t.remind_at))";

impl Db {
    /// Earliest fire time among reminders that have not been notified yet.
    pub fn next_reminder_deadline(&self) -> DbResult<Option<Ts>> {
        let sql = format!(
            "SELECT MIN(COALESCE(t.snooze_until, t.remind_at)) FROM tasks t WHERE {DUE_FILTER}"
        );
        Ok(self
            .conn()
            .prepare_cached(&sql)?
            .query_row([], |r| r.get(0))?)
    }

    pub fn due_reminders(&self, now: Ts) -> DbResult<Vec<Task>> {
        let sql = format!(
            "SELECT t.id FROM tasks t WHERE {DUE_FILTER} \
             AND COALESCE(t.snooze_until, t.remind_at) <= ?1 \
             ORDER BY COALESCE(t.snooze_until, t.remind_at) ASC, t.id ASC"
        );
        let ids: Vec<i64> = {
            let mut st = self.conn().prepare_cached(&sql)?;
            let rows = st.query_map([now], |r| r.get(0))?;
            rows.collect::<Result<_, _>>()?
        };
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(t) = fetch_task(self.conn(), id)? {
                out.push(t);
            }
        }
        Ok(out)
    }

    /// Record that `(task, scheduled_for)` was handled. Returns `false` if it already was —
    /// the UNIQUE key makes this the single source of truth for "never alert twice".
    pub fn record_notification(
        &mut self,
        task_id: i64,
        scheduled_for: Ts,
        now: Ts,
        outcome: Outcome,
    ) -> DbResult<bool> {
        let n = self.conn.execute(
            "INSERT OR IGNORE INTO notifications (task_id, scheduled_for, fired_at, outcome) \
             VALUES (?1, ?2, ?3, ?4)",
            params![task_id, scheduled_for, now, outcome.as_str()],
        )?;
        Ok(n > 0)
    }

    pub fn resolve_notification(
        &mut self,
        task_id: i64,
        scheduled_for: Ts,
        outcome: Outcome,
    ) -> DbResult<()> {
        self.conn.execute(
            "UPDATE notifications SET outcome = ?1 WHERE task_id = ?2 AND scheduled_for = ?3",
            params![outcome.as_str(), task_id, scheduled_for],
        )?;
        Ok(())
    }

    /// Reschedule a fired reminder `minutes` from `now`. Returns the new fire time.
    pub fn snooze_reminder(
        &mut self,
        task_id: i64,
        scheduled_for: Ts,
        now: Ts,
        minutes: u32,
    ) -> DbResult<Ts> {
        let until = now + i64::from(minutes.clamp(1, 24 * 60)) * 60;
        let tx = self.conn.transaction()?;
        let n = tx.execute(
            "UPDATE tasks SET snooze_until = ?1, updated_at = ?2 \
             WHERE id = ?3 AND reminder_enabled = 1 AND completed_at IS NULL",
            params![until, now, task_id],
        )?;
        if n == 0 {
            return Err(DbError::NotFound(task_id));
        }
        tx.execute(
            "UPDATE notifications SET outcome = 'snoozed' WHERE task_id = ?1 AND scheduled_for = ?2",
            params![task_id, scheduled_for],
        )?;
        // The new key must be able to fire even if an identical one somehow exists.
        tx.execute(
            "DELETE FROM notifications WHERE task_id = ?1 AND scheduled_for = ?2",
            params![task_id, until],
        )?;
        tx.commit()?;
        Ok(until)
    }

    pub fn dismiss_reminder(&mut self, task_id: i64, scheduled_for: Ts) -> DbResult<()> {
        self.resolve_notification(task_id, scheduled_for, Outcome::Dismissed)
    }
}

/// Atomically claim every due reminder. Safe to call at startup and from the scheduler: a
/// reminder that was already claimed is never returned again, so restarts and repeated calls
/// cannot produce duplicate alerts.
pub fn collect_due(db: &mut Db, now: Ts, policy: &ReminderPolicy) -> DbResult<DueBatch> {
    let due = db.due_reminders(now)?;
    let mut batch = DueBatch::default();
    if due.is_empty() {
        return Ok(batch);
    }
    let tx = db.conn.transaction()?;
    for task in due {
        let Some(at) = task.effective_fire_at() else {
            continue;
        };
        let late = now - at;
        let too_old = late > policy.missed_window_secs;
        let outcome = if too_old {
            Outcome::Missed
        } else {
            Outcome::Shown
        };
        let inserted = tx.execute(
            "INSERT OR IGNORE INTO notifications (task_id, scheduled_for, fired_at, outcome) \
             VALUES (?1, ?2, ?3, ?4)",
            params![task.id, at, now, outcome.as_str()],
        )? > 0;
        if inserted {
            if too_old {
                batch.missed.push(task);
            } else {
                batch.show.push(task);
            }
        }
    }
    tx.commit()?;
    Ok(batch)
}

/// How long the scheduler may sleep.
///
/// The sleep ends at the earlier of the next reminder and the next local midnight (to refresh
/// "today"). While a reminder is pending the sleep is capped (60 s when it is within an hour,
/// else 5 min) so that a system clock change or a suspend/resume — during which monotonic
/// sleeps can stall — delays an alert by at most that cap. With no reminder pending the app
/// sleeps until midnight.
pub fn plan_wait(now: Ts, reminder_deadline: Option<Ts>, day_end: Ts) -> Duration {
    let mut target = day_end;
    let mut cap: Option<i64> = None;
    if let Some(d) = reminder_deadline {
        target = target.min(d);
        cap = Some(if d - now <= 3600 { 60 } else { 300 });
    }
    let mut secs = (target - now).max(0);
    if let Some(c) = cap {
        secs = secs.min(c);
    }
    Duration::from_secs(secs as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait_is_capped_only_when_a_reminder_is_pending() {
        let now = 1_000_000;
        // Nothing pending: sleep to midnight.
        assert_eq!(
            plan_wait(now, None, now + 40_000),
            Duration::from_secs(40_000)
        );
        // Reminder in 10 s: exact.
        assert_eq!(
            plan_wait(now, Some(now + 10), now + 40_000),
            Duration::from_secs(10)
        );
        // Reminder in 30 min: capped to 60 s.
        assert_eq!(
            plan_wait(now, Some(now + 1800), now + 40_000),
            Duration::from_secs(60)
        );
        // Reminder tomorrow: capped to 5 min.
        assert_eq!(
            plan_wait(now, Some(now + 90_000), now + 40_000),
            Duration::from_secs(300)
        );
        // Overdue deadline: wake immediately.
        assert_eq!(plan_wait(now, Some(now - 5), now + 40_000), Duration::ZERO);
        // Midnight sooner than the capped reminder wake.
        assert_eq!(
            plan_wait(now, Some(now + 90_000), now + 20),
            Duration::from_secs(20)
        );
    }
}
