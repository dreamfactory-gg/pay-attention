#!/usr/bin/env bash
# Core checks: pa-core + the pay-attention CLI.
# Runs entirely in throwaway HOMEs under a temp dir; the real ~/.claude, ~/.codex, ~/.hermes,
# Terminal tabs and running sessions are never touched.
set -euo pipefail
export TZ=UTC
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

cargo fmt --all --check
cargo clippy --workspace --all-targets -q -- -D warnings
cargo build -q -p pa
cargo build -q --release -p pa
BIN="$ROOT/target/debug/pay-attention"
REL="$ROOT/target/release/pay-attention"

T=$(mktemp -d "${TMPDIR:-/tmp}/pa-check.XXXXXX")
touch "$T/pids"
cleanup() {
  while read -r p; do
    pkill -P "$p" 2>/dev/null || true
    kill "$p" 2>/dev/null || true
  done <"$T/pids"
  chmod -R u+w "$T" 2>/dev/null || true
  rm -rf "$T"
}
trap cleanup EXIT
unset COTAL_NAME
mkdir -p "$T/bin" "$T/q/codex" "$T/q/hermes"
SEQ=0

fail() { echo "FAIL: $*" >&2; exit 1; }
ok() { echo "ok: $*"; }
track() { echo "$1" >>"$T/pids"; }
wait_dead() { for _ in $(seq 100); do kill -0 "$1" 2>/dev/null || return 0; sleep 0.05; done; fail "pid $1 did not exit"; }
tty_of() { echo "/dev/$(ps -o tty= -p "$1" | tr -d ' ')"; }
# shellcheck source=scripts/fixtures.sh
. "$ROOT/scripts/fixtures.sh"

new_home() {
  export HOME="$T/$1"
  mkdir -p "$HOME/.claude/sessions" "$HOME/.claude/projects/-tmp-proj" "$HOME/.codex" "$HOME/.hermes" \
    "$HOME/.config/pay-attention"
}

# A process with no controlling tty (its own session), like a headless run.
bg_proc() {
  python3 -c 'import os, sys; os.setsid(); os.execvp("sh", ["sh", "-c", sys.argv[1]])' "$1" </dev/null >/dev/null 2>&1 &
  local p=$!
  track "$p"
  sleep 0.2
  echo "$p"
}

tx() { cat >"$HOME/.claude/projects/-tmp-proj/$1.jsonl"; }
old_mtime() { touch -t "$(date -v-10M +%Y%m%d%H%M.%S)" "$1"; }

status() { "$BIN" status --json; }
field() {
  status | jq -r --arg k "$1" --arg f "$2" \
    '[.rows[] | select(.key == $k)] | if length == 0 then "MISSING" else (.[0][$f] | tostring) end'
}
expect() {
  local got
  got=$(field "$1" "$2")
  [ "$got" = "$3" ] || fail "$1 .$2 = '$got', want '$3'"
}
expect_has() {
  local got
  got=$(field "$1" "$2")
  case "$got" in *"$3"*) ;; *) fail "$1 .$2 = '$got', want it to contain '$3'" ;; esac
}
expect_lacks() {
  local got
  got=$(field "$1" "$2")
  case "$got" in *"$3"*) fail "$1 .$2 = '$got', must not contain '$3'" ;; esac
}
expect_light() {
  local got
  got=$(status | jq -r .light)
  [ "$got" = "$1" ] || fail "light = '$got', want '$1'"
}

# A long-lived fake agent process (argv named via a /bin/sh symlink) that runs queued hook calls as its
# children, so each hook sees the agent as an ancestor, as it would under the real harness.
cat >"$T/loop.sh" <<'EOF'
while :; do
  for q in "$QDIR"/*.sh; do
    [ -e "$q" ] || continue
    sh "$q"
    rm -f "$q"
  done
  sleep 0.05
done
EOF
# send HARNESS QDIR EVENT JSON [ENV_PREFIX]: run one hook under the fake agent; it must exit 0 silently.
send() {
  local h=$1 dir=$2 ev=$3 json=$4 envp=${5:-}
  SEQ=$((SEQ + 1))
  printf '%s' "$json" >"$T/payload.$SEQ"
  printf '%s "%s" hook %s %s <"%s" >"%s" 2>"%s"; echo $? >"%s"\n' "$envp" "$BIN" "$h" "$ev" \
    "$T/payload.$SEQ" "$T/out.$SEQ" "$T/err.$SEQ" "$T/rc.$SEQ" >"$T/tmp.$SEQ"
  mv "$T/tmp.$SEQ" "$dir/$SEQ.sh"
  for _ in $(seq 100); do
    [ -e "$dir/$SEQ.sh" ] || break
    sleep 0.05
  done
  [ ! -e "$dir/$SEQ.sh" ] || fail "fake agent did not run hook $h $ev"
  [ "$(cat "$T/rc.$SEQ")" = 0 ] || fail "hook $h $ev exited $(cat "$T/rc.$SEQ")"
  [ ! -s "$T/out.$SEQ" ] || fail "hook $h $ev wrote stdout"
  [ ! -s "$T/err.$SEQ" ] || fail "hook $h $ev wrote stderr"
}
# hook_direct HARNESS EVENT STDIN: run a hook from this script (no agent ancestor); must exit 0 silently.
hook_direct() {
  local rc=0
  printf '%s' "$3" | "$BIN" hook "$1" "$2" >"$T/hd.out" 2>"$T/hd.err" || rc=$?
  [ "$rc" = 0 ] || fail "hook $1 $2 exited $rc"
  [ ! -s "$T/hd.out" ] && [ ! -s "$T/hd.err" ] || fail "hook $1 $2 printed output"
}

########## Claude registry rows ##########
new_home main
printf 'stalled_after_secs = 60\n' >"$HOME/.config/pay-attention/config.toml"

P_BUSY=$(tty_proc 'sleep 300; :')
reg "$P_BUSY" s-busy busy "" "" 1793700000000
expect claude:s-busy harness claude
expect claude:s-busy state working
expect claude:s-busy reason null
expect claude:s-busy surface terminal
expect claude:s-busy tty "$(tty_of "$P_BUSY")"
expect claude:s-busy pid "$P_BUSY"
expect claude:s-busy name name-s-busy
expect claude:s-busy cwd /tmp/proj
expect claude:s-busy since 1793700000000
expect claude:s-busy acked false
ok "claude busy -> working, row fields"

# Claude 2.1.317 writes no transcript entry for a pending AskUserQuestion (seen live, 2026-11-03); the PreToolUse hook
# carries the questions.
P_Q=$(tty_proc 'sleep 300; :')
tx s-q <<'EOF'
{"type":"user","message":{"role":"user","content":"set up the db"}}
{"type":"tools-used","tools":["AskUserQuestion"],"sessionId":"s-q"}
EOF
hook_direct claude PreToolUse '{"session_id":"s-q","hook_event_name":"PreToolUse","tool_name":"AskUserQuestion","tool_input":{"questions":[{"question":"Which database should we use?","header":"DB","multiSelect":false,"options":[{"label":"Postgres","description":"x"},{"label":"SQLite","description":"y"}]}]},"cwd":"/tmp/proj"}'
hook_direct claude PreToolUse '{"session_id":"s-q","hook_event_name":"PreToolUse","tool_name":"Bash","tool_input":{"command":"Decoy"},"cwd":"/tmp/proj"}'
# In default permission mode Claude also fires PermissionRequest for AskUserQuestion itself (seen live).
hook_direct claude PermissionRequest '{"session_id":"s-q","hook_event_name":"PermissionRequest","tool_name":"AskUserQuestion","tool_input":{"questions":[]},"cwd":"/tmp/proj"}'
reg "$P_Q" s-q waiting "input needed"
expect claude:s-q state waiting
expect claude:s-q reason question
expect_has claude:s-q detail "Which database should we use?"
expect_has claude:s-q detail Postgres
expect_has claude:s-q detail SQLite
expect_lacks claude:s-q detail Decoy
reg "$P_Q" s-q idle "" "" $(($(now_ms) + 1000))
expect claude:s-q state idle
hook_direct claude Stop '{"session_id":"s-q","hook_event_name":"Stop","cwd":"/tmp/proj"}'
reg "$P_Q" s-q waiting "input needed" "" $(($(now_ms) + 2000))
expect claude:s-q detail null
ok "claude waiting/input needed -> question, detail from the PreToolUse(AskUserQuestion) hook, cleared at turn end"

# A pending tool call isn't reliably in the transcript before its permission prompt (seen live on 2.1.317); the
# PermissionRequest hook carries it.
P_PERM=$(tty_proc 'sleep 300; :')
printf '{"type":"user","message":{"role":"user","content":"clean up"}}\n' | tx s-perm
hook_direct claude PermissionRequest '{"session_id":"s-perm","hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"rm -rf build","description":"clean"},"cwd":"/tmp/proj"}'
reg "$P_PERM" s-perm waiting "permission prompt"
expect claude:s-perm reason permission
expect_has claude:s-perm detail "rm -rf build"
P_EDIT=$(tty_proc 'sleep 300; :')
hook_direct claude PermissionRequest '{"session_id":"s-edit","hook_event_name":"PermissionRequest","tool_name":"Edit","tool_input":{"file_path":"/tmp/proj/src/main.rs","old_string":"a","new_string":"b"},"cwd":"/tmp/proj"}'
reg "$P_EDIT" s-edit waiting "permission prompt"
expect_has claude:s-edit detail /tmp/proj/src/main.rs
P_KIND=$(tty_proc 'sleep 300; :')
hook_direct claude PreToolUse '{"session_id":"s-kind","hook_event_name":"PreToolUse","tool_name":"AskUserQuestion","tool_input":{"questions":[{"question":"Wrong kind?","header":"K","options":[{"label":"A"}]}]},"cwd":"/tmp/proj"}'
reg "$P_KIND" s-kind waiting "permission prompt"
expect claude:s-kind detail null
P_OLD=$(tty_proc 'sleep 300; :')
hook_direct claude PermissionRequest '{"session_id":"s-old","hook_event_name":"PermissionRequest","tool_name":"Bash","tool_input":{"command":"stale"},"cwd":"/tmp/proj"}'
reg "$P_OLD" s-old waiting "permission prompt" "" $(($(now_ms) + 60000))
expect claude:s-old detail null
for wf in "sandbox request" "worker request" "goal proposal"; do
  P=$(tty_proc 'sleep 300; :')
  sid="s-${wf// /-}"
  reg "$P" "$sid" waiting "$wf"
  expect "claude:$sid" state waiting
  expect "claude:$sid" reason permission
done
P_DLG=$(tty_proc 'sleep 300; :')
reg "$P_DLG" s-dlg waiting "dialog open"
expect claude:s-dlg reason dialog
ok "claude permission reasons, Bash command and Edit path in detail, dialog"

P_IDLE=$(tty_proc 'sleep 300; :')
reg "$P_IDLE" s-idle idle
tx s-idle <<'EOF'
{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_i","name":"Bash","input":{"command":"cargo test"}}]}}
{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_i","content":"ok"}]}}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"All done,   tests\npass."}]}}
EOF
expect claude:s-idle state idle
expect claude:s-idle reason null
expect_has claude:s-idle detail "All done, tests pass."
P_LONG=$(tty_proc 'sleep 300; :')
reg "$P_LONG" s-long idle
python3 -c 'import json; print(json.dumps({"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"word " * 400}]}}))' | tx s-long
[ "$(field claude:s-long detail | wc -m | tr -d ' ')" -le 301 ] || fail "detail longer than 300 chars"
ok "claude idle -> idle, last assistant text, collapsed, <=300 chars"

P_DEAD=$(tty_proc 'sleep 300; :')
reg "$P_DEAD" s-dead busy "" "$(utc_start "$P_DEAD")"
kill "$P_DEAD"
wait_dead "$P_DEAD"
expect claude:s-dead state error
expect claude:s-dead reason crashed
P_REUSE=$(tty_proc 'sleep 300; :')
reg "$P_REUSE" s-reuse busy "" "Mon Jan  5 00:00:00 2026"
expect claude:s-reuse state error
expect claude:s-reuse reason crashed
ok "claude dead pid and reused pid -> error/crashed"

P_UNK=$(tty_proc 'sleep 300; :')
reg "$P_UNK" s-unk frobbing
expect claude:s-unk state unknown
expect claude:s-unk reason null
ok "claude unknown status -> unknown"

P_HL=$(bg_proc 'sleep 300; :')
reg "$P_HL" s-hl busy
expect claude:s-hl surface headless
expect claude:s-hl tty null
expect claude:s-hl state working
ok "claude without a tty -> headless"

ln -s /bin/sh "$T/bin/node"
printf 'sleep 300 & sleep 300; :\n' >"$T/bin/cotal"
P_COTAL=$(bg_proc "$T/bin/node $T/bin/cotal; :")
S_COTAL=""
for _ in $(seq 40); do
  S_COTAL=$(pgrep -P "$(pgrep -P "$P_COTAL" | head -1)" -x sleep | head -1 || true)
  [ -n "$S_COTAL" ] && break
  sleep 0.05
done
[ -n "$S_COTAL" ] || fail "cotal fixture did not start"
reg "$S_COTAL" s-cotal busy
expect claude:s-cotal state MISSING
S_COTAL2=$(pgrep -P "$(pgrep -P "$P_COTAL" | head -1)" -x sleep | grep -vx "$S_COTAL" | head -1)
[ -n "$S_COTAL2" ] || fail "cotal fixture's second sleep did not start"
reg "$S_COTAL2" s-cotal-reused busy "" "Mon Jan  5 00:00:00 2026"
expect claude:s-cotal-reused state error
expect claude:s-cotal-reused reason crashed
ok "claude session under a cotal ancestor (node …/cotal) -> excluded; a reused pid there -> crashed"

P_ST=$(tty_proc 'sleep 300; :')
reg "$P_ST" s-stall busy
printf '{"type":"user","message":{"role":"user","content":"go"}}\n' | tx s-stall
old_mtime "$HOME/.claude/projects/-tmp-proj/s-stall.jsonl"
P_CPU=$(tty_proc 'yes >/dev/null; :')
reg "$P_CPU" s-cpu busy
printf '{"type":"user","message":{"role":"user","content":"go"}}\n' | tx s-cpu
old_mtime "$HOME/.claude/projects/-tmp-proj/s-cpu.jsonl"
sleep 2
expect claude:s-stall state error
expect claude:s-stall reason stalled
expect_has claude:s-stall detail "no output"
expect claude:s-cpu state working
pkill -P "$P_CPU" yes || true
# status "shell": the turn ended while a background shell runs; the session resumes on its own.
P_SH=$(tty_proc 'sleep 300; :')
reg "$P_SH" s-shell shell
printf '{"type":"user","message":{"role":"user","content":"go"}}\n' | tx s-shell
old_mtime "$HOME/.claude/projects/-tmp-proj/s-shell.jsonl"
expect claude:s-shell state working
expect claude:s-shell reason null
expect claude:s-busy state working
ok "stalled: old transcript + idle CPU -> error/stalled; busy descendant CPU -> working; no transcript -> never stalled; shell -> working, never stalled"

hook_direct claude StopFailure '{"session_id":"s-busy","hook_event_name":"StopFailure","error":"rate_limit","transcript_path":"/nonexistent","cwd":"/tmp/proj"}'
expect claude:s-busy state error
expect claude:s-busy reason rate_limit
hook_direct claude StopFailure '{"session_id":"s-idle","hook_event_name":"StopFailure","error":"overloaded","cwd":"/tmp/proj"}'
expect claude:s-idle state error
expect claude:s-idle reason api_error
reg "$P_BUSY" s-busy idle "" "" $(($(now_ms) + 1000))
expect claude:s-busy state error
reg "$P_BUSY" s-busy busy "" "" $(($(now_ms) + 2000))
expect claude:s-busy state working
expect claude:s-busy reason null
hook_direct claude Stop '{"session_id":"s-busy","hook_event_name":"Stop","cwd":"/tmp/proj"}'
reg "$P_BUSY" s-busy idle "" "" $(($(now_ms) + 3000))
expect claude:s-busy state idle
hook_direct claude Stop '{"session_id":"s-idle","hook_event_name":"Stop","cwd":"/tmp/proj"}'
expect claude:s-idle state idle
ok "StopFailure -> rate_limit/api_error, survives idle, hidden while busy, cleared by the next successful Stop"

# A Claude session's failure, pending prompt and crash are independent (review 4).
P_FQ=$(tty_proc 'sleep 300; :')
reg "$P_FQ" s-fq idle
hook_direct claude StopFailure '{"session_id":"s-fq","hook_event_name":"StopFailure","error":"rate_limit","cwd":"/tmp/proj"}'
hook_direct claude PreToolUse '{"session_id":"s-fq","hook_event_name":"PreToolUse","tool_name":"AskUserQuestion","tool_input":{"questions":[{"question":"Retry now?","header":"R","options":[{"label":"Yes"}]}]},"cwd":"/tmp/proj"}'
reg "$P_FQ" s-fq waiting "input needed" "" $(($(now_ms) + 1000))
expect claude:s-fq state waiting
expect_has claude:s-fq detail "Retry now?"
reg "$P_FQ" s-fq idle "" "" $(($(now_ms) + 2000))
expect claude:s-fq state error
expect claude:s-fq reason rate_limit
P_FS=$(tty_proc 'sleep 300; :')
reg "$P_FS" s-fs idle
hook_direct claude StopFailure '{"session_id":"s-fs","hook_event_name":"StopFailure","error":"rate_limit","cwd":"/tmp/proj"}'
reg "$P_FS" s-fs shell "" "" $(($(now_ms) + 1000))
expect claude:s-fs state error
expect claude:s-fs reason rate_limit
for ev in question failure; do
  P=$(tty_proc 'sleep 300; :')
  reg "$P" "s-c$ev" waiting "input needed" "" 1793700000000
  if [ "$ev" = question ]; then
    hook_direct claude PreToolUse '{"session_id":"s-cquestion","hook_event_name":"PreToolUse","tool_name":"AskUserQuestion","tool_input":{"questions":[{"question":"Q?","header":"Q","options":[{"label":"A"}]}]},"cwd":"/tmp/proj"}'
  else
    hook_direct claude StopFailure '{"session_id":"s-cfailure","hook_event_name":"StopFailure","error":"overloaded","cwd":"/tmp/proj"}'
  fi
  kill "$P"
  wait_dead "$P"
  expect "claude:s-c$ev" reason crashed
  rm "$HOME/.claude/sessions/$P.json"
  expect "claude:s-c$ev" state error
  expect "claude:s-c$ev" reason crashed
done
ok "a question doesn't erase a stored failure; shell doesn't hide one; a crash after a newer hook record still outlives the sweep"

########## Light and acks ##########
new_home light
L_BUSY=$(tty_proc 'sleep 300; :')
reg "$L_BUSY" l-busy busy
L_IDLE=$(tty_proc 'sleep 300; :')
reg "$L_IDLE" l-idle idle
L_WAIT=$(tty_proc 'sleep 300; :')
reg "$L_WAIT" l-wait waiting "input needed"
L_DEAD=$(tty_proc 'sleep 300; :')
reg "$L_DEAD" l-dead busy "" "$(utc_start "$L_DEAD")"
L_HLDEAD=$(bg_proc 'sleep 300; :')
reg "$L_HLDEAD" l-hldead busy "" "$(utc_start "$L_HLDEAD")"
L_HL=$(bg_proc 'sleep 300; :')
reg "$L_HL" l-hl busy
L_UNK=$(tty_proc 'sleep 300; :')
reg "$L_UNK" l-unk frobbing
kill "$L_DEAD" "$L_HLDEAD"
wait_dead "$L_DEAD"
wait_dead "$L_HLDEAD"
expect_light red
"$BIN" ack claude:l-dead >/dev/null
expect claude:l-dead acked true
expect_light red
"$BIN" ack claude:l-hldead >/dev/null
expect_light blue
"$BIN" ack claude:l-wait >/dev/null
expect_light yellow
"$BIN" ack claude:l-idle >/dev/null
expect_light green
reg "$L_IDLE" l-idle waiting "permission prompt" "" $(($(now_ms) + 1000))
expect claude:l-idle acked false
expect_light blue
"$BIN" ack claude:l-idle >/dev/null
rm "$HOME/.claude/sessions/$L_BUSY.json"
expect_light green
rm "$HOME/.claude/sessions/$L_HL.json"
expect_light grey
L_AC=$(tty_proc 'sleep 300; :')
reg "$L_AC" l-ac idle "" "$(utc_start "$L_AC")"
"$BIN" ack claude:l-ac >/dev/null
expect claude:l-ac acked true
expect_light grey
kill "$L_AC"
wait_dead "$L_AC"
expect claude:l-ac state error
expect claude:l-ac acked false
expect_light red
if "$BIN" ack claude:nope >/dev/null 2>&1; then fail "ack of an unknown key exited 0"; fi
ok "light precedence red>blue>yellow>green>grey over unacked rows; headless only red; acks end with the episode; a crash after an ack is a new alert"

########## Crashes outlive Claude's sweep ##########
# Claude deletes dead sessions' registry files whenever any Claude session starts (measured 40 s after a kill -9).
new_home sweep
S_A=$(tty_proc 'sleep 300; :')
reg "$S_A" sw-a busy "" "" 1793700000000
kill "$S_A"
wait_dead "$S_A"
expect claude:sw-a reason crashed
rm "$HOME/.claude/sessions/$S_A.json"
expect claude:sw-a state error
expect claude:sw-a reason crashed
expect claude:sw-a since 1793700000000
expect claude:sw-a name name-sw-a
expect claude:sw-a cwd /tmp/proj
expect_light red
"$BIN" ack claude:sw-a >/dev/null
expect claude:sw-a state MISSING
expect_light grey
S_B=$(tty_proc 'sleep 300; :')
reg "$S_B" sw-b busy
kill "$S_B"
wait_dead "$S_B"
expect claude:sw-b reason crashed
"$BIN" ack claude:sw-b >/dev/null
expect claude:sw-b acked true
rm "$HOME/.claude/sessions/$S_B.json"
expect claude:sw-b state MISSING
S_C=$(tty_proc 'sleep 300; :')
reg "$S_C" sw-c busy
kill "$S_C"
wait_dead "$S_C"
expect claude:sw-c reason crashed
rm "$HOME/.claude/sessions/$S_C.json"
sleep 1.1 # procStart has one-second resolution
S_C2=$(tty_proc 'sleep 300; :')
reg "$S_C2" sw-c idle
expect claude:sw-c state idle
rm "$HOME/.claude/sessions/$S_C2.json"
expect claude:sw-c state MISSING
S_D=$(tty_proc 'sleep 300; :')
reg "$S_D" sw-d busy
kill "$S_D"
wait_dead "$S_D"
expect claude:sw-d reason crashed
# Holding sessions.lock cannot delay Claude crash acknowledgement or status cleanup.
python3 -c 'import fcntl, subprocess, sys
f = open(sys.argv[1], "a"); fcntl.flock(f, fcntl.LOCK_EX)
subprocess.run([sys.argv[2], "ack", "claude:sw-d"], capture_output=True, check=True)' \
  "$HOME/.local/state/pay-attention/sessions.lock" "$BIN"
rm "$HOME/.claude/sessions/$S_D.json"
expect claude:sw-d state MISSING
# Two registry files for one session (resumed before Claude swept the dead one): the newest process is the row, and an
# older live process never clears a newer crash.
S_E1=$(tty_proc 'sleep 300; :')
reg "$S_E1" sw-e idle
sleep 1.1 # procStart has one-second resolution
S_E2=$(tty_proc 'sleep 300; :')
reg "$S_E2" sw-e busy
kill "$S_E2"
wait_dead "$S_E2"
expect claude:sw-e reason crashed
rm "$HOME/.claude/sessions/$S_E1.json" "$HOME/.claude/sessions/$S_E2.json"
expect claude:sw-e reason crashed
sleep 1.1 # procStart has one-second resolution
S_E3=$(tty_proc 'sleep 300; :')
reg "$S_E3" sw-e idle
expect claude:sw-e state idle
rm "$HOME/.claude/sessions/$S_E3.json"
expect claude:sw-e state MISSING
# A crash file is private, and an invalid one (e.g. a writer killed mid-write) is replaced while the evidence remains.
S_F=$(tty_proc 'sleep 300; :')
reg "$S_F" sw-f busy
kill "$S_F"
wait_dead "$S_F"
expect claude:sw-f reason crashed
CF=$(ls "$HOME"/.local/state/pay-attention/crashes/claude-sw-f-*.json)
[ "$(stat -f %Lp "$CF")" = 600 ] || fail "crash file mode $(stat -f %Lp "$CF"), want 600"
printf '{' >"$CF"
expect claude:sw-f reason crashed
rm "$HOME/.claude/sessions/$S_F.json"
expect claude:sw-f reason crashed
"$BIN" ack claude:sw-f >/dev/null
expect claude:sw-f state MISSING
# A file whose name isn't exactly what its own valid contents give (here an older name without the pid, whose id ends
# in digits) is ignored, so it can't outrank or clear a real crash.
S_G=$(tty_proc 'sleep 300; :')
reg "$S_G" swg-999999999999 busy
kill "$S_G"
wait_dead "$S_G"
expect claude:swg-999999999999 reason crashed
G_SINCE=$(field claude:swg-999999999999 since)
G_CF=$(ls "$HOME"/.local/state/pay-attention/crashes/claude-swg-999999999999-*.json)
jq '.since = 1' "$G_CF" >"$HOME/.local/state/pay-attention/crashes/claude-swg-999999999999-1793700000.json"
expect claude:swg-999999999999 reason crashed
rm "$HOME/.claude/sessions/$S_G.json"
expect claude:swg-999999999999 reason crashed
expect claude:swg-999999999999 since "$G_SINCE"
"$BIN" ack claude:swg-999999999999 >/dev/null
expect claude:swg-999999999999 state MISSING
rm "$HOME/.local/state/pay-attention/crashes/claude-swg-999999999999-1793700000.json"
# An invalid crash entry that is a symlink is replaced itself; its target is never written.
S_H=$(tty_proc 'sleep 300; :')
reg "$S_H" sw-h busy
kill "$S_H"
wait_dead "$S_H"
expect claude:sw-h reason crashed
H_CF=$(ls "$HOME"/.local/state/pay-attention/crashes/claude-sw-h-*.json)
printf 'victim' >"$T/victim"
rm "$H_CF"
ln -s "$T/victim" "$H_CF"
expect claude:sw-h reason crashed
[ "$(cat "$T/victim")" = victim ] || fail "a crash write followed a symlink"
[ ! -L "$H_CF" ] && [ "$(stat -f %Lp "$H_CF")" = 600 ] || fail "an invalid crash symlink was not replaced by a private file"
rm "$HOME/.claude/sessions/$S_H.json"
"$BIN" ack claude:sw-h >/dev/null
expect claude:sw-h state MISSING
for d in sessions crashes; do
  [ -z "$(ls -A "$HOME/.local/state/pay-attention/$d" 2>/dev/null)" ] || fail "a crash outlived its ack or a successor ($d not empty)"
done
mkdir -p "$HOME/.local/state/pay-attention/sessions"
printf '{"session_id":"c1","harness":"codex","pid":null,"proc_start":null,"cwd":"/tmp/proj-c","transcript_path":null,"state":"waiting","reason":"permission","detail":"x","since":1793700000000}' \
  >"$HOME/.local/state/pay-attention/sessions/codex-c1.json"
expect codex:c1 state waiting
expect codex:c1 name proj-c
rm "$HOME/.local/state/pay-attention/sessions/codex-c1.json"
ok "a Claude crash outlives the sweep of its registry file until acked; an ack or a later process of the session ends it; codex records carry no name"

new_home ro
R_P=$(tty_proc 'sleep 300; :')
reg "$R_P" ro-a busy
expect claude:ro-a state working
[ ! -e "$HOME/.local/state/pay-attention" ] || fail "status created state for a live session"
ok "status leaves no state behind for live sessions"

########## Snapshot cost (the app snapshots every second) ##########
new_home cost
for i in 1 2 3 4 5; do
  P=$(tty_proc 'sleep 300; :')
  reg "$P" "c-$i" busy
  printf '{"type":"user","message":{"role":"user","content":"go"}}\n' | tx "c-$i"
done
python3 - "$REL" <<'EOF'
import resource, subprocess, sys
before = resource.getrusage(resource.RUSAGE_CHILDREN)
for _ in range(20):
    subprocess.run([sys.argv[1], "status", "--json"], check=True, capture_output=True)
after = resource.getrusage(resource.RUSAGE_CHILDREN)
ms = ((after.ru_utime - before.ru_utime) + (after.ru_stime - before.ru_stime)) * 1000 / 20
print(f"status cpu per call: {ms:.1f}ms")
if ms >= 10:
    sys.exit(f"FAIL: status --json costs {ms:.1f}ms CPU per call with 5 sessions (budget 10ms)")
EOF
ok "snapshot with 5 live sessions and fresh transcripts costs < 10 ms CPU"

########## Codex hooks ##########
new_home codex
ln -s /bin/sh "$T/bin/codex"
tty_proc "QDIR=$T/q/codex $T/bin/codex $T/loop.sh" >/dev/null
CX=""
for _ in $(seq 40); do
  CX=$(pgrep -f "^$T/bin/codex $T/loop.sh" | head -1 || true)
  [ -n "$CX" ] && break
  sleep 0.05
done
[ -n "$CX" ] || fail "fake codex did not start"
track "$CX"
RO="$T/rollout-cx1.jsonl"
cx() { # cx EVENT EXTRA_JSON [ENV_PREFIX]
  send codex "$T/q/codex" "$1" "$(jq -nc --arg ev "$1" --arg ro "$RO" --argjson x "$2" \
    '{session_id: ($x.session_id // "cx1"), hook_event_name: $ev, cwd: "/tmp/cxproj", model: "gpt", permission_mode: "default", transcript_path: $ro} + $x')" "${3:-}"
}
printf '{"type":"event_msg","payload":{"type":"task_started"}}\n' >"$RO"
cx SessionStart '{"source":"startup"}'
expect codex:cx1 state idle
expect codex:cx1 harness codex
expect codex:cx1 name cxproj
expect codex:cx1 cwd /tmp/cxproj
expect codex:cx1 pid "$CX"
expect codex:cx1 surface terminal
expect codex:cx1 tty "$(tty_of "$CX")"
cx UserPromptSubmit '{"prompt":"ship it","turn_id":"t1"}'
expect codex:cx1 state working
cx PreToolUse '{"tool_name":"exec_command","tool_input":{"cmd":"ls"},"turn_id":"t1"}'
expect codex:cx1 state working
cx PermissionRequest '{"tool_name":"Bash","tool_input":{"command":"git push --force"},"turn_id":"t1"}'
expect codex:cx1 state waiting
expect codex:cx1 reason permission
expect_has codex:cx1 detail "git push --force"
cx PostToolUse '{"tool_name":"Bash","tool_input":{"command":"git push --force"},"tool_response":"ok","turn_id":"t1"}'
expect codex:cx1 state working
cx PermissionRequest '{"tool_name":"shell","tool_input":{"command":["bash","-lc","make deploy"]},"turn_id":"t1"}'
expect_has codex:cx1 detail "make deploy"
cx PostToolUse '{"tool_name":"shell","tool_input":{},"turn_id":"t1"}'
cx PreToolUse '{"tool_name":"request_user_input","tool_input":{"questions":[{"id":"q1","header":"Ship","question":"Ship it now?","options":[{"label":"Yes","description":"a"},{"label":"Later","description":"b"}]}]},"turn_id":"t1"}'
expect codex:cx1 state waiting
expect codex:cx1 reason question
expect_has codex:cx1 detail "Ship it now?"
expect_has codex:cx1 detail Later
cx PostToolUse '{"tool_name":"request_user_input","tool_input":{},"turn_id":"t1"}'
printf '{"type":"event_msg","payload":{"type":"task_started"}}\n{"type":"event_msg","payload":{"type":"task_complete"}}\n' >"$RO"
cx Stop '{"last_assistant_message":"Done.  Pushed to main.","stop_hook_active":false,"turn_id":"t1"}'
expect codex:cx1 state idle
expect_has codex:cx1 detail "Done. Pushed to main."
cx UserPromptSubmit '{"prompt":"again","turn_id":"t2"}'
printf '{"type":"event_msg","payload":{"type":"error","message":"old"}}\n{"type":"event_msg","payload":{"type":"task_started"}}\n{"type":"event_msg","payload":{"type":"error","message":"stream disconnected before completion"}}\n' >"$RO"
cx Stop '{"last_assistant_message":"","turn_id":"t2"}'
expect codex:cx1 state error
expect codex:cx1 reason api_error
cx UserPromptSubmit '{"prompt":"retry","turn_id":"t3"}'
expect codex:cx1 state working
printf '{"type":"event_msg","payload":{"type":"error","message":"old"}}\n{"type":"event_msg","payload":{"type":"task_started"}}\n' >"$RO"
cx Interrupt '{"turn_id":"t3"}'
expect codex:cx1 state idle
cx SessionEnd '{"reason":"exit"}'
expect codex:cx1 state MISSING
cx SessionStart '{"session_id":"cx-cotal","source":"startup"}' "COTAL_NAME=mesh-agent"
expect codex:cx-cotal state MISSING
[ -z "$(ls -A "$HOME/.local/state/pay-attention/sessions" 2>/dev/null)" ] || fail "COTAL_NAME hook or SessionEnd left a session file"
cx SessionStart '{"session_id":"cx2","source":"startup"}'
expect codex:cx2 state idle
printf '{"type":"event_msg","payload":{"type":"task_started"}}\n{"type":"event_msg","payload":{"type":"error","message":"boom"}}\n' >"$RO"
cx Stop '{"session_id":"cx2","last_assistant_message":"","turn_id":"t9"}'
expect codex:cx2 reason api_error
"$BIN" ack codex:cx2 >/dev/null
expect codex:cx2 acked true
pkill -P "$CX" 2>/dev/null || true
kill "$CX"
wait_dead "$CX"
expect codex:cx2 state error
expect codex:cx2 reason crashed
expect codex:cx2 acked false
"$BIN" ack codex:cx2 >/dev/null
expect codex:cx2 state MISSING
ok "codex hook mapping, detail, rollout error scan, COTAL_NAME skip, SessionEnd delete, crash on agent exit (a new alert even after an acked error), gone once the crash is acked"

########## Hermes hooks ##########
new_home hermes
ln -s /bin/sh "$T/bin/python3"
cp "$T/loop.sh" "$T/bin/hermes"
tty_proc "QDIR=$T/q/hermes $T/bin/python3 $T/bin/hermes" >/dev/null
HX=""
for _ in $(seq 40); do
  HX=$(pgrep -f "^$T/bin/python3 $T/bin/hermes" | head -1 || true)
  [ -n "$HX" ] && break
  sleep 0.05
done
[ -n "$HX" ] || fail "fake hermes did not start"
track "$HX"
hx() { # hx EVENT EXTRA_JSON
  send hermes "$T/q/hermes" "$1" "$(jq -nc --arg ev "$1" --argjson x "$2" \
    '{hook_event_name: $ev, tool_name: null, tool_input: null, session_id: "hx1", cwd: "/tmp/hxproj", profile: "default", extra: $x}')"
}
hx on_session_start '{}'
expect hermes:hx1 state idle
expect hermes:hx1 name hxproj
expect hermes:hx1 pid "$HX"
expect hermes:hx1 surface terminal
hx pre_llm_call '{"user_message":"refactor","is_first_turn":true,"model":"m","platform":"cli"}'
expect hermes:hx1 state working
hx on_human_input_request '{"kind":"clarify","request_id":"r1","prompt":"Which branch should I use?","platform":"cli"}'
expect hermes:hx1 state waiting
expect hermes:hx1 reason question
expect_has hermes:hx1 detail "Which branch should I use?"
hx on_human_input_resolved '{"kind":"clarify","request_id":"r1","prompt":"Which branch should I use?","outcome":"answered"}'
expect hermes:hx1 state working
hx on_human_input_request '{"kind":"sudo","request_id":"r2","prompt":"Password:"}'
expect hermes:hx1 reason permission
hx on_human_input_resolved '{"kind":"sudo","request_id":"r2","outcome":"answered"}'
hx pre_approval_request '{"command":"rm -rf /tmp/x"}'
expect hermes:hx1 state waiting
expect hermes:hx1 reason permission
hx post_approval_response '{"decision":"approved"}'
expect hermes:hx1 state working
hx post_llm_call '{"user_message":"refactor","assistant_response":"Finished the refactor.","model":"m","platform":"cli"}'
expect hermes:hx1 state idle
expect_has hermes:hx1 detail "Finished the refactor."
hx on_session_end '{"completed":true,"interrupted":false,"model":"m","platform":"cli"}'
expect hermes:hx1 state idle
expect_has hermes:hx1 detail "Finished the refactor."
hx pre_llm_call '{"user_message":"more","is_first_turn":false}'
hx on_session_end '{"completed":false,"interrupted":true}'
expect hermes:hx1 state idle
hx pre_llm_call '{"user_message":"again","is_first_turn":false}'
hx on_session_end '{"completed":false,"interrupted":false}'
expect hermes:hx1 state error
expect hermes:hx1 reason api_error
hx on_session_finalize '{"platform":"cli"}'
expect hermes:hx1 state MISSING
ok "hermes hook mapping (argv[1] agent detection), detail, per-turn on_session_end outcome, finalize removes"

new_home hstall
printf 'stalled_after_secs = 60\n' >"$HOME/.config/pay-attention/config.toml"
hook_direct hermes pre_llm_call '{"hook_event_name":"pre_llm_call","session_id":"hs1","cwd":"/tmp/hs","extra":{}}'
expect hermes:hs1 state working
old_mtime "$HOME/.local/state/pay-attention/sessions/hermes-hs1.json"
expect hermes:hs1 state error
expect hermes:hs1 reason stalled
"$BIN" ack hermes:hs1 >/dev/null
expect hermes:hs1 acked true
hook_direct hermes pre_llm_call '{"hook_event_name":"pre_llm_call","session_id":"hs1","cwd":"/tmp/hs","extra":{}}'
expect hermes:hs1 state working
sleep 1
old_mtime "$HOME/.local/state/pay-attention/sessions/hermes-hs1.json"
expect hermes:hs1 reason stalled
expect hermes:hs1 acked false
hook_direct hermes post_llm_call '{"hook_event_name":"post_llm_call","session_id":"hs1","cwd":"/tmp/hs","extra":{"assistant_response":"ok"}}'
expect hermes:hs1 state idle
hook_direct hermes pre_llm_call '{"hook_event_name":"pre_llm_call","session_id":"cron1","cwd":"/tmp/c","extra":{"platform":"cron"}}'
hook_direct hermes on_session_end '{"hook_event_name":"on_session_end","session_id":"tg1","cwd":"/tmp/c","extra":{"completed":true,"interrupted":false,"platform":"telegram"}}'
hook_direct hermes pre_approval_request '{"hook_event_name":"pre_approval_request","session_id":"gw1","cwd":"/tmp/c","extra":{"surface":"gateway","command":"ls"}}'
expect hermes:cron1 state MISSING
expect hermes:tg1 state MISSING
expect hermes:gw1 state MISSING
hook_direct hermes pre_llm_call '{"hook_event_name":"pre_llm_call","session_id":"cli1","cwd":"/tmp/c","extra":{"platform":"cli"}}'
expect hermes:cli1 state working
ok "hermes: no hook event for stalled_after while working -> stalled (hooks are its heartbeat); a second stall is a new alert; only platform cli is tracked"

########## Hook robustness and latency ##########
new_home robust
SD="$HOME/.local/state/pay-attention/sessions"
for input in 'not json' '' '{"hook_event_name":"Stop"}' '[1,2,3]'; do
  hook_direct codex Stop "$input"
done
hook_direct codex Bogus '{"session_id":"z1","cwd":"/tmp"}'
hook_direct bogus Stop '{"session_id":"z2","cwd":"/tmp"}'
hook_direct claude Stop '{"session_id":"z3","cwd":"/tmp"}'
[ -z "$(ls -A "$SD" 2>/dev/null)" ] || fail "garbage, unknown event or unknown harness wrote a session file"
hook_direct codex SessionStart '{"session_id":"z4","cwd":"/tmp/zz"}'
expect codex:z4 surface headless
expect codex:z4 pid null
chmod 500 "$SD"
hook_direct codex UserPromptSubmit '{"session_id":"z5","cwd":"/tmp"}'
"$BIN" status --json >/dev/null || fail "status failed with a read-only state dir (status must never write)"
chmod 700 "$SD"
python3 - "$REL" <<'EOF'
import subprocess, sys, time
payload = b'{"session_id":"lat","hook_event_name":"UserPromptSubmit","cwd":"/tmp/lat","prompt":"x"}'
times = []
for _ in range(200):
    t = time.perf_counter()
    r = subprocess.run([sys.argv[1], "hook", "codex", "UserPromptSubmit"], input=payload, capture_output=True)
    times.append((time.perf_counter() - t) * 1000)
    if r.returncode != 0 or r.stdout or r.stderr:
        sys.exit(f"FAIL: latency run exited {r.returncode} or printed output")
times.sort()
p99 = times[int(len(times) * 0.99) - 1]
print(f"hook latency p50={times[len(times) // 2]:.1f}ms p99={p99:.1f}ms")
if p99 >= 30:
    sys.exit(f"FAIL: hook p99 {p99:.1f}ms >= 30ms")
EOF
ok "hooks: garbage/unknown no-ops, unwritable dir, headless without agent ancestor, p99 < 30 ms"

########## Installer ##########
new_home inst
cat >"$HOME/.claude/settings.json" <<'EOF'
{
  "env": {"FOO": "bar"},
  "permissions": {"allow": ["Bash(ls:*)"]},
  "hooks": {
    "PreToolUse": [
      {"matcher": "Bash", "hooks": [{"type": "command", "command": "/usr/local/libexec/gatekeeper/gatekeeper hook pre-tool-use", "timeout": 1500}]},
      {"matcher": "AskUserQuestion", "hooks": [{"type": "command", "command": "/usr/local/libexec/gatekeeper/gatekeeper hook pre-tool-use", "timeout": 1500}]}
    ],
    "PermissionRequest": [{"hooks": [{"type": "command", "command": "/usr/local/libexec/gatekeeper/gatekeeper hook permission-request", "timeout": 1500}]}],
    "Stop": [{"hooks": [{"type": "command", "command": "/usr/local/libexec/gatekeeper/gatekeeper hook stop", "timeout": 1500}]}]
  },
  "statusLine": {"type": "command", "command": "x"},
  "model": "opus"
}
EOF
cat >"$HOME/.codex/hooks.json" <<'EOF'
{
  "hooks": {
    "PreCompact": [{"hooks": [{"type": "command", "command": "/x/compactor-hook PreCompact", "timeout": 120}]}],
    "SessionStart": [{"matcher": "^compact$", "hooks": [{"type": "command", "command": "/x/compactor-hook CompactSessionStart", "timeout": 15, "additionalContextLimit": 0}]}]
  }
}
EOF
cat >"$HOME/.codex/config.toml" <<'EOF'
model = "gpt-6"
# keep this comment
notify = ["/x/y", "turn-ended"]

[features]
# feature comment
web_search = true

[projects."/Users/x"]
trust_level = "trusted"
EOF
cat >"$HOME/.hermes/config.yaml" <<'EOF'
model:
  default: x
# a comment
agent:
  max_turns: 90
EOF
FILES=("$HOME/.claude/settings.json" "$HOME/.codex/hooks.json")
UNTOUCHED=("$HOME/.codex/config.toml" "$HOME/.hermes/config.yaml")
mkdir -p "$T/orig"
for f in "${FILES[@]}" "${UNTOUCHED[@]}"; do cp "$f" "$T/orig/$(basename "$f")"; done
sums() { for f in "${FILES[@]}" "${UNTOUCHED[@]}"; do shasum "$f"; done; }
before=$(sums)
"$BIN" install-hooks --dry-run >/dev/null 2>&1
[ "$(sums)" = "$before" ] || fail "install-hooks --dry-run changed a file"
[ -z "$(ls "$HOME"/.claude/*.bak-pay-attention-* "$HOME"/.codex/*.bak-pay-attention-* "$HOME"/.hermes/*.bak-pay-attention-* 2>/dev/null)" ] ||
  fail "--dry-run wrote a backup"
"$BIN" install-hooks >"$T/inst.out" 2>&1
S="$HOME/.claude/settings.json"
[ "$(jq -r '.hooks.StopFailure[].hooks[].command' "$S")" = "$BIN hook claude StopFailure" ] ||
  fail "settings.json StopFailure command is not '$BIN hook claude StopFailure'"
[ "$(jq -c '.hooks.Stop[0]' "$S")" = "$(jq -c '.hooks.Stop[0]' "$T/orig/settings.json")" ] &&
  [ "$(jq '.hooks.Stop | length' "$S")" = 2 ] &&
  [ "$(jq -r '.hooks.Stop[1].hooks[0].command' "$S")" = "$BIN hook claude Stop" ] ||
  fail "settings.json Stop: gatekeeper's group must stay first, with '$BIN hook claude Stop' appended after it"
[ "$(jq -c '.hooks.PreToolUse[:2]' "$S")" = "$(jq -c '.hooks.PreToolUse' "$T/orig/settings.json")" ] &&
  [ "$(jq '.hooks.PreToolUse | length' "$S")" = 3 ] &&
  [ "$(jq -r '.hooks.PreToolUse[2] | .matcher + " " + .hooks[0].command' "$S")" = "AskUserQuestion $BIN hook claude PreToolUse" ] ||
  fail "settings.json PreToolUse: gatekeeper's groups must stay first, with an AskUserQuestion group running '$BIN hook claude PreToolUse' after them"
[ "$(jq -c '.hooks.PermissionRequest[0]' "$S")" = "$(jq -c '.hooks.PermissionRequest[0]' "$T/orig/settings.json")" ] &&
  [ "$(jq '.hooks.PermissionRequest | length' "$S")" = 2 ] &&
  [ "$(jq -r '.hooks.PermissionRequest[1].hooks[0].command' "$S")" = "$BIN hook claude PermissionRequest" ] ||
  fail "settings.json PermissionRequest: gatekeeper's group must stay first, with '$BIN hook claude PermissionRequest' after it"
[ "$(jq '[.. | .command? // empty | select(type == "string" and contains("gatekeeper"))] | length' "$S")" = 4 ] ||
  fail "gatekeeper entries changed"
[ "$(jq -S 'del(.hooks.StopFailure) | .hooks.Stop |= .[:1] | .hooks.PreToolUse |= .[:2] | .hooks.PermissionRequest |= .[:1]' "$S")" = "$(jq -S . "$T/orig/settings.json")" ] ||
  fail "install changed settings.json beyond adding the PreToolUse, PermissionRequest, StopFailure and Stop hooks"
H="$HOME/.codex/hooks.json"
for ev in SessionStart UserPromptSubmit PreToolUse PostToolUse PermissionRequest Stop Interrupt SessionEnd; do
  [ "$(jq --arg c "$BIN hook codex $ev" --arg e "$ev" '[.hooks[$e][] | select(.matcher == null) | .hooks[] | select(.command == $c)] | length' "$H")" = 1 ] ||
    fail "codex hooks.json lacks exactly one unmatched '$BIN hook codex $ev' group"
done
[ "$(jq '[.. | .command? // empty | select(type == "string" and contains("compactor"))] | length' "$H")" = 2 ] ||
  fail "compactor entries changed"
[ "$(jq -c '.hooks.SessionStart[0]' "$H")" = "$(jq -c '.hooks.SessionStart[0]' "$T/orig/hooks.json")" ] ||
  fail "compactor's SessionStart group was modified"
for f in "${UNTOUCHED[@]}"; do
  cmp -s "$f" "$T/orig/$(basename "$f")" || fail "install modified $f; it must only print instructions for it"
  [ -z "$(ls "$f".bak-pay-attention-* 2>/dev/null)" ] || fail "install backed up $f, which it must not change"
done
for ev in on_session_start pre_llm_call post_llm_call on_human_input_request on_human_input_resolved pre_approval_request \
  post_approval_response on_session_end on_session_finalize; do
  grep -qF "$BIN hook hermes $ev" "$T/inst.out" || fail "install did not print the hermes hook block entry for $ev"
done
grep -qF 'hooks = true' "$T/inst.out" || fail "install did not print how to enable codex hooks while [features] hooks is off"
for f in "${FILES[@]}"; do
  n=$( (ls "$f".bak-pay-attention-* 2>/dev/null || true) | wc -l | tr -d ' ')
  [ "$n" = 1 ] || fail "$f has $n backups, want 1"
  cmp -s "$f".bak-pay-attention-* "$T/orig/$(basename "$f")" || fail "backup of $f differs from the original"
done
once=$(sums)
"$BIN" install-hooks >/dev/null 2>&1
[ "$(sums)" = "$once" ] || fail "second install-hooks changed files"
for f in "${FILES[@]}"; do
  [ "$(ls "$f".bak-pay-attention-* | wc -l | tr -d ' ')" = 1 ] || fail "second install made another backup of $f"
  cp "$f" "$T/installed-$(basename "$f")"
done
sleep 1
"$BIN" uninstall-hooks >/dev/null 2>&1
for f in "${FILES[@]}"; do
  [ "$(ls "$f".bak-pay-attention-* | wc -l | tr -d ' ')" = 2 ] || fail "uninstall did not make a fresh backup of $f"
  newest=$(ls "$f".bak-pay-attention-* | sort | tail -1)
  cmp -s "$newest" "$T/installed-$(basename "$f")" || fail "uninstall's backup of $f is not the installed version"
done
[ "$(jq -S . "$S")" = "$(jq -S . "$T/orig/settings.json")" ] || fail "uninstall: settings.json not JSON-equal to the original"
[ "$(jq -S . "$H")" = "$(jq -S . "$T/orig/hooks.json")" ] || fail "uninstall: hooks.json not JSON-equal to the original"
for f in "${UNTOUCHED[@]}"; do cmp -s "$f" "$T/orig/$(basename "$f")" || fail "uninstall modified $f"; done
ok "installer: dry run, merge beside gatekeeper/compactor, a fresh backup per write, idempotent, clean uninstall; codex config and hermes only get printed instructions"

new_home inst2
rmdir "$HOME/.hermes"
printf '{\n  "model": "opus"\n}\n' >"$HOME/.claude/settings.json"
printf '[features]\nhooks = true\n' >"$HOME/.codex/config.toml"
cp "$HOME/.claude/settings.json" "$T/orig/s2.json"
cp "$HOME/.codex/config.toml" "$T/orig/c2.toml"
rc=0
"$BIN" install-hooks >"$T/inst2.out" 2>&1 || rc=$?
[ "$rc" = 0 ] || fail "install-hooks exited $rc"
[ -f "$HOME/.codex/hooks.json" ] || fail "install did not create a missing codex hooks.json"
[ ! -e "$HOME/.hermes" ] || fail "install created ~/.hermes"
! grep -qF 'hook hermes' "$T/inst2.out" || fail "install printed hermes hooks although ~/.hermes does not exist"
! grep -qF 'hooks = true' "$T/inst2.out" || fail "install asked to enable codex hooks that are already on"
"$BIN" uninstall-hooks >/dev/null 2>&1
[ "$(jq -S . "$HOME/.claude/settings.json")" = "$(jq -S . "$T/orig/s2.json")" ] || fail "uninstall left hooks in a settings.json that had none"
cmp -s "$HOME/.codex/config.toml" "$T/orig/c2.toml" || fail "uninstall modified config.toml"
ok "installer: no ~/.hermes -> nothing printed for it; codex hooks already on -> no notice; empty hooks object removed"

new_home inst4
rmdir "$HOME/.hermes"
rm -r "$HOME/.codex"
"$BIN" install-hooks >"$T/inst4.out" 2>&1 || fail "install-hooks failed with only ~/.claude"
[ ! -e "$HOME/.codex" ] || fail "install created ~/.codex although Codex is not installed"
! grep -qi 'codex' "$T/inst4.out" || fail "install mentioned Codex although ~/.codex does not exist"
grep -qF "$BIN hook claude Stop" "$HOME/.claude/settings.json" || fail "install skipped Claude"
"$BIN" uninstall-hooks >/dev/null 2>&1
[ ! -e "$HOME/.codex" ] || fail "uninstall created ~/.codex"
ok "installer: no ~/.codex -> no codex hooks.json and no codex notice"

new_home inst3
mkdir -p "$T/dotfiles" "$T/My Apps"
printf '{\n  "model": "opus"\n}\n' >"$T/dotfiles/settings.json"
cp "$T/dotfiles/settings.json" "$T/orig/s3.json"
ln -s "$T/dotfiles/settings.json" "$HOME/.claude/settings.json"
chmod 640 "$T/dotfiles/settings.json"
printf '{\n  "hooks": {}\n}\n' >"$HOME/.codex/hooks.json"
chmod 600 "$HOME/.codex/hooks.json"
printf 'features = "on"\n' >"$HOME/.codex/config.toml"
cp "$HOME/.codex/config.toml" "$T/orig/c3.toml"
cp "$BIN" "$T/My Apps/pay-attention"
"$T/My Apps/pay-attention" install-hooks >"$T/inst3.out" 2>&1
[ -L "$HOME/.claude/settings.json" ] || fail "install replaced the settings.json symlink with a file"
CMD=$(jq -r '.hooks.StopFailure[0].hooks[0].command' "$T/dotfiles/settings.json")
python3 -c 'import shlex, sys; assert shlex.split(sys.argv[1]) == [sys.argv[2], "hook", "claude", "StopFailure"], shlex.split(sys.argv[1])' \
  "$CMD" "$T/My Apps/pay-attention" || fail "hook command for a binary path with a space does not split back to that path"
sh -c "$CMD" </dev/null >/dev/null 2>&1 || fail "installed hook command for a path with a space does not run"
[ "$(stat -f %Lp "$T/dotfiles/settings.json")" = 640 ] || fail "install changed settings.json's mode"
[ "$(stat -f %Lp "$HOME/.codex/hooks.json")" = 600 ] || fail "install changed hooks.json's mode"
cmp -s "$HOME/.codex/config.toml" "$T/orig/c3.toml" || fail "install edited a config.toml whose features is not a table"
grep -qF 'hooks = true' "$T/inst3.out" || fail "install did not print the codex hooks notice for a non-table features"
"$BIN" install-hooks >/dev/null 2>&1
[ "$(jq '[.hooks.PreToolUse[].hooks[], .hooks.PermissionRequest[].hooks[], .hooks.StopFailure[].hooks[], .hooks.Stop[].hooks[]] | length' "$T/dotfiles/settings.json")" = 4 ] ||
  fail "reinstall from a new path duplicated the Claude hooks"
[ "$(jq -r '.hooks.StopFailure[0].hooks[0].command' "$T/dotfiles/settings.json")" = "$BIN hook claude StopFailure" ] ||
  fail "reinstall did not move the Claude hook to the new binary path"
[ "$(jq '[.hooks[][] | .hooks[]] | length' "$HOME/.codex/hooks.json")" = 8 ] || fail "reinstall from a new path duplicated codex hooks"
"$BIN" uninstall-hooks >/dev/null 2>&1
[ -L "$HOME/.claude/settings.json" ] || fail "uninstall replaced the settings.json symlink with a file"
[ "$(jq -S . "$T/dotfiles/settings.json")" = "$(jq -S . "$T/orig/s3.json")" ] || fail "uninstall through a symlink not JSON-equal"
[ "$(stat -f %Lp "$T/dotfiles/settings.json")" = 640 ] || fail "uninstall changed settings.json's mode"
[ "$(stat -f %Lp "$HOME/.codex/hooks.json")" = 600 ] || fail "uninstall changed hooks.json's mode"
ok "installer: writes through symlinks, keeps modes, quotes paths with spaces, reinstall from a new path replaces, non-table [features] left alone"

########## Escalation schedule ##########
new_home sched
check_sched() { # check_sched NAME PYTHON_ASSERTS ARGS...
  local name=$1 asserts=$2
  shift 2
  "$BIN" schedule "$@" >"$T/sched.out" || fail "schedule $* exited nonzero"
  python3 - "$T/sched.out" "$asserts" <<'EOF' || fail "schedule $name"
import re, sys
rings = []
for line in open(sys.argv[1]):
    m = re.fullmatch(r"t=(\d+\.\d\d) vol=(\d\.\d\d)\n?", line)
    if not m:
        sys.exit(f"bad line: {line!r}")
    rings.append((float(m[1]), float(m[2])))
t = [r[0] for r in rings]
v = [r[1] for r in rings]
gaps = [b - a for a, b in zip(t, t[1:])]
exec(sys.argv[2])
EOF
}
check_sched blue '
assert t[0] == 0 and v[0] == 0.30, rings[:2]
assert all(g > 0 for g in gaps) and all(b <= a + 0.011 for a, b in zip(gaps, gaps[1:])), gaps
assert abs(gaps[0] - 60) < 0.011 and all(g >= 9.99 for g in gaps) and all(abs(g - 10) < 0.011 for a, g in zip(t, gaps) if a >= 300), gaps
assert all(b >= a for a, b in zip(v, v[1:])) and max(v) == 1.00 and all(x == 1.00 for a, x in zip(t, v) if a >= 300), v
assert 25 <= len(t) <= 45 and t[-1] < 600, len(t)
' --color blue --minutes 10 --start 12:00
check_sched red 'assert t[0] == 0 and len(t) >= 25, t' --color red --minutes 10 --start 12:00
check_sched yellow 'assert len(t) == 3 and t[0] == 2.00, t' --color yellow --minutes 10 --start 12:00
check_sched dnd-begins 'assert t and all(x < 120 for x in t), t' --color blue --minutes 10 --start 22:58
check_sched dnd-inside 'assert t == [], t' --color blue --minutes 10 --start 23:30
check_sched dnd-ends 'assert t[0] == 120.00 and abs(v[0] - 0.58) < 0.011 and len(t) >= 2, rings[:3]' --color blue --minutes 5 --start 07:58
printf 'dnd = ""\n[escalation]\nstart_interval_secs = 30\nyellow_max_rings = 1\n' >"$HOME/.config/pay-attention/config.toml"
check_sched config 'assert abs(gaps[0] - 30) < 0.011, gaps' --color blue --minutes 2 --start 23:30
check_sched config-yellow 'assert len(t) == 1, t' --color yellow --minutes 10 --start 12:00
ok "schedule: ramp 60s->10s and 0.30->1.00 over 5 min, yellow 3 rings, DND drop + catch-up at window end, config honored"

########## Stats ##########
new_home stats
mkdir -p "$HOME/.local/state/pay-attention"
python3 - "$HOME/.local/state/pay-attention/events.jsonl" <<'EOF'
import json, sys
from datetime import datetime, timezone
def ms(s):
    return int(datetime.strptime(s, "%Y-%m-%d %H:%M").replace(tzinfo=timezone.utc).timestamp() * 1000)
ev = [
    ("2026-10-13 23:30", "claude:d", "D", "working", "idle"),
    ("2026-10-14 00:30", "claude:d", "D", "idle", "working"),
    ("2026-10-14 10:00", "claude:a", "A", "working", "waiting"),
    ("2026-10-14 10:05", "claude:a", "A", "waiting", "working"),
    ("2026-10-14 11:00", "codex:b", "B", "working", "idle"),
    ("2026-10-14 11:30", "codex:b", "B", "idle", "gone"),
    ("2026-10-14 12:00", "hermes:c", "C", "working", "error"),
    ("2026-10-14 12:10", "hermes:c", "C", "error", "working"),
    ("2026-10-14 23:00", "claude:a", "A", "working", "waiting"),
    ("2026-10-15 01:00", "claude:a", "A", "waiting", "working"),
]
with open(sys.argv[1], "w") as f:
    for ts, key, name, a, b in ev:
        f.write(json.dumps({"ts": ms(ts), "key": key, "name": name, "from": a, "to": b}) + "\n")
EOF
"$BIN" stats --json --day 2026-10-14 >"$T/stats.json"
python3 - "$T/stats.json" <<'EOF' || fail "stats totals"
import json, sys
s = json.load(open(sys.argv[1]))
m = 60_000
assert s["by_color"] == {"blue": 65 * m, "yellow": 60 * m, "red": 10 * m}, s["by_color"]
assert s["total_ms"] == 135 * m, s["total_ms"]
got = [(r["key"], r["name"], r["ms"]) for r in s["sessions"]]
assert got[0] == ("claude:a", "A", 65 * m) and got[-1] == ("hermes:c", "C", 10 * m), got
assert sorted(got[1:3]) == [("claude:d", "D", 30 * m), ("codex:b", "B", 30 * m)], got
EOF
ok "stats: per-color and per-session wait time, clipped to the day, gone ends an interval"

"$BIN" status >/dev/null
echo "ALL CHECKS PASSED"
