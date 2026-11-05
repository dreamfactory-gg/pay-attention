# pay-attention

A macOS menu bar light for Claude Code, Codex and Hermes sessions that need you. Run several agents at once and the dot tells you when one is waiting, finished or broken, so you stop polling tabs. Click the dot for a list of sessions, and click a row to jump to that tab.

| Light | Meaning |
|---|---|
| red | A session crashed, hit an API error or rate limit, or stalled (working with no output for 10 minutes and no CPU) |
| blue | A session is waiting on you: a question, a permission prompt or a plan to approve |
| yellow | A session finished its turn; your move |
| green | Something is working and nothing needs you |
| grey | Nothing is running, or a state it doesn't recognize |

The light shows the worst unacknowledged alert, in the order red, blue, yellow. A number beside the dot counts unacknowledged sessions. Each alert rings: at once for red and blue, after 2 s for yellow. Then it rings faster and louder over 5 minutes, from every 60 s to every 10 s. Yellow stops after 3 rings. An alert is acknowledged when you click its row, return to its Terminal tab, or the session's state changes. Quiet hours (23:00–08:00 by default) and "Quiet for 1 hour" silence sound and phone pings; the light keeps updating.

## Status and limits

- v0.1, macOS only. You build it from source; there are no prebuilt binaries.
- **Claude Code** (CLI) is verified end-to-end on real sessions. **Codex** and **Hermes** support is built and covered by scripted checks, but less tested on live sessions. Claude desktop and VS Code sessions show up only if Claude Code writes its session registry for them, which is unverified.
- **Click-to-focus and auto-acknowledge on return work only in Terminal.app.** Clicking a Claude desktop row brings that app forward. In iTerm2, Ghostty, Warp and other terminals the light and sound work, but you acknowledge by clicking the row, and the click can't switch tabs for you.
- The build is unsigned. macOS asks once to let Pay Attention control Terminal, and may ask again after a rebuild.
- Claude Code's session registry is internal to Claude Code and can change in any update. States it doesn't recognize show grey, never a guess.
- Hooks reach only agent sessions started after they're installed.

## Install with your agent

Paste this into Claude Code or Codex on the Mac:

```text
Install pay-attention on this Mac by following the "Agent install" section of https://github.com/dreamfactory-gg/pay-attention
```

To install it yourself, follow the same steps. In step 5, run `bash scripts/setup.sh` in Terminal and answer its two questions.

## Agent install

You are installing a menu bar app and agent hooks on someone's Mac. Follow the steps in order. Each step gives the command, the result to expect, and what to do if it fails.

### 1. Rules

- Ask the human before you install anything, before you install hooks, and before each config file edit. Show them what will change.
- Back up any file before you edit it.
- Never restart, kill or reconfigure the human's running agent sessions or apps. Tell them what needs a restart, and let them choose when.
- Install missing toolchains only with the human's OK, using the manager they already use (rustup, mise, nvm, Homebrew). Don't install a second copy beside an existing one.
- Commands use full paths such as `~/.local/bin/pay-attention`; `~/.local/bin` is often not on `PATH`.
- Steps 3–8 need the network and write outside the repo (`~/Applications`, `~/.local`, `~/Library/LaunchAgents`, agent configs). If your sandbox blocks that (Codex's default does), ask the human to approve running them outside it, or have them run the step 5 command in Terminal.

### 2. Check the platform

```sh
sw_vers -productName
```

Expect `macOS`. On anything else, stop and tell the human that pay-attention is macOS only.

### 3. Prerequisites

```sh
xcode-select -p
rustc --version; cargo --version
node --version; npm --version
```

| Needed | Pass | If missing or too old |
|---|---|---|
| Xcode command-line tools | `xcode-select -p` prints a path | Ask the human to run `xcode-select --install` and click Install in the dialog (you can't click it). Recheck when they're done. |
| Rust, current stable | `rustc` and `cargo` print versions | Update it through the manager that owns `command -v rustc`: under `~/.cargo/bin`, `rustup update stable`; under `/opt/homebrew` or `/usr/local`, `brew upgrade rust`; under another manager (mise, asdf), that manager. With no Rust at all and the human's OK, install rustup: `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \| sh -s -- -y`, then `. "$HOME/.cargo/env"`. |
| Node.js `^20.19.0 \|\| >=22.12.0`, with npm | `node --version` is in range | Install Node 22 or newer through the manager that owns `command -v node` (a path under `.local/share/mise` is mise, `.nvm` is nvm, `/opt/homebrew` or `/usr/local` is Homebrew): `mise install node@22` (then run step 5's `bash scripts/setup.sh` as `mise exec node@22 -- bash scripts/setup.sh`), `nvm install 22`, or `brew install node`. With no Node at all, use the human's manager, or Homebrew if they have it. |

Any command that changes the default toolchain for the human's other projects (for example `mise use -g`, `nvm alias default`, `brew upgrade`) goes beyond this install: show it to them and get their OK first.

If your shell doesn't keep state between commands and `cargo` isn't found after installing rustup, start later commands with `. "$HOME/.cargo/env" &&`. `scripts/setup.sh` checks for `xcode-select`, `cargo` and `npm` itself. If one is missing, it exits with `setup.sh: missing: …` before asking anything.

### 4. Get the source

```sh
git clone https://github.com/dreamfactory-gg/pay-attention ~/src/pay-attention
```

If `~/src/pay-attention` already exists, update it instead with `git -C ~/src/pay-attention pull --ff-only`. Installed hooks call `~/.local/bin/pay-attention`, not the clone. Keep the clone for updates.

### 5. Build and install the app and CLI

Tell the human what this does, and wait for their yes:
- builds from source (several minutes the first time, while it downloads crates and npm packages);
- copies `Pay Attention.app` to `~/Applications` and the CLI to `~/.local/bin/pay-attention`;
- registers a login item at `~/Library/LaunchAgents/gg.dreamfactory.pay-attention.plist`, which launchd reads at the next login;
- opens the app;
- touches no agent config.

If Pay Attention is already running (an update), ask the human to quit it first: right-click the dot, then Quit. A second copy exits at once.

```sh
cd ~/src/pay-attention && printf 'y\nn\n' | bash scripts/setup.sh
```

The piped answers are "yes" to install and "no" to hooks; step 6 installs the hooks after a dry run. Allow at least 10 minutes: give the command a timeout of 15 minutes or more, or run it in the background if your tool can, and wait for its exit status. Expect exit status 0, the last line `Hooks not installed.`, and a dot in the menu bar.

- `setup.sh: missing: …` means go back to step 3.
- `setup.sh: no answer to "…"` means the answers didn't reach it; run the exact command above.
- `Another setup holds …/setup.lock` means another setup is running. Wait for it to finish. If none is running, ask the human before removing `~/.local/state/pay-attention/setup.lock`.

### 6. Install hooks

```sh
~/.local/bin/pay-attention install-hooks --dry-run
```

On stdout, one line per file it would change (or create): `~/.claude/settings.json` if `~/.claude` exists, and `~/.codex/hooks.json` if `~/.codex` exists. Agents that aren't installed are skipped. It adds 4 Claude hooks (`PreToolUse` for AskUserQuestion, `PermissionRequest`, `StopFailure`, `Stop`) and 8 Codex hooks, each running `~/.local/bin/pay-attention hook <agent> <Event>` with a 5 s timeout. On stderr come notices for configs it never writes (steps 7 and 8). An empty stdout with exit status 0 means the hooks are already current; a non-zero exit means something is wrong (for example a malformed `settings.json`), so read the error and don't go on. Show this to the human; on their yes:

```sh
~/.local/bin/pay-attention install-hooks
```

It keeps every other hook and prints the files it changed. Before each write it backs the file up as `<file>.bak-pay-attention-<timestamp>-<pid>`. If a backup fails, it exits non-zero without changing that file. If the human installs Claude Code or Codex later, run it again.

### 7. Codex: turn hooks on (only if the notice was printed)

The notice reads `Enable Codex hooks in …/.codex/config.toml (merge into the features table):`, followed by `[features]` and `hooks = true`. Edit the file yourself only in these two shapes:
- **A:** the file has a `[features]` header line, and the lines up to the next `[` header have no `hooks` key, or have exactly `hooks = false`. Add `hooks = true` directly under the header, or change `false` to `true`.
- **B:** the word `features` appears nowhere in the file, and it has no `\u` or `\U` escapes (or the file doesn't exist). Append `[features]` and `hooks = true` at the end, after a newline if the file doesn't end with one.

In any other shape (dotted `features.…` keys, an inline `features = {…}`, `features = "on"`, `hooks = "on"`, or anything you're unsure of), don't edit. Show the human the two lines and where they go, and let them make the change.

With the human's OK:
1. Back it up, if it exists: `cp -p ~/.codex/config.toml "$(mktemp ~/.codex/config.toml.bak-XXXXXX)"` (`mktemp` picks a name no earlier backup has).
2. Make the shape A or B edit, and show the human the diff.
3. Rerun `~/.local/bin/pay-attention install-hooks --dry-run`. pay-attention parses the file as TOML, so with exit status 0 the notice goes away only when the file parses and `features.hooks` is `true`. A non-zero exit says nothing about the TOML; fix what the error names first. If the notice is still there, restore the backup and show the human.

### 8. Hermes: merge the hooks block (only if it was printed)

The notice starts `Paste these hooks into …/.hermes/config.yaml.` and is followed by a `hooks:` block with one command per event. It is printed on every run while `~/.hermes` exists, so first run the check in item 3 below. If it already prints `ok: all 9 events`, skip this step.

Edit the file yourself only if this check prints `safe to append`. It rejects flow style, document markers, more than one document, and any `hooks` key, including one inherited through `<<:` merge keys or spelled with escapes:

```sh
/usr/bin/ruby -ryaml -e '
  text = File.read(ARGV[0])
  first = text.lines.find { |l| !l.strip.empty? && !l.lstrip.start_with?("#") }
  abort "flow style" if first.to_s.lstrip.start_with?("{")
  abort "document markers" if text.lines.any? { |l| l.start_with?("---", "...") }
  docs = YAML.load_stream(text)
  abort "not one mapping" unless docs.size == 1 && docs[0].is_a?(Hash)
  abort "already has hooks" if docs[0].key?("hooks")
  after = YAML.load_stream(text + (text.end_with?("\n") ? "" : "\n") + "hooks: {}\n")
  abort "append would change the file" unless after == [docs[0].merge("hooks" => {})]
  puts "safe to append"' ~/.hermes/config.yaml
```

On any other result, including a file that already has a `hooks:` key, don't edit. Show the human the printed block, and tell them to back the file up and merge its events under their existing `hooks:` key, keeping their handlers. When they're done, run the item 3 check below.

With the human's OK:
1. Back it up: `cp -p ~/.hermes/config.yaml "$(mktemp ~/.hermes/config.yaml.bak-XXXXXX)"`.
2. Append the printed block (from `hooks:` on) at the end of the file, after a newline if the file doesn't end with one. Show the human the diff.
3. Check that the whole file parses as one mapping, and that each of the 9 events runs its own pay-attention command:

   ```sh
   /usr/bin/ruby -ryaml -rshellwords -e '
     bin = File.expand_path("~/.local/bin/pay-attention")
     docs = YAML.load_stream(File.read(ARGV[0]))
     abort "not one YAML mapping" unless docs.size == 1 && docs[0].is_a?(Hash)
     hooks = docs[0]["hooks"]
     abort "no hooks mapping" unless hooks.is_a?(Hash)
     events = %w[on_session_start pre_llm_call post_llm_call on_human_input_request on_human_input_resolved
                 pre_approval_request post_approval_response on_session_end on_session_finalize]
     missing = events.reject { |e| Array(hooks[e]).any? { |h| h.is_a?(Hash) && (Shellwords.split(h["command"].to_s) rescue []) == [bin, "hook", "hermes", e] } }
     abort "missing: #{missing.join(" ")}" unless missing.empty?
     puts "ok: all 9 events"' ~/.hermes/config.yaml
   ```

   Expect `ok: all 9 events`. On any other result, restore the backup and show the human.
4. Tell the human that Hermes asks for consent the first time each hook event fires.

### 9. Verify

```sh
pgrep -x pay-attention-app
~/.local/bin/pay-attention status
~/.local/bin/pay-attention install-hooks --dry-run
```

Pass:
- `pgrep` prints a pid.
- `status` starts with `Light:`, followed by a table. If you are Claude Code, the table has a `claude:…` row for your own session, in state `working`. Claude rows come from Claude Code's own session registry, so they show even before hooks. `status --json` gives the same data, with `cwd` per row.
- If you are Codex, your own session may not show: Codex rows come only from hooks, and a session started before them may not report until it restarts. Check `grep -cE "pay-attention'? hook codex" ~/.codex/hooks.json` instead; expect `8`.
- The dry run exits 0 and prints nothing on stdout. With `~/.hermes` present, the Hermes block still prints on stderr every time; that's expected.
- Rows are keyed `claude:…`, `codex:…` or `hermes:…`. A Hermes row appears once a Hermes session starts after step 8.

### 10. Tell the human

- The dot in the menu bar is pay-attention. Click it for the session list, and right-click it for Quiet for 1 hour, Settings… and Quit.
- The first time pay-attention talks to Terminal (a click on a Terminal row, or checking which tab is in front while Terminal is frontmost and an alert is up), macOS asks to let Pay Attention control Terminal. Click Allow; focusing tabs and auto-acknowledging need it. People who never use Terminal.app may never see this prompt, and their choice doesn't matter for other terminals.
- Agent sessions that were already running don't have the hooks yet. Restart them when convenient (Claude Code rows show anyway, without question and permission detail).
- It starts at login. Settings has the login toggle, sounds, quiet hours, the phone command and the hotkey (default ⌃⌥⌘P).
- Mention any limit from [Status and limits](#status-and-limits) that applies to them, such as using a terminal other than Terminal.app.

### 11. Troubleshooting

| Symptom | Fix |
|---|---|
| `setup.sh: missing: …` | Install what it names (step 3), then rerun step 5. |
| `setup.sh: no answer to "…"` | Pipe in the answers exactly as in step 5, or have the human run `bash scripts/setup.sh` in Terminal. |
| `Another setup holds …/setup.lock; refusing to run.` | Wait for the other setup. If none is running, ask the human, then remove `~/.local/state/pay-attention/setup.lock`. |
| The app exits at once | Another copy is running (it holds `~/.local/state/pay-attention/app.lock`). Quit that one first. |
| Your session isn't in `status` | Claude Code: check `ls ~/.claude/sessions/`. Codex and Hermes: the session must start after the hooks (and, for Codex, after `hooks = true`). |
| Clicking a row doesn't focus the tab | The tab must be in Terminal.app, and Automation must be allowed: System Settings › Privacy & Security › Automation › Pay Attention › Terminal. |
| `install-hooks` says a file changed while editing | Something else wrote the file during the install. Rerun it. |
| `git pull --ff-only` fails | The clone has local changes or has diverged. Show the human the git output; don't reset or discard anything without their OK. |

## Using it

- **The list:** one row per session, with its state and a detail line (the pending question, the command awaiting permission, the error, or the last reply). Sessions without a terminal sit in a collapsed "headless" group and color the light only when red.
- **Acknowledge:** click a row. That also focuses the session (Terminal tab or Claude desktop). Returning to the session's Terminal tab and using it also acknowledges it. An acknowledged row is dimmed, doesn't color the light and doesn't ring. Acks survive restarts.
- **Hotkey:** ⌃⌥⌘P jumps to the oldest unacknowledged alert, blue first, then red, then yellow. Press it again for the next one.
- **Quiet:** quiet hours (`dnd`) and "Quiet for 1 hour" silence sound and phone pings. Missed alerts catch up when quiet ends.
- **Phone:** when you're away (idle 5 minutes, or the screen is locked), a blue or red alert left unacknowledged for 2 minutes runs `phone_command` once. It's an argv list with no shell; `{name}`, `{state}` and `{detail}` are substituted. Leave it empty to turn this off.
- **CLI:**

```sh
~/.local/bin/pay-attention status [--json]
~/.local/bin/pay-attention ack claude:SESSION_ID            # key as shown by status
~/.local/bin/pay-attention schedule --color blue [--minutes 10] [--start 12:00]   # ring times and volumes
~/.local/bin/pay-attention stats [--json] [--day 2026-10-14]                      # time spent waiting on you
~/.local/bin/pay-attention install-hooks [--dry-run]
~/.local/bin/pay-attention uninstall-hooks
~/.local/bin/pay-attention login-item on '/path/to/Pay Attention.app/Contents/MacOS/pay-attention-app'
~/.local/bin/pay-attention login-item off
```

## Configuration

Settings live in `~/.config/pay-attention/config.toml`. Edit them in the app (Settings…) or by hand. The app rereads the file every second. Every key is optional.

| Key | Default | Meaning |
|---|---|---|
| `stalled_after_secs` | `600` | Working with no output this long, and no CPU, turns red (stalled) |
| `dnd` | `"23:00-08:00"` | Quiet hours, local time, `HH:MM-HH:MM`; `""` turns them off |
| `away_after_secs` | `300` | Idle this long (or a locked screen) counts as away |
| `phone_after_secs` | `120` | How long a blue or red alert waits, while you're away, before `phone_command` runs |
| `phone_command` | `[]` | argv list run once per alert; `{name}`, `{state}`, `{detail}` substituted |
| `hotkey` | `"Ctrl+Alt+Cmd+P"` | Jump to the next unacknowledged alert |
| `[escalation] start_interval_secs` | `60` | First gap between rings |
| `[escalation] min_interval_secs` | `10` | Shortest gap between rings |
| `[escalation] start_volume` | `0.3` | First ring's volume (0–1); it rises to 1.0 |
| `[escalation] ramp_secs` | `300` | Time to go from start to shortest gap and full volume |
| `[escalation] yellow_max_rings` | `3` | Yellow alerts stop after this many rings |
| `[escalation] yellow_delay_secs` | `2` | Yellow waits this long for the state to hold before ringing |
| `[sounds] blue`, `yellow`, `red` | `"synth"` | `"synth"`, a system sound such as `"system:Glass"`, or a path to an audio file |

Example:

```toml
dnd = "22:30-07:30"
phone_command = ["/path/to/your-notifier", "{name}: {detail}"]

[sounds]
blue = "system:Glass"
```

`{detail}` can hold a pending command, a question or a file path. Send it only to a destination that you control and that requires authentication.

## Update

Ask the human to quit the app (right-click the dot, then Quit). Then:

```sh
git -C ~/src/pay-attention pull --ff-only
cd ~/src/pay-attention && printf 'y\nn\n' | bash scripts/setup.sh
~/.local/bin/pay-attention install-hooks --dry-run
```

Setup replaces the app and CLI, and restores the previous app if the copy fails. If the dry run lists files, the hooks need updating; with the human's OK, run `~/.local/bin/pay-attention install-hooks`. macOS may ask again for Terminal control after a rebuild.

## Uninstall

Remove the hooks first and delete the CLI last: an agent whose hook points at a missing CLI reports hook errors. With the human's OK:

1. Quit the app (right-click the dot, then Quit).
2. Remove the JSON hooks. `uninstall-hooks` removes only pay-attention's own entries, and backs up each file before changing it. Run it twice: the second run must exit 0 and print nothing on stdout, which means nothing is left to remove. On a non-zero exit or a "changed while editing" warning, stop and fix the cause first.

   ```sh
   ~/.local/bin/pay-attention uninstall-hooks
   ~/.local/bin/pay-attention uninstall-hooks
   ```

3. If `~/.hermes/config.yaml` exists:
   - The human removes the `pay-attention hook hermes` entries (or you do, with their OK, after backing it up as in step 8).
   - Then check that the file parses and no command still runs pay-attention. Expect `ok: none left`:

   ```sh
   /usr/bin/ruby -ryaml -rshellwords -e '
     cmds = []
     walk = ->(x) { x.is_a?(Hash) ? x.each { |k, v| k == "command" ? cmds << v.to_s : walk.(v) } : (x.each(&walk) if x.is_a?(Array)) }
     YAML.load_stream(File.read(ARGV[0])).each(&walk)
     left = cmds.select { |c| w = (Shellwords.split(c) rescue nil); w ? File.basename(w[0].to_s) == "pay-attention" && w[1] == "hook" : c.include?("pay-attention") }
     abort "still there: #{left.join(", ")}" unless left.empty?
     puts "ok: none left"' ~/.hermes/config.yaml
   ```

   - Hermes sessions that were running keep their hooks in memory. Ask the human to restart them before you delete the CLI.
4. Remove the login item, the app and the CLI:

   ```sh
   ~/.local/bin/pay-attention login-item off
   rm -rf ~/Applications/'Pay Attention.app'
   rm ~/.local/bin/pay-attention
   ```

Leave Codex's `hooks = true` unless nothing else uses Codex hooks. State and settings stay in `~/.local/state/pay-attention` and `~/.config/pay-attention` until you delete them.

## How it works

- **Claude Code:** pay-attention reads Claude Code's session registry (`~/.claude/sessions/*.json`) and transcripts, and checks each session's process every second. Its Claude hooks add the text of pending questions and permission prompts, and record API errors.
- **Codex and Hermes:** hooks write a small record per session under `~/.local/state/pay-attention/sessions/`.
- `pa-core` merges these into one snapshot. The app (Tauri) polls it every second for the light, list and rings; the CLI reads the same snapshot.
- Hooks fail open: they exit 0, print nothing, spawn nothing, and work with the app not running. The scripted checks hold them to a p99 under 30 ms. When another hook holds the state lock, a hook retries for at most 500 ms, then drops its write.
- Every path derives from `$HOME`.

Details (locks, crash records, logs, stats): [docs/BEHAVIOR.md](docs/BEHAVIOR.md).

## Development

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
bash scripts/check-core.sh     # pa-core and the CLI
bash scripts/check-engine.sh   # the alert engine, plus check-core
bash scripts/check-app.sh      # the app's lint, types and build, plus a 6 s launch smoke
```

Tests run in throwaway HOMEs and never touch your real agent configs, Terminal tabs or sessions. `check-app.sh` and `scripts/e2e-app.sh` put a real dot in the menu bar while they run. `scripts/e2e.sh` runs real Claude Code sessions. Run `check-core.sh` from a normal shell, not from inside a Codex session. `CLAUDE.md` has the rules and pitfalls for working on this repo.

## License

MIT. See [LICENSE](LICENSE).
