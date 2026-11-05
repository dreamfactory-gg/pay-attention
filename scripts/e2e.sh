#!/usr/bin/env bash
# End-to-end judge on real Claude sessions.
# - Each case runs its own Claude session in a hidden pty (`script`, a terminal without a window) in a scratch dir under
#   $TMPDIR. No Terminal windows open, and existing tabs and sessions are never touched. Claude shows no trust dialog in
#   temp dirs; elsewhere a new dir blocks on that dialog without writing a registry file.
# - Claude turns run on Haiku with user settings skipped (--setting-sources project,local), so no user hooks or plugins
#   (your own hooks never see these sessions).
# - `status` and `hook` run under a fake HOME whose .claude links to the real one, so pay-attention's own state and
#   config stay out of the real HOME.
# Covers items 1, 2, 3 (state), 4, 5 and 8. Item 10 is scripts/check-core.sh. The app, Codex and Hermes items come later.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
BIN="$ROOT/target/debug/pay-attention"
# Two steps: a failed mktemp then stops the script, instead of leaving `cd ""` in the current directory.
S=$(mktemp -d "${TMPDIR:-/tmp}/pa-e2e.XXXXXX")
S=$(cd "$S" && pwd -P)
FH="$S/home"
mkdir -p "$FH/.config/pay-attention"
ln -s "$HOME/.claude" "$FH/.claude"
printf 'stalled_after_secs = 60\ndnd = ""\n' >"$FH/.config/pay-attention/config.toml"
# Our Claude hooks for these sessions only, via --settings, writing to the fake HOME's state. Never run install-hooks
# under $FH: its .claude is the real ~/.claude.
jq -n --arg h "HOME=$(printf %q "$FH") $(printf %q "$BIN") hook claude" '{hooks: {
  PreToolUse: [{matcher: "AskUserQuestion", hooks: [{type: "command", command: ($h + " PreToolUse")}]}],
  PermissionRequest: [{hooks: [{type: "command", command: ($h + " PermissionRequest")}]}],
  StopFailure: [{hooks: [{type: "command", command: ($h + " StopFailure")}]}],
  Stop: [{hooks: [{type: "command", command: ($h + " Stop")}]}]}}' >"$S/settings.json"
CLAUDE="claude --model haiku --setting-sources project,local --strict-mcp-config --settings $S/settings.json"

# shellcheck source=scripts/fixtures.sh
. "$ROOT/scripts/fixtures.sh"
# Stops only what this script started: each pty wrapper starts in $S/NAME, and its session and tools inherit that
# working directory.
cleanup() { stop_dir "$S" || echo "cleanup: could not list processes; check for leftovers under $S" >&2; }
trap cleanup EXIT
fail() {
  echo "FAIL: $*" >&2
  exit 1
}
ok() { echo "ok: $*"; }

# run_script NAME ARGS...: writes $S/NAME/run.sh, which runs `claude ARGS...` in $S/NAME.
run_script() {
  local d="$S/$1"
  shift
  mkdir -p "$d"
  { printf '#!/bin/bash\ncd %q\nexec %s' "$d" "$CLAUDE"; (($#)) && printf ' %q' "$@"; echo; } >"$d/run.sh"
  chmod +x "$d/run.sh"
}
# open_pty NAME ARGS...: `claude ARGS...` in a hidden pty, with a clean env (no inherited Claude Code session markers)
# and stdin from /dev/null, so nothing typed into this script's terminal reaches the session.
open_pty() {
  run_script "$@"
  (cd "$S/$1" && exec env -i HOME="$HOME" PATH="$PATH" TERM=xterm-256color USER="$USER" SHELL="$SHELL" \
    LANG="${LANG:-en_US.UTF-8}" script -q /dev/null "$S/$1/run.sh") </dev/null >"$S/$1/pty.out" 2>&1 &
}
status() { HOME="$FH" "$BIN" status --json; }
row() { status | jq -c --arg c "$S/$1" '[.rows[] | select(.cwd == $c)][0] // empty'; }
# wait_row NAME SECS FILTER: poll every 0.5 s until NAME's row matches the jq FILTER; prints the row.
wait_row() {
  local end=$((SECONDS + $2)) r=""
  while ((SECONDS < end)); do
    r=$(row "$1")
    if [ -n "$r" ] && jq -e "$3" <<<"$r" >/dev/null; then
      echo "$r"
      return 0
    fi
    sleep 0.5
  done
  fail "$1: no row matching '$3' within $2 s; last row: ${r:-none}"
}
# lag ROW: ms between the row entering its state and now.
lag() { echo $(($(now_ms) - $(jq -r .since <<<"$1"))); }
within_2s() {
  local l
  l=$(lag "$1")
  ((l <= 2000)) || fail "row seen ${l} ms after it entered its state: $1"
  echo "$l"
}
transcript_age() {
  local f
  f=$(find "$HOME/.claude/projects" -name "$1.jsonl" -print -quit)
  echo $(($(date +%s) - $(stat -f %m "$f")))
}

open_pty crash
open_pty question "Call the AskUserQuestion tool exactly once to ask me: \"Which database should we use?\" with the options Postgres and SQLite. Do nothing else."
open_pty permission --permission-mode default "Run this exact shell command with the Bash tool: touch e2e-permission-marker"
open_pty idle "Reply with the single sentence: All done here."
# --allowedTools takes several values, so the =form keeps it from swallowing the prompt.
# Claude Code refuses a foreground `sleep` (the model then backgrounds it, and the registry says `shell`), so the quiet
# hang is `tail -f /dev/null`. Both commands end at the Bash tool's 2-minute timeout, after the checks below.
open_pty stall "--allowedTools=Bash(tail:*)" "Run this exact shell command with the Bash tool in the foreground (not in the background) and wait for it: tail -f /dev/null"
open_pty cpu "--allowedTools=Bash(yes:*)" "Run this exact shell command with the Bash tool in the foreground (not in the background) and wait for it: yes > /dev/null"

# Every Claude start sweeps dead sessions' registry files, so kill only once the other sessions have started.
for n in question permission idle stall cpu; do wait_row "$n" 60 true >/dev/null; done
r=$(wait_row crash 60 '.state == "idle"')
CRASH_PID=$(jq -r .pid <<<"$r")
kill -9 "$CRASH_PID"
t0=$SECONDS
r=$(wait_row crash 2 '.state == "error" and .reason == "crashed"')
CRASH_KEY=$(jq -r .key <<<"$r")
ok "item 5: kill -9 -> red/crashed within $((SECONDS - t0)) s"

r=$(wait_row question 180 '.state == "waiting" and .reason == "question"
  and (.detail // "" | contains("Which database should we use?") and contains("Postgres"))')
ok "item 1: AskUserQuestion -> blue/question with the question and options in the detail ($(within_2s "$r") ms)"

r=$(wait_row permission 180 '.state == "waiting" and .reason == "permission"
  and (.detail // "" | contains("touch e2e-permission-marker"))')
ok "item 2: permission prompt -> blue/permission with the command in the detail ($(within_2s "$r") ms)"

r=$(wait_row idle 180 '.state == "idle" and (.detail // "" | contains("All done here."))')
IDLE_SID=$(jq -r '.key | sub("^claude:"; "")' <<<"$r")
ok "item 3 (state): finished turn -> yellow/idle, detail has the last reply (focus ack: with the app)"

printf '{"session_id":"%s","hook_event_name":"StopFailure","error":"rate_limit","cwd":"%s"}' "$IDLE_SID" "$S/idle" |
  HOME="$FH" "$BIN" hook claude StopFailure
r=$(wait_row idle 2 '.state == "error" and .reason == "rate_limit"')
ok "item 8: replayed StopFailure rate_limit -> red/rate_limit"

r=$(wait_row stall 120 '.state == "working"')
STALL_SID=$(jq -r '.key | sub("^claude:"; "")' <<<"$r")
r=$(wait_row stall 180 '.state == "error" and .reason == "stalled"')
a=$(transcript_age "$STALL_SID")
((a <= 75)) || fail "stalled only after ${a} s without output"
ok "item 4: a quiet hang (tail -f /dev/null) -> red/stalled ${a} s after the last output"

r=$(wait_row cpu 120 '.state == "working"')
CPU_SID=$(jq -r '.key | sub("^claude:"; "")' <<<"$r")
end=$((SECONDS + 200))
until (($(transcript_age "$CPU_SID") >= 90)); do
  ((SECONDS < end)) || fail "the CPU-busy session kept writing its transcript; can't test 90 s without output"
  sleep 1
done
r=$(row cpu)
[ "$(jq -r .state <<<"$r")" = working ] || fail "a CPU-busy tool with no output for 90 s left working: $r"
ok "item 4: a CPU-busy loop with no output for 90 s stays green"

open_pty sweep
wait_row sweep 60 '.state == "idle"' >/dev/null
[ -e "$HOME/.claude/sessions/$CRASH_PID.json" ] && echo "note: Claude did not sweep the crashed registry file this time"
r=$(status | jq -c --arg k "$CRASH_KEY" '[.rows[] | select(.key == $k)][0] // empty')
[ "$(jq -r .reason <<<"${r:-null}")" = crashed ] || fail "the crash vanished after a new session started: ${r:-none}"
HOME="$FH" "$BIN" ack "$CRASH_KEY"
[ -z "$(status | jq -c --arg k "$CRASH_KEY" '.rows[] | select(.key == $k)')" ] || fail "an acked crash still listed"
ok "item 5: the crash outlives Claude's registry sweep until acked"

echo "E2E CLAUDE ITEMS PASSED"
