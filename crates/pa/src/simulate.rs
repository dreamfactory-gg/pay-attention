use anyhow::Result;
use pa_core::{Engine, Row};
use serde::Deserialize;
use std::io::{self, BufRead, Write};

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Input {
    Tick {
        now: i64,
        rows: Vec<Row>,
        frontmost_tty: Option<String>,
        idle_secs: u64,
        locked: bool,
    },
    Click {
        now: i64,
        key: String,
    },
    Hotkey {
        now: i64,
    },
    Quiet {
        now: i64,
        until: i64,
    },
}

pub fn run() -> Result<()> {
    let mut engine = Engine::new(pa_core::config()?)?;
    let mut stdout = io::BufWriter::new(io::stdout().lock());
    for line in io::stdin().lock().lines() {
        let actions = match serde_json::from_str::<Input>(&line?)? {
            Input::Tick {
                now,
                rows,
                frontmost_tty,
                idle_secs,
                locked,
            } => engine.tick(now, &rows, frontmost_tty.as_deref(), idle_secs, locked),
            Input::Click { now, key } => engine.click(now, &key),
            Input::Hotkey { now } => engine.hotkey(now),
            Input::Quiet { now, until } => engine.quiet(now, until),
        };
        for action in actions {
            serde_json::to_writer(&mut stdout, &action)?;
            writeln!(stdout)?;
        }
        stdout.flush()?;
    }
    Ok(())
}
