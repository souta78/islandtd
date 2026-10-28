mod common;

use common::*;
use focus_island::database::{apply_migrations, Db, DbError, MIGRATIONS};
use focus_island::export::export_all;
use focus_island::models::*;
use focus_island::settings::{Settings, Theme};
use focus_island::timeutil::{day_bounds, parse_due};

const NOW_STR: (&str, &str) = ("2026-10-25", "10:00");

fn now() -> Ts {
    at(NOW_STR.0, NOW_STR.1)
}

fn day() -> (Ts, Ts) {
    day_bounds(&tz(), now())
}

// ---- create / edit / complete / delete -------------------------------------------------

#[test]
fn create_edit_complete_delete_roundtrip() {
    let mut db = Db::open_in_memory().unwrap();
    let t = db.create_task(&input("  Write report  "), now()).unwrap();
    assert_eq!(t.title, "Write report");
    assert_eq!(t.created_at, now());
    assert_eq!(t.updated_at, now());
    assert!(!t.is_completed());
    assert!(t.id > 0);

    let mut edit = input("Write final report");
    edit.description = Some("with charts".into());
    edit.priority = Priority::High;
    edit.due_at = Some(at("2026-10-26", "09:00"));
    let e = db.update_task(t.id, &edit, now() + 5).unwrap();
    assert_eq!(e.title, "Write final report");
    assert_eq!(e.description.as_deref(), Some("with charts"));
    assert_eq!(e.priority, Priority::High);
    assert_eq!(e.created_at, now(), "creation time never changes");
    assert_eq!(e.updated_at, now() + 5);

    let done = db.set_completed(t.id, true, now() + 10).unwrap();
    assert_eq!(done.completed_at, Some(now() + 10));
    let reopened = db.set_completed(t.id, false, now() + 20).unwrap();
    assert_eq!(reopened.completed_at, None);
    assert_eq!(reopened.updated_at, now() + 20);

    assert!(db.delete_task(t.id).unwrap());
    assert!(db.get_task(t.id).unwrap().is_none());
    assert!(
        !db.delete_task(t.id).unwrap(),
        "deleting twice is a no-op, not an error"
    );
}

#[test]
fn operations_on_missing_tasks_report_not_found() {
    let mut db = Db::open_in_memory().unwrap();
    assert!(matches!(
        db.update_task(99, &input("x"), now()),
        Err(DbError::NotFound(99))
    ));
    assert!(matches!(
        db.set_completed(99, true, now()),
        Err(DbError::NotFound(99))
    ));
}

// ---- validation ----------------------------------------------------------------------------

#[test]
fn invalid_input_is_rejected_and_nothing_is_stored() {
    let mut db = Db::open_in_memory().unwrap();
    assert!(matches!(
        db.create_task(&input("   "), now()),
        Err(DbError::Validation(ValidationError::EmptyTitle))
    ));
    assert!(matches!(
        db.create_task(&input(&"a".repeat(201)), now()),
        Err(DbError::Validation(ValidationError::TitleTooLong))
    ));
    let mut bad = input("x");
    bad.reminder_enabled = true; // no due date
    assert!(matches!(
        db.create_task(&bad, now()),
        Err(DbError::Validation(ValidationError::ReminderWithoutDue))
    ));
    assert!(matches!(
        db.create_task(&timed("x", now() - 7200, true, 0), now()),
        Err(DbError::Validation(ValidationError::ReminderInPast))
    ));
    assert!(db
        .list_tasks(View::Today, now(), day(), 50)
        .unwrap()
        .is_empty());
    assert_eq!(db.summary(now(), day()).unwrap(), Summary::default());

    // Failed update leaves the original untouched.
    let t = db.create_task(&input("keep"), now()).unwrap();
    assert!(db.update_task(t.id, &input(""), now()).is_err());
    assert_eq!(db.get_task(t.id).unwrap().unwrap().title, "keep");
}

#[test]
fn date_fields_are_validated() {
    let z = tz();
    assert!(parse_due(&z, "2026-13-01", "09:00").is_err());
    assert!(
        parse_due(&z, "2026-02-29", "09:00").is_err(),
        "2026 is not a leap year"
    );
    assert!(parse_due(&z, "tomorrow", "09:00").is_err());
    assert!(parse_due(&z, "2026-10-25", "9am").is_err());
    assert!(parse_due(&z, "2026-10-25", "24:00").is_err());
    assert!(parse_due(&z, "2026-10-25", "23:59").unwrap().is_some());
}

// ---- persistence -------------------------------------------------------------------------

#[test]
fn tasks_and_settings_survive_restart() {
    let dir = TestDir::new("persist");
    let (id_a, id_b);
    {
        let mut db = Db::open(&dir.db()).unwrap();
        assert!(db.is_persistent());
        id_a = db
            .create_task(
                &timed("Dentist", at("2026-10-25", "15:00"), true, 15),
                now(),
            )
            .unwrap()
            .id;
        id_b = db.create_task(&input("Inbox item"), now()).unwrap().id;
        db.set_completed(id_b, true, now() + 1).unwrap();

        let s = Settings {
            always_on_top: false,
            focus_minutes: 50,
            snooze_minutes: 5,
            theme: Theme::Light,
            high_contrast: true,
            anchor_x: Some(1234),
            anchor_y: Some(8),
            export_dir: "C:\\Backups".into(),
            ..Settings::default()
        };
        s.save(&mut db).unwrap();
    } // connection dropped = app closed

    let db = Db::open(&dir.db()).unwrap();
    let a = db.get_task(id_a).unwrap().unwrap();
    assert_eq!(a.title, "Dentist");
    assert_eq!(a.remind_lead_min, 15);
    assert_eq!(a.remind_at, Some(at("2026-10-25", "15:00") - 900));
    assert!(db.get_task(id_b).unwrap().unwrap().is_completed());

    let s = Settings::load(&db).unwrap();
    assert!(!s.always_on_top);
    assert_eq!((s.focus_minutes, s.snooze_minutes), (50, 5));
    assert_eq!(s.theme, Theme::Light);
    assert!(s.high_contrast);
    assert_eq!((s.anchor_x, s.anchor_y), (Some(1234), Some(8)));
    assert_eq!(s.export_dir, "C:\\Backups");
    // Untouched fields keep their defaults.
    assert!(s.start_collapsed && s.reminder_sound && !s.reminder_takes_focus);
}

#[test]
fn settings_defaults_and_clamping() {
    let db = Db::open_in_memory().unwrap();
    assert_eq!(Settings::load(&db).unwrap(), Settings::default());

    let s = Settings {
        focus_minutes: 9999,
        snooze_minutes: 0,
        missed_window_min: 0,
        ..Settings::default()
    }
    .sanitized();
    assert_eq!(
        (s.focus_minutes, s.snooze_minutes, s.missed_window_min),
        (240, 1, 1)
    );

    // Garbage rows fall back to defaults instead of failing.
    let s = Settings::from_pairs(vec![
        ("focus_minutes".into(), "abc".into()),
        ("theme".into(), "neon".into()),
        ("always_on_top".into(), "maybe".into()),
    ]);
    assert_eq!(s, Settings::default());
}

// ---- views and sorting ----------------------------------------------------------------

#[test]
fn today_upcoming_overdue_views_and_sorting() {
    let mut db = Db::open_in_memory().unwrap();
    let c = |db: &mut Db, i: TaskInput| db.create_task(&i, now() - 100_000).unwrap();

    c(
        &mut db,
        timed("late yesterday", at("2026-10-24", "18:00"), false, 0),
    );
    c(
        &mut db,
        timed(
            "earlier today (overdue)",
            at("2026-10-25", "08:00"),
            false,
            0,
        ),
    );
    c(
        &mut db,
        with_priority(
            timed("today 15:00 low", at("2026-10-25", "15:00"), false, 0),
            Priority::Low,
        ),
    );
    c(
        &mut db,
        with_priority(
            timed("today 15:00 high", at("2026-10-25", "15:00"), false, 0),
            Priority::High,
        ),
    );
    c(
        &mut db,
        timed("today 12:00", at("2026-10-25", "12:00"), false, 0),
    );
    c(&mut db, input("no date"));
    c(
        &mut db,
        with_priority(input("no date urgent"), Priority::High),
    );
    c(
        &mut db,
        timed("tomorrow", at("2026-10-26", "09:00"), false, 0),
    );
    c(
        &mut db,
        timed("next week", at("2026-11-01", "09:00"), false, 0),
    );
    let done = c(
        &mut db,
        timed("done today", at("2026-10-25", "09:00"), false, 0),
    );
    db.set_completed(done.id, true, now() - 60).unwrap();
    let old_done = c(&mut db, input("done last week"));
    db.set_completed(old_done.id, true, now() - 7 * 86_400)
        .unwrap();

    let today = db.list_tasks(View::Today, now(), day(), 50).unwrap();
    assert_eq!(
        titles(&today),
        // open timed (by time, then priority), open undated (by priority), then completed today
        [
            "today 12:00",
            "today 15:00 high",
            "today 15:00 low",
            "no date urgent",
            "no date",
            "done today"
        ]
    );
    let upcoming = db.list_tasks(View::Upcoming, now(), day(), 50).unwrap();
    assert_eq!(titles(&upcoming), ["tomorrow", "next week"]);
    let overdue = db.list_tasks(View::Overdue, now(), day(), 50).unwrap();
    assert_eq!(
        titles(&overdue),
        ["late yesterday", "earlier today (overdue)"]
    );

    let s = db.summary(now(), day()).unwrap();
    assert_eq!((s.overdue, s.today_open, s.upcoming), (2, 5, 2));
    assert_eq!(s.next.unwrap().title, "today 12:00");

    // Limit is honoured.
    assert_eq!(
        db.list_tasks(View::Today, now(), day(), 2).unwrap().len(),
        2
    );
}

#[test]
fn completing_removes_from_overdue_and_counts() {
    let mut db = Db::open_in_memory().unwrap();
    let t = db
        .create_task(
            &timed("x", at("2026-10-24", "09:00"), false, 0),
            now() - 200_000,
        )
        .unwrap();
    assert_eq!(db.summary(now(), day()).unwrap().overdue, 1);
    db.set_completed(t.id, true, now()).unwrap();
    assert_eq!(db.summary(now(), day()).unwrap().overdue, 0);
    assert!(db
        .list_tasks(View::Overdue, now(), day(), 50)
        .unwrap()
        .is_empty());
}

// ---- data safety --------------------------------------------------------------------------

#[test]
fn corrupt_database_is_moved_aside_not_deleted() {
    let dir = TestDir::new("corrupt");
    std::fs::write(
        dir.db(),
        b"this is definitely not an sqlite database, just garbage bytes \0\0\0",
    )
    .unwrap();

    let opened = Db::open_or_recover(&dir.db(), 1_800_000_000);
    assert!(
        opened.db.is_persistent(),
        "a fresh database replaces the unreadable one"
    );
    let notice = opened.notice.expect("user is told");
    assert!(notice.contains("corrupt-1800000000"), "{notice}");
    let kept = dir.path().join("focus-island.db.corrupt-1800000000");
    assert!(kept.exists(), "original bytes are preserved for recovery");
    assert!(std::fs::read(kept)
        .unwrap()
        .starts_with(b"this is definitely"));

    // And the new database is fully usable.
    let mut db = opened.db;
    db.create_task(&input("after recovery"), now()).unwrap();
}

#[test]
fn newer_schema_is_never_modified() {
    let dir = TestDir::new("newer");
    {
        let mut db = Db::open(&dir.db()).unwrap();
        db.create_task(&input("from the future"), now()).unwrap();
    }
    {
        let conn = rusqlite::Connection::open(dir.db()).unwrap();
        conn.pragma_update(None, "user_version", 999).unwrap();
    }
    let before = std::fs::read(dir.db()).unwrap();

    let opened = Db::open_or_recover(&dir.db(), now());
    assert!(!opened.db.is_persistent(), "falls back to memory");
    assert!(opened.notice.as_deref().unwrap().contains("newer"));
    drop(opened);

    assert_eq!(
        std::fs::read(dir.db()).unwrap(),
        before,
        "file is byte-for-byte untouched"
    );
}

#[test]
fn migrations_upgrade_in_order_and_back_up_first() {
    let dir = TestDir::new("migrate");
    let v1 = MIGRATIONS[0];
    let v2 = "ALTER TABLE tasks ADD COLUMN color TEXT;";

    // An existing v1 database with data in it.
    {
        let mut db = Db::open(&dir.db()).unwrap();
        db.create_task(&input("precious"), now()).unwrap();
    }

    let mut conn = rusqlite::Connection::open(dir.db()).unwrap();
    apply_migrations(&mut conn, &[v1, v2], Some(&dir.db())).unwrap();
    let ver: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(ver, 2);
    let n: i64 = conn
        .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1, "data survives the upgrade");
    conn.execute("UPDATE tasks SET color = 'x'", []).unwrap(); // new column exists

    let bak = dir.path().join("focus-island.db.pre-v1.bak");
    assert!(bak.exists(), "backup written before upgrading");
    let b = rusqlite::Connection::open(&bak).unwrap();
    let bn: i64 = b
        .query_row("SELECT COUNT(*) FROM tasks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(bn, 1);

    // Re-applying is a no-op.
    apply_migrations(&mut conn, &[v1, v2], Some(&dir.db())).unwrap();
}

#[test]
fn failed_migration_rolls_back() {
    let mut conn = rusqlite::Connection::open_in_memory().unwrap();
    let bad = "CREATE TABLE a (x); THIS IS NOT SQL;";
    assert!(apply_migrations(&mut conn, &[bad], None).is_err());
    let ver: i64 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(ver, 0);
    let tables: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE name = 'a'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(tables, 0, "partial migration left nothing behind");
}

#[test]
fn sql_injection_in_text_is_just_text() {
    let mut db = Db::open_in_memory().unwrap();
    let nasty = "x'); DROP TABLE tasks; --";
    let t = db.create_task(&input(nasty), now()).unwrap();
    assert_eq!(db.get_task(t.id).unwrap().unwrap().title, nasty);
    assert_eq!(
        db.list_tasks(View::Today, now(), day(), 10).unwrap().len(),
        1
    );
}

#[test]
fn export_writes_backup_and_json_into_chosen_folder() {
    let dir = TestDir::new("export");
    let mut db = Db::open(&dir.db()).unwrap();
    db.create_task(&input("Task \"one\""), now()).unwrap();
    db.record_focus_session(None, now(), now() + 1500, 1500)
        .unwrap();

    let out = dir.path().join("exports");
    let p = export_all(&db, &out, now()).unwrap();
    assert!(p.db_path.starts_with(&out) && p.json_path.starts_with(&out));

    let backup = Db::open(&p.db_path).unwrap();
    assert_eq!(
        backup
            .list_tasks(View::Today, now(), day(), 10)
            .unwrap()
            .len(),
        1
    );
    let json = std::fs::read_to_string(&p.json_path).unwrap();
    assert!(json.contains(r#""title": "Task \"one\"""#));
    assert!(json.contains("\"planned_seconds\": 1500"));

    // Relative or empty paths are refused (only explicit absolute folders are written to).
    assert!(export_all(&db, std::path::Path::new("relative/dir"), now()).is_err());
    assert!(export_all(&db, std::path::Path::new(""), now()).is_err());
}

#[test]
fn focus_sessions_keep_history_when_task_is_deleted() {
    let mut db = Db::open_in_memory().unwrap();
    let t = db.create_task(&input("deep work"), now()).unwrap();
    db.record_focus_session(Some(t.id), now(), now() + 1500, 1500)
        .unwrap();
    db.delete_task(t.id).unwrap();
    assert_eq!(db.focus_session_count().unwrap(), 1);
    // A session for a task deleted while the timer ran is stored without the link.
    db.record_focus_session(Some(t.id), now(), now() + 1500, 1500)
        .unwrap();
    assert_eq!(db.focus_session_count().unwrap(), 2);
}
