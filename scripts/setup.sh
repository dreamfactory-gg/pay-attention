#!/usr/bin/env bash
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
missing=
xcode-select -p >/dev/null 2>&1 || missing="$missing xcode-command-line-tools"
command -v cargo >/dev/null || missing="$missing cargo"
command -v npm >/dev/null || missing="$missing npm"
[ -z "$missing" ] || { echo "setup.sh: missing:$missing. See README.md § Agent install, step 3." >&2; exit 1; }

# ask PROMPT: succeeds on a yes. With no input (a pipe that's run dry, or a command with no terminal), say so instead
# of exiting silently. Both questions come before any build, so npm and cargo can't read piped answers.
ask() {
  read -r -p "$1 [y/N] " answer ||
    { echo "setup.sh: no answer to \"$1\". Run it in a Terminal window, or pipe in the answers." >&2; exit 1; }
  case "$answer" in y|Y|yes|YES) ;; *) return 1 ;; esac
}

ask "Build and install Pay Attention into ~/Applications and ~/.local/bin?" || { echo "Cancelled."; exit 0; }
hooks=no
ask "Install hooks for the agents found here (Claude Code, Codex; Hermes gets printed instructions)?" && hooks=yes

lock="$HOME/.local/state/pay-attention/setup.lock"
mkdir -p "$(dirname "$lock")"
if ! mkdir "$lock" 2>/dev/null; then
  holders=("$lock"/*)
  if [ "${#holders[@]}" -ne 1 ] || ! read -r pid 2>/dev/null < "${holders[0]}" ||
     kill -0 "$pid" 2>/dev/null ||
     ! { rm "${holders[0]}" && rmdir "$lock" && mkdir "$lock"; }; then
    echo "Another setup holds $lock; refusing to run." >&2
    exit 1
  fi
fi
release_lock() { rm -f "$lock/$$"; rmdir "$lock"; }
trap release_lock EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
printf '%s\n' "$$" > "$lock/$$"

cd "$ROOT/app"
npm ci
npx tauri build
cd "$ROOT"
cargo build --release -p pa
mkdir -p "$HOME/Applications" "$HOME/.local/bin"
app="$HOME/Applications/Pay Attention.app"
stage=$(mktemp -d "$HOME/Applications/.pay-attention.XXXXXX")
cleanup() {
  if { [ -e "$stage/previous.app" ] || [ -L "$stage/previous.app" ]; } &&
     [ ! -e "$app" ] && [ ! -L "$app" ]; then
    mv "$stage/previous.app" "$app" || { echo "Could not restore the previous app; it remains at $stage/previous.app" >&2; return; }
  fi
  rm -rf "$stage" "$HOME/.local/bin/.pay-attention.new"
}
trap 'cleanup; release_lock' EXIT
cp -R "target/release/bundle/macos/Pay Attention.app" "$stage/Pay Attention.app"
[ -x "$stage/Pay Attention.app/Contents/MacOS/pay-attention-app" ]
if [ -e "$app" ] || [ -L "$app" ]; then mv "$app" "$stage/previous.app"; fi
mv "$stage/Pay Attention.app" "$app"
# Installed hooks run this binary: stage it in the same dir and rename, so a failed copy never leaves it partial.
cp "target/release/pay-attention" "$HOME/.local/bin/.pay-attention.new"
mv -f "$HOME/.local/bin/.pay-attention.new" "$HOME/.local/bin/pay-attention"
"$HOME/.local/bin/pay-attention" login-item on "$app/Contents/MacOS/pay-attention-app"

if [ "$hooks" = yes ]; then "$HOME/.local/bin/pay-attention" install-hooks; else echo "Hooks not installed."; fi
open "$HOME/Applications/Pay Attention.app"
