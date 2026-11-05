use crate::{atomic_write_if, home, lock, resolve_path};
use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File, Metadata},
    io::Read,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub stalled_after_secs: u64,
    pub dnd: String,
    pub away_after_secs: u64,
    pub phone_after_secs: u64,
    pub phone_command: Vec<String>,
    pub hotkey: String,
    pub escalation: Escalation,
    pub sounds: Sounds,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            stalled_after_secs: 600,
            dnd: "23:00-08:00".into(),
            away_after_secs: 300,
            phone_after_secs: 120,
            phone_command: vec![],
            hotkey: "Ctrl+Alt+Cmd+P".into(),
            escalation: Escalation::default(),
            sounds: Sounds::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct Escalation {
    pub start_interval_secs: f64,
    pub min_interval_secs: f64,
    pub start_volume: f64,
    pub ramp_secs: f64,
    pub yellow_max_rings: usize,
    pub yellow_delay_secs: f64,
}

impl Default for Escalation {
    fn default() -> Self {
        Self {
            start_interval_secs: 60.0,
            min_interval_secs: 10.0,
            start_volume: 0.3,
            ramp_secs: 300.0,
            yellow_max_rings: 3,
            yellow_delay_secs: 2.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(default)]
pub struct Sounds {
    pub blue: String,
    pub yellow: String,
    pub red: String,
}

impl Default for Sounds {
    fn default() -> Self {
        Self {
            blue: "synth".into(),
            yellow: "synth".into(),
            red: "synth".into(),
        }
    }
}

fn config_path() -> Result<PathBuf> {
    Ok(home()?.join(".config/pay-attention/config.toml"))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConfigVersion {
    contents: Vec<u8>,
    identity: String,
}

#[derive(Serialize)]
pub struct LoadedConfig {
    pub config: Config,
    pub version: Option<ConfigVersion>,
}

fn identity(metadata: &Metadata) -> String {
    format!(
        "{}:{}:{}:{}:{}:{}",
        metadata.dev(),
        metadata.ino(),
        metadata.mtime(),
        metadata.mtime_nsec(),
        metadata.ctime(),
        metadata.ctime_nsec()
    )
}

fn version(path: &Path) -> Result<Option<ConfigVersion>> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let before = identity(&file.metadata()?);
    let mut contents = Vec::new();
    file.read_to_end(&mut contents)?;
    ensure!(
        before == identity(&file.metadata()?) && before == identity(&fs::metadata(path)?),
        "Config changed while reading; reopen Settings"
    );
    Ok(Some(ConfigVersion {
        contents,
        identity: before,
    }))
}

fn load_config(path: &Path) -> Result<LoadedConfig> {
    let version = version(path)?;
    let config = match &version {
        Some(version) => toml::from_str(std::str::from_utf8(&version.contents)?)?,
        None => Config::default(),
    };
    config.validate()?;
    Ok(LoadedConfig { config, version })
}

pub fn config_with_version() -> Result<LoadedConfig> {
    load_config(&config_path()?)
}

pub fn config() -> Result<Config> {
    Ok(config_with_version()?.config)
}

const STALE_CONFIG: &str = "Config changed since Settings opened; reopen Settings and try again";

fn save_config_at(
    path: &Path,
    config: &Config,
    expected: &Option<ConfigVersion>,
) -> Result<Option<ConfigVersion>> {
    config.validate()?;
    let _guard = lock(&path.with_extension("lock"))?;
    let target = resolve_path(path)?;
    let contents = toml::to_string_pretty(config)?.into_bytes();
    let saved = atomic_write_if(&target, &contents, || {
        Ok(resolve_path(path)? == target && &version(path)? == expected)
    })?
    .ok_or_else(|| anyhow::anyhow!(STALE_CONFIG))?;
    Ok(Some(ConfigVersion {
        contents,
        identity: identity(&saved.metadata()?),
    }))
}

pub fn save_config(
    config: &Config,
    expected: &Option<ConfigVersion>,
) -> Result<Option<ConfigVersion>> {
    save_config_at(&config_path()?, config, expected)
}

impl Config {
    pub(crate) fn validate(&self) -> Result<()> {
        let e = &self.escalation;
        if !e.start_interval_secs.is_finite()
            || !e.min_interval_secs.is_finite()
            || e.min_interval_secs <= 0.0
            || e.start_interval_secs < e.min_interval_secs
            || !e.ramp_secs.is_finite()
            || e.ramp_secs <= 0.0
            || !(0.0..=1.0).contains(&e.start_volume)
            || !e.yellow_delay_secs.is_finite()
            || e.yellow_delay_secs < 0.0
        {
            bail!("invalid escalation configuration");
        }
        self.dnd_window()?;
        Ok(())
    }

    pub(crate) fn dnd_window(&self) -> Result<Option<(f64, f64)>> {
        if self.dnd.is_empty() {
            return Ok(None);
        }
        let (start, end) = self
            .dnd
            .split_once('-')
            .ok_or_else(|| anyhow::anyhow!("DND must be HH:MM-HH:MM"))?;
        Ok(Some((clock_seconds(start)?, clock_seconds(end)?)))
    }
}

pub(crate) fn clock_seconds(s: &str) -> Result<f64> {
    use chrono::Timelike;
    let t = chrono::NaiveTime::parse_from_str(s, "%H:%M")?;
    Ok(t.num_seconds_from_midnight() as f64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_token_keeps_submitted_bytes_and_rejects_later_edits() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let config = Config::default();
        let saved = save_config_at(&path, &config, &None).unwrap();
        assert_eq!(
            saved.as_ref().unwrap().contents,
            toml::to_string_pretty(&config).unwrap().as_bytes()
        );
        let replacement = dir.path().join("replacement");
        fs::write(&replacement, "dnd = \"\"\n").unwrap();
        fs::rename(replacement, &path).unwrap();
        assert!(save_config_at(&path, &config, &saved).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "dnd = \"\"\n");
    }

    #[test]
    fn stale_saves_preserve_external_edits_and_detect_creation_and_replacement() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let missing = load_config(&path).unwrap();
        fs::write(&path, "dnd = \"\"\n").unwrap();
        assert!(save_config_at(&path, &missing.config, &missing.version).is_err());
        let loaded = load_config(&path).unwrap();
        fs::write(&path, "dnd = \"10:00-11:00\"\n").unwrap();
        assert!(save_config_at(&path, &loaded.config, &loaded.version).is_err());
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "dnd = \"10:00-11:00\"\n"
        );
        let loaded = load_config(&path).unwrap();
        let replacement = dir.path().join("replacement");
        fs::write(&replacement, fs::read(&path).unwrap()).unwrap();
        fs::rename(replacement, &path).unwrap();
        assert!(save_config_at(&path, &loaded.config, &loaded.version).is_err());
        let current = load_config(&path).unwrap();
        let wire = serde_json::to_string(&current.version).unwrap();
        let version = serde_json::from_str(&wire).unwrap();
        let saved = save_config_at(&path, &current.config, &version).unwrap();
        assert_eq!(saved, load_config(&path).unwrap().version);
        save_config_at(&path, &current.config, &saved).unwrap();
        assert_eq!(load_config(&path).unwrap().config.dnd, "10:00-11:00");
    }
}
