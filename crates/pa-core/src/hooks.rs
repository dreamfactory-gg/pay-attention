use crate::{
    detail, now_ms, process::Inspector, read_json, session_lock, state_dir, text, transcript,
    write_json,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::{Instant, SystemTime},
};

pub(crate) const CLAUDE_EVENTS: &[&str] =
    &["PreToolUse", "PermissionRequest", "StopFailure", "Stop"];
pub(crate) const CODEX_EVENTS: &[&str] = &[
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PermissionRequest",
    "Stop",
    "Interrupt",
    "SessionEnd",
];
pub(crate) const HERMES_EVENTS: &[&str] = &[
    "on_session_start",
    "pre_llm_call",
    "post_llm_call",
    "on_human_input_request",
    "on_human_input_resolved",
    "pre_approval_request",
    "post_approval_response",
    "on_session_end",
    "on_session_finalize",
];

#[derive(Default, Serialize, Deserialize)]
pub(crate) struct ClaudeRecord {
    pub failure: Option<Failure>,
    pub pending: Option<Pending>,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Failure {
    pub reason: String,
    pub detail: Option<String>,
    pub since: i64,
}

#[derive(Serialize, Deserialize)]
pub(crate) struct Pending {
    pub kind: String,
    pub detail: Option<String>,
    pub since: i64,
}

impl ClaudeRecord {
    pub(crate) fn read(path: &Path) -> Self {
        read_json(path)
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default()
    }

    pub(crate) fn update(
        path: &Path,
        change: impl FnOnce(&mut Self) -> bool,
    ) -> anyhow::Result<()> {
        let _guard = session_lock(Instant::now(), true)?;
        let mut record = Self::read(path);
        if change(&mut record) {
            if record.failure.is_none() && record.pending.is_none() {
                match fs::remove_file(path) {
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                    _ => {}
                }
            } else {
                write_json(path, &record)?;
            }
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Stored {
    pub session_id: String,
    pub harness: String,
    pub pid: Option<u32>,
    pub proc_start: Option<u64>,
    pub cwd: String,
    pub transcript_path: Option<PathBuf>,
    pub state: String,
    pub reason: Option<String>,
    pub detail: Option<String>,
    pub since: i64,
}

impl Stored {
    pub(crate) fn read(path: &Path) -> Option<Self> {
        read_json(path).and_then(|v| serde_json::from_value(v).ok())
    }

    pub(crate) fn crashed(&self, inspector: &mut Inspector) -> bool {
        self.pid.is_some_and(|pid| {
            !inspector
                .get(pid)
                .is_some_and(|p| Some(p.start) == self.proc_start)
        })
    }
}

pub(crate) fn session_path(harness: &str, id: &str) -> anyhow::Result<PathBuf> {
    anyhow::ensure!(
        matches!(harness, "claude" | "codex" | "hermes"),
        "invalid harness"
    );
    anyhow::ensure!(
        !id.is_empty() && !id.contains(['/', '\\']) && id != "." && id != "..",
        "invalid session id"
    );
    Ok(state_dir()?
        .join("sessions")
        .join(format!("{harness}-{id}.json")))
}

pub fn hook(harness: &str, event: &str, input: impl Read) {
    let _ = handle(harness, event, input);
}

fn handle(harness: &str, event: &str, input: impl Read) -> anyhow::Result<()> {
    if std::env::var_os("COTAL_NAME").is_some() {
        return Ok(());
    }
    let valid = match harness {
        "claude" => CLAUDE_EVENTS.contains(&event),
        "codex" => CODEX_EVENTS.contains(&event),
        "hermes" => HERMES_EVENTS.contains(&event),
        _ => false,
    };
    if !valid {
        return Ok(());
    }
    let value: Value = serde_json::from_reader(input)?;
    if harness == "hermes"
        && ["platform", "surface"]
            .iter()
            .any(|k| value["extra"].get(k).is_some_and(|p| p != "cli"))
    {
        return Ok(());
    }
    let tool = text(&value["tool_name"]);
    // In default permission mode Claude also fires PermissionRequest for AskUserQuestion; the PreToolUse question
    // record already holds its text.
    if harness == "claude"
        && match event {
            "PreToolUse" => tool != "AskUserQuestion",
            "PermissionRequest" => tool == "AskUserQuestion",
            _ => false,
        }
    {
        return Ok(());
    }
    let id = value["session_id"]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("missing session_id"))?;
    let path = session_path(harness, id)?;
    if harness == "claude" {
        let since = now_ms();
        let pending = match event {
            "PreToolUse" => Some(("question", transcript::questions(&value["tool_input"]))),
            "PermissionRequest" => Some((
                "permission",
                transcript::claude_tool(&value["tool_input"], tool),
            )),
            _ => None,
        };
        return ClaudeRecord::update(&path, |record| {
            match event {
                "Stop" => return record.failure.take().is_some() | record.pending.take().is_some(),
                "StopFailure" => {
                    record.failure = Some(Failure {
                        reason: if value["error"] == "rate_limit" {
                            "rate_limit"
                        } else {
                            "api_error"
                        }
                        .into(),
                        detail: detail(text(&value["error"])),
                        since,
                    })
                }
                _ => {
                    let (kind, message) = pending.unwrap();
                    record.pending = Some(Pending {
                        kind: kind.into(),
                        detail: detail(&message),
                        since,
                    });
                }
            }
            true
        });
    }
    if matches!(event, "SessionEnd" | "on_session_finalize") {
        let _guard = session_lock(Instant::now(), true)?;
        let _ = fs::remove_file(path);
        return Ok(());
    }
    let mut inspector = Inspector::default();
    let agent = inspector
        .chain(std::process::id())
        .into_iter()
        .skip(1)
        .find(|p| p.named(harness));
    let transcript_path = value["transcript_path"]
        .as_str()
        .filter(|p| !p.is_empty())
        .map(PathBuf::from);
    let start = Instant::now();
    let mut scanned = None;
    let (_guard, old, transcript_path, error) = loop {
        let guard = session_lock(start, scanned.is_none())?;
        let old = Stored::read(&path);
        let selected = transcript_path
            .clone()
            .or_else(|| old.as_ref().and_then(|s| s.transcript_path.clone()));
        if harness == "codex"
            && matches!(event, "Stop" | "Interrupt")
            && scanned.as_ref().is_none_or(|(path, _)| path != &selected)
        {
            drop(guard);
            let error = selected.as_deref().and_then(transcript::codex_error);
            scanned = Some((selected, error));
            continue;
        }
        break (guard, old, selected, scanned.and_then(|(_, error)| error));
    };
    let extra = &value["extra"];
    let (state, reason, message) = match (harness, event) {
        ("codex", "SessionStart") | ("hermes", "on_session_start") => ("idle", None, String::new()),
        ("codex", "PreToolUse") if tool == "request_user_input" => (
            "waiting",
            Some("question"),
            transcript::questions(&value["tool_input"]),
        ),
        ("codex", "PermissionRequest") => (
            "waiting",
            Some("permission"),
            transcript::command(&value["tool_input"], tool),
        ),
        ("codex", "Stop" | "Interrupt") => match error {
            Some(error) => ("error", Some("api_error"), error),
            None => (
                "idle",
                None,
                text(&value["last_assistant_message"]).to_owned(),
            ),
        },
        ("hermes", "on_session_end") => {
            if extra["completed"] == true || extra["interrupted"] == true {
                (
                    "idle",
                    None,
                    old.as_ref()
                        .and_then(|s| s.detail.clone())
                        .unwrap_or_default(),
                )
            } else {
                ("error", Some("api_error"), String::new())
            }
        }
        ("hermes", "post_llm_call") => {
            ("idle", None, text(&extra["assistant_response"]).to_owned())
        }
        ("hermes", "on_human_input_request") => (
            "waiting",
            Some(if extra["kind"] == "clarify" {
                "question"
            } else {
                "permission"
            }),
            text(&extra["prompt"]).to_owned(),
        ),
        ("hermes", "pre_approval_request") => (
            "waiting",
            Some("permission"),
            transcript::command(extra, tool),
        ),
        _ => ("working", None, String::new()),
    };
    let mut stored = Stored {
        session_id: id.into(),
        harness: harness.into(),
        pid: agent.as_ref().map(|p| p.pid),
        proc_start: agent.map(|p| p.start),
        cwd: text(&value["cwd"]).into(),
        transcript_path,
        state: state.into(),
        reason: reason.map(str::to_owned),
        detail: detail(&message),
        since: now_ms(),
    };
    if let Some(old) = old
        && old.state == stored.state
        && old.reason == stored.reason
        && old.pid == stored.pid
        && old.proc_start == stored.proc_start
    {
        stored.since = old.since;
        if old == stored {
            fs::File::open(&path)?.set_modified(SystemTime::now())?;
            return Ok(());
        }
    }
    write_json(&path, &stored)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unreadable_claude_record_counts_as_empty() {
        let file = tempfile::NamedTempFile::new().unwrap();
        fs::write(file.path(), br#"{"failure": "unreadable"}"#).unwrap();
        let record = ClaudeRecord::read(file.path());
        assert!(record.failure.is_none() && record.pending.is_none());
    }

    #[test]
    fn non_cli_hermes_events_exit_before_session_lookup() {
        for platform in [
            json!("cron"),
            json!("gateway"),
            json!("telegram"),
            Value::Null,
            json!(""),
        ] {
            let payload = serde_json::to_vec(&json!({"extra": {"platform": platform}})).unwrap();
            for event in HERMES_EVENTS {
                assert!(handle("hermes", event, payload.as_slice()).is_ok());
            }
        }
    }
}
