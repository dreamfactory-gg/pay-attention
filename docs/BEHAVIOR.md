# Behavior reference

How pay-attention behaves in detail, for contributors and for debugging. Install and everyday use are in the [README](../README.md).

## Setup, login item and single instance

`scripts/setup.sh` checks for the Xcode command-line tools, `cargo` and `npm`, then asks both of its questions before building, so piped answers are read before `npm` or `cargo` run. It copies `Pay Attention.app` to `~/Applications` and the CLI to `~/.local/bin`, registers the installed app to launch at the next login, and installs hooks only if the second answer was yes.

The settings' login-item toggle works only for the installed app, under `/Applications` or `~/Applications`. The executable itself and the Applications roots are resolved through symlinks before comparison; resolution failures mean not installed. The login item is an escaped, atomically written plist at `~/Library/LaunchAgents/gg.dreamfactory.pay-attention.plist`, holding the resolved executable. A path that doesn't resolve to a file is refused, and the old registration is kept. The toggle shows on only when the plist launches this copy; turning it on retargets it. Nothing loads it immediately: launchd reads it at the next login. The app never registers or refreshes it at startup.

Quit the running app before opening a newly installed build: a second instance exits immediately without focusing or replacing the first. A per-user exclusive lock at `~/.local/state/pay-attention/app.lock` is held for the app's lifetime; lock errors fail closed. Concurrent setup runs are refused using `~/.local/state/pay-attention/setup.lock` (a directory). Its holder PID is written into a file named for that PID; exit traps remove the lock, and a later setup reclaims a lock whose holder is dead. An empty or unreadable lock is left alone; remove it manually only after confirming no setup is running.

## Settings writes

If the config file changes while Settings is open, reopen Settings before saving. The stale-version check runs immediately before replacement. An external editor can still write in the instant between that check and the rename: editors do not take our advisory lock, so that narrow race is a known limit. Successful config writes refresh the form's version from the saved bytes and file identity, without rereading the path, even if changing autostart subsequently fails. Edits made after the save still count as outside changes. Hotkey registration success is the strongest available signal; proving delivery requires pressing the shortcut.

## Terminal automation

macOS asks for permission for Pay Attention to control Terminal. It is used to acknowledge the selected tab and to focus a session. Unsigned rebuilds may ask again. The app does not need Terminal automation for headless or Claude desktop sessions.

## Hooks and the installer

`hook <claude|codex|hermes> <Event>` consumes JSON on stdin, silently fails open, and works without the app. `install-hooks` writes hooks only for agents whose directory exists (`~/.claude`, `~/.codex`; `~/.hermes` for the printed block), so it never creates config for an agent that isn't installed. Installation preserves other hooks, symlinks and file modes, backs up changed files, and updates our JSON hook commands when the binary moves. Concurrent edits are left alone with a warning. Codex `config.toml` and Hermes `config.yaml` are never written: installation prints any needed Codex enablement notice and the Hermes hooks to merge manually. If Hermes already has a `hooks:` key, merge the event entries into it; do not add a second key or replace existing handlers. Uninstall removes only our JSON hooks and reminds you to remove the manually merged Hermes commands.

## Paths and defaults

All paths derive from `$HOME`: Claude's registry and transcripts under `.claude`, our sessions, crashes, acks and events under `.local/state/pay-attention`, and optional settings in `.config/pay-attention/config.toml`. Defaults include quiet hours 23:00–08:00 local time, a 600-second stall threshold, and escalating rings every 60–10 seconds. Set `dnd = ""` to disable quiet hours.

## Claude sessions and crash records

For multiple Claude registry files with the same session id, the latest process start wins. Status preserves unacked crashes in private (0600), atomically written `crashes/claude-<id>-<start>-<pid>.json` files, storing UTC epoch seconds parsed from `procStart` alongside the row. Only Claude `error`/`crashed` rows whose contents give their exact filename count; all other files are ignored for ranking, display and clearing. Crash changes hold `crashes.lock`, waiting at most 200 ms before skipping writes. An unlocked pass only detects work; under the lock, status re-reads registries, processes, crash files and acks from scratch, inspecting each process before reading its registry and re-inspecting an apparently dead or reused pid without the cache afterward. A live process matching the registry's `procStart` is not crashed. Valid crash files are preserved; an invalid file is replaced only at the current crash's exact path. After the registry sweep, the newest valid crash is the row.

Status deletes each exact crash file when acked, superseded by a newer crash, or followed by a strictly later live process. Same-second starts have no defined ordering and do not supersede a crash. Without crash work, status takes no crash lock and creates no state.

## Hook records

Claude hook records keep independent `failure` and `pending` parts; empty records are deleted and unreadable records count as empty. Hook writes and Codex/Hermes ack cleanup hold `sessions.lock` only for read/change/write. Under contention they retry for up to 500 ms from the start of the operation (the first attempt is always made), then skip. An unchanged Codex or Hermes hook record only refreshes its mtime, keeping the heartbeat without another atomic replacement.

Process inspection and transcript reads happen outside `sessions.lock`. Codex selects its transcript path under the lock and revalidates it after scanning. Claude's `PreToolUse` hook captures AskUserQuestion text and option labels; `PermissionRequest` captures the tool's command, file path or name without deciding permission. Waiting detail comes only from a matching pending kind within 10 seconds of the registry update, never from the transcript; idle detail is the last assistant text. `Stop` clears failure and pending after a successful turn. Only a later raw `busy` or `waiting` registry status hides a stored failure; `idle` and `shell` do not.

Claude's registry `shell` state is working and never stalled. Claude crash files preserve the registry name; Codex and Hermes derive it from `cwd`. Hermes `on_session_end` records each turn's outcome: completed/interrupted is idle; otherwise API error. Only `on_session_finalize` removes the row. Hermes events with an explicit platform other than `cli` are ignored, including finalize; events without a platform remain supported. A working Hermes session with no recent hook event becomes stalled. Codex retains its last supplied transcript path and uses that transcript's mtime for stalls; without a path it is never stalled. Process lookup is subprocess-free except CPU checks for stall candidates. Sessions run under a Cotal mesh agent (a `cotal` ancestor process, or `COTAL_NAME` set for hooks) are excluded. Headless sessions only raise red alerts. Acks match state, reason and timestamp; concurrent acknowledgements are serialized with a file lock.

## Library, logs and stats

`pa-core` exposes `snapshot`, `ack`, `config`, `Engine`, `schedule`, `stats`, `transitions`, `hook`, `install`, `uninstall` and `login_item`. The app appends transitions to `events.jsonl` and ring/phone actions to `rings.jsonl`; the CLI does not log polling. The app also appends a `start` line on launch and a `heartbeat` every five minutes, retrying a failed heartbeat on each subsequent tick until it succeeds.

Stats depend only on `events.jsonl`: a new `start` closes prior open intervals at the last valid line before it, excluding downtime with at most five minutes of undercount. There are no checkpoints, recovery writes, or legacy-log migration. Malformed lines are skipped, unterminated tails are separated before appending, and negative durations from clock rollback count as zero. The stats cache reloads when the log changes, the local date changes, or the UTC offset changes.

The app reads the config every tick and applies any change, so edits take effect within a second and a failed read is retried. Sound files that cannot be read or decoded fall back to synthesized earcons.

## Simulate

`pay-attention simulate` streams JSONL ticks, clicks, hotkeys and quiet requests to `pa-core`'s alert engine and returns JSONL actions. Episode timing, ring counts and phone state survive missing rows for up to an hour. Each tick checks DND and quiet against the current time; suppressed alerts catch up once silence ends. Failed outside-ack reloads keep the current acks and retry on the next tick.
