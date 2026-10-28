//! Quick-add shorthand: trailing tokens in the one-line field.
//!
//! ```text
//! Call mom @15:30        today 15:30 (tomorrow if that time has passed), reminder on
//! Pay rent @tomorrow     tomorrow 09:00
//! Standup @tmr @9:30am   tomorrow 09:30
//! Ship release !!!       priority High   (! = Low, !! = Medium, !!! = High)
//! ```
//! Only *trailing* tokens are interpreted, so a title like "Email @ noon" stays intact.

use chrono::{Duration, NaiveTime, TimeZone};

use crate::models::{Priority, TaskInput};
use crate::timeutil::{local_date, resolve_local, Ts};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct QuickAdd {
    pub title: String,
    pub priority: Priority,
    pub time: Option<NaiveTime>,
    /// `Some(0)` = today, `Some(1)` = tomorrow.
    pub day_offset: Option<i64>,
}

fn parse_time_token(s: &str) -> Option<NaiveTime> {
    let lower = s.to_ascii_lowercase();
    let (num, meridiem) = if let Some(p) = lower.strip_suffix("am") {
        (p, Some(false))
    } else if let Some(p) = lower.strip_suffix("pm") {
        (p, Some(true))
    } else {
        (lower.as_str(), None)
    };
    let (h, m) = match num.split_once(':') {
        Some((h, m)) => (h.parse::<u32>().ok()?, m.parse::<u32>().ok()?),
        None => {
            meridiem?; // a bare number is not a time
            (num.parse::<u32>().ok()?, 0)
        }
    };
    let h = match meridiem {
        Some(pm) => {
            if !(1..=12).contains(&h) {
                return None;
            }
            h % 12 + if pm { 12 } else { 0 }
        }
        None => {
            if h > 23 {
                return None;
            }
            h
        }
    };
    if m > 59 {
        return None;
    }
    NaiveTime::from_hms_opt(h, m, 0)
}

fn priority_token(s: &str) -> Option<Priority> {
    if s.is_empty() || s.len() > 3 || !s.chars().all(|c| c == '!') {
        return None;
    }
    Some(match s.len() {
        1 => Priority::Low,
        2 => Priority::Medium,
        _ => Priority::High,
    })
}

pub fn parse(input: &str) -> QuickAdd {
    let mut toks: Vec<&str> = input.split_whitespace().collect();
    let mut qa = QuickAdd::default();
    while let Some(&last) = toks.last() {
        if let Some(p) = priority_token(last) {
            qa.priority = p;
        } else if let Some(rest) = last.strip_prefix('@') {
            match rest.to_ascii_lowercase().as_str() {
                "today" => qa.day_offset = Some(0),
                "tomorrow" | "tmr" | "tom" => qa.day_offset = Some(1),
                other => match parse_time_token(other) {
                    Some(t) => qa.time = Some(t),
                    None => break,
                },
            }
        } else {
            break;
        }
        toks.pop();
    }
    qa.title = toks.join(" ");
    qa
}

impl QuickAdd {
    /// Build a task. A task with a time (or day) gets a reminder at that time if it is still
    /// in the future.
    pub fn into_input<Tz: TimeZone>(&self, tz: &Tz, now: Ts) -> TaskInput {
        let mut due_at = None;
        if self.time.is_some() || self.day_offset.is_some() {
            let time = self
                .time
                .unwrap_or_else(|| NaiveTime::from_hms_opt(9, 0, 0).unwrap_or_default());
            let today = local_date(tz, now);
            let at = |days: i64| {
                let naive = (today + Duration::days(days)).and_time(time);
                resolve_local(tz, naive).or_else(|| resolve_local(tz, naive + Duration::hours(1)))
            };
            due_at = match self.day_offset {
                Some(d) => at(d),
                None => match at(0) {
                    Some(t) if t > now => Some(t),
                    _ => at(1),
                },
            };
        }
        TaskInput {
            title: self.title.clone(),
            description: None,
            due_at,
            priority: self.priority,
            reminder_enabled: due_at.is_some_and(|d| d > now),
            remind_lead_min: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::timeutil::{format_date_field, format_time_field, parse_due};
    use chrono::FixedOffset;

    fn tz() -> FixedOffset {
        FixedOffset::east_opt(7 * 3600).unwrap()
    }

    #[test]
    fn plain_titles_are_untouched() {
        let q = parse("Buy milk");
        assert_eq!(q.title, "Buy milk");
        assert_eq!(q.time, None);
        assert_eq!(q.priority, Priority::None);
        // Tokens in the middle or malformed ones are part of the title.
        assert_eq!(parse("Meet @ 5 pm sharp").title, "Meet @ 5 pm sharp");
        assert_eq!(parse("Wow! great").title, "Wow! great");
        assert_eq!(parse("Call @25:00").title, "Call @25:00");
        assert_eq!(parse("Call @15").title, "Call @15");
    }

    #[test]
    fn trailing_tokens_are_parsed() {
        let q = parse("Call mom @15:30 !!");
        assert_eq!(q.title, "Call mom");
        assert_eq!(q.time, NaiveTime::from_hms_opt(15, 30, 0));
        assert_eq!(q.priority, Priority::Medium);

        let q = parse("Standup @tmr @9:30am");
        assert_eq!(q.title, "Standup");
        assert_eq!(q.day_offset, Some(1));
        assert_eq!(q.time, NaiveTime::from_hms_opt(9, 30, 0));

        assert_eq!(parse("x @12am").time, NaiveTime::from_hms_opt(0, 0, 0));
        assert_eq!(parse("x @12pm").time, NaiveTime::from_hms_opt(12, 0, 0));
        assert_eq!(parse("x @3PM").time, NaiveTime::from_hms_opt(15, 0, 0));
        assert_eq!(parse("x !!!").priority, Priority::High);
        assert_eq!(parse("x !").priority, Priority::Low);
    }

    #[test]
    fn only_tokens_means_empty_title() {
        assert_eq!(parse("@9:00 !!").title, "");
        assert!(parse("@9:00")
            .into_input(&tz(), 0)
            .validate(0, None)
            .is_err());
    }

    #[test]
    fn time_in_the_past_rolls_to_tomorrow() {
        let tz = tz();
        let now = parse_due(&tz, "2026-10-25", "10:00").unwrap().unwrap();

        let later = parse("x @15:30").into_input(&tz, now);
        let due = later.due_at.unwrap();
        assert_eq!(format_date_field(&tz, due), "2026-10-25");
        assert_eq!(format_time_field(&tz, due), "15:30");
        assert!(later.reminder_enabled);

        let rolled = parse("x @09:00").into_input(&tz, now);
        assert_eq!(format_date_field(&tz, rolled.due_at.unwrap()), "2026-10-26");
        assert!(rolled.reminder_enabled);
    }

    #[test]
    fn tomorrow_defaults_to_nine() {
        let tz = tz();
        let now = parse_due(&tz, "2026-10-25", "22:00").unwrap().unwrap();
        let i = parse("Pay rent @tomorrow").into_input(&tz, now);
        let due = i.due_at.unwrap();
        assert_eq!(format_date_field(&tz, due), "2026-10-26");
        assert_eq!(format_time_field(&tz, due), "09:00");
    }

    #[test]
    fn explicit_past_today_has_no_reminder() {
        let tz = tz();
        let now = parse_due(&tz, "2026-10-25", "10:00").unwrap().unwrap();
        let i = parse("x @today @08:00").into_input(&tz, now);
        assert!(i.due_at.is_some());
        assert!(!i.reminder_enabled);
        assert!(i.validate(now, None).is_ok());
    }
}
