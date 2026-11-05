#!/usr/bin/env bash
# Engine checks: the alert engine via `pay-attention simulate`, plus the core checks.
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"
bash scripts/check-core.sh
python3 -I scripts/check_engine.py "$ROOT/target/debug/pay-attention"
