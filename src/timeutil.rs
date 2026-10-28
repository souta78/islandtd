//! Time helpers. All persistence uses UTC unix seconds; local time is only used at the edges
//! (parsing user input, day boundaries, display). Functions take a `TimeZone` so tests are
//! deterministic and DST gaps/folds are handled in one place.

use std::fmt::Display;

use chrono::{
    DateTime, Datelike, Duration, LocalResult, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc,
};

use crate::models::ValidationError;

/// Unix timestamp in seconds (UTC).
pub type Ts = i64;

pub fn now_ts() -> Ts {
    Utc::now().timestamp()
}

pub fn now_ms() -> i64 {
    Utc::now().timestamp_millis()
}

/// Resolve a naive local date-time to a timestamp. Ambiguous (DST fold) times resolve to the
/// earlier instant; non-existent (DST gap) times return `None`.
pub fn resolve_local<Tz: TimeZone>(tz: &Tz, naive: NaiveDateTime) -> Option<Ts> {
    match tz.from_local_datetime(&naive) {
        LocalResult::Single(dt) => Some(dt.timestamp()),
        LocalResult::Ambiguous(earliest, _) => Some(earliest.timestamp()),
        LocalResult::None => None,
    }
}

/// First instant of `date` in `tz`. If midnight does not exist (DST gap at midnight), the first
/// valid half-hour after it is used.
pub fn local_day_start<Tz: TimeZone>(tz: &Tz, date: NaiveDate) -> Ts {
    let midnight = date.and_hms_opt(0, 0, 0).unwrap_or_default();
    let mut t = midnight;
    for _ in 0..8 {
        if let Some(ts) = resolve_local(tz, t) {
            return ts;
        }
        t += Duration::minutes(30);
    }
    Utc.from_utc_datetime(&midnight).timestamp()
}

/// The local calendar date containing `ts`.
pub fn local_date<Tz: TimeZone>(tz: &Tz, ts: Ts) -> NaiveDate {
    match tz.timestamp_opt(ts, 0) {
        LocalResult::Single(d) | LocalResult::Ambiguous(d, _) => d.date_naive(),
        LocalResult::None => Utc
            .timestamp_opt(ts, 0)
            .single()
            .map(|d| d.date_naive())
            .unwrap_or_default(),
    }
}

/// `[start, end)` of the local day containing `now`.
pub fn day_bounds<Tz: TimeZone>(tz: &Tz, now: Ts) -> (Ts, Ts) {
    let date = local_date(tz, now);
    let next = date.succ_opt().unwrap_or(date);
    (local_day_start(tz, date), local_day_start(tz, next))
}

pub fn parse_date(s: &str) -> Result<NaiveDate, ValidationError> {
    let s = s.trim();
    let d = NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .map_err(|_| ValidationError::InvalidDate(s.to_string()))?;
    if !(2000..=2099).contains(&d.year()) {
        return Err(ValidationError::DateOutOfRange);
    }
    Ok(d)
}

pub fn parse_time(s: &str) -> Result<NaiveTime, ValidationError> {
    let s = s.trim();
    NaiveTime::parse_from_str(s, "%H:%M").map_err(|_| ValidationError::InvalidTime(s.to_string()))
}

/// Parse the two date/time text fields of the edit form. Both empty means "no due date".
pub fn parse_due<Tz: TimeZone>(
    tz: &Tz,
    date: &str,
    time: &str,
) -> Result<Option<Ts>, ValidationError> {
    match (date.trim().is_empty(), time.trim().is_empty()) {
        (true, true) => Ok(None),
        (false, true) => Err(ValidationError::DateWithoutTime),
        (true, false) => Err(ValidationError::TimeWithoutDate),
        (false, false) => {
            let d = parse_date(date)?;
            let t = parse_time(time)?;
            resolve_local(tz, d.and_time(t))
                .map(Some)
                .ok_or(ValidationError::NonexistentLocalTime)
        }
    }
}

fn to_local<Tz: TimeZone>(tz: &Tz, ts: Ts) -> Option<DateTime<Tz>> {
    match tz.timestamp_opt(ts, 0) {
        LocalResult::Single(d) | LocalResult::Ambiguous(d, _) => Some(d),
        LocalResult::None => None,
    }
}

pub fn format_time<Tz: TimeZone>(tz: &Tz, ts: Ts, clock_24h: bool) -> String
where
    Tz::Offset: Display,
{
    match to_local(tz, ts) {
        Some(d) if clock_24h => d.format("%H:%M").to_string(),
        Some(d) => d.format("%-I:%M %p").to_string(),
        None => String::new(),
    }
}

/// `2026-10-25`, for the edit form.
pub fn format_date_field<Tz: TimeZone>(tz: &Tz, ts: Ts) -> String
where
    Tz::Offset: Display,
{
    to_local(tz, ts)
        .map(|d| d.format("%Y-%m-%d").to_string())
        .unwrap_or_default()
}

/// `15:30` (24h) for the edit form's time field.
pub fn format_time_field<Tz: TimeZone>(tz: &Tz, ts: Ts) -> String
where
    Tz::Offset: Display,
{
    format_time(tz, ts, true)
}

/// `Sun, Oct 25` — header date.
pub fn format_header_date<Tz: TimeZone>(tz: &Tz, ts: Ts) -> String
where
    Tz::Offset: Display,
{
    to_local(tz, ts)
        .map(|d| d.format("%a, %b %-d").to_string())
        .unwrap_or_default()
}

/// Compact label for a due time relative to `now`: `15:30`, `Tomorrow 15:30`, `Yesterday 15:30`
/// or `Oct 28, 15:30`.
pub fn format_due_label<Tz: TimeZone>(tz: &Tz, ts: Ts, now: Ts, clock_24h: bool) -> String
where
    Tz::Offset: Display,
{
    let time = format_time(tz, ts, clock_24h);
    let (d, today) = (local_date(tz, ts), local_date(tz, now));
    let diff = d.signed_duration_since(today).num_days();
    match diff {
        0 => time,
        1 => format!("Tomorrow {time}"),
        -1 => format!("Yesterday {time}"),
        _ => match to_local(tz, ts) {
            Some(dt) => format!("{}, {time}", dt.format("%b %-d")),
            None => time,
        },
    }
}

/// `mm:ss`, or `h:mm:ss` for an hour or more.
pub fn format_countdown(total_secs: i64) -> String {
    let s = total_secs.max(0);
    let (h, m, sec) = (s / 3600, (s % 3600) / 60, s % 60);
    if h > 0 {
        format!("{h}:{m:02}:{sec:02}")
    } else {
        format!("{m:02}:{sec:02}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    fn tz() -> FixedOffset {
        FixedOffset::east_opt(7 * 3600).unwrap()
    }

    #[test]
    fn day_bounds_follow_local_midnight() {
        let tz = tz();
        let now = resolve_local(
            &tz,
            parse_date("2026-10-25")
                .unwrap()
                .and_hms_opt(7, 18, 0)
                .unwrap(),
        )
        .unwrap();
        let (s, e) = day_bounds(&tz, now);
        assert_eq!(e - s, 86_400);
        assert_eq!(format_time(&tz, s, true), "00:00");
        assert_eq!(format_date_field(&tz, s), "2026-10-25");
        assert_eq!(format_date_field(&tz, e), "2026-10-26");
    }

    #[test]
    fn parse_due_validates_each_field() {
        let tz = tz();
        assert_eq!(parse_due(&tz, "", "").unwrap(), None);
        assert!(matches!(
            parse_due(&tz, "2026-10-25", ""),
            Err(ValidationError::DateWithoutTime)
        ));
        assert!(matches!(
            parse_due(&tz, "", "09:00"),
            Err(ValidationError::TimeWithoutDate)
        ));
        assert!(matches!(
            parse_due(&tz, "2026-02-30", "09:00"),
            Err(ValidationError::InvalidDate(_))
        ));
        assert!(matches!(
            parse_due(&tz, "2026-10-25", "25:00"),
            Err(ValidationError::InvalidTime(_))
        ));
        assert!(matches!(
            parse_due(&tz, "1999-01-01", "09:00"),
            Err(ValidationError::DateOutOfRange)
        ));
        assert!(parse_due(&tz, "2026-10-25", "09:30").unwrap().is_some());
    }

    #[test]
    fn labels_are_relative_to_today() {
        let tz = tz();
        let d = |s: &str, t: &str| parse_due(&tz, s, t).unwrap().unwrap();
        let now = d("2026-10-25", "08:00");
        assert_eq!(
            format_due_label(&tz, d("2026-10-25", "15:30"), now, true),
            "15:30"
        );
        assert_eq!(
            format_due_label(&tz, d("2026-10-26", "09:00"), now, true),
            "Tomorrow 09:00"
        );
        assert_eq!(
            format_due_label(&tz, d("2026-10-24", "09:00"), now, true),
            "Yesterday 09:00"
        );
        assert_eq!(
            format_due_label(&tz, d("2026-11-02", "09:00"), now, true),
            "Nov 2, 09:00"
        );
        assert_eq!(
            format_due_label(&tz, d("2026-10-25", "15:30"), now, false),
            "3:30 PM"
        );
    }

    #[test]
    fn countdown_formats() {
        assert_eq!(format_countdown(25 * 60), "25:00");
        assert_eq!(format_countdown(59), "00:59");
        assert_eq!(format_countdown(3_725), "1:02:05");
        assert_eq!(format_countdown(-5), "00:00");
    }
}
