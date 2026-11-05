#!/usr/bin/env bash
# App checks: the engine and core checks, the app's lint/types/build, and a 6 s launch smoke under a temp HOME (its dot
# shows in the menu bar meanwhile). The real end-to-end run is scripts/e2e.sh.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"
bash scripts/check-engine.sh
cargo clippy -p pay-attention-app --all-targets -q -- -D warnings
cd app
npm ci --silent
npx tsc --noEmit
npx tauri build --debug --bundles app >"${TMPDIR:-/tmp}/pa-tauri-build.log" 2>&1 || { tail -40 "${TMPDIR:-/tmp}/pa-tauri-build.log"; exit 1; }
cd "$ROOT"
APP="$ROOT/target/debug/bundle/macos/Pay Attention.app"
[ -d "$APP" ] || { echo "FAIL: $APP not built" >&2; exit 1; }
H=$(mktemp -d "${TMPDIR:-/tmp}/pa-smoke.XXXXXX")
trap 'kill "$P" 2>/dev/null || true; rm -rf "$H"' EXIT
mkdir -p "$H/.claude/sessions" "$H/.config/pay-attention"
EXE=$(find "$APP/Contents/MacOS" -type f -perm -u+x | head -1)
HOME="$H" "$EXE" >"$H/app.log" 2>&1 &
P=$!
sleep 6
kill -0 "$P" 2>/dev/null || { echo "FAIL: app exited within 6 s" >&2; tail -20 "$H/app.log" >&2; exit 1; }
echo "APP CHECKS PASSED"
