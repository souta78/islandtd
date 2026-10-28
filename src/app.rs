//! UI controller. Lives on the Slint (main) thread; talks to the database only through the
//! worker thread, so the UI never waits on disk.
//!
//! Everything here is event driven. The only timers are:
//!   * `tick`   1 s repeating, running only while a focus session is *running*;
//!   * `clock`  one-shot aligned to the next minute, re-armed only while the panel is open.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::time::Duration;

use chrono::{Local, Timelike};
use slint::{ComponentHandle, ModelRc, SharedString, Timer, TimerMode, VecModel};

use focus_island::models::{Priority, Snapshot, Task, TaskInput, View};
use focus_island::paths;
use focus_island::settings::{Settings, Theme};
use focus_island::timer::{FocusTimer, TimerState};
use focus_island::timeutil::*;
use focus_island::worker::{Cmd, Event, Worker};

use crate::platform;
use crate::{AppWindow, Nav, Pal, TaskRow};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Collapsed = 0,
    Expanded = 1,
    Reminder = 2,
    Settings = 3,
    Edit = 4,
}

enum Notice {
    Task(Task),
    Focus {
        task_id: Option<i64>,
        minutes: u32,
        title: String,
    },
}

struct State {
    settings: Settings,
    timer: FocusTimer,
    timer_task_title: String,
    snapshot: Option<Snapshot>,
    view: View,
    mode: Mode,
    /// Where to go back to when the reminder banner is dismissed.
    return_mode: Mode,
    queue: VecDeque<Notice>,
    current: Option<Notice>,
    editing: Option<i64>,
    confirm: Option<i64>,
    error: String,
    info: String,
}

pub struct App {
    ui: slint::Weak<AppWindow>,
    worker: RefCell<Option<Worker>>,
    st: RefCell<State>,
    tick: Timer,
    clock: Timer,
    startup_place: Timer,
}

thread_local! {
    static APP: RefCell<Option<Rc<App>>> = const { RefCell::new(None) };
}

/// Run `f` with the app if it exists (callbacks can outlive it during shutdown).
pub fn with_app(f: impl FnOnce(&Rc<App>)) {
    let app = APP.with(|a| a.borrow().clone());
    if let Some(app) = app {
        f(&app);
    }
}

/// Deliver a worker event to the UI thread.
pub fn dispatch(e: Event) {
    let _ = slint::invoke_from_event_loop(move || with_app(|a| a.on_event(e)));
}

/// Stop the worker (after the event loop ends). Everything is already committed.
pub fn shutdown() {
    let app = APP.with(|a| a.borrow_mut().take());
    if let Some(app) = app {
        let w = app.worker.borrow_mut().take();
        if let Some(w) = w {
            w.shutdown();
        }
    }
}

pub fn install(
    ui: &AppWindow,
    worker: Worker,
    settings: Settings,
    startup_notice: Option<String>,
    reset_position: bool,
) {
    let mut settings = settings;
    if reset_position {
        settings.anchor_x = None;
        settings.anchor_y = None;
    }
    let mode = if settings.start_collapsed {
        Mode::Collapsed
    } else {
        Mode::Expanded
    };
    let app = Rc::new(App {
        ui: ui.as_weak(),
        worker: RefCell::new(Some(worker)),
        st: RefCell::new(State {
            timer: FocusTimer::new(settings.focus_minutes),
            timer_task_title: String::new(),
            snapshot: None,
            view: View::Today,
            mode,
            return_mode: mode,
            queue: VecDeque::new(),
            current: None,
            editing: None,
            confirm: None,
            error: startup_notice.unwrap_or_default(),
            info: String::new(),
            settings,
        }),
        tick: Timer::default(),
        clock: Timer::default(),
        startup_place: Timer::default(),
    });
    APP.with(|a| *a.borrow_mut() = Some(app.clone()));
    wire(ui);
    app.apply_settings_to_ui();
    app.refresh();
    if app.st.borrow().mode != Mode::Collapsed {
        app.schedule_clock();
    }
    if reset_position {
        app.send(Cmd::SaveSettings(app.st.borrow().settings.clone()));
    }
}

fn wire(ui: &AppWindow) {
    ui.global::<Nav>().on_escape(|| with_app(|a| a.escape()));
    ui.global::<Nav>()
        .on_shortcut(|k| with_app(|a| a.shortcut(k.as_str())));

    ui.on_expand(|| with_app(|a| a.expand()));
    ui.on_collapse(|| with_app(|a| a.collapse()));
    ui.on_open_settings(|| with_app(|a| a.open_settings()));
    ui.on_close_settings(|| with_app(|a| a.set_mode(Mode::Expanded)));
    ui.on_quick_add(|t| with_app(|a| a.quick_add(t.as_str())));
    ui.on_toggle_done(|id, done| {
        with_app(|a| a.send(Cmd::SetCompleted(i64::from(id), done)));
    });
    ui.on_ask_delete(|id| with_app(|a| a.set_confirm(Some(i64::from(id)))));
    ui.on_confirm_delete(|id| with_app(|a| a.delete(i64::from(id))));
    ui.on_cancel_delete(|| with_app(|a| a.set_confirm(None)));
    ui.on_begin_edit(|id| with_app(|a| a.send(Cmd::LoadTask(i64::from(id)))));
    ui.on_save_edit(|| with_app(|a| a.save_edit()));
    ui.on_cancel_edit(|| with_app(|a| a.cancel_edit()));
    ui.on_set_view(|v| with_app(|a| a.set_view(View::from_index(v))));
    ui.on_timer_start(|| with_app(|a| a.timer_start()));
    ui.on_timer_pause(|| with_app(|a| a.timer_pause()));
    ui.on_timer_resume(|| with_app(|a| a.timer_resume()));
    ui.on_timer_reset(|| with_app(|a| a.timer_reset()));
    ui.on_timer_preset(|m| with_app(|a| a.timer_preset(m as u32)));
    ui.on_timer_custom(|t| with_app(|a| a.timer_custom(t.as_str())));
    ui.on_timer_attach(|id| with_app(|a| a.timer_attach(i64::from(id))));
    ui.on_rem_complete(|| with_app(|a| a.rem_complete()));
    ui.on_rem_snooze(|| with_app(|a| a.rem_snooze()));
    ui.on_rem_dismiss(|| with_app(|a| a.rem_dismiss()));
    ui.on_setting_changed(|| with_app(|a| a.setting_changed()));
    ui.on_export_data(|| with_app(|a| a.export()));
    ui.on_reset_position(|| with_app(|a| a.reset_position()));
    ui.on_quit_app(|| {
        let _ = slint::quit_event_loop();
    });
    ui.on_drag_window(|| with_app(|a| a.drag()));
    ui.on_dismiss_error(|| with_app(|a| a.dismiss_messages()));
}

fn theme_palette(ui: &AppWindow, theme: Theme, contrast: bool) {
    use slint::Color as C;
    let p = ui.global::<Pal>();
    let rgb = |h: u32| C::from_rgb_u8((h >> 16) as u8, (h >> 8) as u8, h as u8);
    match theme {
        Theme::Black | Theme::Graphite => {
            let g = theme == Theme::Graphite;
            p.set_bg(rgb(if g { 0x1c1c1e } else { 0x000000 }));
            p.set_panel(rgb(if g { 0x2c2c2e } else { 0x1c1c1e }));
            p.set_line(rgb(if g { 0x3a3a3c } else { 0x2c2c2e }));
            p.set_fg(rgb(0xffffff));
            p.set_dim(rgb(if contrast { 0xdcdcdc } else { 0x98989f }));
            p.set_accent(rgb(0x0a6cf0));
            p.set_danger(rgb(if contrast { 0xff6b61 } else { 0xff453a }));
            p.set_warn(rgb(0xff9f0a));
            p.set_ok(rgb(0x30d158));
            p.set_on_ok(rgb(0x000000));
        }
        Theme::Light => {
            p.set_bg(rgb(0xf5f5f7));
            p.set_panel(rgb(0xe5e5ea));
            p.set_line(rgb(if contrast { 0x8e8e93 } else { 0xd1d1d6 }));
            p.set_fg(rgb(0x111114));
            p.set_dim(rgb(if contrast { 0x2c2c2e } else { 0x5f5f66 }));
            p.set_accent(rgb(0x0058c7));
            p.set_danger(rgb(0xc4271f));
            p.set_warn(rgb(0x9a5b00));
            p.set_ok(rgb(0x1e7e34));
            p.set_on_ok(rgb(0xffffff));
        }
    }
    p.set_on_accent(rgb(0xffffff));
    let border = match (contrast, theme) {
        (false, _) => C::from_argb_u8(0, 0, 0, 0),
        (true, Theme::Light) => rgb(0x000000),
        (true, _) => rgb(0xffffff),
    };
    p.set_border(border);
}

impl App {
    fn ui(&self) -> Option<AppWindow> {
        self.ui.upgrade()
    }

    fn send(&self, cmd: Cmd) {
        if let Some(w) = self.worker.borrow().as_ref() {
            w.send(cmd);
        }
    }

    fn save_settings(&self) {
        let s = self.st.borrow().settings.clone();
        self.send(Cmd::SaveSettings(s));
    }

    // ---- presentation -------------------------------------------------------------------

    /// Push all state to the UI. Slint ignores writes that do not change a value, so calling
    /// this is cheap and does not cause repaints by itself.
    fn refresh(&self) {
        let Some(ui) = self.ui() else { return };
        let st = self.st.borrow();
        let now = now_ts();
        let now_ms = now_ms();
        let s = &st.settings;

        ui.set_mode(st.mode as i32);
        ui.set_error_text(SharedString::from(st.error.as_str()));
        ui.set_info_text(SharedString::from(st.info.as_str()));
        ui.set_confirm_id(st.confirm.map_or(-1, |v| v as i32));
        ui.set_view(st.view.index());
        ui.set_date_text(format_header_date(&Local, now).into());
        ui.set_clock_text(format_time(&Local, now, s.clock_24h).into());
        ui.set_snooze_minutes(s.snooze_minutes as i32);

        if let Some(snap) = &st.snapshot {
            let rows: Vec<TaskRow> = snap
                .tasks
                .iter()
                .map(|t| TaskRow {
                    id: t.id as i32,
                    title: t.title.as_str().into(),
                    due: t
                        .due_at
                        .map(|d| format_due_label(&Local, d, now, s.clock_24h))
                        .unwrap_or_default()
                        .into(),
                    priority: t.priority.as_i64() as i32,
                    done: t.is_completed(),
                    overdue: t.due_at.is_some_and(|d| d < now),
                })
                .collect();
            ui.set_tasks(ModelRc::new(VecModel::from(rows)));
            ui.set_n_today(snap.summary.today_open as i32);
            ui.set_n_upcoming(snap.summary.upcoming as i32);
            ui.set_n_overdue(snap.summary.overdue as i32);
            ui.set_persistent(snap.persistent);
        }

        // timer
        let t = &st.timer;
        ui.set_timer_state(match t.state() {
            TimerState::Idle => 0,
            TimerState::Running { .. } => 1,
            TimerState::Paused { .. } => 2,
            TimerState::Finished { .. } => 3,
        });
        ui.set_timer_text(t.display(now_ms).into());
        ui.set_timer_progress(t.progress(now_ms));
        ui.set_timer_minutes(t.minutes() as i32);
        ui.set_timer_task(st.timer_task_title.as_str().into());

        // reminder banner
        match &st.current {
            Some(Notice::Task(task)) => {
                ui.set_rem_is_focus(false);
                ui.set_rem_can_complete(true);
                ui.set_rem_title(task.title.as_str().into());
                let when = match task.due_at {
                    Some(d) => format!("Due {}", format_due_label(&Local, d, now, s.clock_24h)),
                    None => "Reminder".to_string(),
                };
                ui.set_rem_time(when.into());
            }
            Some(Notice::Focus {
                task_id,
                minutes,
                title,
            }) => {
                ui.set_rem_is_focus(true);
                ui.set_rem_can_complete(task_id.is_some());
                ui.set_rem_title("Focus session complete".into());
                let detail = if title.is_empty() {
                    format!("{minutes} minutes")
                } else {
                    format!("{minutes} minutes · {title}")
                };
                ui.set_rem_time(detail.into());
            }
            None => {}
        }
        ui.set_rem_more(
            if st.queue.is_empty() {
                String::new()
            } else {
                format!("+{} more", st.queue.len())
            }
            .into(),
        );

        // collapsed pill
        let (kind, text) = pill(&st, now, now_ms);
        ui.set_pill_kind(kind);
        ui.set_pill_text(text.into());
    }

    fn apply_settings_to_ui(&self) {
        let Some(ui) = self.ui() else { return };
        let st = self.st.borrow();
        let s = &st.settings;
        theme_palette(&ui, s.theme, s.high_contrast);
        ui.set_on_top(s.always_on_top);
        ui.set_s_startup(s.launch_on_startup);
        ui.set_s_collapsed(s.start_collapsed);
        ui.set_s_top(s.always_on_top);
        ui.set_s_sound(s.reminder_sound);
        ui.set_s_focus_steal(s.reminder_takes_focus);
        ui.set_s_clock24(s.clock_24h);
        ui.set_s_contrast(s.high_contrast);
        ui.set_s_wake(s.scheduled_wake);
        ui.set_s_theme(s.theme.index());
        ui.set_s_focus_min(s.focus_minutes as i32);
        ui.set_s_snooze_min(s.snooze_minutes as i32);
        let note = if cfg!(windows) {
            "The Task Scheduler option only starts Focus Island at the next reminder time; it \
             can be turned off here at any time."
        } else {
            "Start-with-Windows and Task Scheduler options only work on Windows."
        };
        ui.set_s_note(note.into());
    }

    /// Place the window at its remembered or default position. Returns false if the native
    /// window is not created yet.
    pub fn place_window(&self) -> bool {
        let Some(ui) = self.ui() else { return true };
        let anchor = {
            let s = &self.st.borrow().settings;
            s.anchor_x.zip(s.anchor_y)
        };
        platform::place(ui.window(), anchor)
    }

    /// At startup the native window appears only once the event loop runs: try again shortly
    /// (50 ms steps, at most 2 s) instead of blocking.
    pub fn place_window_soon(self: &Rc<Self>) {
        let tries = Rc::new(std::cell::Cell::new(0u32));
        self.startup_place
            .start(TimerMode::Repeated, Duration::from_millis(50), move || {
                tries.set(tries.get() + 1);
                with_app(|a| {
                    if a.place_window() || tries.get() >= 40 {
                        a.startup_place.stop();
                    }
                });
            });
    }

    // ---- modes -----------------------------------------------------------------------------

    fn set_mode(&self, mode: Mode) {
        {
            let mut st = self.st.borrow_mut();
            st.mode = mode;
            if mode != Mode::Expanded {
                st.confirm = None;
            }
        }
        if mode == Mode::Collapsed {
            self.clock.stop();
        } else {
            self.schedule_clock();
        }
        self.refresh();
        // A reminder that arrived while editing is shown as soon as editing ends.
        if matches!(mode, Mode::Collapsed | Mode::Expanded | Mode::Settings) {
            self.show_next_notice();
        }
    }

    /// Esc steps back one level: reminder -> dismiss, form/settings -> panel, panel -> pill.
    fn escape(&self) {
        let mode = self.st.borrow().mode;
        match mode {
            Mode::Reminder => self.rem_dismiss(),
            Mode::Edit => self.cancel_edit(),
            Mode::Settings => self.set_mode(Mode::Expanded),
            Mode::Expanded => self.collapse(),
            Mode::Collapsed => {}
        }
    }

    fn shortcut(&self, key: &str) {
        let mode = self.st.borrow().mode;
        match key {
            "n" => {
                if mode == Mode::Collapsed {
                    self.expand();
                }
                if matches!(self.st.borrow().mode, Mode::Expanded) {
                    if let Some(ui) = self.ui() {
                        ui.invoke_focus_add();
                    }
                }
            }
            "," => {
                if matches!(mode, Mode::Collapsed | Mode::Expanded) {
                    self.expand();
                    self.open_settings();
                }
            }
            "t" => {
                let (running, paused) = {
                    let st = self.st.borrow();
                    (st.timer.is_running(), st.timer.is_paused())
                };
                if running {
                    self.timer_pause();
                } else if paused {
                    self.timer_resume();
                } else if !self.st.borrow().timer.is_finished() {
                    self.timer_start();
                }
            }
            _ => {}
        }
    }

    fn expand(&self) {
        let _ = self.place_window();
        self.send(Cmd::Refresh);
        self.set_mode(Mode::Expanded);
    }

    fn collapse(&self) {
        self.remember_anchor();
        self.set_mode(Mode::Collapsed);
    }

    fn open_settings(&self) {
        self.set_mode(Mode::Settings);
    }

    fn remember_anchor(&self) {
        let Some(ui) = self.ui() else { return };
        let (x, y) = platform::anchor_of(ui.window());
        let changed = {
            let mut st = self.st.borrow_mut();
            let new = (Some(x), Some(y));
            let old = (st.settings.anchor_x, st.settings.anchor_y);
            st.settings.anchor_x = new.0;
            st.settings.anchor_y = new.1;
            old != new
        };
        if changed {
            self.save_settings();
        }
    }

    fn drag(&self) {
        if let Some(ui) = self.ui() {
            platform::start_drag(ui.window());
        }
    }

    fn reset_position(&self) {
        {
            let mut st = self.st.borrow_mut();
            st.settings.anchor_x = None;
            st.settings.anchor_y = None;
        }
        self.save_settings();
        let _ = self.place_window();
    }

    fn dismiss_messages(&self) {
        {
            let mut st = self.st.borrow_mut();
            st.error.clear();
            st.info.clear();
        }
        self.refresh();
    }

    fn set_error(&self, msg: String) {
        self.st.borrow_mut().error = msg;
        self.refresh();
    }

    /// Re-arm the minute-boundary timer that keeps the open panel's clock right.
    fn schedule_clock(&self) {
        let now = Local::now();
        let ms = 60_000 - u64::from(now.second()) * 1000 - u64::from(now.nanosecond() / 1_000_000);
        self.clock.start(
            TimerMode::SingleShot,
            Duration::from_millis(ms.max(250) + 20),
            || {
                with_app(|a| {
                    if a.st.borrow().mode != Mode::Collapsed {
                        a.refresh();
                        a.schedule_clock();
                    }
                });
            },
        );
    }

    // ---- tasks ---------------------------------------------------------------------------

    fn set_view(&self, v: View) {
        self.st.borrow_mut().view = v;
        self.send(Cmd::SetView(v));
        self.refresh();
    }

    fn set_confirm(&self, id: Option<i64>) {
        self.st.borrow_mut().confirm = id;
        self.refresh();
    }

    fn quick_add(&self, text: &str) {
        let qa = focus_island::quickadd::parse(text);
        self.st.borrow_mut().error.clear();
        self.send(Cmd::Create(qa.into_input(&Local, now_ts())));
    }

    fn delete(&self, id: i64) {
        {
            let mut st = self.st.borrow_mut();
            st.confirm = None;
            if st.timer.task_id() == Some(id) {
                st.timer.set_task(None);
                st.timer_task_title.clear();
            }
        }
        self.send(Cmd::Delete(id));
        self.refresh();
    }

    fn begin_edit(&self, t: Task) {
        let Some(ui) = self.ui() else { return };
        self.st.borrow_mut().editing = Some(t.id);
        ui.set_edit_title(t.title.as_str().into());
        ui.set_edit_notes(t.description.as_deref().unwrap_or("").into());
        ui.set_edit_date(
            t.due_at
                .map(|d| format_date_field(&Local, d))
                .unwrap_or_default()
                .into(),
        );
        ui.set_edit_time(
            t.due_at
                .map(|d| format_time_field(&Local, d))
                .unwrap_or_default()
                .into(),
        );
        ui.set_edit_priority(t.priority.as_i64() as i32);
        ui.set_edit_reminder(t.reminder_enabled);
        ui.set_edit_lead(t.remind_lead_min as i32);
        ui.set_edit_error("".into());
        self.set_mode(Mode::Edit);
    }

    fn save_edit(&self) {
        let Some(ui) = self.ui() else { return };
        let Some(id) = self.st.borrow().editing else {
            return;
        };
        let due = match parse_due(
            &Local,
            ui.get_edit_date().as_str(),
            ui.get_edit_time().as_str(),
        ) {
            Ok(d) => d,
            Err(e) => {
                ui.set_edit_error(e.to_string().into());
                return;
            }
        };
        let input = TaskInput {
            title: ui.get_edit_title().to_string(),
            description: Some(ui.get_edit_notes().to_string()),
            due_at: due,
            priority: Priority::from_i64(i64::from(ui.get_edit_priority())),
            reminder_enabled: ui.get_edit_reminder(),
            remind_lead_min: i64::from(ui.get_edit_lead()),
        };
        self.send(Cmd::Update(id, input));
    }

    fn cancel_edit(&self) {
        self.st.borrow_mut().editing = None;
        self.set_mode(Mode::Expanded);
    }

    // ---- focus timer ---------------------------------------------------------------------

    fn arm_tick(&self) {
        if self.st.borrow().timer.is_running() {
            self.tick
                .start(TimerMode::Repeated, Duration::from_secs(1), || {
                    with_app(|a| a.on_tick());
                });
        } else {
            self.tick.stop();
        }
    }

    fn timer_start(&self) {
        self.st.borrow_mut().timer.start(now_ms());
        self.arm_tick();
        self.refresh();
    }

    fn timer_pause(&self) {
        self.st.borrow_mut().timer.pause(now_ms());
        self.arm_tick();
        self.refresh();
    }

    fn timer_resume(&self) {
        self.st.borrow_mut().timer.resume(now_ms());
        self.arm_tick();
        self.refresh();
    }

    fn timer_reset(&self) {
        {
            let mut st = self.st.borrow_mut();
            st.timer.reset();
            let m = st.settings.focus_minutes;
            st.timer.set_minutes(m);
            st.timer.set_task(None);
            st.timer_task_title.clear();
        }
        self.arm_tick();
        self.refresh();
    }

    fn timer_preset(&self, minutes: u32) {
        self.st.borrow_mut().timer.set_minutes(minutes);
        self.refresh();
    }

    fn timer_custom(&self, text: &str) {
        match text.trim().parse::<u32>() {
            Ok(m) if (1..=240).contains(&m) => {
                self.st.borrow_mut().timer.set_minutes(m);
                self.refresh();
            }
            _ => self.set_error(focus_island::models::ValidationError::InvalidDuration.to_string()),
        }
    }

    fn timer_attach(&self, id: i64) {
        let title = {
            let st = self.st.borrow();
            st.snapshot
                .as_ref()
                .and_then(|s| s.tasks.iter().find(|t| t.id == id))
                .map(|t| t.title.clone())
                .unwrap_or_default()
        };
        let idle = {
            let mut st = self.st.borrow_mut();
            st.timer.set_task(Some(id));
            st.timer_task_title = title;
            !st.timer.is_active()
        };
        if idle {
            self.st.borrow_mut().timer.reset();
            self.timer_start();
        } else {
            self.refresh();
        }
    }

    fn on_tick(&self) {
        let finished = self.st.borrow_mut().timer.poll(now_ms());
        if let Some(done) = finished {
            self.arm_tick();
            self.send(Cmd::RecordFocus {
                task_id: done.task_id,
                started_at: done.started_ms / 1000,
                ended_at: done.ended_ms / 1000,
                planned_secs: done.planned_secs,
            });
            let title = self.st.borrow().timer_task_title.clone();
            self.push_notice(Notice::Focus {
                task_id: done.task_id,
                minutes: (done.planned_secs / 60) as u32,
                title,
            });
        }
        self.refresh();
    }

    // ---- reminders ---------------------------------------------------------------------------

    fn push_notice(&self, n: Notice) {
        self.st.borrow_mut().queue.push_back(n);
        self.show_next_notice();
    }

    fn show_next_notice(&self) {
        let (play, focus) = {
            let mut st = self.st.borrow_mut();
            // One at a time, and never over a form the user is typing in.
            if st.current.is_some() || st.mode == Mode::Edit {
                drop(st);
                self.refresh();
                return;
            }
            let Some(n) = st.queue.pop_front() else {
                return;
            };
            st.current = Some(n);
            st.return_mode = st.mode;
            st.mode = Mode::Reminder;
            (st.settings.reminder_sound, st.settings.reminder_takes_focus)
        };
        self.clock.stop();
        if play {
            platform::windows::play_reminder_sound_if_supported();
        }
        self.refresh();
        if focus {
            if let Some(ui) = self.ui() {
                platform::request_focus(ui.window());
            }
        }
    }

    fn finish_notice(&self) {
        let back = {
            let mut st = self.st.borrow_mut();
            st.current = None;
            st.return_mode
        };
        let back = if back == Mode::Reminder {
            Mode::Collapsed
        } else {
            back
        };
        self.st.borrow_mut().mode = back;
        if back != Mode::Collapsed {
            self.schedule_clock();
        }
        self.refresh();
        self.show_next_notice();
    }

    fn rem_complete(&self) {
        let id = match &self.st.borrow().current {
            Some(Notice::Task(t)) => Some(t.id),
            Some(Notice::Focus { task_id, .. }) => *task_id,
            None => None,
        };
        if let Some(id) = id {
            self.send(Cmd::SetCompleted(id, true));
        }
        self.finish_notice();
    }

    fn rem_snooze(&self) {
        let cmd = {
            let st = self.st.borrow();
            match &st.current {
                Some(Notice::Task(t)) => t.effective_fire_at().map(|at| Cmd::Snooze {
                    task_id: t.id,
                    scheduled_for: at,
                    minutes: st.settings.snooze_minutes,
                }),
                _ => None,
            }
        };
        if let Some(c) = cmd {
            self.send(c);
        }
        self.finish_notice();
    }

    fn rem_dismiss(&self) {
        let cmd = match &self.st.borrow().current {
            Some(Notice::Task(t)) => t.effective_fire_at().map(|at| Cmd::Dismiss {
                task_id: t.id,
                scheduled_for: at,
            }),
            _ => None,
        };
        if let Some(c) = cmd {
            self.send(c);
        }
        self.finish_notice();
    }

    // ---- settings ---------------------------------------------------------------------------

    fn setting_changed(&self) {
        let Some(ui) = self.ui() else { return };
        let old = self.st.borrow().settings.clone();
        let new = Settings {
            launch_on_startup: ui.get_s_startup(),
            start_collapsed: ui.get_s_collapsed(),
            always_on_top: ui.get_s_top(),
            reminder_sound: ui.get_s_sound(),
            reminder_takes_focus: ui.get_s_focus_steal(),
            clock_24h: ui.get_s_clock24(),
            high_contrast: ui.get_s_contrast(),
            scheduled_wake: ui.get_s_wake(),
            theme: Theme::from_index(ui.get_s_theme()),
            focus_minutes: ui.get_s_focus_min() as u32,
            snooze_minutes: ui.get_s_snooze_min() as u32,
            ..old.clone()
        }
        .sanitized();

        if new.launch_on_startup != old.launch_on_startup {
            let exe = std::env::current_exe().unwrap_or_default();
            if let Err(e) = platform::windows_set_startup(new.launch_on_startup, &exe) {
                self.set_error(e);
                // The toggle did not take effect: show the real state again.
                let mut keep = new.clone();
                keep.launch_on_startup = old.launch_on_startup;
                self.st.borrow_mut().settings = keep;
                self.apply_settings_to_ui();
                self.save_settings();
                return;
            }
        }
        {
            let mut st = self.st.borrow_mut();
            if new.focus_minutes != old.focus_minutes {
                st.timer.set_minutes(new.focus_minutes);
            }
            st.settings = new;
        }
        self.apply_settings_to_ui();
        self.save_settings();
        self.refresh();
    }

    fn export(&self) {
        let dir = {
            let s = &self.st.borrow().settings;
            if s.export_dir.is_empty() {
                paths::default_export_dir()
            } else {
                s.export_dir.clone().into()
            }
        };
        self.send(Cmd::Export(dir));
    }

    // ---- worker events ---------------------------------------------------------------------

    fn on_event(&self, e: Event) {
        match e {
            Event::Snapshot(s) => {
                {
                    let mut st = self.st.borrow_mut();
                    // The timer's linked task may have been completed/removed elsewhere; the
                    // title text stays as it is for the session's duration.
                    st.view = s.view;
                    st.snapshot = Some(s);
                }
                self.refresh();
            }
            Event::Reminders { show, missed } => {
                if missed > 0 {
                    let msg = if missed == 1 {
                        "1 reminder came due while Focus Island was closed. See Overdue."
                            .to_string()
                    } else {
                        format!("{missed} reminders came due while Focus Island was closed. See Overdue.")
                    };
                    self.st.borrow_mut().info = msg;
                }
                for t in show {
                    self.st.borrow_mut().queue.push_back(Notice::Task(t));
                }
                self.show_next_notice();
                self.refresh();
            }
            Event::TaskLoaded(t) => self.begin_edit(t),
            Event::Saved(_) => {
                self.st.borrow_mut().error.clear();
                let editing = self.st.borrow().mode == Mode::Edit;
                if editing {
                    self.st.borrow_mut().editing = None;
                    self.set_mode(Mode::Expanded);
                }
            }
            Event::Deleted(_) => {}
            Event::Rejected(msg) => {
                if self.st.borrow().mode == Mode::Edit {
                    if let Some(ui) = self.ui() {
                        ui.set_edit_error(msg.into());
                    }
                } else {
                    self.set_error(msg);
                }
            }
            Event::Error(msg) => self.set_error(msg),
            Event::Exported(p) => {
                self.st.borrow_mut().info = format!(
                    "Exported to {}",
                    p.db_path.parent().unwrap_or(&p.db_path).display()
                );
                self.refresh();
            }
        }
    }
}

/// What the collapsed pill shows.
fn pill(st: &State, now: i64, now_ms: i64) -> (i32, String) {
    let t = &st.timer;
    let s = &st.settings;
    match t.state() {
        TimerState::Running { .. } => {
            let mut text = t.display(now_ms);
            if !st.timer_task_title.is_empty() {
                text = format!("{text} · {}", st.timer_task_title);
            }
            return (2, text);
        }
        TimerState::Paused { .. } => return (4, format!("Paused · {}", t.display(now_ms))),
        TimerState::Finished { .. } => return (4, "Focus session complete".to_string()),
        TimerState::Idle => {}
    }
    if let Some(snap) = &st.snapshot {
        if let Some(next) = &snap.summary.next {
            let text = match next.due_at {
                Some(d) => format!("{} {}", format_time(&Local, d, s.clock_24h), next.title),
                None => next.title.clone(),
            };
            return (1, text);
        }
        if snap.summary.overdue > 0 {
            return (3, format!("{} overdue", snap.summary.overdue));
        }
    }
    let _ = now;
    (0, "Focus Island".to_string())
}
