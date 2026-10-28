//! User settings, persisted as key/value rows in the `settings` table.
//! Unknown or malformed values fall back to defaults so a bad row can never block startup.

use std::collections::HashMap;

use crate::database::{Db, DbResult};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Theme {
    #[default]
    Black,
    Graphite,
    Light,
}

impl Theme {
    pub fn as_str(self) -> &'static str {
        match self {
            Theme::Black => "black",
            Theme::Graphite => "graphite",
            Theme::Light => "light",
        }
    }
    pub fn parse(s: &str) -> Option<Theme> {
        match s {
            "black" => Some(Theme::Black),
            "graphite" => Some(Theme::Graphite),
            "light" => Some(Theme::Light),
            _ => None,
        }
    }
    pub fn from_index(i: i32) -> Theme {
        match i {
            1 => Theme::Graphite,
            2 => Theme::Light,
            _ => Theme::Black,
        }
    }
    pub fn index(self) -> i32 {
        match self {
            Theme::Black => 0,
            Theme::Graphite => 1,
            Theme::Light => 2,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub launch_on_startup: bool,
    pub start_collapsed: bool,
    pub always_on_top: bool,
    pub reminder_sound: bool,
    pub focus_minutes: u32,
    pub snooze_minutes: u32,
    pub theme: Theme,
    pub high_contrast: bool,
    pub tray_enabled: bool,
    /// Register a Windows Scheduled Task that starts the app at the next reminder time.
    pub scheduled_wake: bool,
    /// Allow a reminder to take keyboard focus. Off by default so it never interrupts typing.
    pub reminder_takes_focus: bool,
    pub clock_24h: bool,
    /// Reminders later than this many minutes are summarised instead of popping up.
    pub missed_window_min: u32,
    /// Folder for "Export". Empty means the default (`Documents\Focus Island`).
    pub export_dir: String,
    /// Saved island anchor (top-centre, physical pixels). `None` = top-centre of the primary monitor.
    pub anchor_x: Option<i32>,
    pub anchor_y: Option<i32>,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            launch_on_startup: false,
            start_collapsed: true,
            always_on_top: true,
            reminder_sound: true,
            focus_minutes: 25,
            snooze_minutes: 10,
            theme: Theme::Black,
            high_contrast: false,
            tray_enabled: false,
            scheduled_wake: false,
            reminder_takes_focus: false,
            clock_24h: true,
            missed_window_min: 15,
            export_dir: String::new(),
            anchor_x: None,
            anchor_y: None,
        }
    }
}

fn b(map: &HashMap<String, String>, key: &str, default: bool) -> bool {
    match map.get(key).map(String::as_str) {
        Some("1") => true,
        Some("0") => false,
        _ => default,
    }
}

fn n<T: std::str::FromStr>(map: &HashMap<String, String>, key: &str, default: T) -> T {
    map.get(key).and_then(|v| v.parse().ok()).unwrap_or(default)
}

impl Settings {
    pub fn from_pairs(pairs: Vec<(String, String)>) -> Settings {
        let m: HashMap<String, String> = pairs.into_iter().collect();
        let d = Settings::default();
        Settings {
            launch_on_startup: b(&m, "launch_on_startup", d.launch_on_startup),
            start_collapsed: b(&m, "start_collapsed", d.start_collapsed),
            always_on_top: b(&m, "always_on_top", d.always_on_top),
            reminder_sound: b(&m, "reminder_sound", d.reminder_sound),
            focus_minutes: n(&m, "focus_minutes", d.focus_minutes),
            snooze_minutes: n(&m, "snooze_minutes", d.snooze_minutes),
            theme: m
                .get("theme")
                .and_then(|s| Theme::parse(s))
                .unwrap_or(d.theme),
            high_contrast: b(&m, "high_contrast", d.high_contrast),
            tray_enabled: b(&m, "tray_enabled", d.tray_enabled),
            scheduled_wake: b(&m, "scheduled_wake", d.scheduled_wake),
            reminder_takes_focus: b(&m, "reminder_takes_focus", d.reminder_takes_focus),
            clock_24h: b(&m, "clock_24h", d.clock_24h),
            missed_window_min: n(&m, "missed_window_min", d.missed_window_min),
            export_dir: m.get("export_dir").cloned().unwrap_or(d.export_dir),
            anchor_x: m.get("anchor_x").and_then(|v| v.parse().ok()),
            anchor_y: m.get("anchor_y").and_then(|v| v.parse().ok()),
        }
        .sanitized()
    }

    pub fn to_pairs(&self) -> Vec<(&'static str, String)> {
        let bit = |v: bool| if v { "1" } else { "0" }.to_string();
        vec![
            ("launch_on_startup", bit(self.launch_on_startup)),
            ("start_collapsed", bit(self.start_collapsed)),
            ("always_on_top", bit(self.always_on_top)),
            ("reminder_sound", bit(self.reminder_sound)),
            ("focus_minutes", self.focus_minutes.to_string()),
            ("snooze_minutes", self.snooze_minutes.to_string()),
            ("theme", self.theme.as_str().to_string()),
            ("high_contrast", bit(self.high_contrast)),
            ("tray_enabled", bit(self.tray_enabled)),
            ("scheduled_wake", bit(self.scheduled_wake)),
            ("reminder_takes_focus", bit(self.reminder_takes_focus)),
            ("clock_24h", bit(self.clock_24h)),
            ("missed_window_min", self.missed_window_min.to_string()),
            ("export_dir", self.export_dir.clone()),
            (
                "anchor_x",
                self.anchor_x.map(|v| v.to_string()).unwrap_or_default(),
            ),
            (
                "anchor_y",
                self.anchor_y.map(|v| v.to_string()).unwrap_or_default(),
            ),
        ]
    }

    /// Clamp numeric fields into their supported ranges.
    pub fn sanitized(mut self) -> Settings {
        self.focus_minutes = self.focus_minutes.clamp(1, 240);
        self.snooze_minutes = self.snooze_minutes.clamp(1, 240);
        self.missed_window_min = self.missed_window_min.clamp(1, 24 * 60);
        self.export_dir = self.export_dir.trim().to_string();
        self
    }

    pub fn load(db: &Db) -> DbResult<Settings> {
        Ok(Settings::from_pairs(db.all_settings()?))
    }

    pub fn save(&self, db: &mut Db) -> DbResult<()> {
        db.put_settings(&self.to_pairs())
    }
}
