//! Plain data types and input validation. No I/O here.

use std::fmt;

pub use crate::timeutil::Ts;

pub const MAX_TITLE_CHARS: usize = 200;
pub const MAX_DESC_CHARS: usize = 2000;
pub const MAX_LEAD_MIN: i64 = 7 * 24 * 60;
/// Earliest/latest due timestamps accepted (2000-01-01 .. 2100-01-01 UTC).
const MIN_TS: Ts = 946_684_800;
const MAX_TS: Ts = 4_102_444_800;
/// A reminder may be at most this far in the past when saved (covers clock skew and a user
/// pressing "save" a few seconds after the minute rolled over).
const PAST_GRACE_SECS: Ts = 60;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Default, Hash)]
pub enum Priority {
    #[default]
    None = 0,
    Low = 1,
    Medium = 2,
    High = 3,
}

impl Priority {
    pub fn from_i64(v: i64) -> Priority {
        match v {
            1 => Priority::Low,
            2 => Priority::Medium,
            3 => Priority::High,
            _ => Priority::None,
        }
    }
    pub fn as_i64(self) -> i64 {
        self as i64
    }
    pub fn label(self) -> &'static str {
        match self {
            Priority::None => "None",
            Priority::Low => "Low",
            Priority::Medium => "Medium",
            Priority::High => "High",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    EmptyTitle,
    TitleTooLong,
    DescriptionTooLong,
    InvalidDate(String),
    InvalidTime(String),
    DateOutOfRange,
    DateWithoutTime,
    TimeWithoutDate,
    NonexistentLocalTime,
    ReminderWithoutDue,
    ReminderInPast,
    InvalidLead,
    InvalidDuration,
    InvalidPath(String),
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use ValidationError::*;
        match self {
            EmptyTitle => write!(f, "Give the task a title."),
            TitleTooLong => write!(f, "Title is too long (max {MAX_TITLE_CHARS} characters)."),
            DescriptionTooLong => {
                write!(f, "Notes are too long (max {MAX_DESC_CHARS} characters).")
            }
            InvalidDate(s) => write!(
                f,
                "\u{201c}{s}\u{201d} is not a valid date. Use YYYY-MM-DD."
            ),
            InvalidTime(s) => write!(
                f,
                "\u{201c}{s}\u{201d} is not a valid time. Use HH:MM (24h)."
            ),
            DateOutOfRange => write!(f, "Date must be between 2000 and 2099."),
            DateWithoutTime => write!(f, "Add a time, or clear the date."),
            TimeWithoutDate => write!(f, "Add a date, or clear the time."),
            NonexistentLocalTime => {
                write!(
                    f,
                    "That local time does not exist (daylight saving change)."
                )
            }
            ReminderWithoutDue => write!(f, "A reminder needs a due date and time."),
            ReminderInPast => write!(f, "The reminder time is in the past."),
            InvalidLead => write!(
                f,
                "Reminder lead time must be between 0 minutes and 7 days."
            ),
            InvalidDuration => write!(f, "Enter a number of minutes between 1 and 240."),
            InvalidPath(s) => write!(f, "{s}"),
        }
    }
}

impl std::error::Error for ValidationError {}

/// A stored task.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Task {
    pub id: i64,
    pub title: String,
    pub description: Option<String>,
    pub created_at: Ts,
    pub updated_at: Ts,
    pub due_at: Option<Ts>,
    pub completed_at: Option<Ts>,
    pub priority: Priority,
    pub reminder_enabled: bool,
    /// Minutes before `due_at` the reminder fires (kept so the edit form can show it).
    pub remind_lead_min: i64,
    /// Absolute time the reminder is scheduled for (`due_at - lead`). `None` when disabled.
    pub remind_at: Option<Ts>,
    /// Set by "snooze"; replaces `remind_at` as the next fire time until it fires.
    pub snooze_until: Option<Ts>,
}

impl Task {
    pub fn is_completed(&self) -> bool {
        self.completed_at.is_some()
    }

    /// The instant this task's reminder will next fire (or last fired).
    pub fn effective_fire_at(&self) -> Option<Ts> {
        if !self.reminder_enabled {
            return None;
        }
        self.snooze_until.or(self.remind_at)
    }
}

/// Unvalidated user input for creating or editing a task.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TaskInput {
    pub title: String,
    pub description: Option<String>,
    pub due_at: Option<Ts>,
    pub priority: Priority,
    pub reminder_enabled: bool,
    pub remind_lead_min: i64,
}

/// Output of [`TaskInput::validate`]: normalised and safe to store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidTask {
    pub title: String,
    pub description: Option<String>,
    pub due_at: Option<Ts>,
    pub priority: Priority,
    pub reminder_enabled: bool,
    pub remind_lead_min: i64,
    pub remind_at: Option<Ts>,
}

impl TaskInput {
    /// Validate and normalise. `unchanged_remind_at` is the reminder time already stored when
    /// editing, which is allowed to be in the past if the user did not change it.
    pub fn validate(
        &self,
        now: Ts,
        unchanged_remind_at: Option<Ts>,
    ) -> Result<ValidTask, ValidationError> {
        let title: String = self
            .title
            .chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect::<String>()
            .trim()
            .to_string();
        if title.is_empty() {
            return Err(ValidationError::EmptyTitle);
        }
        if title.chars().count() > MAX_TITLE_CHARS {
            return Err(ValidationError::TitleTooLong);
        }

        let description = match self.description.as_deref().map(str::trim) {
            None | Some("") => None,
            Some(d) => {
                if d.chars().count() > MAX_DESC_CHARS {
                    return Err(ValidationError::DescriptionTooLong);
                }
                Some(d.to_string())
            }
        };

        if let Some(due) = self.due_at {
            if !(MIN_TS..MAX_TS).contains(&due) {
                return Err(ValidationError::DateOutOfRange);
            }
        }
        if !(0..=MAX_LEAD_MIN).contains(&self.remind_lead_min) {
            return Err(ValidationError::InvalidLead);
        }

        let remind_at = if self.reminder_enabled {
            let due = self.due_at.ok_or(ValidationError::ReminderWithoutDue)?;
            let at = due - self.remind_lead_min * 60;
            if at < now - PAST_GRACE_SECS && Some(at) != unchanged_remind_at {
                return Err(ValidationError::ReminderInPast);
            }
            Some(at)
        } else {
            None
        };

        Ok(ValidTask {
            title,
            description,
            due_at: self.due_at,
            priority: self.priority,
            reminder_enabled: self.reminder_enabled,
            remind_lead_min: self.remind_lead_min,
            remind_at,
        })
    }
}

/// Which list the panel is showing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default, Hash)]
pub enum View {
    #[default]
    Today,
    Upcoming,
    Overdue,
}

impl View {
    pub fn from_index(i: i32) -> View {
        match i {
            1 => View::Upcoming,
            2 => View::Overdue,
            _ => View::Today,
        }
    }
    pub fn index(self) -> i32 {
        match self {
            View::Today => 0,
            View::Upcoming => 1,
            View::Overdue => 2,
        }
    }
}

/// Counts and the "next" task, for tab badges and the collapsed pill.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub overdue: i64,
    pub today_open: i64,
    pub upcoming: i64,
    pub next: Option<Task>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snapshot {
    pub view: View,
    pub tasks: Vec<Task>,
    pub summary: Summary,
    pub now: Ts,
    /// `false` when running on the in-memory fallback database.
    pub persistent: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: Ts = 1_800_000_000;

    fn input(title: &str) -> TaskInput {
        TaskInput {
            title: title.into(),
            ..Default::default()
        }
    }

    #[test]
    fn title_is_trimmed_and_required() {
        assert_eq!(
            input("  hello \n").validate(NOW, None).unwrap().title,
            "hello"
        );
        assert_eq!(
            input("   ").validate(NOW, None),
            Err(ValidationError::EmptyTitle)
        );
        assert_eq!(
            input("").validate(NOW, None),
            Err(ValidationError::EmptyTitle)
        );
        let long = "x".repeat(MAX_TITLE_CHARS + 1);
        assert_eq!(
            input(&long).validate(NOW, None),
            Err(ValidationError::TitleTooLong)
        );
        assert!(input(&"x".repeat(MAX_TITLE_CHARS))
            .validate(NOW, None)
            .is_ok());
    }

    #[test]
    fn empty_notes_become_none() {
        let mut i = input("t");
        i.description = Some("   ".into());
        assert_eq!(i.validate(NOW, None).unwrap().description, None);
        i.description = Some(" a\nb ".into());
        assert_eq!(
            i.validate(NOW, None).unwrap().description.as_deref(),
            Some("a\nb")
        );
        i.description = Some("y".repeat(MAX_DESC_CHARS + 1));
        assert_eq!(
            i.validate(NOW, None),
            Err(ValidationError::DescriptionTooLong)
        );
    }

    #[test]
    fn reminder_rules() {
        let mut i = input("t");
        i.reminder_enabled = true;
        assert_eq!(
            i.validate(NOW, None),
            Err(ValidationError::ReminderWithoutDue)
        );

        i.due_at = Some(NOW + 3600);
        i.remind_lead_min = 15;
        let v = i.validate(NOW, None).unwrap();
        assert_eq!(v.remind_at, Some(NOW + 3600 - 900));

        i.remind_lead_min = MAX_LEAD_MIN + 1;
        assert_eq!(i.validate(NOW, None), Err(ValidationError::InvalidLead));
        i.remind_lead_min = -1;
        assert_eq!(i.validate(NOW, None), Err(ValidationError::InvalidLead));

        i.remind_lead_min = 0;
        i.due_at = Some(NOW - 3600);
        assert_eq!(i.validate(NOW, None), Err(ValidationError::ReminderInPast));
        // Unchanged past reminders (editing only the title) are fine.
        assert!(i.validate(NOW, Some(NOW - 3600)).is_ok());

        i.reminder_enabled = false;
        assert_eq!(i.validate(NOW, None).unwrap().remind_at, None);
    }

    #[test]
    fn due_out_of_range_rejected() {
        let mut i = input("t");
        i.due_at = Some(10);
        assert_eq!(i.validate(NOW, None), Err(ValidationError::DateOutOfRange));
        i.due_at = Some(MAX_TS + 1);
        assert_eq!(i.validate(NOW, None), Err(ValidationError::DateOutOfRange));
    }
}
