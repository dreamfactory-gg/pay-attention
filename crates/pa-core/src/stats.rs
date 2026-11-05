use crate::{Snapshot, now_ms, state_dir};
use anyhow::{Context, Result};
use chrono::{Local, NaiveDate, TimeZone};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{BufRead, BufReader},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Event {
    pub ts: i64,
    pub key: String,
    pub name: String,
    pub from: String,
    pub to: String,
}

pub fn transitions(before: &Snapshot, after: &Snapshot, ts: i64) -> Vec<Event> {
    let previous: BTreeMap<_, _> = before.rows.iter().map(|r| (&r.key, r)).collect();
    let current: BTreeMap<_, _> = after.rows.iter().map(|r| (&r.key, r)).collect();
    let mut events = Vec::new();
    for row in &after.rows {
        let from = previous.get(&row.key).map_or("new", |r| r.state.as_str());
        if from != row.state {
            events.push(Event {
                ts,
                key: row.key.clone(),
                name: row.name.clone(),
                from: from.into(),
                to: row.state.clone(),
            });
        }
    }
    for row in &before.rows {
        if !current.contains_key(&row.key) {
            events.push(Event {
                ts,
                key: row.key.clone(),
                name: row.name.clone(),
                from: row.state.clone(),
                to: "gone".into(),
            });
        }
    }
    events
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionStats {
    pub key: String,
    pub name: String,
    pub ms: i64,
}

#[derive(Debug, Serialize)]
pub struct Stats {
    pub total_ms: i64,
    pub by_color: BTreeMap<String, i64>,
    pub sessions: Vec<SessionStats>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "lowercase")]
enum Lifecycle {
    Start,
    Heartbeat,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum LogLine {
    Lifecycle {
        #[serde(rename = "type")]
        kind: Lifecycle,
        ts: i64,
    },
    Transition(Event),
}

fn read_events() -> Result<Vec<LogLine>> {
    let mut events = Vec::new();
    match File::open(state_dir()?.join("events.jsonl")) {
        Ok(file) => {
            for line in BufReader::new(file).split(b'\n') {
                if let Ok(event) = serde_json::from_slice::<LogLine>(&line?) {
                    events.push(event);
                }
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(events)
}

fn color(state: &str) -> Option<&'static str> {
    match state {
        "waiting" => Some("blue"),
        "idle" => Some("yellow"),
        "error" => Some("red"),
        _ => None,
    }
}

struct Totals {
    by_color: BTreeMap<String, i64>,
    sessions: BTreeMap<String, SessionStats>,
}

impl Totals {
    fn new() -> Self {
        Self {
            by_color: ["blue", "yellow", "red"].map(|c| (c.into(), 0)).into(),
            sessions: BTreeMap::new(),
        }
    }

    fn add(&mut self, event: &Event, duration: i64) {
        if duration <= 0 {
            return;
        }
        let Some(color) = color(&event.to) else {
            return;
        };
        *self.by_color.get_mut(color).unwrap() += duration;
        let session = self
            .sessions
            .entry(event.key.clone())
            .or_insert_with(|| SessionStats {
                key: event.key.clone(),
                name: event.name.clone(),
                ms: 0,
            });
        session.name.clone_from(&event.name);
        session.ms += duration;
    }

    fn finish(self) -> Stats {
        let total_ms = self.by_color.values().sum();
        let mut sessions: Vec<_> = self.sessions.into_values().collect();
        sessions.sort_by(|a, b| b.ms.cmp(&a.ms).then(a.key.cmp(&b.key)));
        sessions.truncate(5);
        Stats {
            total_ms,
            by_color: self.by_color,
            sessions,
        }
    }
}

pub struct StatsTracker {
    day: NaiveDate,
    offset: i32,
    start: i64,
    end: i64,
    intervals: Vec<(Event, Option<i64>)>,
}

impl StatsTracker {
    fn from_events(events: Vec<LogLine>, day: NaiveDate, start: i64, end: i64) -> Self {
        let mut intervals = Vec::new();
        let mut open = BTreeMap::<String, Event>::new();
        let mut last_ts = None;
        for line in events {
            let ts = match line {
                LogLine::Lifecycle { kind, ts } => {
                    if matches!(kind, Lifecycle::Start) {
                        for event in std::mem::take(&mut open).into_values() {
                            intervals.push((event, last_ts));
                        }
                    }
                    ts
                }
                LogLine::Transition(event) => {
                    let ts = event.ts;
                    if let Some(previous) = open.remove(&event.key) {
                        intervals.push((previous, Some(ts)));
                    }
                    if color(&event.to).is_some() {
                        open.insert(event.key.clone(), event);
                    }
                    ts
                }
            };
            last_ts = Some(ts);
        }
        intervals.extend(open.into_values().map(|event| (event, None)));
        Self {
            day,
            offset: Local::now().offset().local_minus_utc(),
            start,
            end,
            intervals,
        }
    }

    fn load(day: NaiveDate) -> Result<Self> {
        let midnight = |day: NaiveDate| -> Result<i64> {
            Ok(Local
                .from_local_datetime(&day.and_hms_opt(0, 0, 0).unwrap())
                .earliest()
                .context("local midnight does not exist")?
                .timestamp_millis())
        };
        let start = midnight(day)?;
        let end = midnight(day.succ_opt().context("day out of range")?)?;
        Ok(Self::from_events(read_events()?, day, start, end))
    }

    pub fn today() -> Result<Self> {
        Self::load(Local::now().date_naive())
    }

    pub fn is_current(&self) -> bool {
        let now = Local::now();
        self.day == now.date_naive() && self.offset == now.offset().local_minus_utc()
    }

    fn at(&self, now: i64) -> Stats {
        let mut totals = Totals::new();
        let end = now.min(self.end);
        for (event, until) in &self.intervals {
            totals.add(
                event,
                (until.unwrap_or(end).min(end) - event.ts.max(self.start)).max(0),
            );
        }
        totals.finish()
    }

    pub fn stats(&self) -> Stats {
        self.at(now_ms())
    }
}

pub fn stats(day: Option<NaiveDate>) -> Result<Stats> {
    Ok(StatsTracker::load(day.unwrap_or_else(|| Local::now().date_naive()))?.stats())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(ts: i64, key: &str, to: &str) -> LogLine {
        LogLine::Transition(Event {
            ts,
            key: key.into(),
            name: key.into(),
            from: "working".into(),
            to: to.into(),
        })
    }

    fn tracker(events: Vec<LogLine>, start: i64, end: i64) -> StatsTracker {
        StatsTracker::from_events(events, NaiveDate::MIN, start, end)
    }

    #[test]
    fn restart_closes_at_the_previous_line_not_the_new_start() {
        let events = vec![
            LogLine::Lifecycle {
                kind: Lifecycle::Start,
                ts: 0,
            },
            event(1_000, "blue", "waiting"),
            event(1_200, "yellow", "idle"),
            LogLine::Lifecycle {
                kind: Lifecycle::Heartbeat,
                ts: 1_500,
            },
            LogLine::Lifecycle {
                kind: Lifecycle::Start,
                ts: 10_000,
            },
            event(10_000, "blue", "waiting"),
        ];
        let tracker = tracker(events, 0, 20_000);
        assert_eq!(tracker.at(10_500).by_color["blue"], 1_000);
        assert_eq!(tracker.at(11_000).by_color["blue"], 1_500);
        assert_eq!(tracker.at(11_000).by_color["yellow"], 300);
    }

    #[test]
    fn rollback_uses_log_order_and_cached_future_intervals_keep_advancing() {
        let tracker = tracker(
            vec![event(100, "a", "waiting"), event(200, "a", "working")],
            0,
            500,
        );
        assert_eq!(tracker.at(150).total_ms, 50);
        assert_eq!(tracker.at(160).total_ms, 60);
        assert_eq!(tracker.at(210).total_ms, 100);
        let tracker = self::tracker(
            vec![event(200, "a", "waiting"), event(100, "a", "idle")],
            0,
            500,
        );
        assert_eq!(tracker.at(150).by_color["blue"], 0);
        assert_eq!(tracker.at(150).by_color["yellow"], 50);
    }

    #[test]
    fn date_and_offset_invalidate_the_tracker() {
        let mut tracker = tracker(vec![], 0, 500);
        tracker.day = Local::now().date_naive();
        assert!(tracker.is_current());
        tracker.offset += 3_600;
        assert!(!tracker.is_current());
        tracker.offset -= 3_600;
        tracker.day = tracker.day.pred_opt().unwrap();
        assert!(!tracker.is_current());
    }

    #[test]
    fn cached_open_intervals_update_rankings_without_reloading_history() {
        let mut events = Vec::new();
        for i in 0..5 {
            events.push(event(0, &format!("old{i}"), "waiting"));
            events.push(event(1_100, &format!("old{i}"), "gone"));
        }
        events.push(event(1_000, "new", "waiting"));
        let tracker = tracker(events, 0, 10_000);
        assert!(
            tracker
                .at(1_100)
                .sessions
                .iter()
                .all(|session| session.key != "new")
        );
        let current = tracker.at(2_201);
        assert_eq!(current.sessions[0].key, "new");
        assert_eq!(current.total_ms, 5 * 1_100 + 1_201);
    }

    #[test]
    fn open_interval_and_boundary_clipping() {
        let s = tracker(
            vec![event(5, "a", "waiting"), event(15, "a", "idle")],
            10,
            20,
        )
        .at(20);
        assert_eq!(s.total_ms, 10);
        assert_eq!(s.by_color["blue"], 5);
        assert_eq!(s.by_color["yellow"], 5);
    }
}
