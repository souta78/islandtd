//! Focus timer. Pure state machine: every method takes `now_ms` explicitly, so there is no
//! counter that drifts, and the result is correct however late or often the UI asks.
//!
//! Time is wall-clock based so a suspended PC still finishes a session at the right moment.
//! `remaining` is clamped to `[0, total]`, so a system clock change can shorten or restart a
//! running session but can never produce a negative or oversized countdown.

use crate::timeutil::format_countdown;

pub const MIN_MINUTES: u32 = 1;
pub const MAX_MINUTES: u32 = 240;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TimerState {
    Idle,
    Running {
        started_ms: i64,
        deadline_ms: i64,
        total_ms: i64,
    },
    Paused {
        started_ms: i64,
        remaining_ms: i64,
        total_ms: i64,
    },
    Finished {
        started_ms: i64,
        ended_ms: i64,
        total_ms: i64,
    },
}

/// A completed session, returned exactly once by [`FocusTimer::poll`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinishedSession {
    pub task_id: Option<i64>,
    pub started_ms: i64,
    pub ended_ms: i64,
    pub planned_secs: i64,
}

#[derive(Clone, Debug)]
pub struct FocusTimer {
    state: TimerState,
    minutes: u32,
    task_id: Option<i64>,
}

impl FocusTimer {
    pub fn new(minutes: u32) -> FocusTimer {
        FocusTimer {
            state: TimerState::Idle,
            minutes: minutes.clamp(MIN_MINUTES, MAX_MINUTES),
            task_id: None,
        }
    }

    pub fn state(&self) -> &TimerState {
        &self.state
    }
    pub fn minutes(&self) -> u32 {
        self.minutes
    }
    pub fn task_id(&self) -> Option<i64> {
        self.task_id
    }
    pub fn is_running(&self) -> bool {
        matches!(self.state, TimerState::Running { .. })
    }
    pub fn is_paused(&self) -> bool {
        matches!(self.state, TimerState::Paused { .. })
    }
    pub fn is_finished(&self) -> bool {
        matches!(self.state, TimerState::Finished { .. })
    }
    /// Running or paused: a session is in progress.
    pub fn is_active(&self) -> bool {
        self.is_running() || self.is_paused()
    }

    /// Change the preset. Only allowed while no session is in progress. Returns whether it applied.
    pub fn set_minutes(&mut self, minutes: u32) -> bool {
        if self.is_active() {
            return false;
        }
        if let TimerState::Finished { .. } = self.state {
            self.state = TimerState::Idle;
        }
        self.minutes = minutes.clamp(MIN_MINUTES, MAX_MINUTES);
        true
    }

    /// Link (or unlink) the session to a task. Allowed any time before it finishes.
    pub fn set_task(&mut self, task_id: Option<i64>) {
        self.task_id = task_id;
    }

    pub fn start(&mut self, now_ms: i64) {
        if self.is_active() {
            return;
        }
        let total_ms = i64::from(self.minutes) * 60_000;
        self.state = TimerState::Running {
            started_ms: now_ms,
            deadline_ms: now_ms + total_ms,
            total_ms,
        };
    }

    pub fn pause(&mut self, now_ms: i64) {
        if let TimerState::Running {
            started_ms,
            deadline_ms,
            total_ms,
        } = self.state
        {
            let remaining_ms = (deadline_ms - now_ms).clamp(0, total_ms);
            self.state = TimerState::Paused {
                started_ms,
                remaining_ms,
                total_ms,
            };
        }
    }

    pub fn resume(&mut self, now_ms: i64) {
        if let TimerState::Paused {
            started_ms,
            remaining_ms,
            total_ms,
        } = self.state
        {
            self.state = TimerState::Running {
                started_ms,
                deadline_ms: now_ms + remaining_ms,
                total_ms,
            };
        }
    }

    /// Back to idle with the same preset. The linked task is kept.
    pub fn reset(&mut self) {
        self.state = TimerState::Idle;
    }

    pub fn remaining_ms(&self, now_ms: i64) -> i64 {
        match self.state {
            TimerState::Idle => i64::from(self.minutes) * 60_000,
            TimerState::Running {
                deadline_ms,
                total_ms,
                ..
            } => (deadline_ms - now_ms).clamp(0, total_ms),
            TimerState::Paused { remaining_ms, .. } => remaining_ms,
            TimerState::Finished { .. } => 0,
        }
    }

    /// Whole seconds left, rounded up so the display reads `25:00` at the start and `00:00`
    /// only when finished.
    pub fn remaining_secs(&self, now_ms: i64) -> i64 {
        (self.remaining_ms(now_ms) + 999) / 1000
    }

    /// `mm:ss` / `h:mm:ss` of what is left.
    pub fn display(&self, now_ms: i64) -> String {
        format_countdown(self.remaining_secs(now_ms))
    }

    /// 0.0..=1.0 progress through the session.
    pub fn progress(&self, now_ms: i64) -> f32 {
        let total = match self.state {
            TimerState::Idle => return 0.0,
            TimerState::Finished { .. } => return 1.0,
            TimerState::Running { total_ms, .. } | TimerState::Paused { total_ms, .. } => total_ms,
        };
        if total <= 0 {
            return 1.0;
        }
        1.0 - self.remaining_ms(now_ms) as f32 / total as f32
    }

    /// Milliseconds until the deadline, if a session is running.
    pub fn ms_until_finish(&self, now_ms: i64) -> Option<i64> {
        match self.state {
            TimerState::Running {
                deadline_ms,
                total_ms,
                ..
            } => Some((deadline_ms - now_ms).clamp(0, total_ms)),
            _ => None,
        }
    }

    /// Advance the state machine. Returns the finished session exactly once.
    pub fn poll(&mut self, now_ms: i64) -> Option<FinishedSession> {
        if let TimerState::Running {
            started_ms,
            deadline_ms,
            total_ms,
        } = self.state
        {
            if now_ms >= deadline_ms {
                self.state = TimerState::Finished {
                    started_ms,
                    ended_ms: deadline_ms,
                    total_ms,
                };
                return Some(FinishedSession {
                    task_id: self.task_id,
                    started_ms,
                    ended_ms: deadline_ms,
                    planned_secs: total_ms / 1000,
                });
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_down_from_a_deadline_not_a_counter() {
        let mut t = FocusTimer::new(25);
        assert_eq!(t.display(0), "25:00");
        t.start(1_000);
        assert_eq!(t.display(1_000), "25:00");
        // Even if the UI only looks once, 10 minutes later, the answer is exact.
        assert_eq!(t.display(1_000 + 10 * 60_000), "15:00");
        assert_eq!(
            t.remaining_ms(1_000 + 10 * 60_000 + 250),
            14 * 60_000 + 59_750
        );
        assert_eq!(t.remaining_secs(1_000 + 10 * 60_000 + 250), 14 * 60 + 60);
    }

    #[test]
    fn pause_freezes_and_resume_continues() {
        let mut t = FocusTimer::new(25);
        t.start(0);
        t.pause(5 * 60_000);
        assert!(t.is_paused());
        // Time passing while paused changes nothing.
        assert_eq!(t.remaining_ms(99 * 60_000), 20 * 60_000);
        t.resume(60 * 60_000);
        assert_eq!(t.remaining_ms(60 * 60_000), 20 * 60_000);
        assert_eq!(t.remaining_ms(70 * 60_000), 10 * 60_000);
    }

    #[test]
    fn finishes_once_even_if_polled_late() {
        let mut t = FocusTimer::new(25);
        t.set_task(Some(7));
        t.start(0);
        assert_eq!(t.poll(24 * 60_000), None);
        // UI was busy / hidden for an hour: still finishes, and only once.
        let done = t.poll(60 * 60_000).expect("finished");
        assert_eq!(done.task_id, Some(7));
        assert_eq!(done.planned_secs, 25 * 60);
        assert_eq!(done.ended_ms, 25 * 60_000);
        assert_eq!(t.poll(61 * 60_000), None);
        assert!(t.is_finished());
        assert_eq!(t.remaining_ms(61 * 60_000), 0);
    }

    #[test]
    fn presets_only_change_when_idle() {
        let mut t = FocusTimer::new(25);
        assert!(t.set_minutes(50));
        assert_eq!(t.display(0), "50:00");
        t.start(0);
        assert!(!t.set_minutes(5));
        assert_eq!(t.minutes(), 50);
        t.reset();
        assert!(t.set_minutes(300)); // clamped
        assert_eq!(t.minutes(), MAX_MINUTES);
        assert!(t.set_minutes(0));
        assert_eq!(t.minutes(), MIN_MINUTES);
    }

    #[test]
    fn reset_returns_to_idle_and_keeps_task() {
        let mut t = FocusTimer::new(25);
        t.set_task(Some(3));
        t.start(0);
        t.reset();
        assert_eq!(*t.state(), TimerState::Idle);
        assert_eq!(t.task_id(), Some(3));
        assert_eq!(t.poll(10 * 60 * 60_000), None);
    }

    #[test]
    fn clock_jumps_are_clamped() {
        let mut t = FocusTimer::new(25);
        t.start(1_000_000);
        // Clock moved back an hour: never show more than the full duration.
        assert_eq!(t.remaining_ms(1_000_000 - 3_600_000), 25 * 60_000);
        // Start/pause/resume are idempotent in wrong states.
        t.resume(0);
        assert!(t.is_running());
        t.start(5);
        assert_eq!(t.ms_until_finish(1_000_000), Some(25 * 60_000));
    }

    #[test]
    fn progress_runs_zero_to_one() {
        let mut t = FocusTimer::new(50);
        assert_eq!(t.progress(0), 0.0);
        t.start(0);
        assert!((t.progress(25 * 60_000) - 0.5).abs() < 1e-6);
        t.poll(50 * 60_000);
        assert_eq!(t.progress(50 * 60_000), 1.0);
    }
}
