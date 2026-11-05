use crate::Config;
use anyhow::{Result, ensure};
use chrono::{NaiveTime, Timelike};
use serde::Serialize;

#[derive(Debug, Clone, Copy, Serialize)]
pub struct Ring {
    pub t: f64,
    pub volume: f64,
}

pub fn schedule(config: &Config, color: &str, minutes: f64, start: NaiveTime) -> Result<Vec<Ring>> {
    ensure!(
        matches!(color, "blue" | "yellow" | "red"),
        "invalid alert color"
    );
    ensure!(
        minutes.is_finite() && minutes >= 0.0,
        "minutes must be finite and nonnegative"
    );
    config.validate()?;
    let dnd = config.dnd_window()?;
    let start = start.num_seconds_from_midnight() as f64;
    let mut cursor = Schedule::new(color);
    let mut rings = Vec::new();
    while let Some(t) = cursor.next(config) {
        let t = t + dnd_delay(dnd, start + t);
        if t >= minutes * 60.0 {
            break;
        }
        rings.push(cursor.advance(config, t));
    }
    Ok(rings)
}

pub(crate) fn dnd_delay(window: Option<(f64, f64)>, clock: f64) -> f64 {
    let Some((from, to)) = window else {
        return 0.0;
    };
    let clock = clock.rem_euclid(86400.0);
    let inside = if from < to {
        clock >= from && clock < to
    } else {
        from != to && (clock >= from || clock < to)
    };
    if inside {
        (to - clock).rem_euclid(86400.0)
    } else {
        0.0
    }
}

// Only facts are kept. The next ring and the yellow cap come from the config passed in, so a reload applies to every
// episode, including ones whose rows are briefly missing.
pub(crate) struct Schedule {
    yellow: bool,
    last: Option<f64>,
    advances: usize,
}

impl Schedule {
    pub(crate) fn new(color: &str) -> Self {
        Self {
            yellow: color == "yellow",
            last: None,
            advances: 0,
        }
    }

    pub(crate) fn next(&self, config: &Config) -> Option<f64> {
        let e = &config.escalation;
        if self.yellow && self.advances >= e.yellow_max_rings {
            return None;
        }
        Some(match self.last {
            Some(t) => {
                let progress = (t / e.ramp_secs).clamp(0.0, 1.0);
                t + e.start_interval_secs + (e.min_interval_secs - e.start_interval_secs) * progress
            }
            None if self.yellow => e.yellow_delay_secs,
            None => 0.0,
        })
    }

    pub(crate) fn advance(&mut self, config: &Config, t: f64) -> Ring {
        let e = &config.escalation;
        self.last = Some(t);
        self.advances += 1;
        let progress = (t / e.ramp_secs).clamp(0.0, 1.0);
        Ring {
            t,
            volume: e.start_volume + (1.0 - e.start_volume) * progress,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dnd_end_is_exact_and_yellow_counts_only_played_rings() {
        let rings = schedule(
            &Config::default(),
            "yellow",
            5.0,
            NaiveTime::from_hms_opt(7, 58, 0).unwrap(),
        )
        .unwrap();
        assert_eq!(rings.len(), 3);
        assert_eq!(rings[0].t, 120.0);
        assert!((rings[0].volume - 0.58).abs() < 0.00001);
    }
}
