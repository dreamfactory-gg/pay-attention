#!/usr/bin/env bash
# End-to-end items 9 (DND, phone) and 11 (idle CPU and memory), against the release app. Its dot shows in the menu bar.
# Fixture Claude sessions under a temp HOME: the real ~/.claude, Terminal tabs and sessions are never touched.
# Every sound is a silent WAV. The alert is a crash, which has no tty, so the app never queries Terminal (no
# Automation prompt); the blue-in-a-tab variant of item 9 runs with item 3, after you allow Automation.
# E2E_APP_IDLE_SECS shortens item 11's 600 s window for a smoke run.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"
IDLE=${E2E_APP_IDLE_SECS:-600}
# Two steps: a failed mktemp then stops the script, instead of leaving `cd ""` in the current directory (which cleanup
# would delete).
T=$(mktemp -d "${TMPDIR:-/tmp}/pa-e2e-app.XXXXXX")
T=$(cd "$T" && pwd -P)
STATE="$T/home/.local/state/pay-attention"
mkdir -p "$T/home/.claude/sessions" "$T/home/.config/pay-attention"
# shellcheck source=scripts/fixtures.sh
. "$ROOT/scripts/fixtures.sh"
# Cleanup stops only what runs in or from $T: the fixtures start there, and the app is a copy of the bundle under it.
# If the process listing fails, $T is kept so nothing it holds disappears under a live process.
track() { :; }
cleanup() {
  if stop_dir "$T"; then rm -rf "$T"; else echo "cleanup: could not list processes; kept $T" >&2; fi
}
trap cleanup EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }
ok() { echo "ok: $*"; }

(cd app && npm ci --silent && npx tauri build --bundles app) >"$T/build.log" 2>&1 || {
  tail -40 "$T/build.log"
  fail "app build"
}
cp -R "$ROOT/target/release/bundle/macos/Pay Attention.app" "$T/"
APPDIR="$T/Pay Attention.app"
EXE="$APPDIR/Contents/MacOS/pay-attention-app"
export HOME="$T/home"

python3 -c 'import sys, wave
w = wave.open(sys.argv[1], "wb"); w.setnchannels(1); w.setsampwidth(2); w.setframerate(22050)
w.writeframes(b"\0\0" * 4410); w.close()' "$T/silent.wav"
printf '#!/bin/sh\necho "$*" >>"%s/phone.log"\n' "$T" >"$T/phone.sh"
chmod +x "$T/phone.sh"
config() { # config DND
  printf '%s\n' "dnd = \"$1\"" 'away_after_secs = 0' 'phone_after_secs = 5' \
    "phone_command = [\"$T/phone.sh\", \"{name}\", \"{state}\"]" '[sounds]' \
    "blue = \"$T/silent.wav\"" "yellow = \"$T/silent.wav\"" "red = \"$T/silent.wav\"" >"$T/config.new"
  mv "$T/config.new" "$HOME/.config/pay-attention/config.toml"
}
count() { awk -v p="\"type\":\"$1\"" 'index($0, p) { n++ } END { print n + 0 }' "$STATE/rings.jsonl" 2>/dev/null || echo 0; }
DND_NOW="$(date -v-1H +%H:%M)-$(date -v+1H +%H:%M)"

for i in 1 2 3 4 5; do reg "$(cd "$T" && tty_proc 'sleep 900; :')" "busy-$i" busy; done
P=$(cd "$T" && tty_proc 'sleep 900; :')
reg "$P" crash busy
pkill -P "$P" || true
kill "$P" 2>/dev/null || true
while kill -0 "$P" 2>/dev/null; do sleep 0.05; done

config "$DND_NOW"
# Launched by launchd (open), the app is its own responsible process, so item 11 can attribute its WebKit helpers to
# it; launched from a shell, they'd all belong to the terminal.
open -n -g --env "HOME=$HOME" --stdout "$T/app.log" --stderr "$T/app.log" "$APPDIR"
APP_PID=""
for _ in $(seq 100); do
  APP_PID=$(ps -U "$(id -u)" -o pid=,comm= | awk -v e="$EXE" '{ p = $1; sub(/^ *[0-9]+ /, ""); if ($0 == e) print p }')
  [ -n "$APP_PID" ] && break
  sleep 0.1
done
[ -n "$APP_PID" ] || fail "app did not start"
sleep 15
kill -0 "$APP_PID" 2>/dev/null || { tail -20 "$T/app.log"; fail "app exited"; }
grep -q '"key":"claude:crash".*"to":"error"' "$STATE/events.jsonl" || fail "the app never saw the crash"
[ "$(count ring)" -eq 0 ] && [ "$(count phone)" -eq 0 ] && [ ! -e "$T/phone.log" ] ||
  fail "rings or phone during DND: $(cat "$STATE/rings.jsonl")"
ok "item 9: DND covering now -> a live red alert gets 0 rings and no phone call in 15 s"

config ""
for _ in $(seq 30); do [ "$(count phone)" -ge 1 ] && break; sleep 1; done
sleep 20
[ "$(count ring)" -ge 1 ] || fail "no ring after DND ended:
$(tail -5 "$STATE/rings.jsonl" "$STATE/events.jsonl" "$T/app.log" "$HOME/.config/pay-attention/config.toml" 2>&1)"
[ "$(count phone)" -eq 1 ] && [ "$(wc -l <"$T/phone.log")" -eq 1 ] ||
  fail "phone calls: $(cat "$T/phone.log" 2>/dev/null)"
grep -qx 'name-crash error' "$T/phone.log" || fail "phone args: $(cat "$T/phone.log")"
ok "item 9: away + DND off -> rings resume, and phone_command runs exactly once ($(count ring) rings)"

config "$DND_NOW"
sleep 30
# procs: the app plus the WebKit helpers it is responsible for (they run under launchd, not as its children), as
# pid@start. Fails if discovery fails or finds no helper; the set must stay the same for the whole window.
procs() {
  local found p
  found=$(python3 -I -c 'import ctypes, subprocess, sys
f = ctypes.CDLL(None).responsibility_get_pid_responsible_for_pid
f.restype, f.argtypes = ctypes.c_int, [ctypes.c_int]
for line in subprocess.run(["ps", "-axo", "pid=,comm="], capture_output=True, text=True, check=True).stdout.splitlines():
    pid, comm = line.split(None, 1)
    if "com.apple.WebKit." not in comm:
        continue
    owner = f(int(pid))
    if owner < 0:
        sys.exit(f"responsibility lookup failed for {pid}")
    if owner == int(sys.argv[1]):
        print(pid)' "$APP_PID") || return 1
  [ -n "$found" ] || return 1
  for p in "$APP_PID" $found; do echo "$p@$(ident "$p")"; done
}
SET=$(procs) || fail "found no WebKit helpers for the app"
PROCS=()
while IFS= read -r e; do PROCS+=("${e%@*}"); done <<<"$SET"
cpu() { ps -o time= -p "$(IFS=,; echo "${PROCS[*]}")" | awk -F: '{ s = 0; for (i = 1; i <= NF; i++) s = s * 60 + $i; t += s } END { print t }'; }
# Physical footprint, as Activity Monitor counts memory (RSS double-counts WebKit's shared pages).
footprint_mb() {
  footprint "${PROCS[@]}" 2>/dev/null | awk '$1 == "phys_footprint:" {
    t += $3 == "GB" ? $2 * 1024 : $3 == "KB" ? $2 / 1024 : $3 == "B" ? $2 / 1048576 : $2 } END { printf "%d", t }'
}
c0=$(cpu) peak=0
for _ in $(seq $((IDLE / 30))); do
  sleep 30
  kill -0 "$APP_PID" 2>/dev/null || fail "app exited during the idle window"
  [ "$(procs)" = "$SET" ] || fail "the app's WebKit helpers changed during the window"
  mb=$(footprint_mb)
  [ "$mb" -gt "$peak" ] && peak=$mb
done
pct=$(awk -v a="$c0" -v b="$(cpu)" -v s="$IDLE" 'BEGIN { printf "%.2f", (b - a) / s * 100 }')
awk -v p="$pct" 'BEGIN { exit !(p < 1) }' && [ "$peak" -lt 150 ] ||
  fail "idle CPU ${pct}%, peak footprint ${peak} MB (${#PROCS[@]} processes)"
ok "item 11: idle ${IDLE} s with 6 sessions -> CPU ${pct}%, peak footprint ${peak} MB (app + $((${#PROCS[@]} - 1)) WebKit helpers)"
echo "E2E APP ITEMS PASSED"
