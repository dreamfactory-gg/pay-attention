use crate::{
    Config, Event, Row, Snapshot,
    schedule::{Schedule, dnd_delay},
    sessions::{Ack, light, load_acks, save_ack},
    state_dir, transitions,
};
use anyhow::Result;
use chrono::{Local, TimeZone, Timelike};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    Ring {
        now: i64,
        color: &'static str,
        vol: f64,
        keys: Vec<String>,
    },
    Ack {
        now: i64,
        key: String,
        by: AckBy,
    },
    Focus {
        now: i64,
        key: String,
    },
    Phone {
        now: i64,
        key: String,
        argv: Vec<String>,
    },
    Event(Event),
    Light {
        now: i64,
        color: &'static str,
        count: usize,
    },
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AckBy {
    Focus,
    Click,
}

struct Episode {
    identity: Ack,
    first_seen: i64,
    schedule: Schedule,
    rings: usize,
    missing_since: Option<i64>,
    dropped_ring: bool,
    phoned: bool,
}

#[derive(Serialize)]
pub struct DisplayRow {
    #[serde(flatten)]
    pub row: Row,
    pub color: &'static str,
    pub section: &'static str,
}

pub struct Engine {
    config: Config,
    dnd: Option<(f64, f64)>,
    ack_path: PathBuf,
    acks: BTreeMap<String, Ack>,
    ack_mtime: Option<SystemTime>,
    unsaved_acks: BTreeMap<String, Ack>,
    snapshot: Snapshot,
    episodes: BTreeMap<String, Episode>,
    last_now: Option<i64>,
    quiet_until: i64,
    last_light: Option<(&'static str, usize)>,
    last_focus: Option<(u8, i64, String)>,
}

impl Engine {
    pub fn new(config: Config) -> Result<Self> {
        Self::with_ack_path(config, state_dir()?.join("acks.json"))
    }

    fn with_ack_path(config: Config, ack_path: PathBuf) -> Result<Self> {
        config.validate()?;
        let mtime = ack_mtime(&ack_path);
        let (acks, ack_mtime) = match load_acks(&ack_path) {
            Ok(acks) => (acks, mtime),
            Err(_) => (BTreeMap::new(), None),
        };
        Ok(Self {
            dnd: config.dnd_window()?,
            config,
            ack_mtime,
            acks,
            unsaved_acks: BTreeMap::new(),
            ack_path,
            snapshot: Snapshot {
                light: "grey".into(),
                rows: Vec::new(),
            },
            episodes: BTreeMap::new(),
            last_now: None,
            quiet_until: i64::MIN,
            last_light: None,
            last_focus: None,
        })
    }

    pub fn reload_config(&mut self, config: Config) -> Result<()> {
        config.validate()?;
        self.dnd = config.dnd_window()?;
        self.config = config;
        Ok(())
    }

    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    pub fn display_rows(&self) -> Vec<DisplayRow> {
        let mut rows: Vec<_> = self
            .snapshot
            .rows
            .iter()
            .map(|row| DisplayRow {
                row: row.clone(),
                color: row.color(),
                section: row.section(),
            })
            .collect();
        rows.sort_by(|a, b| hotkey_order(&a.row).cmp(&hotkey_order(&b.row)));
        rows
    }

    pub fn quiet_until(&self) -> i64 {
        self.quiet_until.max(0)
    }

    pub fn ring_pitch(&self, color: &str, keys: &[String]) -> usize {
        self.snapshot
            .rows
            .iter()
            .filter(|row| row.color() == color && keys.contains(&row.key))
            .filter_map(|row| self.episodes.get(&row.key))
            .map(|episode| episode.rings.saturating_sub(1).min(5))
            .max()
            .unwrap_or(0)
    }

    /// A crashed process has no tty or surface left to read. Give its row the tab (or Claude Desktop) the same process
    /// last had, so a click, the hotkey or focusing that tab still reaches it. Call before choosing what to ask Terminal.
    pub fn keep_focus_targets(&self, rows: &mut [Row]) {
        for row in rows {
            if row.reason.as_deref() == Some("crashed")
                && row.surface == "headless"
                && let Some(previous) = self.snapshot.rows.iter().find(|previous| {
                    previous.key == row.key
                        && previous.pid == row.pid
                        && previous.surface != "headless"
                })
            {
                row.tty.clone_from(&previous.tty);
                row.surface.clone_from(&previous.surface);
            }
        }
    }

    pub fn tick(
        &mut self,
        now_ms: i64,
        rows: &[Row],
        frontmost_tty: Option<&str>,
        idle_secs: u64,
        locked: bool,
    ) -> Vec<Action> {
        self.observe_time(now_ms);
        let mtime = ack_mtime(&self.ack_path);
        if mtime != self.ack_mtime
            && let Ok(acks) = load_acks(&self.ack_path)
        {
            self.acks = acks;
            self.ack_mtime = mtime;
        }
        let mut current = Snapshot {
            light: String::new(),
            rows: rows.to_vec(),
        };
        self.keep_focus_targets(&mut current.rows);
        let mut actions: Vec<_> = transitions(&self.snapshot, &current, now_ms)
            .into_iter()
            .map(Action::Event)
            .collect();
        self.episodes.retain(|key, episode| {
            if episode
                .missing_since
                .is_some_and(|since| now_ms - since >= 3_600_000)
            {
                return false;
            }
            if let Some(row) = current.rows.iter().find(|row| row.key == *key) {
                episode.identity.matches(row)
            } else {
                episode.missing_since.get_or_insert(now_ms);
                true
            }
        });
        for row in &mut current.rows {
            row.acked = [&self.acks, &self.unsaved_acks]
                .iter()
                .any(|acks| acks.get(&row.key).is_some_and(|ack| ack.matches(row)));
            self.episodes
                .entry(row.key.clone())
                .or_insert_with(|| Episode {
                    identity: Ack::from(&*row),
                    first_seen: now_ms,
                    rings: 0,
                    schedule: Schedule::new(if row.state == "idle" {
                        "yellow"
                    } else {
                        "blue"
                    }),
                    missing_since: None,
                    dropped_ring: false,
                    phoned: false,
                })
                .missing_since = None;
        }
        self.snapshot = current;
        for index in 0..self.snapshot.rows.len() {
            let row = &self.snapshot.rows[index];
            if row.alert_color().is_some()
                && frontmost_tty.is_some()
                && frontmost_tty == row.tty.as_deref()
                && idle_secs <= 30
            {
                self.acknowledge(now_ms, index, AckBy::Focus, &mut actions);
            }
        }

        let now = Local.timestamp_millis_opt(now_ms).single().unwrap();
        let clock = now.num_seconds_from_midnight() as f64 + now.nanosecond() as f64 / 1e9;
        let silenced = dnd_delay(self.dnd, clock) > 0.0 || now_ms < self.quiet_until;
        let mut due = Vec::new();
        let mut loudest: Option<(&str, f64)> = None;
        for row in &self.snapshot.rows {
            let Some(color) = row.alert_color() else {
                continue;
            };
            let episode = self.episodes.get_mut(&row.key).unwrap();
            let elapsed = (now_ms - episode.first_seen) as f64 / 1000.0;
            let mut volume = None;
            while let Some(t) = episode.schedule.next(&self.config) {
                // A ring suppressed by DND or quiet stays due, even after a reload moves the schedule later.
                if t > elapsed && !episode.dropped_ring {
                    break;
                }
                if silenced {
                    episode.dropped_ring = true;
                    break;
                }
                let t = if episode.dropped_ring { elapsed } else { t };
                volume = Some(episode.schedule.advance(&self.config, t).volume);
                episode.dropped_ring = false;
            }
            if let Some(volume) = volume {
                episode.rings += 1;
                due.push(row.key.clone());
                if loudest.is_none_or(|(previous, _)| priority(color) < priority(previous)) {
                    loudest = Some((color, volume));
                }
            }
            if !episode.phoned
                && matches!(color, "blue" | "red")
                && (locked || idle_secs >= self.config.away_after_secs)
                && elapsed >= self.config.phone_after_secs as f64
                && !silenced
                && !self.config.phone_command.is_empty()
            {
                episode.phoned = true;
                actions.push(Action::Phone {
                    now: now_ms,
                    key: row.key.clone(),
                    argv: self
                        .config
                        .phone_command
                        .iter()
                        .map(|arg| phone_arg(arg, row))
                        .collect(),
                });
            }
        }
        if let Some((color, volume)) = loudest {
            actions.push(Action::Ring {
                now: now_ms,
                color,
                vol: (volume * 100.0).round() / 100.0,
                keys: due,
            });
        }
        self.update_light(now_ms, &mut actions);
        actions
    }

    pub fn click(&mut self, now_ms: i64, key: &str) -> Vec<Action> {
        self.observe_time(now_ms);
        let Some(index) = self.snapshot.rows.iter().position(|row| row.key == key) else {
            return Vec::new();
        };
        let mut actions = Vec::new();
        self.acknowledge(now_ms, index, AckBy::Click, &mut actions);
        actions.push(Action::Focus {
            now: now_ms,
            key: key.into(),
        });
        self.update_light(now_ms, &mut actions);
        actions
    }

    pub fn hotkey(&mut self, now_ms: i64) -> Vec<Action> {
        self.observe_time(now_ms);
        let mut alerts: Vec<_> = self
            .snapshot
            .rows
            .iter()
            .filter(|row| row.alert_color().is_some())
            .collect();
        alerts.sort_by_key(|row| hotkey_order(row));
        if alerts.is_empty() {
            self.last_focus = None;
            return Vec::new();
        }
        let index = self
            .last_focus
            .as_ref()
            .and_then(|(priority, since, key)| {
                alerts
                    .iter()
                    .position(|row| hotkey_order(row) > (*priority, *since, key.as_str()))
            })
            .unwrap_or(0);
        let (priority, since, key) = hotkey_order(alerts[index]);
        let key = key.to_owned();
        self.last_focus = Some((priority, since, key.clone()));
        vec![Action::Focus { now: now_ms, key }]
    }

    pub fn quiet(&mut self, now_ms: i64, until_ms: i64) -> Vec<Action> {
        self.observe_time(now_ms);
        self.quiet_until = until_ms;
        Vec::new()
    }

    fn observe_time(&mut self, now_ms: i64) {
        if let Some(previous) = self.last_now
            && now_ms < previous
        {
            for episode in self.episodes.values_mut() {
                episode.first_seen += now_ms - previous;
                if let Some(since) = &mut episode.missing_since {
                    *since += now_ms - previous;
                }
            }
        }
        self.last_now = Some(now_ms);
    }

    fn acknowledge(&mut self, now: i64, index: usize, by: AckBy, actions: &mut Vec<Action>) {
        let row = &mut self.snapshot.rows[index];
        if !row.acked {
            match save_ack(&self.ack_path, row) {
                Ok(acks) => {
                    self.acks = acks;
                    self.unsaved_acks.remove(&row.key);
                }
                Err(_) => {
                    self.unsaved_acks.insert(row.key.clone(), Ack::from(&*row));
                }
            }
            row.acked = true;
            actions.push(Action::Ack {
                now,
                key: row.key.clone(),
                by,
            });
        }
    }

    fn update_light(&mut self, now: i64, actions: &mut Vec<Action>) {
        let color = light(&self.snapshot.rows);
        let count = self
            .snapshot
            .rows
            .iter()
            .filter(|row| row.alert_color().is_some())
            .count();
        if self.last_light != Some((color, count)) {
            self.last_light = Some((color, count));
            actions.push(Action::Light { now, color, count });
        }
        self.snapshot.light = color.into();
    }
}

#[cfg(test)]
mod tests;

fn priority(color: &str) -> u8 {
    match color {
        "red" => 0,
        "blue" => 1,
        _ => 2,
    }
}

fn ack_mtime(path: &Path) -> Option<SystemTime> {
    fs::metadata(path).ok()?.modified().ok()
}

fn hotkey_order(row: &Row) -> (u8, i64, &str) {
    let priority = match row.state.as_str() {
        "waiting" => 0,
        "error" => 1,
        _ => 2,
    };
    (priority, row.since, &row.key)
}

fn phone_arg(arg: &str, row: &Row) -> String {
    let mut parts = arg.split('{');
    let mut result = parts.next().unwrap().to_owned();
    for part in parts {
        let replacement = [
            ("name}", row.name.as_str()),
            ("state}", row.state.as_str()),
            ("detail}", row.detail.as_deref().unwrap_or_default()),
        ]
        .into_iter()
        .find_map(|(token, value)| part.strip_prefix(token).map(|rest| (value, rest)));
        if let Some((value, rest)) = replacement {
            result.push_str(value);
            result.push_str(rest);
        } else {
            result.push('{');
            result.push_str(part);
        }
    }
    result
}
