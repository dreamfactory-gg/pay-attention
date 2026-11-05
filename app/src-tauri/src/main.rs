mod audio;
mod platform;

use anyhow::{Context, Result};
use audio::Sound;
use pa_core::{Action, Config, ConfigVersion, Engine};
use serde::Serialize;
use std::{
    fs::{self, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant, SystemTime},
};
use tauri::{
    Emitter, Manager, State,
    image::Image,
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
};
use tauri_plugin_global_shortcut::{GlobalShortcutExt, Shortcut, ShortcutState};
use tauri_plugin_positioner::{Position, WindowExt};

enum Control {
    Click(String),
    Hotkey,
    Quiet(bool),
    Refresh,
    Save {
        config: Box<Config>,
        version: Option<ConfigVersion>,
        reply: tauri::async_runtime::Sender<Result<Option<ConfigVersion>, String>>,
    },
    Preview {
        color: String,
        source: String,
    },
}

struct AppState {
    tx: mpsc::Sender<Control>,
    dialog_open: Arc<AtomicBool>,
}

fn send(state: &State<'_, AppState>, control: Control) -> Result<(), String> {
    state.tx.send(control).map_err(|error| error.to_string())
}

#[tauri::command]
fn click(state: State<'_, AppState>, key: String) -> Result<(), String> {
    send(&state, Control::Click(key))
}

#[tauri::command]
fn quiet(state: State<'_, AppState>, enabled: bool) -> Result<(), String> {
    send(&state, Control::Quiet(enabled))
}

#[tauri::command]
fn refresh(state: State<'_, AppState>) -> Result<(), String> {
    send(&state, Control::Refresh)
}

#[tauri::command]
fn get_config() -> Result<pa_core::LoadedConfig, String> {
    pa_core::config_with_version().map_err(|error| error.to_string())
}

#[tauri::command]
async fn save_config(
    state: State<'_, AppState>,
    config: Config,
    version: Option<ConfigVersion>,
) -> Result<Option<ConfigVersion>, String> {
    let (reply, mut response) = tauri::async_runtime::channel(1);
    send(
        &state,
        Control::Save {
            config: Box::new(config),
            version,
            reply,
        },
    )?;
    response
        .recv()
        .await
        .ok_or_else(|| "App loop stopped".to_owned())?
}

#[tauri::command]
fn is_autostart_enabled() -> bool {
    std::env::current_exe().is_ok_and(|exe| pa_core::login_item::is_enabled(&exe))
}

#[tauri::command]
fn set_autostart(enabled: bool) -> Result<(), String> {
    (|| -> Result<()> {
        let executable = std::env::current_exe()?;
        anyhow::ensure!(
            pa_core::autostart_allowed(&executable),
            "Install the app in /Applications or ~/Applications before changing Launch at login"
        );
        if enabled {
            pa_core::login_item::enable(&executable)?;
        } else {
            pa_core::login_item::disable()?;
        }
        Ok(())
    })()
    .map_err(|error| error.to_string())
}

#[tauri::command]
fn preview(state: State<'_, AppState>, color: String, source: String) -> Result<(), String> {
    send(&state, Control::Preview { color, source })
}

#[tauri::command]
fn set_dialog_open(state: State<'_, AppState>, open: bool) {
    state.dialog_open.store(open, Ordering::SeqCst);
}

#[tauri::command]
fn system_sounds() -> Vec<String> {
    let mut names: Vec<_> = fs::read_dir("/System/Library/Sounds")
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("aiff"))
        })
        .filter_map(|path| {
            path.file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
        })
        .collect();
    names.sort();
    names
}

fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

fn home() -> Result<PathBuf> {
    Ok(PathBuf::from(
        std::env::var_os("HOME").context("HOME is not set")?,
    ))
}

fn state_dir() -> Result<PathBuf> {
    Ok(home()?.join(".local/state/pay-attention"))
}

fn append(file: &str, value: &impl Serialize) -> Result<()> {
    let dir = state_dir()?;
    fs::create_dir_all(&dir)?;
    let mut output = OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .mode(0o600)
        .open(dir.join(file))?;
    if output.metadata()?.len() > 0 {
        output.seek(SeekFrom::End(-1))?;
        let mut last = [0];
        output.read_exact(&mut last)?;
        if last[0] != b'\n' {
            output.write_all(b"\n")?;
        }
    }
    let mut line = serde_json::to_vec(value)?;
    line.push(b'\n');
    output.write_all(&line)?;
    Ok(())
}

fn icon(color: &str) -> Image<'static> {
    let rgb = match color {
        "green" => [52, 199, 89],
        "yellow" => [255, 190, 35],
        "blue" => [0, 122, 255],
        "red" => [255, 69, 58],
        _ => [142, 142, 147],
    };
    let mut rgba = Vec::with_capacity(24 * 24 * 4);
    for y in 0..24 {
        for x in 0..24 {
            let mut covered = 0;
            for sy in 0..4 {
                for sx in 0..4 {
                    let dx = x as f64 + (sx as f64 + 0.5) / 4.0 - 12.0;
                    let dy = y as f64 + (sy as f64 + 0.5) / 4.0 - 12.0;
                    covered += usize::from(dx * dx + dy * dy <= 9.0 * 9.0);
                }
            }
            rgba.extend_from_slice(&[rgb[0], rgb[1], rgb[2], (covered * 255 / 16) as u8]);
        }
    }
    Image::new_owned(rgba, 24, 24)
}

fn focus_window(window: &tauri::WebviewWindow) {
    let _ = window.show();
    let _ = window.set_focus();
}

fn show_window(app: &tauri::AppHandle, settings: bool, toggle: bool) {
    let Some(window) = app.get_webview_window("main") else {
        return;
    };
    if toggle && window.is_visible().unwrap_or(false) {
        let _ = window.hide();
        return;
    }
    let _ = window.move_window_constrained(Position::TrayBottomCenter);
    focus_window(&window);
    let _ = app.state::<AppState>().tx.send(Control::Refresh);
    if settings {
        let _ = window.emit("open-settings", ());
    }
}

fn perform(
    app: &tauri::AppHandle,
    engine: &Engine,
    config: &Config,
    audio: &mpsc::Sender<Sound>,
    actions: Vec<Action>,
) {
    for action in actions {
        match &action {
            Action::Ring {
                color, vol, keys, ..
            } => {
                if let Err(error) = append("rings.jsonl", &action) {
                    eprintln!("ring log: {error:#}");
                }
                let source = match *color {
                    "blue" => &config.sounds.blue,
                    "red" => &config.sounds.red,
                    _ => &config.sounds.yellow,
                };
                let _ = audio.send(Sound {
                    color: (*color).into(),
                    source: source.clone(),
                    volume: *vol,
                    pitch: engine.ring_pitch(color, keys),
                });
            }
            Action::Phone { argv, .. } => {
                if let Err(error) = append("rings.jsonl", &action) {
                    eprintln!("phone log: {error:#}");
                }
                let argv = argv.clone();
                thread::spawn(
                    move || match Command::new(&argv[0]).args(&argv[1..]).output() {
                        Ok(output) if output.status.success() => {}
                        Ok(output) => eprintln!(
                            "phone: {}: {}",
                            output.status,
                            String::from_utf8_lossy(&output.stderr).trim()
                        ),
                        Err(error) => eprintln!("phone: {error}"),
                    },
                );
            }
            Action::Event(event) => {
                if let Err(error) = append("events.jsonl", event) {
                    eprintln!("event log: {error:#}");
                }
            }
            Action::Light { color, count, .. } => {
                if let Some(tray) = app.tray_by_id("main") {
                    let _ = tray.set_icon(Some(icon(color)));
                    let _ = tray.set_title(if *count == 0 {
                        None
                    } else {
                        Some(count.to_string())
                    });
                }
            }
            Action::Focus { key, .. } => {
                if let Some(row) = engine.snapshot().rows.iter().find(|row| row.key == *key) {
                    platform::focus(row);
                }
            }
            Action::Ack { .. } => {}
        }
    }
}

#[derive(Serialize)]
struct View {
    rows: Vec<pa_core::DisplayRow>,
    light: String,
    count: usize,
    quiet_until: i64,
    stats: Option<pa_core::Stats>,
}

type FileStamp = (u64, u64, u64, SystemTime);

fn stamp(path: &Path) -> Option<FileStamp> {
    let metadata = fs::metadata(path).ok()?;
    Some((
        metadata.dev(),
        metadata.ino(),
        metadata.len(),
        metadata.modified().ok()?,
    ))
}

#[derive(Default)]
struct CachedStats {
    version: Option<FileStamp>,
    tracker: Option<pa_core::StatsTracker>,
}

impl CachedStats {
    fn get(&mut self) -> Option<pa_core::Stats> {
        let version = state_dir()
            .ok()
            .and_then(|dir| stamp(&dir.join("events.jsonl")));
        if self.version != version
            || self
                .tracker
                .as_ref()
                .is_none_or(|tracker| !tracker.is_current())
        {
            self.tracker = pa_core::StatsTracker::today()
                .map_err(|error| eprintln!("stats: {error:#}"))
                .ok();
            self.version = version;
        }
        self.tracker.as_ref().map(|tracker| tracker.stats())
    }
}

fn publish(app: &tauri::AppHandle, engine: &Engine, stats: &mut CachedStats) {
    // Only a shown popover renders, and opening it sends a Refresh. An emit per tick to a hidden one kept WebKit busy
    // in an idle app (e2e item 11).
    if !app
        .get_webview_window("main")
        .is_some_and(|window| window.is_visible().unwrap_or(false))
    {
        return;
    }
    let snapshot = engine.snapshot();
    let view = View {
        rows: engine.display_rows(),
        light: snapshot.light.clone(),
        count: snapshot
            .rows
            .iter()
            .filter(|row| row.alert_color().is_some())
            .count(),
        quiet_until: engine.quiet_until(),
        stats: stats.get(),
    };
    let _ = app.emit("state", &view);
}

fn register_hotkey<T>(
    app: &tauri::AppHandle,
    previous: &mut Option<Shortcut>,
    text: &str,
    commit: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let shortcut: Shortcut = text.parse()?;
    let changed = previous.as_ref() != Some(&shortcut);
    if changed {
        app.global_shortcut().register(shortcut)?;
    }
    let saved = match commit() {
        Ok(saved) => saved,
        Err(error) => {
            if changed && let Err(rollback) = app.global_shortcut().unregister(shortcut) {
                eprintln!("hotkey rollback: {rollback}");
            }
            return Err(error);
        }
    };
    if changed
        && let Some(old) = previous.replace(shortcut)
        && let Err(error) = app.global_shortcut().unregister(old)
    {
        eprintln!("hotkey: {error}");
    }
    Ok(saved)
}

fn run_loop(app: tauri::AppHandle, rx: mpsc::Receiver<Control>) {
    let mut heartbeat = Instant::now();
    let mut config = pa_core::config().unwrap_or_else(|error| {
        eprintln!("config: {error:#}");
        Config::default()
    });
    let mut engine = Engine::new(config.clone()).unwrap();
    let audio = audio::start();
    let terminal = platform::TerminalQuery::new();
    let mut hotkey = None;
    let mut stats = CachedStats::default();
    if let Err(error) = register_hotkey(&app, &mut hotkey, &config.hotkey, || Ok(())) {
        eprintln!("hotkey: {error:#}");
    }
    let mut config_error = None;
    let mut tick = Instant::now();
    loop {
        match rx.recv_timeout(tick.saturating_duration_since(Instant::now())) {
            Ok(control) => {
                let now = now();
                let actions = match control {
                    Control::Click(key) => engine.click(now, &key),
                    Control::Hotkey => engine.hotkey(now),
                    Control::Quiet(enabled) => {
                        engine.quiet(now, if enabled { now + 3_600_000 } else { now })
                    }
                    Control::Preview { color, source } => {
                        let _ = audio.send(Sound {
                            color,
                            source,
                            volume: 0.5,
                            pitch: 0,
                        });
                        Vec::new()
                    }
                    Control::Save {
                        config: updated,
                        version,
                        reply,
                    } => {
                        let result = register_hotkey(&app, &mut hotkey, &updated.hotkey, || {
                            pa_core::save_config(&updated, &version)
                        })
                        .inspect(|_| {
                            engine
                                .reload_config((*updated).clone())
                                .expect("saved config is valid");
                            config = *updated;
                        })
                        .map_err(|error| format!("{error:#}"));
                        let _ = reply.blocking_send(result);
                        Vec::new()
                    }
                    Control::Refresh => Vec::new(),
                };
                perform(&app, &engine, &config, &audio, actions);
                publish(&app, &engine, &mut stats);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if Instant::now() < tick {
            continue;
        }
        tick = Instant::now() + Duration::from_secs(1);
        if heartbeat.elapsed() >= Duration::from_secs(300) {
            match append(
                "events.jsonl",
                &serde_json::json!({"type": "heartbeat", "ts": now()}),
            ) {
                Ok(()) => heartbeat = Instant::now(),
                Err(error) => eprintln!("heartbeat: {error:#}"),
            }
        }
        // Read every tick and apply on any difference: a failed read or apply is simply retried next tick, with no
        // file stamp to fall out of step with the config actually loaded.
        match pa_core::config().and_then(|new| {
            if new != config {
                engine.reload_config(new.clone())?;
            }
            Ok(new)
        }) {
            Ok(new) => {
                config_error = None;
                if new != config {
                    if let Err(error) = register_hotkey(&app, &mut hotkey, &new.hotkey, || Ok(())) {
                        eprintln!("hotkey: {error:#}");
                    }
                    config = new;
                }
            }
            Err(error) => {
                let error = format!("{error:#}");
                if config_error.as_ref() != Some(&error) {
                    eprintln!("config: {error}");
                    config_error = Some(error);
                }
            }
        }
        match pa_core::snapshot_with_config(&config) {
            Ok(mut snapshot) => {
                engine.keep_focus_targets(&mut snapshot.rows);
                let tty = terminal.selected_tty(&snapshot.rows);
                let tick_now = now();
                let actions = engine.tick(
                    tick_now,
                    &snapshot.rows,
                    tty.as_deref(),
                    platform::idle_secs(),
                    platform::locked(),
                );
                perform(&app, &engine, &config, &audio, actions);
                publish(&app, &engine, &mut stats);
            }
            Err(error) => eprintln!("snapshot: {error:#}"),
        }
    }
}

fn main() {
    let _instance = match state_dir().and_then(|dir| pa_core::try_lock(&dir.join("app.lock"))) {
        Ok(Some(lock)) => lock,
        Ok(None) => {
            eprintln!("app lock: another instance is running");
            return;
        }
        Err(error) => {
            eprintln!("app lock: {error:#}");
            std::process::exit(1);
        }
    };
    if let Err(error) = append(
        "events.jsonl",
        &serde_json::json!({"type": "start", "ts": now()}),
    ) {
        eprintln!("start log: {error:#}");
        std::process::exit(1);
    }
    tauri::Builder::default()
        .plugin(tauri_plugin_positioner::init())
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _, event| {
                    if event.state == ShortcutState::Pressed {
                        let _ = app.state::<AppState>().tx.send(Control::Hotkey);
                    }
                })
                .build(),
        )
        .invoke_handler(tauri::generate_handler![
            click,
            quiet,
            refresh,
            get_config,
            save_config,
            set_autostart,
            is_autostart_enabled,
            preview,
            system_sounds,
            set_dialog_open
        ])
        .setup(move |app| {
            app.set_activation_policy(tauri::ActivationPolicy::Accessory);
            let (tx, rx) = mpsc::channel();
            let dialog_open = Arc::new(AtomicBool::new(false));
            app.manage(AppState {
                tx,
                dialog_open: dialog_open.clone(),
            });
            let window = app.get_webview_window("main").unwrap();
            let hide = window.clone();
            window.on_window_event(move |event| {
                if matches!(event, tauri::WindowEvent::Focused(false))
                    && !dialog_open.load(Ordering::SeqCst)
                {
                    let _ = hide.hide();
                }
            });
            let quiet = MenuItem::with_id(app, "quiet", "Quiet for 1 hour", true, None::<&str>)?;
            let settings = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?;
            let quit = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&quiet, &settings, &quit])?;
            TrayIconBuilder::with_id("main")
                .icon(icon("grey"))
                .icon_as_template(false)
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "quiet" => {
                        let _ = app.state::<AppState>().tx.send(Control::Quiet(true));
                    }
                    "settings" => show_window(app, true, false),
                    "quit" => app.exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    tauri_plugin_positioner::on_tray_event(tray.app_handle(), &event);
                    if matches!(
                        event,
                        TrayIconEvent::Click {
                            button: MouseButton::Left,
                            button_state: MouseButtonState::Up,
                            ..
                        }
                    ) {
                        show_window(tray.app_handle(), false, true);
                    }
                })
                .build(app)?;
            let handle = app.handle().clone();
            thread::spawn(move || run_loop(handle, rx));
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error running Pay Attention");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_hotkey_parses() {
        assert!(Config::default().hotkey.parse::<Shortcut>().is_ok());
    }
}
