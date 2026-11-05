mod simulate;

use anyhow::Result;
use chrono::{Local, NaiveDate, NaiveTime};
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "pay-attention",
    version,
    about = "Agent sessions that need your attention"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Simulate,
    Status {
        #[arg(long)]
        json: bool,
    },
    Hook {
        harness: String,
        event: String,
    },
    Ack {
        key: String,
    },
    Schedule {
        #[arg(long, value_parser = ["blue", "yellow", "red"])]
        color: String,
        #[arg(long, default_value_t = 10.0)]
        minutes: f64,
        #[arg(long)]
        start: Option<String>,
    },
    Stats {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        day: Option<NaiveDate>,
    },
    InstallHooks {
        #[arg(long)]
        dry_run: bool,
    },
    UninstallHooks,
    LoginItem {
        #[command(subcommand)]
        action: LoginItem,
    },
}

#[derive(Subcommand)]
enum LoginItem {
    On { exe: std::path::PathBuf },
    Off,
}

fn main() {
    // Bypass clap's error output even for malformed hook invocations.
    let args: Vec<_> = std::env::args_os().collect();
    if args.get(1).is_some_and(|s| s == "hook") {
        if let (Some(harness), Some(event)) = (
            args.get(2).and_then(|s| s.to_str()),
            args.get(3).and_then(|s| s.to_str()),
        ) {
            pa_core::hook(harness, event, std::io::stdin().lock());
        }
        return;
    }
    if let Err(error) = run(Cli::parse()) {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Simulate => simulate::run()?,
        Command::Status { json } => {
            let snapshot = pa_core::snapshot()?;
            if json {
                println!("{}", serde_json::to_string(&snapshot)?);
            } else {
                println!(
                    "Light: {}\n{:<30} {:<10} {:<16} NAME / DETAIL",
                    snapshot.light, "KEY", "STATE", "SURFACE"
                );
                for row in snapshot.rows {
                    println!(
                        "{:<30} {:<10} {:<16} {}{}  {}",
                        row.key,
                        row.state,
                        row.surface,
                        row.name,
                        if row.acked { " (acked)" } else { "" },
                        row.detail.unwrap_or_default()
                    );
                }
            }
        }
        Command::Hook { harness, event } => {
            pa_core::hook(&harness, &event, std::io::stdin().lock())
        }
        Command::Ack { key } => pa_core::ack(&key)?,
        Command::Schedule {
            color,
            minutes,
            start,
        } => {
            let start = start
                .map(|s| NaiveTime::parse_from_str(&s, "%H:%M"))
                .transpose()?
                .unwrap_or_else(|| Local::now().time());
            for ring in pa_core::schedule(&pa_core::config()?, &color, minutes, start)? {
                println!("t={:.2} vol={:.2}", ring.t, ring.volume);
            }
        }
        Command::Stats { json, day } => {
            let stats = pa_core::stats(day)?;
            if json {
                println!("{}", serde_json::to_string(&stats)?);
            } else {
                println!("Waited: {:.1} minutes", stats.total_ms as f64 / 60_000.0);
                for session in stats.sessions {
                    println!(
                        "{}  {}  {:.1}m",
                        session.key,
                        session.name,
                        session.ms as f64 / 60_000.0
                    );
                }
            }
        }
        Command::InstallHooks { dry_run } => {
            print_report(pa_core::install(&std::env::current_exe()?, dry_run)?)
        }
        Command::UninstallHooks => print_report(pa_core::uninstall()?),
        Command::LoginItem { action } => match action {
            LoginItem::On { exe } => pa_core::login_item::enable(&exe)?,
            LoginItem::Off => pa_core::login_item::disable()?,
        },
    }
    Ok(())
}

fn print_report(report: pa_core::InstallReport) {
    for path in report.changed {
        println!("{}", path.display());
    }
    for warning in report.warnings {
        eprintln!("{warning}");
    }
}
