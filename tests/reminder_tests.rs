mod common;

use std::sync::mpsc;
use std::time::{Duration, Instant};

use common::*;
use focus_island::database::Db;
use focus_island::models::*;
use focus_island::reminders::{collect_due, Outcome, ReminderPolicy};
use focus_island::settings::Settings;
use focus_island::timeutil::now_ts;
use focus_island::worker::{Cmd, Event, WakeAction, WakeHook, Worker};

const POLICY: ReminderPolicy = ReminderPolicy {
    missed_window_secs: 15 * 60,
};

fn now() -> Ts {
    at("2026-10-25", "10:00")
}

fn memdb() -> Db {
    Db::open_in_memory().unwrap()
}

// ---- due-time calculations ---------------------------------------------------------------

#[test]
fn reminder_time_is_due_time_minus_lead() {
    let mut db = memdb();
    let due = at("2026-10-25", "15:00");
    let t = db
        .create_task(&timed("call", due, true, 15), now())
        .unwrap();
    assert_eq!(t.remind_at, Some(due - 15 * 60));
    assert_eq!(t.effective_fire_at(), Some(due - 15 * 60));
    assert_eq!(db.next_reminder_deadline().unwrap(), Some(due - 15 * 60));

    // Disabled reminder: no deadline, no fire time.
    let off = db
        .create_task(&timed("quiet", due, false, 0), now())
        .unwrap();
    assert_eq!(off.remind_at, None);
    assert_eq!(off.effective_fire_at(), None);
}

#[test]
fn deadline_is_the_earliest_pending_reminder() {
    let mut db = memdb();
    assert_eq!(db.next_reminder_deadline().unwrap(), None);
    db.create_task(&timed("later", at("2026-10-25", "18:00"), true, 0), now())
        .unwrap();
    db.create_task(&timed("sooner", at("2026-10-25", "11:00"), true, 0), now())
        .unwrap();
    db.create_task(
        &timed("no reminder", at("2026-10-25", "10:30"), false, 0),
        now(),
    )
    .unwrap();
    assert_eq!(
        db.next_reminder_deadline().unwrap(),
        Some(at("2026-10-25", "11:00"))
    );
}

#[test]
fn nothing_is_due_before_its_time() {
    let mut db = memdb();
    let due = at("2026-10-25", "11:00");
    db.create_task(&timed("x", due, true, 0), now()).unwrap();
    assert!(collect_due(&mut db, due - 1, &POLICY)
        .unwrap()
        .show
        .is_empty());
    let b = collect_due(&mut db, due, &POLICY).unwrap();
    assert_eq!(titles(&b.show), ["x"]);
}

// ---- duplicate prevention -----------------------------------------------------------------

#[test]
fn a_reminder_fires_exactly_once() {
    let mut db = memdb();
    let due = at("2026-10-25", "11:00");
    db.create_task(&timed("x", due, true, 0), now()).unwrap();

    assert_eq!(
        collect_due(&mut db, due + 2, &POLICY).unwrap().show.len(),
        1
    );
    for t in [due + 3, due + 60, due + 600] {
        let b = collect_due(&mut db, t, &POLICY).unwrap();
        assert!(b.show.is_empty() && b.missed.is_empty(), "no repeat alert");
    }
    assert_eq!(
        db.next_reminder_deadline().unwrap(),
        None,
        "nothing left to wait for"
    );
}

#[test]
fn restart_does_not_duplicate_reminders() {
    let dir = TestDir::new("restart");
    let due = at("2026-10-25", "11:00");
    {
        let mut db = Db::open(&dir.db()).unwrap();
        db.create_task(&timed("x", due, true, 0), now()).unwrap();
        assert_eq!(
            collect_due(&mut db, due + 1, &POLICY).unwrap().show.len(),
            1
        );
    }
    // App restarted a minute later, then again.
    for _ in 0..2 {
        let mut db = Db::open(&dir.db()).unwrap();
        let b = collect_due(&mut db, due + 60, &POLICY).unwrap();
        assert!(b.show.is_empty() && b.missed.is_empty());
    }
}

#[test]
fn record_notification_is_idempotent_per_scheduled_time() {
    let mut db = memdb();
    let t = db
        .create_task(&timed("x", at("2026-10-25", "11:00"), true, 0), now())
        .unwrap();
    let at1 = t.remind_at.unwrap();
    assert!(db
        .record_notification(t.id, at1, now(), Outcome::Shown)
        .unwrap());
    assert!(!db
        .record_notification(t.id, at1, now(), Outcome::Shown)
        .unwrap());
    // A different scheduled time for the same task is a different notification.
    assert!(db
        .record_notification(t.id, at1 + 600, now(), Outcome::Shown)
        .unwrap());
}

// ---- snooze / dismiss / complete ------------------------------------------------------------

#[test]
fn snooze_reschedules_and_fires_again_once() {
    let mut db = memdb();
    let due = at("2026-10-25", "11:00");
    let t = db.create_task(&timed("x", due, true, 0), now()).unwrap();
    let first = collect_due(&mut db, due, &POLICY).unwrap();
    assert_eq!(first.show.len(), 1);

    let until = db.snooze_reminder(t.id, due, due + 5, 10).unwrap();
    assert_eq!(until, due + 5 + 600);
    assert_eq!(db.next_reminder_deadline().unwrap(), Some(until));

    // Not yet.
    assert!(collect_due(&mut db, until - 1, &POLICY)
        .unwrap()
        .show
        .is_empty());
    // Fires at the snoozed time...
    let again = collect_due(&mut db, until, &POLICY).unwrap();
    assert_eq!(titles(&again.show), ["x"]);
    assert_eq!(again.show[0].effective_fire_at(), Some(until));
    // ...and only once.
    assert!(collect_due(&mut db, until + 30, &POLICY)
        .unwrap()
        .show
        .is_empty());

    // Snoozing again works the same way.
    let until2 = db.snooze_reminder(t.id, until, until + 1, 5).unwrap();
    assert_eq!(until2, until + 1 + 300);
    assert_eq!(collect_due(&mut db, until2, &POLICY).unwrap().show.len(), 1);
}

#[test]
fn snooze_duration_is_clamped_to_a_sane_range() {
    let mut db = memdb();
    let due = at("2026-10-25", "11:00");
    let t = db.create_task(&timed("x", due, true, 0), now()).unwrap();
    assert_eq!(db.snooze_reminder(t.id, due, due, 0).unwrap(), due + 60);
    assert_eq!(
        db.snooze_reminder(t.id, due, due, 100_000).unwrap(),
        due + 24 * 3600
    );
}

#[test]
fn dismiss_and_complete_stop_further_alerts() {
    let mut db = memdb();
    let due = at("2026-10-25", "11:00");
    let a = db.create_task(&timed("a", due, true, 0), now()).unwrap();
    let b = db.create_task(&timed("b", due, true, 0), now()).unwrap();
    assert_eq!(collect_due(&mut db, due, &POLICY).unwrap().show.len(), 2);

    db.dismiss_reminder(a.id, due).unwrap();
    db.set_completed(b.id, true, due + 1).unwrap();
    assert!(collect_due(&mut db, due + 600, &POLICY)
        .unwrap()
        .show
        .is_empty());
    assert_eq!(db.next_reminder_deadline().unwrap(), None);
}

#[test]
fn completing_before_the_time_cancels_the_reminder() {
    let mut db = memdb();
    let due = at("2026-10-25", "11:00");
    let t = db.create_task(&timed("x", due, true, 0), now()).unwrap();
    db.set_completed(t.id, true, now()).unwrap();
    assert_eq!(db.next_reminder_deadline().unwrap(), None);
    assert!(collect_due(&mut db, due + 1, &POLICY)
        .unwrap()
        .show
        .is_empty());
}

#[test]
fn snoozing_a_completed_task_is_refused() {
    let mut db = memdb();
    let due = at("2026-10-25", "11:00");
    let t = db.create_task(&timed("x", due, true, 0), now()).unwrap();
    db.set_completed(t.id, true, due).unwrap();
    assert!(db.snooze_reminder(t.id, due, due, 10).is_err());
}

// ---- edit / delete interplay ----------------------------------------------------------------

#[test]
fn deleting_a_task_removes_its_reminder_everywhere() {
    let mut db = memdb();
    let due = at("2026-10-25", "11:00");
    let t = db.create_task(&timed("x", due, true, 0), now()).unwrap();
    collect_due(&mut db, due, &POLICY).unwrap(); // leaves a notification row
    let t2 = db
        .create_task(&timed("y", due + 3600, true, 0), now())
        .unwrap();
    assert_eq!(db.next_reminder_deadline().unwrap(), Some(due + 3600));

    db.delete_task(t2.id).unwrap();
    assert_eq!(
        db.next_reminder_deadline().unwrap(),
        None,
        "no deadline references the deleted task"
    );
    db.delete_task(t.id).unwrap();
    let rows: i64 = db
        .list_tasks(View::Today, now(), (0, i64::MAX), 10)
        .unwrap()
        .len() as i64;
    assert_eq!(rows, 0);
    assert!(collect_due(&mut db, due + 9999, &POLICY)
        .unwrap()
        .show
        .is_empty());
}

#[test]
fn editing_the_reminder_time_rearms_it_but_editing_the_title_does_not() {
    let mut db = memdb();
    let due = at("2026-10-25", "11:00");
    let t = db.create_task(&timed("x", due, true, 0), now()).unwrap();
    assert_eq!(collect_due(&mut db, due, &POLICY).unwrap().show.len(), 1);

    // Title only: same reminder time, already notified -> silent.
    db.update_task(t.id, &timed("x renamed", due, true, 0), due + 1)
        .unwrap();
    assert!(collect_due(&mut db, due + 2, &POLICY)
        .unwrap()
        .show
        .is_empty());

    // New time in the future: fires again at the new time.
    let new_due = due + 3 * 3600;
    db.update_task(t.id, &timed("x renamed", new_due, true, 0), due + 3)
        .unwrap();
    assert_eq!(db.next_reminder_deadline().unwrap(), Some(new_due));
    assert_eq!(
        collect_due(&mut db, new_due, &POLICY).unwrap().show.len(),
        1
    );
}

#[test]
fn editing_the_task_keeps_a_pending_snooze() {
    let mut db = memdb();
    let due = at("2026-10-25", "11:00");
    let t = db.create_task(&timed("x", due, true, 0), now()).unwrap();
    collect_due(&mut db, due, &POLICY).unwrap();
    let until = db.snooze_reminder(t.id, due, due, 10).unwrap();
    // Rename while snoozed (reminder time unchanged, even though it is now in the past).
    let renamed = db
        .update_task(t.id, &timed("x2", due, true, 0), due + 10)
        .unwrap();
    assert_eq!(renamed.snooze_until, Some(until));
    assert_eq!(db.next_reminder_deadline().unwrap(), Some(until));
}

#[test]
fn disabling_then_deleting_clears_deadline() {
    let mut db = memdb();
    let due = at("2026-10-25", "11:00");
    let t = db.create_task(&timed("x", due, true, 0), now()).unwrap();
    db.update_task(t.id, &timed("x", due, false, 0), now())
        .unwrap();
    assert_eq!(db.next_reminder_deadline().unwrap(), None);
}

// ---- overdue reconciliation (missed-reminder policy) ------------------------------------------

#[test]
fn startup_shows_recent_reminders_and_only_summarises_old_ones() {
    let mut db = memdb();
    let created = at("2026-10-20", "08:00");
    // Fire times (all before "now" = 10:00 today).
    let old_a = db
        .create_task(&timed("old a", at("2026-10-22", "09:00"), true, 0), created)
        .unwrap();
    let _old_b = db
        .create_task(&timed("old b", at("2026-10-24", "17:00"), true, 0), created)
        .unwrap();
    let _stale = db
        .create_task(
            &timed("stale today", at("2026-10-25", "09:00"), true, 0),
            created,
        )
        .unwrap();
    let _recent = db
        .create_task(
            &timed("recent", at("2026-10-25", "09:50"), true, 0),
            created,
        )
        .unwrap();
    let _just = db
        .create_task(
            &timed("just now", at("2026-10-25", "09:59"), true, 0),
            created,
        )
        .unwrap();

    let b = collect_due(&mut db, now(), &POLICY).unwrap();
    assert_eq!(
        titles(&b.show),
        ["recent", "just now"],
        "only the last 15 minutes pop up"
    );
    let mut missed = titles(&b.missed);
    missed.sort_unstable();
    assert_eq!(missed, ["old a", "old b", "stale today"]);
    assert_eq!(b.missed.len(), 3);

    // Missed ones are recorded, never shown later, and remain visible as overdue tasks.
    assert!(collect_due(&mut db, now() + 1, &POLICY)
        .unwrap()
        .missed
        .is_empty());
    let overdue = db
        .list_tasks(View::Overdue, now(), (0, i64::MAX), 50)
        .unwrap();
    assert!(overdue.iter().any(|t| t.id == old_a.id));
}

#[test]
fn long_absence_does_not_flood() {
    let mut db = memdb();
    for i in 0..40 {
        db.create_task(
            &timed(
                &format!("t{i}"),
                at("2026-10-01", "09:00") + i * 3600,
                true,
                0,
            ),
            at("2026-09-30", "08:00"),
        )
        .unwrap();
    }
    let b = collect_due(&mut db, now(), &POLICY).unwrap();
    assert!(b.show.is_empty(), "nothing pops up after a long absence");
    assert_eq!(b.missed.len(), 40, "one summary count instead");
}

#[test]
fn missed_window_is_configurable() {
    let mut db = memdb();
    let due = at("2026-10-25", "09:00"); // an hour before "now"
    db.create_task(&timed("x", due, true, 0), at("2026-10-24", "09:00"))
        .unwrap();
    let wide = ReminderPolicy {
        missed_window_secs: 2 * 3600,
    };
    assert_eq!(collect_due(&mut db, now(), &wide).unwrap().show.len(), 1);
}

#[test]
fn prune_never_drops_the_guard_of_a_live_reminder() {
    let mut db = memdb();
    let due = at("2026-10-25", "11:00");
    let live = db.create_task(&timed("live", due, true, 0), now()).unwrap();
    let gone = db.create_task(&timed("gone", due, true, 0), now()).unwrap();
    collect_due(&mut db, due, &POLICY).unwrap();
    db.set_completed(gone.id, true, due).unwrap();

    db.prune(due + 400 * 86_400).unwrap();
    // `live` still has its reminder enabled: its record must survive so it cannot fire again.
    assert!(collect_due(&mut db, due + 400 * 86_400, &POLICY)
        .unwrap()
        .show
        .is_empty());
    assert!(
        !db.record_notification(live.id, due, due, Outcome::Shown)
            .unwrap(),
        "live guard kept"
    );
    assert!(
        db.record_notification(gone.id, due, due, Outcome::Shown)
            .unwrap(),
        "stale row pruned"
    );
}

// ---- the real scheduler thread, end to end ----------------------------------------------------

fn collect_events(rx: &mpsc::Receiver<Event>, window: Duration) -> Vec<Event> {
    let end = Instant::now() + window;
    let mut v = Vec::new();
    while let Some(left) = end.checked_duration_since(Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(e) => v.push(e),
            Err(_) => break,
        }
    }
    v
}

fn spawn(db: Db, tx: mpsc::Sender<Event>, hook: Option<WakeHook>) -> Worker {
    Worker::spawn(
        db,
        Settings::default(),
        Box::new(move |e| {
            let _ = tx.send(e);
        }),
        hook,
    )
}

fn reminder_events(events: &[Event]) -> Vec<(Vec<String>, usize)> {
    events
        .iter()
        .filter_map(|e| match e {
            Event::Reminders { show, missed } => {
                Some((show.iter().map(|t| t.title.clone()).collect(), *missed))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn scheduler_thread_fires_at_the_deadline_and_not_again() {
    let dir = TestDir::new("sched");
    let (tx, rx) = mpsc::channel();
    let w = spawn(Db::open(&dir.db()).unwrap(), tx, None);

    let start = Instant::now();
    let due = now_ts() + 3;
    w.send(Cmd::Create(timed("ping", due, true, 0)));

    // Wait for the reminder (3 s away) with generous slack.
    let mut got = None;
    let deadline = start + Duration::from_secs(10);
    while got.is_none() && Instant::now() < deadline {
        if let Ok(Event::Reminders { show, .. }) = rx.recv_timeout(Duration::from_millis(200)) {
            got = Some((show, start.elapsed()));
        }
    }
    let (show, took) = got.expect("reminder fired");
    assert_eq!(show.len(), 1);
    assert_eq!(show[0].title, "ping");
    assert!(took >= Duration::from_millis(1500), "not early: {took:?}");
    assert!(took <= Duration::from_secs(6), "not late: {took:?}");

    // No repeat in the following seconds.
    let later = collect_events(&rx, Duration::from_millis(2500));
    assert!(
        reminder_events(&later).is_empty(),
        "duplicate alert: {later:?}"
    );

    // Restart the app: still no alert for it.
    w.shutdown();
    let (tx2, rx2) = mpsc::channel();
    let w2 = spawn(Db::open(&dir.db()).unwrap(), tx2, None);
    let after_restart = collect_events(&rx2, Duration::from_millis(1500));
    assert!(
        reminder_events(&after_restart).is_empty(),
        "{after_restart:?}"
    );
    w2.shutdown();
}

#[test]
fn scheduler_reconciles_on_startup() {
    let dir = TestDir::new("reconcile");
    let now = now_ts();
    {
        let mut db = Db::open(&dir.db()).unwrap();
        // Created "earlier"; one came due 2 minutes ago, one a day ago.
        db.create_task(&timed("recent", now - 120, true, 0), now - 86_400 * 2)
            .unwrap();
        db.create_task(&timed("ancient", now - 86_400, true, 0), now - 86_400 * 2)
            .unwrap();
    }
    let (tx, rx) = mpsc::channel();
    let w = spawn(Db::open(&dir.db()).unwrap(), tx, None);
    let events = collect_events(&rx, Duration::from_millis(1000));
    let r = reminder_events(&events);
    assert_eq!(
        r,
        vec![(vec!["recent".to_string()], 1)],
        "one popup, one missed"
    );
    w.shutdown();
}

#[test]
fn snooze_command_reschedules_through_the_worker() {
    let dir = TestDir::new("snooze");
    let (tx, rx) = mpsc::channel();
    let w = spawn(Db::open(&dir.db()).unwrap(), tx, None);
    let due = now_ts() + 1;
    w.send(Cmd::Create(timed("s", due, true, 0)));

    let first = loop {
        if let Event::Reminders { show, .. } =
            rx.recv_timeout(Duration::from_secs(8)).expect("fired")
        {
            break show;
        }
    };
    let remind_at = first[0].effective_fire_at().unwrap();
    w.send(Cmd::Snooze {
        task_id: first[0].id,
        scheduled_for: remind_at,
        minutes: 10,
    });
    // A snapshot after the snooze shows the task is not repeated now.
    let rest = collect_events(&rx, Duration::from_millis(1500));
    assert!(reminder_events(&rest).is_empty());
    w.shutdown();

    // 10 minutes later (simulated at the database level) it is due again, exactly once.
    let mut db = Db::open(&dir.db()).unwrap();
    let t = db.get_task(first[0].id).unwrap().unwrap();
    let until = t.snooze_until.expect("snoozed");
    assert!(until >= now_ts() + 9 * 60 && until <= now_ts() + 10 * 60 + 5);
    assert_eq!(collect_due(&mut db, until, &POLICY).unwrap().show.len(), 1);
    assert!(collect_due(&mut db, until + 1, &POLICY)
        .unwrap()
        .show
        .is_empty());
}

#[test]
fn wake_hook_follows_the_next_deadline_only_when_enabled() {
    let dir = TestDir::new("wake");
    let (tx, rx) = mpsc::channel();
    let (atx, arx) = mpsc::channel::<WakeAction>();
    let hook: WakeHook = Box::new(move |a| {
        let _ = atx.send(a);
        Ok(())
    });
    let w = spawn(Db::open(&dir.db()).unwrap(), tx, Some(hook));
    let due = now_ts() + 3600;
    w.send(Cmd::Create(timed("later", due, true, 0)));
    let drained = collect_events(&rx, Duration::from_millis(300));
    let id = drained
        .iter()
        .find_map(|e| {
            if let Event::Saved(id) = e {
                Some(*id)
            } else {
                None
            }
        })
        .expect("task saved");
    assert!(
        arx.try_recv().is_err(),
        "disabled by default: the scheduled task is never touched"
    );

    // The user opts in.
    w.send(Cmd::SaveSettings(Settings {
        scheduled_wake: true,
        ..Settings::default()
    }));
    assert_eq!(
        arx.recv_timeout(Duration::from_secs(2)).unwrap(),
        WakeAction::Register(due)
    );

    // Deleting the task removes the scheduled wake.
    w.send(Cmd::Delete(id));
    assert_eq!(
        arx.recv_timeout(Duration::from_secs(2)).unwrap(),
        WakeAction::Unregister
    );

    // And turning the feature off unregisters too (reversible).
    w.send(Cmd::Create(timed("again", due + 60, true, 0)));
    assert_eq!(
        arx.recv_timeout(Duration::from_secs(2)).unwrap(),
        WakeAction::Register(due + 60)
    );
    w.send(Cmd::SaveSettings(Settings::default()));
    assert_eq!(
        arx.recv_timeout(Duration::from_secs(2)).unwrap(),
        WakeAction::Unregister
    );
    w.shutdown();
}
