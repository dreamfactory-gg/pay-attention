use super::*;
use chrono::NaiveTime;

fn config() -> Config {
    Config {
        dnd: String::new(),
        ..Config::default()
    }
}

fn row(key: &str, state: &str) -> Row {
    Row {
        key: key.into(),
        harness: "claude".into(),
        surface: "terminal".into(),
        pid: None,
        tty: Some("/dev/ttys901".into()),
        name: key.into(),
        cwd: "/tmp".into(),
        state: state.into(),
        reason: None,
        detail: None,
        since: 1,
        acked: false,
    }
}

fn rings(actions: &[Action]) -> Vec<(&str, f64, &[String])> {
    actions
        .iter()
        .filter_map(|action| match action {
            Action::Ring {
                color, vol, keys, ..
            } => Some((*color, *vol, keys.as_slice())),
            _ => None,
        })
        .collect()
}

#[test]
fn reload_preserves_episodes_and_releases_old_dnd_deferrals() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = Engine::with_ack_path(
        Config {
            dnd: "00:00-23:59".into(),
            ..config()
        },
        dir.path().join("acks.json"),
    )
    .unwrap();
    let start = Local
        .with_ymd_and_hms(2026, 10, 14, 12, 0, 0)
        .unwrap()
        .timestamp_millis();
    let rows = [row("a", "waiting")];
    assert!(rings(&engine.tick(start, &rows, None, 100, false)).is_empty());
    engine.reload_config(config()).unwrap();
    let actions = engine.tick(start + 1_000, &rows, None, 100, false);
    assert_eq!(rings(&actions).len(), 1);
    assert_eq!(engine.ring_pitch("blue", &["a".into()]), 0);
    engine.reload_config(config()).unwrap();
    assert!(rings(&engine.tick(start + 2_000, &rows, None, 100, false)).is_empty());
    let actions = engine.tick(start + 61_000, &rows, None, 100, false);
    assert_eq!(rings(&actions).len(), 1);
    assert_eq!(engine.ring_pitch("blue", &["a".into()]), 1);
}

#[test]
fn reload_updates_the_next_interval_without_restarting_the_episode() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = Engine::with_ack_path(config(), dir.path().join("acks.json")).unwrap();
    let rows = [row("a", "waiting")];
    assert_eq!(rings(&engine.tick(0, &rows, None, 100, false)).len(), 1);
    let mut updated = config();
    updated.escalation.start_interval_secs = 10.0;
    updated.escalation.min_interval_secs = 10.0;
    engine.reload_config(updated).unwrap();
    assert!(rings(&engine.tick(9_999, &rows, None, 100, false)).is_empty());
    assert_eq!(
        rings(&engine.tick(10_000, &rows, None, 100, false)).len(),
        1
    );
    assert_eq!(engine.ring_pitch("blue", &["a".into()]), 1);
}

#[test]
fn reload_updates_yellow_delay_and_remaining_rings() {
    let dir = tempfile::tempdir().unwrap();
    let mut initial = config();
    initial.escalation.yellow_max_rings = 1;
    let mut engine = Engine::with_ack_path(initial.clone(), dir.path().join("acks.json")).unwrap();
    let rows = [row("a", "idle")];
    engine.tick(0, &rows, None, 100, false);
    initial.escalation.yellow_delay_secs = 5.0;
    engine.reload_config(initial.clone()).unwrap();
    assert!(rings(&engine.tick(2_000, &rows, None, 100, false)).is_empty());
    assert_eq!(rings(&engine.tick(5_000, &rows, None, 100, false)).len(), 1);
    initial.escalation.yellow_max_rings = 2;
    engine.reload_config(initial).unwrap();
    assert_eq!(
        rings(&engine.tick(65_000, &rows, None, 100, false)).len(),
        1
    );
    assert!(rings(&engine.tick(200_000, &rows, None, 100, false)).is_empty());
}

#[test]
fn reload_keeps_a_suppressed_ring_due_and_reaches_absent_episodes() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = Engine::with_ack_path(config(), dir.path().join("acks.json")).unwrap();
    let blue = [row("a", "waiting")];
    assert_eq!(rings(&engine.tick(0, &blue, None, 100, false)).len(), 1);
    engine.quiet(50_000, 80_000);
    assert!(rings(&engine.tick(60_000, &blue, None, 100, false)).is_empty());
    let mut slow = config();
    slow.escalation.start_interval_secs = 600.0;
    slow.escalation.min_interval_secs = 600.0;
    engine.reload_config(slow).unwrap();
    assert!(rings(&engine.tick(70_000, &blue, None, 100, false)).is_empty());
    assert_eq!(
        rings(&engine.tick(80_000, &blue, None, 100, false)).len(),
        1
    );
    assert!(rings(&engine.tick(81_000, &blue, None, 100, false)).is_empty());

    let mut capped = config();
    capped.escalation.yellow_max_rings = 1;
    let mut engine = Engine::with_ack_path(capped.clone(), dir.path().join("acks2.json")).unwrap();
    let yellow = [row("y", "idle")];
    engine.tick(0, &yellow, None, 100, false);
    assert_eq!(
        rings(&engine.tick(2_000, &yellow, None, 100, false)).len(),
        1
    );
    assert!(rings(&engine.tick(70_000, &yellow, None, 100, false)).is_empty());
    engine.tick(71_000, &[], None, 100, false);
    capped.escalation.yellow_max_rings = 2;
    engine.reload_config(capped).unwrap();
    assert_eq!(
        rings(&engine.tick(72_000, &yellow, None, 100, false)).len(),
        1
    );
}

#[test]
fn pitch_caps_per_episode_and_resets_for_a_new_alert() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = Engine::with_ack_path(config(), dir.path().join("acks.json")).unwrap();
    let mut rows = [row("a", "waiting")];
    for (n, expected) in (0..8).zip([0, 1, 2, 3, 4, 5, 5, 5]) {
        let actions = engine.tick(n * 60_000, &rows, None, 100, false);
        assert_eq!(rings(&actions).len(), 1);
        assert_eq!(engine.ring_pitch("blue", &["a".into()]), expected);
    }
    rows[0].since += 1;
    engine.tick(500_000, &rows, None, 100, false);
    assert_eq!(engine.ring_pitch("blue", &["a".into()]), 0);
}

#[test]
fn display_rows_use_core_alert_rules_and_hotkey_order() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = Engine::with_ack_path(config(), dir.path().join("acks.json")).unwrap();
    let mut headless = row("headless", "idle");
    headless.surface = "headless".into();
    let rows = [row("red", "error"), row("blue", "waiting"), headless];
    engine.tick(0, &rows, None, 100, false);
    let display = engine.display_rows();
    assert_eq!(
        display
            .iter()
            .map(|r| (r.row.key.as_str(), r.section))
            .collect::<Vec<_>>(),
        [
            ("blue", "needs"),
            ("red", "needs"),
            ("headless", "headless")
        ]
    );
    engine.click(1, "blue");
    assert_eq!(engine.display_rows()[0].section, "seen");
}

#[test]
fn sparse_ticks_skip_offsets_without_reanchoring_and_all_due_alerts_advance() {
    let dir = tempfile::tempdir().unwrap();
    let config = config();
    let schedule = crate::schedule(&config, "blue", 5.0, NaiveTime::MIN).unwrap();
    let mut engine = Engine::with_ack_path(config, dir.path().join("acks.json")).unwrap();
    let blue = row("blue", "waiting");
    engine.tick(0, std::slice::from_ref(&blue), None, 100, false);
    let actions = engine.tick(120_000, std::slice::from_ref(&blue), None, 100, false);
    let last_due = schedule.iter().rfind(|ring| ring.t <= 120.0).unwrap();
    assert_eq!(
        rings(&actions)[0].1,
        (last_due.volume * 100.0).round() / 100.0
    );
    assert!(
        engine
            .tick(120_000, std::slice::from_ref(&blue), None, 100, false)
            .is_empty()
    );
    let next = schedule.iter().find(|ring| ring.t > 120.0).unwrap();
    let next_ms = (next.t * 1000.0).ceil() as i64;
    assert!(
        engine
            .tick(next_ms - 1, std::slice::from_ref(&blue), None, 100, false)
            .is_empty()
    );
    let rows = [blue, row("red", "error")];
    let actions = engine.tick(next_ms, &rows, None, 100, false);
    assert_eq!(
        rings(&actions),
        vec![("red", 0.3, ["blue".into(), "red".into()].as_slice())]
    );
    assert!(engine.tick(next_ms, &rows, None, 100, false).is_empty());
}

#[test]
fn acknowledgements_ignore_input_and_metadata_but_not_episode_changes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("acks.json");
    let mut engine = Engine::with_ack_path(config(), path.clone()).unwrap();
    let mut rows = [row("a", "waiting")];
    rows[0].acked = true;
    assert_eq!(rings(&engine.tick(0, &rows, None, 100, false)).len(), 1);
    let actions = engine.click(1, "a");
    assert!(matches!(
        &actions[..],
        [
            Action::Ack {
                by: AckBy::Click,
                ..
            },
            Action::Focus { .. },
            Action::Light {
                color: "grey",
                count: 0,
                ..
            }
        ]
    ));
    assert!(matches!(&engine.click(2, "a")[..], [Action::Focus { .. }]));
    assert!(engine.click(3, "missing").is_empty());
    let stored = crate::read_json(&path).unwrap();
    assert_eq!(
        stored["a"],
        serde_json::json!({"since": 1, "state": "waiting", "reason": null})
    );

    let mut engine = Engine::with_ack_path(config(), path).unwrap();
    rows[0].acked = false;
    rows[0].name = "renamed".into();
    assert!(rings(&engine.tick(1000, &rows, None, 100, false)).is_empty());
    rows[0].reason = Some("permission".into());
    assert_eq!(rings(&engine.tick(1500, &rows, None, 100, false)).len(), 1);
    rows[0].since += 1;
    let actions = engine.tick(2000, &rows, None, 100, false);
    assert_eq!(rings(&actions).len(), 1);
    assert!(
        !actions
            .iter()
            .any(|action| matches!(action, Action::Event(_)))
    );
    rows[0].state = "error".into();
    assert_eq!(
        rings(&engine.tick(3000, &rows, None, 100, false))[0].0,
        "red"
    );
    engine.tick(4000, &[], None, 100, false);
    assert!(rings(&engine.tick(5000, &rows, None, 100, false)).is_empty());
}

#[test]
fn hotkey_continues_after_the_focused_alert_is_acked() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = Engine::with_ack_path(config(), dir.path().join("acks.json")).unwrap();
    let mut rows = [
        row("blue1", "waiting"),
        row("blue2", "waiting"),
        row("red", "error"),
    ];
    rows[1].tty = Some("/dev/ttys902".into());
    engine.tick(0, &rows, None, 100, false);
    for (now, key) in [(1, "blue1"), (2, "blue2")] {
        assert!(
            matches!(&engine.hotkey(now)[..], [Action::Focus { key: actual, .. }] if actual == key)
        );
    }
    engine.tick(3, &rows, rows[1].tty.as_deref(), 0, false);
    assert!(matches!(&engine.hotkey(4)[..], [Action::Focus { key, .. }] if key == "red"));
    engine.click(5, "red");
    assert!(matches!(&engine.hotkey(6)[..], [Action::Focus { key, .. }] if key == "blue1"));
}

#[test]
fn phone_values_are_literal_and_dedup_survives_absence() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        phone_after_secs: 0,
        phone_command: vec!["{name}|{state}|{detail}|{other}".into()],
        ..config()
    };
    let mut engine = Engine::with_ack_path(config, dir.path().join("acks.json")).unwrap();
    let mut rows = [row("a", "waiting")];
    rows[0].name = "fix {state} and {detail}".into();
    rows[0].detail = Some("use {name}".into());
    let actions = engine.tick(0, &rows, None, 300, false);
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, Action::Phone { argv, .. }
        if argv == &["fix {state} and {detail}|waiting|use {name}|{other}"]))
    );
    engine.tick(1000, &[], None, 300, false);
    assert!(
        !engine
            .tick(2000, &rows, None, 300, false)
            .iter()
            .any(|action| matches!(action, Action::Phone { .. }))
    );
    rows[0].reason = Some("permission".into());
    assert!(
        engine
            .tick(3000, &rows, None, 300, false)
            .iter()
            .any(|action| matches!(action, Action::Phone { .. }))
    );
}

#[test]
fn failed_acks_survive_reload_and_other_successful_writes() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("state");
    fs::write(&parent, "not a directory").unwrap();
    let path = parent.join("acks.json");
    let mut engine = Engine::with_ack_path(config(), path.clone()).unwrap();
    let mut rows = [
        row("a", "waiting"),
        row("b", "waiting"),
        row("c", "waiting"),
    ];
    rows[0].tty = Some("/dev/ttys902".into());
    let actions = engine.tick(0, &rows, rows[0].tty.as_deref(), 0, false);
    assert!(actions.iter().any(|action| matches!(
        action,
        Action::Ack {
            by: AckBy::Focus,
            ..
        }
    )));
    assert!(matches!(
        &engine.click(1, "b")[0],
        Action::Ack {
            by: AckBy::Click,
            ..
        }
    ));
    fs::remove_file(&parent).unwrap();
    save_ack(&path, &rows[2]).unwrap();
    assert!(rings(&engine.tick(60_000, &rows, None, 100, false)).is_empty());
    rows[2].since += 1;
    engine.tick(61_000, &rows, None, 100, false);
    engine.click(62_000, "c");
    assert!(rings(&engine.tick(120_000, &rows, None, 100, false)).is_empty());
    rows[0].reason = Some("question".into());
    assert_eq!(
        rings(&engine.tick(121_000, &rows, None, 100, false)).len(),
        1
    );
}

#[test]
fn failed_ack_reload_keeps_acks_and_retries_the_same_mtime() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("acks.json");
    let rows = [row("a", "waiting"), row("b", "waiting")];
    save_ack(&path, &rows[0]).unwrap();
    let mut engine = Engine::with_ack_path(config(), path.clone()).unwrap();
    engine.tick(0, &rows, None, 100, false);
    fs::write(&path, "invalid JSON").unwrap();
    let mtime = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1);
    fs::File::open(&path).unwrap().set_modified(mtime).unwrap();
    let loaded_mtime = engine.ack_mtime;
    engine.tick(1000, &rows, None, 100, false);
    assert_eq!(engine.ack_mtime, loaded_mtime);
    assert!(engine.snapshot.rows[0].acked);
    crate::write_json(
        &path,
        &BTreeMap::from([("a", Ack::from(&rows[0])), ("b", Ack::from(&rows[1]))]),
    )
    .unwrap();
    fs::File::open(&path).unwrap().set_modified(mtime).unwrap();
    assert!(rings(&engine.tick(60_000, &rows, None, 100, false)).is_empty());
    assert!(engine.snapshot.rows.iter().all(|row| row.acked));
    fs::remove_file(path).unwrap();
    engine.tick(61_000, &rows, None, 100, false);
    assert!(engine.snapshot.rows.iter().all(|row| !row.acked));
}

#[test]
fn backward_clock_shift_preserves_ring_spacing_and_phone_age() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = Engine::with_ack_path(
        Config {
            phone_command: vec!["unused".into()],
            ..config()
        },
        dir.path().join("acks.json"),
    )
    .unwrap();
    let rows = [row("a", "waiting")];
    engine.tick(3_600_000, &rows, None, 300, false);
    engine.tick(3_605_000, &rows, None, 300, false);
    assert!(engine.tick(0, &rows, None, 300, false).is_empty());
    assert!(engine.tick(54_999, &rows, None, 300, false).is_empty());
    assert_eq!(
        rings(&engine.tick(55_000, &rows, None, 300, false))[0].1,
        0.44
    );
    assert!(
        engine
            .tick(115_000, &rows, None, 300, false)
            .iter()
            .any(|action| matches!(action, Action::Phone { .. }))
    );
}

#[test]
fn shortened_and_cancelled_quiet_release_catch_up_rings() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = Engine::with_ack_path(config(), dir.path().join("acks.json")).unwrap();
    let rows = [row("a", "waiting")];
    engine.quiet(0, 600_000);
    engine.tick(0, &rows, None, 100, false);
    engine.quiet(10_000, 20_000);
    assert_eq!(
        rings(&engine.tick(20_000, &rows, None, 100, false))[0].1,
        0.35
    );
    engine.quiet(30_000, 600_000);
    assert!(rings(&engine.tick(100_000, &rows, None, 100, false)).is_empty());
    engine.quiet(101_000, 101_000);
    assert_eq!(
        rings(&engine.tick(101_000, &rows, None, 100, false)).len(),
        1
    );
    assert!(engine.tick(101_000, &rows, None, 100, false).is_empty());
}

#[test]
fn dnd_boundaries_follow_local_calendar() {
    if std::env::var("TZ").as_deref() != Ok("America/New_York") {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "engine::tests::dnd_boundaries_follow_local_calendar",
            ])
            .env("TZ", "America/New_York")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let ms = |s: &str| {
        chrono::DateTime::parse_from_rfc3339(s)
            .unwrap()
            .timestamp_millis()
    };
    for (end, start, want) in [
        (
            "07:00",
            "2026-03-08T01:30:00-05:00",
            "2026-03-08T07:00:00-04:00",
        ),
        (
            "07:00",
            "2026-11-01T00:30:00-04:00",
            "2026-11-01T07:00:00-05:00",
        ),
        (
            "02:30",
            "2026-03-08T01:30:00-05:00",
            "2026-03-08T03:00:00-04:00",
        ),
        (
            "01:30",
            "2026-11-01T00:30:00-04:00",
            "2026-11-01T01:30:00-04:00",
        ),
        (
            "01:30",
            "2026-11-01T01:15:00-05:00",
            "2026-11-01T01:30:00-05:00",
        ),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let config = Config {
            dnd: format!("22:00-{end}"),
            ..config()
        };
        let mut engine = Engine::with_ack_path(config, dir.path().join("acks.json")).unwrap();
        let rows = [row("a", "waiting")];
        assert!(rings(&engine.tick(ms(start), &rows, None, 100, false)).is_empty());
        assert!(rings(&engine.tick(ms(want) - 1, &rows, None, 100, false)).is_empty());
        assert_eq!(
            rings(&engine.tick(ms(want), &rows, None, 100, false)).len(),
            1
        );
    }
    let dir = tempfile::tempdir().unwrap();
    let mut engine = Engine::with_ack_path(config(), dir.path().join("acks.json")).unwrap();
    let start = ms("2026-03-08T01:30:00-05:00");
    let end = ms("2026-03-08T03:30:00-04:00");
    let rows = [row("a", "waiting")];
    engine.quiet(start, end);
    assert!(rings(&engine.tick(start, &rows, None, 100, false)).is_empty());
    assert!(rings(&engine.tick(end - 1, &rows, None, 100, false)).is_empty());
    assert_eq!(rings(&engine.tick(end, &rows, None, 100, false)).len(), 1);
}

#[test]
fn focus_boundary_and_null_ttys() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = Engine::with_ack_path(config(), dir.path().join("acks.json")).unwrap();
    let mut rows = [row("a", "waiting")];
    let tty = rows[0].tty.clone();
    assert_eq!(
        rings(&engine.tick(0, &rows, tty.as_deref(), 31, false)).len(),
        1
    );
    let actions = engine.tick(1, &rows, tty.as_deref(), 30, false);
    assert!(matches!(
        &actions[..],
        [
            Action::Ack {
                by: AckBy::Focus,
                ..
            },
            Action::Light { count: 0, .. }
        ]
    ));
    rows[0].since += 1;
    rows[0].tty = None;
    rows[0].surface = "headless".into();
    rows[0].state = "error".into();
    let actions = engine.tick(2, &rows, None, 0, false);
    assert_eq!(rings(&actions).len(), 1);
    assert!(
        !actions
            .iter()
            .any(|action| matches!(action, Action::Ack { .. }))
    );
}

#[test]
fn a_crash_keeps_the_tab_or_app_its_process_ran_in() {
    let dir = tempfile::tempdir().unwrap();
    let mut engine = Engine::with_ack_path(config(), dir.path().join("acks.json")).unwrap();
    let mut desktop = row("d", "working");
    (desktop.pid, desktop.tty, desktop.surface) = (Some(20), None, "claude-desktop".into());
    let mut rows = [row("a", "working"), desktop];
    rows[0].pid = Some(10);
    let tty = rows[0].tty.clone();
    engine.tick(0, &rows, None, 100, false);
    for row in &mut rows {
        row.tty = None;
        row.surface = "headless".into();
        row.state = "error".into();
        row.reason = Some("crashed".into());
    }
    let mut resumed = rows.clone();
    resumed[0].pid = Some(11);
    engine.keep_focus_targets(&mut resumed);
    assert_eq!(resumed[0].tty, None, "another process of the same session");
    // The app restores the rows before it decides whether to ask Terminal, and every tick keeps them.
    engine.keep_focus_targets(&mut rows);
    assert_eq!(
        rows.iter()
            .map(|row| (row.tty.as_deref(), row.surface.as_str()))
            .collect::<Vec<_>>(),
        [(tty.as_deref(), "terminal"), (None, "claude-desktop")]
    );
    engine.tick(1_000, &rows, None, 100, false);
    engine.tick(2_000, &rows, None, 100, false);
    let actions = engine.tick(3_000, &rows, tty.as_deref(), 0, false);
    assert!(actions.iter().any(|action| matches!(
        action,
        Action::Ack {
            by: AckBy::Focus,
            ..
        }
    )));
}

#[test]
fn quiet_and_dnd_overlap_without_spending_yellow_rings() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        dnd: "23:00-08:00".into(),
        ..config()
    };
    let mut engine = Engine::with_ack_path(config, dir.path().join("acks.json")).unwrap();
    let start = Local
        .with_ymd_and_hms(2026, 10, 14, 7, 58, 0)
        .unwrap()
        .timestamp_millis();
    let rows = [row("a", "idle")];
    engine.quiet(start, start + 130_000);
    assert!(rings(&engine.tick(start, &rows, None, 100, false)).is_empty());
    assert!(rings(&engine.tick(start + 120_000, &rows, None, 100, false)).is_empty());
    assert_eq!(
        rings(&engine.tick(start + 130_000, &rows, None, 100, false))[0].1,
        0.6
    );
    // Two remaining offsets are coalesced on this late tick, not replayed later.
    assert_eq!(
        rings(&engine.tick(start + 400_000, &rows, None, 100, false)).len(),
        1
    );
    assert!(
        engine
            .tick(start + 500_000, &rows, None, 100, false)
            .is_empty()
    );
}

#[test]
fn phone_waits_for_quiet_and_away_then_resets_only_for_a_new_episode() {
    let dir = tempfile::tempdir().unwrap();
    let config = Config {
        phone_command: vec!["/not/executed".into(), "{name}|{state}|{detail}".into()],
        ..config()
    };
    let mut engine = Engine::with_ack_path(config, dir.path().join("acks.json")).unwrap();
    let mut rows = [row("a", "waiting")];
    engine.quiet(0, 200_000);
    engine.tick(0, &rows, None, 300, false);
    let phones = |actions: Vec<Action>| {
        actions
            .into_iter()
            .filter(|a| matches!(a, Action::Phone { .. }))
            .collect::<Vec<_>>()
    };
    assert!(phones(engine.tick(120_000, &rows, None, 300, false)).is_empty());
    assert!(phones(engine.tick(200_000, &rows, None, 299, false)).is_empty());
    let actions = phones(engine.tick(201_000, &rows, None, 300, false));
    assert!(
        matches!(&actions[..], [Action::Phone { argv, .. }] if argv == &["/not/executed", "a|waiting|"])
    );
    rows[0].detail = Some("updated".into());
    assert!(phones(engine.tick(300_000, &rows, None, 0, true)).is_empty());
    rows[0].since += 1;
    assert!(phones(engine.tick(400_000, &rows, None, 0, true)).is_empty());
    assert_eq!(phones(engine.tick(520_000, &rows, None, 0, true)).len(), 1);
}
