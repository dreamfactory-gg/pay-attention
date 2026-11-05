# shellcheck shell=bash
# Fixture and cleanup helpers shared by check-core.sh, e2e.sh and e2e-app.sh. The caller defines fail, track and $HOME.
now_ms() { python3 -c 'import time; print(int(time.time()*1000))'; }
utc_start() { TZ=UTC ps -o lstart= -p "$1" | sed 's/ *$//'; }
ident() { ps -o lstart= -p "$1" 2>/dev/null; }

# in_dir DIR: pids of this user's processes working in DIR or below, or running an executable from there. A test starts
# its processes in, or from, a fresh temp dir (children inherit the working directory), so only its own processes
# match: a reused pid never does, and an orphan that left the process tree still does. Fails if a listing fails.
in_dir() {
  local cwds exes
  cwds=$(lsof -a -d cwd -u "$(id -u)" -Fpn 2>/dev/null) || return 1
  exes=$(ps -U "$(id -u)" -o pid=,comm=) || return 1
  {
    awk -v d="$1" '/^p/ { p = substr($0, 2) } /^n/ { n = substr($0, 2); if (n == d || index(n, d "/") == 1) print p }' <<<"$cwds"
    awk -v d="$1/" '{ p = $1; sub(/^ *[0-9]+ /, ""); if (index($0, d) == 1) print p }' <<<"$exes"
  } | sort -u
}

# stop_dir DIR: stops every process in_dir finds; what's still there after 5 s is killed. Fails, leaving them, if a
# listing fails.
stop_dir() {
  local p pids
  pids=$(in_dir "$1") || return 1
  for p in $pids; do kill "$p" 2>/dev/null || true; done
  for _ in $(seq 10); do
    pids=$(in_dir "$1") || return 1
    [ -z "$pids" ] && return 0
    sleep 0.5
  done
  for p in $pids; do kill -9 "$p" 2>/dev/null || true; done
}

# A process that owns a fresh pty, like a Terminal tab. Prints the pid of the shell holding the pty.
tty_proc() {
  script -q /dev/null sh -c "$1" </dev/null >/dev/null 2>&1 &
  local s=$! c=""
  track "$s"
  for _ in $(seq 100); do
    c=$(pgrep -P "$s" | head -1 || true)
    [ -n "$c" ] && break
    sleep 0.05
  done
  [ -n "$c" ] || fail "no pty child for: $1"
  echo "$c"
}

# reg PID SID STATUS [WAITINGFOR] [PROCSTART] [STATUS_UPDATED_MS]: a Claude Code registry file.
reg() {
  local pid=$1 sid=$2 st=$3 wf=${4:-} pst=${5:-} upd=${6:-}
  [ -n "$pst" ] || pst=$(utc_start "$pid")
  [ -n "$upd" ] || upd=$(now_ms)
  jq -n --argjson pid "$pid" --arg sid "$sid" --arg st "$st" --arg wf "$wf" --arg pst "$pst" --argjson upd "$upd" '
    {pid: $pid, sessionId: $sid, cwd: "/tmp/proj", startedAt: $upd, procStart: $pst, version: "2.1.317",
     peerProtocol: 1, kind: "interactive", entrypoint: "cli", name: ("name-" + $sid), nameSource: "derived",
     status: $st, updatedAt: $upd, statusUpdatedAt: $upd}
    + (if $wf == "" then {} else {waitingFor: $wf} end)' >"$HOME/.claude/sessions/$pid.json"
}
