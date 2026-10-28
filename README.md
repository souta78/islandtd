# Focus Island

A small floating "island" for Windows: a black pill at the top centre of your screen that
expands into a todo list, reminders and a focus timer. Rust + Slint + SQLite. No network
access, no telemetry, no background services.

> **Read this first — verification status.** This code was written and tested on a **Linux**
> machine. A Windows x64 binary was **not built or run** there (the Windows Rust target could
> not be downloaded in that sandbox). Everything below marked *Windows-only* is implemented
> but **unverified on Windows**. See [Windows testing checklist](#windows-testing-checklist)
> and the included CI workflow, which builds and tests on a real Windows runner.

## Build and run (Windows 10/11 x64)

1. Install Rust (stable, MSVC toolchain) from <https://rustup.rs> and the *Visual Studio Build
   Tools* ("Desktop development with C++"). Requires Rust 1.92 or newer.
2. In this folder:

   ```powershell
   cargo build --release --locked
   .\target\release\focus-island.exe
   ```

The exe is self-contained (SQLite is bundled). To distribute, copy
`target\release\focus-island.exe` anywhere; user data lives separately in
`%LOCALAPPDATA%\FocusIsland`, so replacing the exe never touches your tasks.

A GitHub Actions workflow (`.github/workflows/windows.yml`) runs fmt, clippy, tests and the
release build on `windows-latest` and uploads the exe. It has not been executed yet.

Command-line options: `--reset-position`, `--software-renderer`, `--wake`, `--version`.

## Using it

* Click the pill to expand; `Esc` steps back (reminder → dismiss, editor/settings → panel,
  panel → pill). Drag the header or the pill to move it; the position is remembered.
* **Quick add** (`Ctrl+N` focuses it): `Call mom @15:30 !!` → due 15:30 (tomorrow if already
  past) with a reminder, medium priority. `@tomorrow` = tomorrow 09:00; `!` / `!!` / `!!!` =
  low / medium / high. Only trailing tokens are interpreted.
* Tabs: Today / Upcoming / Overdue, sorted by due time then priority. Hover a row for
  *focus on this task*, *edit*, *delete* (delete asks to confirm).
* **Focus timer:** 25 / 50 / custom minutes (1–240), start, pause, resume, reset. `Ctrl+T`
  toggles it. Completion shows a banner and is recorded.
* `Ctrl+,` opens settings.
* **Reminders:** when one is due the island expands to a banner with *Complete*, *Snooze*,
  *Dismiss*. It does not take keyboard focus unless you enable that in Settings.

## Data, backup, restore

* Database: `%LOCALAPPDATA%\FocusIsland\focus-island.db` (SQLite, WAL mode).
* Settings → **Export / back up…** writes a consistent `.db` copy and a readable `.json`
  to `Documents\Focus Island`. To restore, close the app and copy a backup over
  `focus-island.db`.
* A corrupt database is renamed `*.corrupt-<time>` (never deleted) and a new one is created.
  A database from a *newer* version is left untouched and the app runs in memory only.
* Before any schema upgrade a `*.pre-v<N>.bak` copy is written.

## Reminder behaviour (what works when)

| Situation | Reminders fire? |
|---|---|
| Island collapsed (pill) | Yes — the process is running. |
| Window minimized / behind others | Yes. |
| App process closed or killed | **No**, unless the optional Task Scheduler option below is on. |

There is **no system-tray mode**: it was not implemented (see below), so the window is never
hidden. Closing it (Alt+F4 or Settings → Quit) ends the process.

* **Missed reminders:** on start-up, reminders up to 15 minutes late still pop up; older ones
  are recorded as missed and summarised by one message — they stay visible under *Overdue*.
  No flood after a long absence.
* **No duplicates:** each (task, scheduled time) is stored with a UNIQUE key, so restarting or
  repeated checks cannot alert twice. Snooze creates a new scheduled time.
* **Scheduler:** one background thread sleeps until the next deadline (capped at 60 s / 5 min
  so a clock change or suspend delays an alert by at most that long). No polling.
* **Optional Task Scheduler (Windows-only, off by default):** Settings → *Start app at next
  reminder*. Registers one task, `FocusIsland\Reminder Wake`, whose trigger is moved to the
  next reminder time; the app is started with `--wake`. Turning the setting off (or having no
  pending reminder) deletes it. Remove it manually with
  `schtasks /Delete /TN "FocusIsland\Reminder Wake" /F`. **Not tested on Windows.**
* **Start with Windows (Windows-only, off by default):** writes/removes
  `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\FocusIsland`. Not tested on Windows.

## Implemented / not implemented

Implemented and exercised on Linux (tests and/or running the real UI):
task CRUD, validation, due/priority sorting, Today/Upcoming/Overdue, quick-add,
SQLite persistence with versioned migrations and corruption recovery, reminders with
snooze/dismiss/complete, de-duplication, missed-reminder policy, deadline-based focus timer
(optionally linked to a task), settings persistence, export/backup, themes (Black, Graphite,
Light) + high contrast, keyboard shortcuts, multi-monitor-aware placement.

Implemented but **Windows-only and unverified**: always-on-top, transparent borderless window
and DPI behaviour, start-with-Windows, Task Scheduler wake, reminder sound
(`rundll32 user32.dll,MessageBeep`; the exact tone is not guaranteed).

**Not implemented:**
* System tray icon / hide-to-tray (not done rather than shipped unverified).
* UI to change the export folder and the missed-reminder window (stored settings
  `export_dir`, `missed_window_min` exist; defaults are used).
* Screen-reader testing (accessibility labels are set; not tested with Narrator).
* A "new task" form (new tasks come from quick-add; the form edits existing ones).
* Recovery if the island is dragged off-screen *while running* (it is re-validated against
  connected monitors every time it expands and at start-up; `--reset-position` and Settings →
  *Reset position* also restore it).

## Performance (measured — Linux only)

Measured on a Linux VM inside Xvfb with Mesa software OpenGL (llvmpipe), release build,
30 s samples of the app process only. **These are not Windows numbers.** The OpenGL figures
include Mesa's CPU-based GL driver, which is far heavier than a hardware GPU driver, so
Windows will differ — measure it there.

| | OpenGL (default) | `--software-renderer` |
|---|---|---|
| Idle CPU, collapsed | 0.03 % | 0.03 % |
| Idle CPU, expanded | 0.03 % | 0.00 % |
| CPU, collapsed with timer running | 0.37 % | 0.07 % |
| Memory (RSS) collapsed / expanded | 115 / 131 MiB | 16 / 18 MiB |
| Private memory collapsed | 75 MiB | 13 MiB |
| CPU spent expanding (one-off) | ~420 ms | ~20 ms |
| Threads | 9 | 4 |
| Window mapped after launch | 74 ms – 1.8 s (cold run slow) | 56–88 ms |
| Binary size | 12.1 MiB | (same binary) |

The idle numbers confirm there is no polling or continuous rendering. Animation smoothness
(60 fps) was **not** measured; under software GL the expand cost ~420 ms of CPU for a 200 ms
animation, so nothing can be concluded about GPU machines.

**Renderer choice (important):** the default OpenGL renderer is the one expected to draw
the rounded, shadowed, transparent pill correctly. The much lighter CPU renderer
(`focus-island.exe --software-renderer`) presents through a path that may not honour
per-pixel transparency on Windows, so corners/shadow might draw as a black rectangle. Try it
and keep whichever looks right and uses an acceptable amount of memory.

### Measuring on Windows

Task Manager → Details → *Memory (private working set)* and *CPU* for `focus-island.exe`
(let it sit collapsed for a minute). Or in PowerShell:

```powershell
$p = Get-Process focus-island
$a = $p.TotalProcessorTime; Start-Sleep 60; $p.Refresh()
"CPU % over 60 s: " + [math]::Round(($p.TotalProcessorTime - $a).TotalSeconds / 60 * 100, 2)
"Private MiB: " + [math]::Round($p.PrivateMemorySize64 / 1MB, 1)
"Working set MiB: " + [math]::Round($p.WorkingSet64 / 1MB, 1)
```

Try both `focus-island.exe` and `focus-island.exe --software-renderer`.

## Tests

Run on Linux, results as executed:

| Command | Result |
|---|---|
| `cargo fmt --check` | passed |
| `cargo clippy --all-targets --all-features` | no warnings |
| `cargo test` | 69 passed, 0 failed (26 unit + 5 placement + 23 reminder/scheduler + 15 task/database) |
| `cargo build --release` | succeeded (Linux x86-64 only) |

Covered: task create/edit/complete/delete, validation, persistence across restart, views and
sorting, due-time calculation, snooze, duplicate prevention (including restart), overdue
reconciliation, scheduler thread end-to-end (real 3-second reminder), focus-timer deadline
maths, settings persistence, migrations and rollback, corrupt/newer database handling,
export, SQL-injection text, quick-add parsing, Task Scheduler XML, multi-monitor placement.

The real UI was also driven under Xvfb (click to expand, quick-add, hover actions, edit form,
settings, timer, a live reminder firing, Esc navigation, restart persistence) and inspected
visually; the window manager, transparency compositing and GPU path are not representative
of Windows.

## Windows testing checklist

- [ ] `cargo build --release --locked` succeeds; exe starts, no console window.
- [ ] Pill appears top-centre of the primary monitor with **rounded transparent corners**
      (if black corners appear, the renderer/transparency path needs work).
- [ ] Looks sharp at 100 %, 150 %, 200 % scaling; moving between monitors of different scale.
- [ ] Always-on-top on/off in Settings; fullscreen video / games behave acceptably.
- [ ] Expand/collapse animation is smooth; no flicker.
- [ ] Add task, restart app, task still there.
- [ ] Reminder fires on time; does not steal typing focus; snooze/dismiss/complete work;
      sound plays when enabled.
- [ ] Change the system clock ±1 h: no duplicate or missing alerts beyond the documented cap.
- [ ] Sleep/resume with a running focus timer finishes at the right time.
- [ ] Start with Windows on → entry appears in `reg query HKCU\Software\Microsoft\Windows\CurrentVersion\Run`; off removes it.
- [ ] Task Scheduler option on → task `FocusIsland\Reminder Wake` appears with the next
      reminder time; closing the app and waiting launches it; off removes it.
- [ ] Unplug a monitor with the island on it → it returns on-screen when expanded or with
      `--reset-position`.
- [ ] Idle CPU ≈ 0 % and memory measured with the commands above.

## Project layout

```
src/lib.rs        library root (no UI dependency)
src/models.rs     tasks, validation            src/timeutil.rs  local time, formatting
src/database.rs   SQLite, migrations, CRUD     src/reminders.rs due/snooze/dedupe, wait plan
src/worker.rs     the one background thread    src/timer.rs     focus timer state machine
src/settings.rs   persisted settings           src/quickadd.rs  quick-add parser
src/export.rs     backup/export                src/wake.rs      Task Scheduler XML
src/paths.rs      data folders
src/main.rs       entry, single instance       src/app.rs       UI controller (Slint)
src/platform/     window placement; windows.rs = reg/schtasks/sound (no unsafe)
ui/app.slint      the interface and animations
tests/            integration tests
```

There is no `unsafe` code in this project. Dependencies are pinned (`=`) and `Cargo.lock` is
committed; use `--locked`.

## Known limitations

* Windows build/behaviour unverified (see top).
* The native window is a fixed 424 px wide transparent rectangle that grows downward, so a
  small transparent strip beside the collapsed pill can catch mouse clicks.
* Only one instance runs; launching a second does nothing (the `--wake` launch relies on this).
* Times are stored as UTC seconds and shown in local time; a DST gap time is rejected when
  entered, a DST overlap resolves to the earlier instant.
* Task list views show at most 100 rows.
