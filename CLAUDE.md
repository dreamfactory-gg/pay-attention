# pay-attention

A menu bar light for Claude Code, Codex and Hermes sessions that need their human.
- **Install and use:** `README.md`. **Internals:** `docs/BEHAVIOR.md`.
- **Maintainers only:** the spec, `TASKS.md` and `REPORT.md` live in `private/`, a separate repo that public clones don't have.

## Rules
- **Tests never touch the real environment:**
  - not the real `~/.claude/settings.json`, `~/.codex/hooks.json`, `~/.codex/config.toml` or `~/.hermes/config.yaml`;
  - not existing Terminal tabs;
  - not running agent sessions.

  Every test points `HOME` at a temp dir. Every path in the code derives from `$HOME`.
- **Hooks fail open:**
  - exit 0;
  - print nothing to stdout or stderr;
  - spawn no subprocesses;
  - finish in under 30 ms;
  - work with the app not running.

## Pitfalls
- **`procStart`:** in `~/.claude/sessions/<pid>.json` it uses `ps -o lstart` format **in UTC** (`TZ=UTC ps -o lstart= -p PID`), not local time.
- **Fake process names in fixtures:** copies of system binaries are SIGKILLed by macOS (exit 137). Use a symlink to `/bin/sh`; argv[0] keeps the symlink's name.
- **Interpreted agents:** Hermes runs as `python3 ~/.local/bin/hermes` and cotal as `node …/cotal`. Match the agent name on the basename of argv[0] **or** argv[1].
- **Where to run `scripts/check-core.sh`:** not from inside a Codex or cotal process. Its headless-hook case expects no `codex` ancestor, and cotal ancestry hides every row.

- **Terminal windows a script opens:**
  - macOS `pgrep -t` matches nothing, in either tty form; use `ps -t ttysNNN -o pid=`.
  - Stop a window's processes before closing it, and never close a window whose tab is still busy. Otherwise Terminal pops up "terminate running processes?" on the screen of whoever is at the machine.
  - Claude shows a trust dialog in new non-temp dirs and writes no registry file until it's answered. e2e scratch dirs go under `$TMPDIR`, resolved with `pwd -P`.
- **Never pipe script text through a shell heredoc to edit a file; use the Edit tool.** On 2026-11-03 a nested `EOF` ended the outer heredoc early. zsh then ran check-script fixture lines against the real `$HOME`, which overwrote `~/.codex/config.toml` and chmodded `~/.local/state`.
- **Never edit a script while a run of it is still executing.** bash reads a script as it goes, so the running copy picks up the new bytes mid-run. Stop the run first (SIGTERM, so its EXIT trap cleans up), or wait for it to finish (2026-11-04).
- **Temp dirs in two steps:** `T=$(mktemp -d …)` and then `T=$(cd "$T" && pwd -P)`. In the one-liner `$(cd "$(mktemp -d …)" && pwd -P)`, a failed mktemp leaves `cd ""` in the current directory, and cleanup's `rm -rf "$T"` would delete it.
- **Test cleanup finds its processes by working directory** (`stop_dir` in `scripts/fixtures.sh`): a pid plus start time kept leaving pid-reuse gaps (three review rounds).
- **`scripts/e2e-app.sh` and `scripts/check-app.sh` put a real dot in the menu bar** for their fake sessions. It looks like the installed app's dot, and clicking its red row acks the test alert and fails item 9. Tell the person at the machine before each run, and don't run them while they use the installed app.

## Checks
`cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `scripts/check-core.sh`.
Changing install steps, CLI output or config keys means updating `README.md` § Agent install in the same change; agents follow it word for word.
