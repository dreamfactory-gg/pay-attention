use crate::{
    config, detail, home,
    hooks::{ClaudeRecord, Stored, session_path},
    json_files, lock,
    process::{self, Inspector},
    read_json, read_optional, session_lock, state_dir, text, timed_lock, transcript, write_json,
};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant, UNIX_EPOCH},
};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Row {
    pub key: String,
    pub harness: String,
    pub surface: String,
    pub pid: Option<u32>,
    pub tty: Option<String>,
    pub name: String,
    pub cwd: String,
    pub state: String,
    pub reason: Option<String>,
    pub detail: Option<String>,
    pub since: i64,
    pub acked: bool,
}

impl Row {
    pub fn color(&self) -> &'static str {
        match self.state.as_str() {
            "error" => "red",
            "waiting" => "blue",
            "idle" => "yellow",
            "working" => "green",
            _ => "grey",
        }
    }

    pub fn alert_color(&self) -> Option<&'static str> {
        (!self.acked && !(self.surface == "headless" && self.state != "error"))
            .then(|| self.color())
            .filter(|color| matches!(*color, "red" | "blue" | "yellow"))
    }

    pub fn section(&self) -> &'static str {
        if self.surface == "headless" && self.state != "error" {
            "headless"
        } else if self.alert_color().is_some() {
            "needs"
        } else if self.acked && matches!(self.state.as_str(), "waiting" | "error" | "idle") {
            "seen"
        } else {
            "working"
        }
    }

    fn error(&mut self, reason: &str) {
        self.state = "error".into();
        self.reason = Some(reason.into());
    }

    fn check_stalled(
        &mut self,
        output: Option<&Path>,
        threshold: u64,
        usage: &mut Option<HashMap<u32, process::Usage>>,
    ) -> Result<()> {
        if self.state == "working"
            && let Some(modified) = output.and_then(|p| fs::metadata(p).ok()?.modified().ok())
            && let Ok(age) = modified.elapsed()
            && age.as_secs() > threshold
        {
            if let Some(pid) = self.pid {
                if usage.is_none() {
                    *usage = Some(process::usage()?);
                }
                if process::tree_cpu(pid, usage.as_ref().unwrap()) >= 1.0 {
                    return Ok(());
                }
            }
            self.error("stalled");
            self.detail = detail(&format!("no output {}m", age.as_secs() / 60));
            self.since =
                modified.duration_since(UNIX_EPOCH)?.as_millis() as i64 + threshold as i64 * 1000;
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
struct Crash {
    start: u64,
    #[serde(flatten)]
    row: Row,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Snapshot {
    pub light: String,
    pub rows: Vec<Row>,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Ack {
    since: i64,
    state: String,
    reason: Option<String>,
}

impl Ack {
    pub(crate) fn matches(&self, row: &Row) -> bool {
        self.since == row.since && self.state == row.state && self.reason == row.reason
    }
}

impl From<&Row> for Ack {
    fn from(row: &Row) -> Self {
        Self {
            since: row.since,
            state: row.state.clone(),
            reason: row.reason.clone(),
        }
    }
}

fn transcript_path(home: &Path, id: &str) -> Option<PathBuf> {
    if id.contains(['/', '\\']) {
        return None;
    }
    fs::read_dir(home.join(".claude/projects"))
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path().join(format!("{id}.jsonl")))
        .find(|p| p.is_file())
}

pub fn snapshot() -> Result<Snapshot> {
    snapshot_with_config(&config()?)
}

pub fn snapshot_with_config(config: &crate::Config) -> Result<Snapshot> {
    let (snapshot, crash_work) = snapshot_pass(config, false)?;
    if crash_work
        && let Ok(_guard) = timed_lock(
            &state_dir()?.join("crashes.lock"),
            Instant::now(),
            Duration::from_millis(200),
            true,
        )
    {
        return snapshot_pass(config, true).map(|(snapshot, _)| snapshot);
    }
    Ok(snapshot)
}

fn snapshot_pass(config: &crate::Config, write_crashes: bool) -> Result<(Snapshot, bool)> {
    let mut crash_work = false;
    let home = home()?;
    let mut inspector = Inspector::default();
    let mut usage = None;
    let ack_path = state_dir()?.join("acks.json");
    let acks = load_acks(&ack_path).unwrap_or_default();
    let mut rows = Vec::new();
    let crash_dir = state_dir()?.join("crashes");
    let mut registries: BTreeMap<String, (u64, u32, i64, PathBuf, serde_json::Value)> =
        BTreeMap::new();
    let mut live_starts: HashMap<String, u64> = HashMap::new();
    for path in json_files(&home.join(".claude/sessions")) {
        let Some(pid) = path.file_stem().and_then(|s| s.to_str()?.parse().ok()) else {
            continue;
        };
        // Inspect before reading: a dead process can no longer update its registry.
        let live_start = inspector.get(pid).map(|p| p.start);
        let Some(v) = read_json(&path) else {
            continue;
        };
        let (Some(id), Some(since), Some(start)) = (
            v["sessionId"].as_str(),
            v["statusUpdatedAt"].as_i64(),
            process::parse_utc_start(text(&v["procStart"])),
        ) else {
            continue;
        };
        let Ok(path) = session_path("claude", id) else {
            continue;
        };
        let key = format!("claude:{id}");
        if live_start != Some(start) {
            inspector.refresh(pid);
        }
        if inspector.get(pid).is_some_and(|p| p.start == start) {
            live_starts
                .entry(key.clone())
                .and_modify(|latest| *latest = (*latest).max(start))
                .or_insert(start);
        }
        if registries
            .get(&key)
            .is_none_or(|(latest, ..)| start > *latest)
        {
            registries.insert(key, (start, pid, since, path, v));
        }
    }
    for (key, (start, pid, since, path, v)) in &registries {
        let state = match text(&v["status"]) {
            "busy" | "shell" => "working",
            "idle" => "idle",
            "waiting" => "waiting",
            _ => "unknown",
        };
        let reason = if state == "waiting" {
            match text(&v["waitingFor"]) {
                "input needed" => Some("question"),
                "permission prompt" | "sandbox request" | "worker request" | "goal proposal" => {
                    Some("permission")
                }
                "dialog open" => Some("dialog"),
                _ => None,
            }
        } else {
            None
        };
        let transcript = transcript_path(&home, text(&v["sessionId"]));
        let mut row = Row {
            key: key.clone(),
            harness: "claude".into(),
            surface: "headless".into(),
            pid: Some(*pid),
            tty: None,
            name: text(&v["name"]).into(),
            cwd: text(&v["cwd"]).into(),
            state: state.into(),
            reason: reason.map(str::to_owned),
            detail: transcript
                .as_ref()
                .filter(|_| state == "idle")
                .and_then(|p| transcript::claude_detail(p)),
            since: *since,
            acked: false,
        };
        let record = ClaudeRecord::read(path);
        if inspector.get(*pid).is_some_and(|p| p.start == *start) {
            if let Some(failure) = record.failure
                && !(matches!(text(&v["status"]), "busy" | "waiting") && *since > failure.since)
            {
                row.error(&failure.reason);
                row.detail = failure.detail;
                row.since = failure.since;
            } else if state == "waiting"
                && let Some(pending) = record.pending
                && Some(pending.kind.as_str()) == reason
                && pending.since.abs_diff(*since) <= 10_000
            {
                row.detail = pending.detail;
            }
        } else {
            row.error("crashed");
            row.detail = None;
            let path = crash_dir.join(crash_name(&row, *start).unwrap());
            if let Some(stored) = read_crash(&path) {
                row = stored.row;
            } else if !acks.get(&row.key).is_some_and(|ack| ack.matches(&row)) {
                crash_work = true;
                if write_crashes {
                    let _ = write_crash(&path, &row, *start);
                }
            }
        }
        if v["status"] != "shell" {
            row.check_stalled(transcript.as_deref(), config.stalled_after_secs, &mut usage)?;
        }
        rows.push(row);
    }
    rows.extend(
        crash_rows(
            &crash_dir,
            &live_starts,
            &acks,
            write_crashes,
            &mut crash_work,
        )
        .into_iter()
        .filter(|row| !registries.contains_key(&row.key)),
    );
    for path in json_files(&state_dir()?.join("sessions")) {
        let Some(s) = Stored::read(&path) else {
            continue;
        };
        let key = format!("{}:{}", s.harness, s.session_id);
        if !matches!(s.harness.as_str(), "codex" | "hermes") {
            continue;
        }
        let crashed = s.crashed(&mut inspector);
        let mut row = Row {
            key,
            harness: s.harness,
            surface: "headless".into(),
            pid: s.pid,
            tty: None,
            name: Path::new(&s.cwd)
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            cwd: s.cwd,
            state: s.state,
            reason: s.reason,
            detail: s.detail,
            since: s.since,
            acked: false,
        };
        if crashed {
            row.error("crashed");
            row.detail = None;
        }
        row.check_stalled(
            s.transcript_path
                .as_deref()
                .or_else(|| (row.harness == "hermes").then_some(path.as_path())),
            config.stalled_after_secs,
            &mut usage,
        )?;
        rows.push(row);
    }
    rows.retain_mut(|row| {
        if let Some(pid) = row.pid
            && row.reason.as_deref() != Some("crashed")
        {
            let chain = inspector.chain(pid);
            if chain.iter().any(|p| p.named("cotal")) {
                return false;
            }
            row.tty = process::tty(pid);
            row.surface = if row.tty.is_some() {
                "terminal"
            } else if chain.iter().any(|p| {
                p.argv.iter().take(2).any(|a| {
                    Path::new(a)
                        .components()
                        .any(|c| c.as_os_str() == "Claude.app")
                })
            }) {
                "claude-desktop"
            } else {
                "headless"
            }
            .into();
        }
        row.acked = acks.get(&row.key).is_some_and(|ack| ack.matches(row));
        true
    });
    rows.sort_by(|a, b| a.key.cmp(&b.key));
    Ok((
        Snapshot {
            light: light(&rows).into(),
            rows,
        },
        crash_work,
    ))
}

pub(crate) fn light(rows: &[Row]) -> &'static str {
    ["red", "blue", "yellow"]
        .into_iter()
        .find(|color| rows.iter().any(|r| r.alert_color() == Some(*color)))
        .unwrap_or_else(|| {
            if rows.iter().any(|r| r.state == "working") {
                "green"
            } else {
                "grey"
            }
        })
}

pub fn ack(key: &str) -> Result<()> {
    let row = snapshot()?
        .rows
        .into_iter()
        .find(|r| r.key == key)
        .context("unknown session key")?;
    save_ack(&state_dir()?.join("acks.json"), &row)?;
    Ok(())
}

pub(crate) fn load_acks(path: &Path) -> Result<BTreeMap<String, Ack>> {
    Ok(match read_optional(path)? {
        Some(bytes) => serde_json::from_slice(&bytes)?,
        None => BTreeMap::new(),
    })
}

fn crash_rows(
    dir: &Path,
    live_starts: &HashMap<String, u64>,
    acks: &BTreeMap<String, Ack>,
    write_crashes: bool,
    crash_work: &mut bool,
) -> Vec<Row> {
    let crashes: Vec<_> = json_files(dir)
        .into_iter()
        .filter_map(|path| {
            let crash = read_crash(&path)?;
            Some((crash.start, path, crash.row))
        })
        .collect();
    let mut newest = HashMap::new();
    for (start, _, row) in &crashes {
        newest
            .entry(&row.key)
            .and_modify(|latest: &mut u64| *latest = (*latest).max(*start))
            .or_insert(*start);
    }
    crashes
        .iter()
        .filter_map(|(start, path, row)| {
            if live_starts.get(&row.key).is_some_and(|live| live > start)
                || newest[&row.key] > *start
                || acks.get(&row.key).is_some_and(|ack| ack.matches(row))
            {
                *crash_work = true;
                if write_crashes {
                    let _ = fs::remove_file(path);
                }
                None
            } else {
                Some(row.clone())
            }
        })
        .map(|row| (row.key.clone(), row))
        .collect::<BTreeMap<_, _>>()
        .into_values()
        .collect()
}

fn crash_name(row: &Row, start: u64) -> Option<String> {
    let id = row.key.strip_prefix("claude:")?;
    Some(format!("claude-{id}-{start}-{}.json", row.pid?))
}

fn read_crash(path: &Path) -> Option<Crash> {
    let crash: Crash = serde_json::from_value(read_json(path)?).ok()?;
    let row = &crash.row;
    (row.harness == "claude"
        && row.state == "error"
        && row.reason.as_deref() == Some("crashed")
        && path.file_name()?.to_str()? == crash_name(row, crash.start)?)
    .then_some(crash)
}

fn write_crash(path: &Path, row: &Row, start: u64) -> Result<()> {
    if read_crash(path).is_none() {
        // Remove an invalid entry itself (a symlink is never followed), so the new file is ours and 0600.
        match fs::remove_file(path) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
        write_json(
            path,
            &Crash {
                start,
                row: row.clone(),
            },
        )?;
    }
    Ok(())
}

pub(crate) fn save_ack(path: &Path, row: &Row) -> Result<BTreeMap<String, Ack>> {
    let _guard = lock(&path.with_extension("lock"))?;
    let mut acks = load_acks(path)?;
    acks.insert(row.key.clone(), Ack::from(row));
    write_json(path, &acks)?;
    if row.reason.as_deref() == Some("crashed")
        && let Some((harness, id)) = row.key.split_once(':')
        && harness == row.harness
        && matches!(harness, "codex" | "hermes")
        && let Ok(path) = session_path(harness, id)
        && let Some(record) = Stored::read(&path)
        && record.harness == harness
        && record.session_id == id
        && record.since == row.since
        && record.crashed(&mut Inspector::default())
        && let Ok(_guard) = session_lock(Instant::now(), true)
        && Stored::read(&path).is_some_and(|current| {
            current.harness == harness
                && current.session_id == id
                && current.since == record.since
                && current.pid == record.pid
                && current.proc_start == record.proc_start
        })
    {
        let _ = fs::remove_file(path);
    }
    Ok(acks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn crash(since: i64) -> Row {
        serde_json::from_value(serde_json::json!({
            "key": "claude:test", "harness": "claude", "surface": "headless",
            "pid": 42, "tty": null, "name": "test", "cwd": "/tmp",
            "state": "error", "reason": "crashed", "detail": null,
            "since": since, "acked": false
        }))
        .unwrap()
    }

    #[test]
    fn crash_file_is_private_preserved_when_valid_and_replaced_when_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let _guard = lock(&dir.path().join("crashes.lock")).unwrap();
        let path = dir.path().join("claude-test-10-42.json");
        write_crash(&path, &crash(1), 10).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let bytes = fs::read(&path).unwrap();
        write_crash(&path, &crash(100), 10).unwrap();
        assert_eq!(fs::read(&path).unwrap(), bytes);
        fs::write(&path, b"{").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        write_crash(&path, &crash(2), 10).unwrap();
        assert_eq!(read_crash(&path).map(|crash| crash.row), Some(crash(2)));
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn invalid_crash_contents_are_ignored_and_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claude-test-10-42.json");
        let row = crash(1);
        write_crash(&path, &row, 10).unwrap();
        let valid: serde_json::Value = read_json(&path).unwrap();
        for (field, value) in [
            ("harness", serde_json::json!("codex")),
            ("state", serde_json::json!("unknown")),
            ("reason", serde_json::json!("question")),
            ("key", serde_json::json!("claude:other")),
            ("pid", serde_json::json!(41)),
            ("start", serde_json::json!(11)),
            ("start", serde_json::Value::Null),
        ] {
            let mut invalid = valid.clone();
            invalid[field] = value;
            write_json(&path, &invalid).unwrap();
            assert!(read_crash(&path).is_none(), "{field}");
            let mut work = false;
            let live = HashMap::from([(row.key.clone(), 20)]);
            let acks = BTreeMap::from([(row.key.clone(), Ack::from(&row))]);
            assert!(crash_rows(dir.path(), &live, &acks, true, &mut work).is_empty());
            assert!(!work);
            assert!(path.exists());
            write_crash(&path, &row, 10).unwrap();
            assert_eq!(read_crash(&path).map(|crash| crash.row), Some(row.clone()));
        }
    }

    #[test]
    fn an_older_format_decoy_cannot_rank_show_or_clear() {
        let dir = tempfile::tempdir().unwrap();
        let mut row = crash(2);
        row.key = "claude:test-999999999999".into();
        let path = dir.path().join(crash_name(&row, 10).unwrap());
        write_crash(&path, &row, 10).unwrap();
        let decoy = dir.path().join("claude-test-999999999999-1793700000.json");
        fs::copy(&path, &decoy).unwrap();
        let mut work = false;
        assert_eq!(
            crash_rows(
                dir.path(),
                &HashMap::new(),
                &BTreeMap::new(),
                true,
                &mut work
            ),
            vec![row.clone()]
        );
        assert!(!work);
        assert!(path.exists());
        let acks = BTreeMap::from([(row.key.clone(), Ack::from(&row))]);
        assert!(crash_rows(dir.path(), &HashMap::new(), &acks, true, &mut work).is_empty());
        assert!(!path.exists());
        assert!(decoy.exists());
    }

    #[test]
    fn only_a_strictly_later_live_process_clears_a_crash() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("claude-test-10-42.json");
        for start in [9, 10, 11] {
            let row = crash(1);
            write_crash(&path, &row, 10).unwrap();
            let live = HashMap::from([(row.key.clone(), start)]);
            let rows = crash_rows(dir.path(), &live, &BTreeMap::new(), true, &mut false);
            assert_eq!(rows.is_empty(), start > 10);
            assert_eq!(path.exists(), start <= 10);
        }
    }

    #[test]
    fn newest_crash_wins_and_ack_cleanup_deletes_only_its_exact_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut old = crash(1);
        old.pid = Some(41);
        let new = crash(2);
        let old_path = dir.path().join("claude-test-9-41.json");
        let new_path = dir.path().join("claude-test-10-42.json");
        write_crash(&old_path, &old, 9).unwrap();
        write_crash(&new_path, &new, 10).unwrap();
        let live = HashMap::from([(old.key.clone(), 9)]);
        let mut acks = BTreeMap::from([(old.key.clone(), Ack::from(&old))]);
        let mut work = false;
        assert_eq!(
            crash_rows(dir.path(), &live, &acks, false, &mut work),
            vec![new.clone()]
        );
        assert!(work);
        assert!(old_path.exists());
        assert_eq!(
            crash_rows(dir.path(), &live, &acks, true, &mut false),
            vec![new.clone()]
        );
        assert!(!old_path.exists());
        assert!(new_path.exists());
        acks.insert(new.key.clone(), Ack::from(&new));
        assert!(crash_rows(dir.path(), &live, &acks, true, &mut false).is_empty());
        assert!(!new_path.exists());
    }

    #[test]
    fn same_second_crashes_have_distinct_files_but_no_finer_ordering() {
        let dir = tempfile::tempdir().unwrap();
        for pid in [41, 42] {
            let mut row = crash(1);
            row.pid = Some(pid);
            write_crash(
                &dir.path().join(format!("claude-test-10-{pid}.json")),
                &row,
                10,
            )
            .unwrap();
        }
        assert_eq!(
            crash_rows(
                dir.path(),
                &HashMap::new(),
                &BTreeMap::new(),
                true,
                &mut false
            )
            .len(),
            1
        );
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn reading_crashes_creates_no_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("absent/crashes");
        let mut work = false;
        assert!(crash_rows(&path, &HashMap::new(), &BTreeMap::new(), false, &mut work).is_empty());
        assert!(!work);
        assert!(!path.parent().unwrap().exists());
    }

    #[test]
    fn stall_since_tracks_output_deadline_and_invalidates_the_previous_ack() {
        let output = tempfile::NamedTempFile::new().unwrap();
        let working = Row {
            key: "hermes:test".into(),
            harness: "hermes".into(),
            surface: "headless".into(),
            pid: None,
            tty: None,
            name: "test".into(),
            cwd: "/tmp".into(),
            state: "working".into(),
            reason: None,
            detail: None,
            since: 1,
            acked: false,
        };
        let mut previous_ack: Option<Ack> = None;
        for seconds in [1000, 1100] {
            output
                .as_file()
                .set_modified(UNIX_EPOCH + Duration::from_secs(seconds))
                .unwrap();
            let mut row = working.clone();
            row.check_stalled(Some(output.path()), 60, &mut None)
                .unwrap();
            assert_eq!(row.reason.as_deref(), Some("stalled"));
            assert_eq!(row.since, (seconds as i64 + 60) * 1000);
            assert!(!previous_ack.as_ref().is_some_and(|ack| ack.matches(&row)));
            let ack = Ack {
                since: row.since,
                state: row.state.clone(),
                reason: row.reason.clone(),
            };
            let mut repeated = working.clone();
            repeated
                .check_stalled(Some(output.path()), 60, &mut None)
                .unwrap();
            assert!(ack.matches(&repeated));
            previous_ack = Some(ack);
        }
        let mut row = working;
        row.check_stalled(None, 60, &mut None).unwrap();
        assert_eq!(row.state, "working");
    }
}
