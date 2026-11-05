use crate::{
    atomic_write_if, home,
    hooks::{CLAUDE_EVENTS, CODEX_EVENTS, HERMES_EVENTS},
    read_optional, resolve_path, text,
};
use anyhow::{Context, Result, ensure};
use chrono::Local;
use serde_json::{Value, json};
use std::{
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
};

#[derive(Debug, Default)]
pub struct InstallReport {
    pub changed: Vec<PathBuf>,
    pub warnings: Vec<String>,
}

pub fn install(binary: &Path, dry_run: bool) -> Result<InstallReport> {
    ensure!(binary.is_absolute(), "hook binary path must be absolute");
    run(Some(binary), dry_run)
}

pub fn uninstall() -> Result<InstallReport> {
    run(None, false)
}

fn command_words(command: &str) -> Option<Vec<String>> {
    let words = shell_words::split(command).ok()?;
    (words.first().is_some_and(|p| {
        Path::new(p)
            .file_name()
            .is_some_and(|n| n == "pay-attention")
    }) && words.get(1).is_some_and(|w| w == "hook"))
    .then_some(words)
}

fn owned(handler: &Value) -> bool {
    command_words(text(&handler["command"])).is_some()
}

fn json_hooks(
    path: &Path,
    harness: &str,
    events: &[&str],
    binary: Option<&Path>,
    dry: bool,
    report: &mut InstallReport,
) -> Result<()> {
    // Keep reads and replacement anchored to the same target if a symlink is retargeted.
    let target = resolve_path(path)?;
    let path = target.as_path();
    let original = read_optional(path)?;
    let mut value: Value = match &original {
        Some(bytes) => serde_json::from_slice(bytes)?,
        None => json!({}),
    };
    let before = value.clone();
    let root = value
        .as_object_mut()
        .context("settings must be a JSON object")?;
    if let Some(hooks) = root.get_mut("hooks").and_then(Value::as_object_mut) {
        hooks.retain(|_, groups| {
            if let Some(groups) = groups.as_array_mut() {
                groups.retain_mut(|group| {
                    if let Some(handlers) = group.get_mut("hooks").and_then(Value::as_array_mut) {
                        let had_ours = handlers.iter().any(owned);
                        handlers.retain(|h| !owned(h));
                        return !had_ours || !handlers.is_empty();
                    }
                    true
                });
                !groups.is_empty()
            } else {
                true
            }
        });
        if hooks.is_empty() {
            root.remove("hooks");
        }
    }
    if let Some(binary) = binary {
        let hooks = root
            .entry("hooks")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .context("hooks must be an object")?;
        for event in events {
            let groups = hooks
                .entry(*event)
                .or_insert_with(|| json!([]))
                .as_array_mut()
                .context("event hooks must be an array")?;
            let mut group = json!({"hooks": [{"type": "command", "command": hook_command(binary, harness, event), "timeout": 5}]});
            if harness == "claude" && *event == "PreToolUse" {
                group["matcher"] = json!("AskUserQuestion");
            }
            groups.push(group);
        }
    }
    if value != before {
        let mut bytes = serde_json::to_vec_pretty(&value)?;
        bytes.push(b'\n');
        change(path, &original, &bytes, dry, report)?;
    }
    Ok(())
}

fn hook_command(binary: &Path, harness: &str, event: &str) -> String {
    let path = binary.to_string_lossy();
    let quoted = if path
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-".contains(c))
    {
        path.into_owned()
    } else {
        format!("'{}'", path.replace('\'', "'\\''"))
    };
    format!("{quoted} hook {harness} {event}")
}

fn change(
    path: &Path,
    original: &Option<Vec<u8>>,
    bytes: &[u8],
    dry: bool,
    report: &mut InstallReport,
) -> Result<()> {
    let mut unchanged = || -> Result<bool> {
        if read_optional(path)? != *original {
            report.warnings.push(format!(
                "{} changed while editing; left alone",
                path.display()
            ));
            return Ok(false);
        }
        Ok(true)
    };
    if !unchanged()? {
        return Ok(());
    }
    if dry {
        report.changed.push(path.into());
        return Ok(());
    }
    // A fresh backup before every write; a failed backup aborts before the file is touched.
    if let Some(original) = original {
        backup(path, original)?;
    }
    if atomic_write_if(path, bytes, unchanged)?.is_some() {
        report.changed.push(path.into());
    }
    Ok(())
}

// Never replaces an earlier backup: two writes to one target in the same second (aliased configs) get distinct names.
fn backup(path: &Path, bytes: &[u8]) -> Result<()> {
    let stem = format!(
        "{}.bak-pay-attention-{}-{}",
        path.file_name()
            .context("path has no filename")?
            .to_string_lossy(),
        Local::now().format("%Y%m%d-%H%M%S"),
        std::process::id()
    );
    let mut tmp = tempfile::NamedTempFile::new_in(path.parent().context("path has no parent")?)?;
    tmp.write_all(bytes)?;
    for n in 0.. {
        let name = if n == 0 {
            stem.clone()
        } else {
            format!("{stem}-{n}")
        };
        match tmp.persist_noclobber(path.with_file_name(name)) {
            Ok(_) => return Ok(()),
            Err(e) if e.error.kind() == ErrorKind::AlreadyExists => tmp = e.file,
            Err(e) => return Err(e.error.into()),
        }
    }
    unreachable!()
}

fn run(binary: Option<&Path>, dry: bool) -> Result<InstallReport> {
    let home = home()?;
    let mut report = InstallReport::default();
    // Only agents installed here: never create a config dir for one that isn't.
    for (dir, file, harness, events) in [
        (".claude", "settings.json", "claude", CLAUDE_EVENTS),
        (".codex", "hooks.json", "codex", CODEX_EVENTS),
    ] {
        let dir = home.join(dir);
        if dir.exists() {
            json_hooks(&dir.join(file), harness, events, binary, dry, &mut report)?;
        }
    }
    instructions(&home, binary, &mut report)?;
    Ok(report)
}

fn instructions(home: &Path, binary: Option<&Path>, report: &mut InstallReport) -> Result<()> {
    if let Some(binary) = binary {
        let path = home.join(".codex/config.toml");
        let enabled = read_optional(&path)?
            .and_then(|bytes| {
                std::str::from_utf8(&bytes)
                    .ok()
                    .and_then(|s| toml::from_str::<toml::Value>(s).ok())
            })
            .is_some_and(|v| {
                v.get("features")
                    .and_then(|v| v.get("hooks"))
                    .and_then(toml::Value::as_bool)
                    == Some(true)
            });
        if !enabled && home.join(".codex").exists() {
            report.warnings.push(format!("Enable Codex hooks in {} (merge into the features table):\n[features]\nhooks = true", path.display()));
        }
        if home.join(".hermes").exists() {
            report.warnings.push(format!(
                "Paste these hooks into {}. If a hooks: key already exists (including inherited hooks), merge these event entries into it; do not add a second hooks: key. Preserve existing handlers for each event.\n{}",
                home.join(".hermes/config.yaml").display(),
                hermes_block(binary)?
            ));
        }
    } else if home.join(".hermes").exists() {
        report.warnings.push(format!(
            "Remove the pay-attention hook commands you pasted into {}.",
            home.join(".hermes/config.yaml").display()
        ));
    }
    Ok(())
}

fn hermes_block(binary: &Path) -> Result<String> {
    let mut block = String::from("hooks:\n");
    for event in HERMES_EVENTS {
        block.push_str(&format!(
            "  {event}:\n    - command: {}\n      timeout: 5\n",
            serde_json::to_string(&hook_command(binary, "hermes", event))?
        ));
    }
    Ok(block)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn commands_round_trip_and_ownership_is_not_a_substring_match() {
        for path in ["/bin/pay-attention", "/My Apps/it's here/pay-attention"] {
            let command = hook_command(Path::new(path), "claude", "StopFailure");
            assert_eq!(
                command_words(&command).unwrap(),
                [path, "hook", "claude", "StopFailure"]
            );
        }
        for command in [
            "echo pay-attention hook",
            "/bin/not-pay-attention hook",
            "pay-attention status",
            "'broken",
        ] {
            assert!(command_words(command).is_none());
        }
    }

    #[test]
    fn json_reinstall_replaces_owned_entries_but_keeps_foreign_handlers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let foreign = json!({"hooks": {"StopFailure": [{"matcher": "x", "hooks": [{"command": "echo pay-attention hook"}]}]}});
        fs::write(&path, serde_json::to_vec(&foreign).unwrap()).unwrap();
        let mut report = InstallReport::default();
        for binary in [
            "/My Apps/pay-attention",
            "/new/pay-attention",
            "/new/pay-attention",
        ] {
            json_hooks(
                &path,
                "claude",
                CLAUDE_EVENTS,
                Some(Path::new(binary)),
                false,
                &mut report,
            )
            .unwrap();
            let installed: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            assert_eq!(
                installed["hooks"]["StopFailure"].as_array().unwrap().len(),
                2
            );
            assert_eq!(
                installed["hooks"]["StopFailure"][0],
                foreign["hooks"]["StopFailure"][0]
            );
            assert_eq!(
                installed["hooks"]["StopFailure"][1]["hooks"][0]["command"],
                hook_command(Path::new(binary), "claude", "StopFailure")
            );
            assert_eq!(installed["hooks"]["Stop"].as_array().unwrap().len(), 1);
            assert_eq!(
                installed["hooks"]["PermissionRequest"]
                    .as_array()
                    .unwrap()
                    .len(),
                1
            );
            assert_eq!(
                installed["hooks"]["PermissionRequest"][0]["hooks"][0]["command"],
                hook_command(Path::new(binary), "claude", "PermissionRequest")
            );
            assert_eq!(
                installed["hooks"]["PreToolUse"].as_array().unwrap().len(),
                1
            );
            assert_eq!(
                installed["hooks"]["PreToolUse"][0]["matcher"],
                "AskUserQuestion"
            );
        }
        json_hooks(&path, "claude", CLAUDE_EVENTS, None, false, &mut report).unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(&path).unwrap()).unwrap(),
            foreign
        );
    }

    #[test]
    fn backups_never_replace_an_earlier_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        for bytes in [b"first", b"again"] {
            backup(&path, bytes).unwrap();
        }
        let mut saved: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| fs::read(e.unwrap().path()).unwrap())
            .collect();
        saved.sort();
        assert_eq!(saved, [b"again".to_vec(), b"first".to_vec()]);
    }

    #[test]
    fn stale_config_is_skipped_without_a_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        fs::write(&path, b"concurrent edit").unwrap();
        let mut report = InstallReport::default();
        change(
            &path,
            &Some(b"old".to_vec()),
            b"replacement",
            false,
            &mut report,
        )
        .unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"concurrent edit");
        assert_eq!(report.warnings.len(), 1);
        assert!(report.changed.is_empty());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn missing_symlink_target_is_created_without_replacing_the_link() {
        let dir = tempfile::tempdir().unwrap();
        let link = dir.path().join("hooks.json");
        std::os::unix::fs::symlink("dotfiles/hooks.json", &link).unwrap();
        json_hooks(
            &link,
            "codex",
            CODEX_EVENTS,
            Some(Path::new("/bin/pay-attention")),
            false,
            &mut InstallReport::default(),
        )
        .unwrap();
        assert!(link.is_symlink());
        assert!(dir.path().join("dotfiles/hooks.json").is_file());
    }

    #[test]
    fn manual_instructions_never_edit_or_create_configs() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let binary = Some(Path::new("/bin/pay-attention"));
        let mut report = InstallReport::default();
        instructions(home, binary, &mut report).unwrap();
        assert!(report.warnings.is_empty());
        assert_eq!(fs::read_dir(home).unwrap().count(), 0);
        fs::create_dir(home.join(".codex")).unwrap();
        fs::create_dir(home.join(".hermes")).unwrap();
        let yaml = home.join(".hermes/config.yaml");
        fs::write(&yaml, "{model: x}\n").unwrap();
        for source in [
            "features = false",
            "[features]\nhooks = false",
            "[features]\nhooks = true",
        ] {
            let toml = home.join(".codex/config.toml");
            fs::write(&toml, source).unwrap();
            report = InstallReport::default();
            instructions(home, binary, &mut report).unwrap();
            let hermes = report.warnings.last().unwrap();
            assert!(hermes.contains("do not add a second hooks: key"));
            assert!(hermes.contains("hook hermes on_session_finalize"));
            assert_eq!(
                report.warnings.len(),
                if source.ends_with("true") { 1 } else { 2 }
            );
            instructions(home, None, &mut report).unwrap();
            assert_eq!(fs::read_to_string(&toml).unwrap(), source);
            assert_eq!(fs::read_to_string(&yaml).unwrap(), "{model: x}\n");
        }
        assert!(!home.join(".local").exists());
    }
}
