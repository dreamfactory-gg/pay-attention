mod config;
mod engine;
mod hooks;
mod install;
pub mod login_item;
mod process;
mod schedule;
mod sessions;
mod stats;
mod transcript;

pub use config::{
    Config, ConfigVersion, Escalation, LoadedConfig, Sounds, config, config_with_version,
    save_config,
};
pub use engine::{AckBy, Action, DisplayRow, Engine};
pub use hooks::hook;
pub use install::{InstallReport, install, uninstall};
pub use schedule::{Ring, schedule};
pub use sessions::{Row, Snapshot, ack, snapshot, snapshot_with_config};
pub use stats::{Event, SessionStats, Stats, StatsTracker, stats, transitions};

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;
use std::{
    fs,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

fn home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME is not set")
}

pub fn autostart_allowed(executable: &Path) -> bool {
    let Ok(executable) = executable.canonicalize() else {
        return false;
    };
    let Some(bundle) = executable.ancestors().nth(3) else {
        return false;
    };
    bundle.extension().is_some_and(|ext| ext == "app")
        && std::iter::once(PathBuf::from("/Applications"))
            .chain(home().ok().map(|home| home.join("Applications")))
            .any(|root| {
                root.canonicalize()
                    .is_ok_and(|root| bundle.starts_with(root))
            })
}

fn state_dir() -> Result<PathBuf> {
    Ok(home()?.join(".local/state/pay-attention"))
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

fn read_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

fn resolve_path(path: &Path) -> Result<PathBuf> {
    match fs::canonicalize(path) {
        Ok(target) => Ok(target),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let parent = path.parent().context("path has no parent")?;
            if let Ok(target) = fs::read_link(path) {
                return resolve_path(&parent.join(target));
            }
            Ok(resolve_path(parent)?.join(path.file_name().context("path has no filename")?))
        }
        Err(e) => Err(e.into()),
    }
}

fn atomic_write_if(
    path: &Path,
    bytes: &[u8],
    accept: impl FnOnce() -> Result<bool>,
) -> Result<Option<fs::File>> {
    let parent = path.parent().context("path has no parent")?;
    fs::create_dir_all(parent)?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    if let Ok(metadata) = fs::metadata(path) {
        tmp.as_file().set_permissions(metadata.permissions())?;
    }
    tmp.write_all(bytes)?;
    if !accept()? {
        return Ok(None);
    }
    Ok(Some(tmp.persist(path)?))
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    atomic_write_if(&resolve_path(path)?, bytes, || Ok(true))?;
    Ok(())
}

fn lock_file(path: &Path) -> Result<fs::File> {
    fs::create_dir_all(path.parent().context("lock has no parent")?)?;
    Ok(fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(path)?)
}

pub fn lock(path: &Path) -> Result<fs::File> {
    let file = lock_file(path)?;
    file.lock()?;
    Ok(file)
}

pub fn try_lock(path: &Path) -> Result<Option<fs::File>> {
    let file = lock_file(path)?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(fs::TryLockError::WouldBlock) => Ok(None),
        Err(fs::TryLockError::Error(error)) => Err(error.into()),
    }
}

fn session_lock(start: std::time::Instant, first: bool) -> Result<fs::File> {
    timed_lock(
        &state_dir()?.join("sessions.lock"),
        start,
        std::time::Duration::from_millis(500),
        first,
    )
}

// An operation's first attempt is unconditional: under load, creating the lock file alone can outlast a short budget,
// and a hook that gives up without trying drops its write. Every later attempt (a retry, or a re-take after a Codex
// rescan) is made only before the operation's deadline.
fn timed_lock(
    path: &Path,
    start: std::time::Instant,
    timeout: std::time::Duration,
    first: bool,
) -> Result<fs::File> {
    let file = lock_file(path)?;
    let mut unconditional = first;
    loop {
        anyhow::ensure!(
            std::mem::take(&mut unconditional) || start.elapsed() < timeout,
            "lock timed out"
        );
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(std::fs::TryLockError::WouldBlock) => {
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    atomic_write(path, &serde_json::to_vec_pretty(value)?)
}

fn text(value: &Value) -> &str {
    value.as_str().unwrap_or_default()
}

fn detail(s: &str) -> Option<String> {
    let s: String = s
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(300)
        .collect();
    (!s.is_empty()).then_some(s)
}

fn json_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<_> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    files.sort();
    files
}

#[cfg(test)]
mod file_tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn autostart_only_accepts_installed_bundles() {
        if let Some(executable) = std::env::var_os("PAY_ATTENTION_TEST_BUNDLE") {
            assert_eq!(
                autostart_allowed(Path::new(&executable)),
                std::env::var("PAY_ATTENTION_TEST_ALLOWED").unwrap() == "true"
            );
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let installed = dir.path().join("Apps & <Games>");
        let bundle = installed.join("Pay Attention.app");
        fs::create_dir_all(bundle.join("Contents/MacOS")).unwrap();
        let home = dir.path().join("home");
        fs::create_dir(&home).unwrap();
        symlink(&installed, home.join("Applications")).unwrap();
        let alias = dir.path().join("home-alias");
        symlink(&home, &alias).unwrap();
        let bundle_alias = dir.path().join("alias.app");
        symlink(&bundle, &bundle_alias).unwrap();
        let executable = bundle_alias.join("Contents/MacOS/pay-attention-app");
        fs::write(&executable, "installed").unwrap();
        let check = |executable: &Path, allowed: bool| {
            assert!(
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "file_tests::autostart_only_accepts_installed_bundles"
                    ])
                    .env("HOME", &alias)
                    .env("PAY_ATTENTION_TEST_BUNDLE", executable)
                    .env("PAY_ATTENTION_TEST_ALLOWED", allowed.to_string())
                    .status()
                    .unwrap()
                    .success()
            );
        };
        check(&executable, true);
        check(
            &home.join("Applications/Pay Attention.app/Contents/MacOS/pay-attention-app"),
            true,
        );
        let development = dir.path().join("development");
        fs::write(&development, "development").unwrap();
        fs::remove_file(&executable).unwrap();
        symlink(&development, &executable).unwrap();
        check(&executable, false);
        fs::remove_file(&development).unwrap();
        check(&executable, false);
        fs::remove_file(home.join("Applications")).unwrap();
        symlink("Applications", home.join("Applications")).unwrap();
        fs::remove_file(&executable).unwrap();
        fs::write(&executable, "installed").unwrap();
        check(&executable, false);
        fs::write(&development, "development").unwrap();
        check(&development, false);
        check(Path::new("/Applications/pay-attention-app"), false);
    }

    #[test]
    fn atomic_write_returns_the_saved_file_not_a_later_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let saved = atomic_write_if(&path, b"saved", || Ok(true))
            .unwrap()
            .unwrap();
        let replacement = dir.path().join("replacement");
        fs::write(&replacement, b"outside edit").unwrap();
        fs::rename(replacement, &path).unwrap();
        use std::os::unix::fs::MetadataExt;
        assert_ne!(
            saved.metadata().unwrap().ino(),
            fs::metadata(path).unwrap().ino()
        );
    }

    #[test]
    fn exclusive_try_lock_fails_closed_and_releases_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("app.lock");
        let held = try_lock(&path).unwrap().unwrap();
        assert!(try_lock(&path).unwrap().is_none());
        drop(held);
        assert!(try_lock(&path).unwrap().is_some());
        assert!(try_lock(dir.path()).is_err());
    }

    #[test]
    fn atomic_writes_follow_symlinks_preserve_modes_and_reject_stale_reads() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        let link = dir.path().join("link");
        fs::write(&target, b"old").unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        symlink(&target, &link).unwrap();
        atomic_write(&link, b"new").unwrap();
        assert!(link.is_symlink());
        assert_eq!(fs::read(&target).unwrap(), b"new");
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let resolved = resolve_path(&link).unwrap();
        let other = dir.path().join("other");
        fs::write(&other, b"new").unwrap();
        assert!(
            atomic_write_if(&resolved, b"stale", || {
                fs::remove_file(&link)?;
                symlink(&other, &link)?;
                fs::write(&target, b"concurrent")?;
                Ok(fs::read(&resolved)? == b"new")
            })
            .unwrap()
            .is_none()
        );
        assert_eq!(fs::read(&target).unwrap(), b"concurrent");
        assert_eq!(fs::read(&other).unwrap(), b"new");
    }

    #[test]
    fn locked_read_modify_writes_keep_every_update() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("acks.json");
        std::thread::scope(|scope| {
            for key in 0..32 {
                let path = &path;
                scope.spawn(move || {
                    let _guard = lock(&path.with_extension("lock")).unwrap();
                    let mut value = read_json(path).unwrap_or_else(|| serde_json::json!({}));
                    std::thread::yield_now();
                    value[key.to_string()] =
                        serde_json::json!({"since": 1, "state": "idle", "reason": null});
                    write_json(path, &value).unwrap();
                });
            }
        });
        assert_eq!(read_json(&path).unwrap().as_object().unwrap().len(), 32);
    }

    #[test]
    fn concurrent_atomic_writers_do_not_remove_each_others_temporary_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("session.json");
        std::thread::scope(|scope| {
            for _ in 0..32 {
                let path = &path;
                scope.spawn(move || atomic_write(path, b"complete").unwrap());
            }
        });
        assert_eq!(fs::read(&path).unwrap(), b"complete");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
    }
}
