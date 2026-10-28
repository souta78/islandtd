//! Windows integration via the operating system's own tools.
//!
//! Every command is built from fixed arguments plus paths the app itself computed (the
//! executable path and the data folder). No user-entered text ever reaches a command line, and
//! nothing goes through a shell. There is no `unsafe` code.
//!
//! Each function is idempotent and reversible, and none runs unless the user turned the
//! matching setting on (or off) in Settings.

use std::path::Path;
use std::process::{Command, Stdio};

use chrono::{Local, TimeZone};

use focus_island::models::Ts;
use focus_island::wake::{encode_utf16le_bom, task_xml, WAKE_TASK_NAME};

const RUN_KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "FocusIsland";

#[cfg(windows)]
fn quiet(cmd: &mut Command) -> &mut Command {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    cmd.creation_flags(CREATE_NO_WINDOW)
}

#[cfg(not(windows))]
fn quiet(cmd: &mut Command) -> &mut Command {
    cmd
}

fn run(mut cmd: Command, what: &str) -> Result<(), String> {
    quiet(&mut cmd).stdin(Stdio::null()).stdout(Stdio::null());
    let out = cmd
        .stderr(Stdio::piped())
        .output()
        .map_err(|e| format!("{what}: {e}"))?;
    if out.status.success() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&out.stderr);
        Err(format!("{what} failed: {}", err.trim()))
    }
}

/// Add or remove the per-user "run at sign-in" entry (HKCU, no administrator rights needed).
pub fn set_startup(enabled: bool, exe: &Path) -> Result<(), String> {
    let mut cmd = Command::new("reg.exe");
    if enabled {
        let value = format!("\"{}\"", exe.display());
        cmd.args(["add", RUN_KEY, "/v", RUN_VALUE, "/t", "REG_SZ", "/d"])
            .arg(value)
            .arg("/f");
        run(cmd, "Enabling start with Windows")
    } else {
        cmd.args(["delete", RUN_KEY, "/v", RUN_VALUE, "/f"]);
        // Deleting a value that is not there is not an error for our purposes.
        let _ = run(cmd, "Disabling start with Windows");
        Ok(())
    }
}

/// Create or replace the one-shot scheduled task that starts the app at `at` (unix seconds).
pub fn register_wake(exe: &Path, xml_dir: &Path, at: Ts) -> Result<(), String> {
    let local = Local
        .timestamp_opt(at, 0)
        .single()
        .ok_or_else(|| "the reminder time is not a valid local time".to_string())?
        .naive_local();
    std::fs::create_dir_all(xml_dir).map_err(|e| e.to_string())?;
    let xml_path = xml_dir.join("wake-task.xml");
    std::fs::write(
        &xml_path,
        encode_utf16le_bom(&task_xml(&exe.display().to_string(), local)),
    )
    .map_err(|e| e.to_string())?;
    let mut cmd = Command::new("schtasks.exe");
    cmd.args(["/Create", "/TN", WAKE_TASK_NAME, "/XML"])
        .arg(&xml_path)
        .arg("/F");
    let r = run(cmd, "Registering the scheduled reminder");
    let _ = std::fs::remove_file(&xml_path);
    r
}

/// Remove the scheduled task. Succeeds if it does not exist.
pub fn unregister_wake() -> Result<(), String> {
    let mut cmd = Command::new("schtasks.exe");
    cmd.args(["/Delete", "/TN", WAKE_TASK_NAME, "/F"]);
    let _ = run(cmd, "Removing the scheduled reminder");
    Ok(())
}

/// Play the standard Windows notification sound.
pub fn play_reminder_sound() {
    let mut cmd = Command::new("rundll32.exe");
    cmd.arg("user32.dll,MessageBeep");
    quiet(&mut cmd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let _ = cmd.spawn().map(|mut c| {
        // Reap the child off-thread so it never lingers as a zombie or blocks the UI.
        std::thread::spawn(move || {
            let _ = c.wait();
        });
    });
}

/// No-op on other operating systems (development builds).
pub fn play_reminder_sound_if_supported() {
    if cfg!(windows) {
        play_reminder_sound();
    }
}
