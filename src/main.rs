// No console window in release builds on Windows.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app;
mod platform;

use std::fs::OpenOptions;
use std::path::PathBuf;

use focus_island::database::Db;
use focus_island::paths;
use focus_island::settings::Settings;
use focus_island::timeutil::now_ts;
use focus_island::worker::Worker;

slint::include_modules!();

const HELP: &str = "Focus Island\n\nUSAGE: focus-island [--reset-position] [--software-renderer] [--wake] [--version]\n\n  --reset-position  put the island back at the top centre of the primary monitor\n  --software-renderer  use the CPU renderer (lower memory; see README)
  --wake            used by the optional Windows scheduled reminder\n  --version         print the version\n";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "--version") {
        println!("focus-island {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if args.iter().any(|a| a == "--help" || a == "-h") {
        print!("{HELP}");
        return;
    }
    let reset_position = args.iter().any(|a| a == "--reset-position");
    let software = args.iter().any(|a| a == "--software-renderer");

    // Single instance. The OS releases the lock when the process ends, so a crash can never
    // leave a stale lock behind. A second launch (e.g. the scheduled task) just exits.
    let dir = paths::data_dir();
    let _ = std::fs::create_dir_all(&dir);
    let lock_path: PathBuf = dir.join("instance.lock");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path);
    if let Ok(f) = &lock {
        if f.try_lock().is_err() {
            return;
        }
    }

    if let Err(e) = run(reset_position, software) {
        eprintln!("Focus Island failed to start: {e}");
        std::process::exit(1);
    }
    drop(lock);
}

fn run(reset_position: bool, software: bool) -> Result<(), slint::PlatformError> {
    if software {
        slint::BackendSelector::new()
            .backend_name("winit".into())
            .renderer_name("software".into())
            .select()?;
    }
    let opened = Db::open_or_recover(&paths::db_path(), now_ts());
    let settings = Settings::load(&opened.db).unwrap_or_default();

    let ui = AppWindow::new()?;
    let worker = Worker::spawn(
        opened.db,
        settings.clone(),
        Box::new(app::dispatch),
        wake_hook(),
    );
    app::install(&ui, worker, settings, opened.notice, reset_position);

    ui.show()?;
    app::with_app(|a| a.place_window_soon());
    let result = slint::run_event_loop();
    app::shutdown();
    result
}

#[cfg(windows)]
fn wake_hook() -> Option<focus_island::worker::WakeHook> {
    use focus_island::worker::WakeAction;
    let exe = std::env::current_exe().ok()?;
    let dir = paths::data_dir();
    Some(Box::new(move |action| match action {
        WakeAction::Register(at) => {
            platform::windows::register_wake(&exe, &dir, at.max(now_ts() + 60))
        }
        WakeAction::Unregister => platform::windows::unregister_wake(),
    }))
}

#[cfg(not(windows))]
fn wake_hook() -> Option<focus_island::worker::WakeHook> {
    None
}
