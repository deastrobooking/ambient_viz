#!/usr/bin/env bash
# Launches the Node SSE bridge and the Python sensor sidecar together,
# interleaving their output with [node]/[py] prefixes. Ctrl-C stops both.
#
# Usage: ./run_kiosk.sh [extra args for python -m ambient_kiosk]
#
# Default sidecar args: --no-pir --no-breath --no-touch  (distance-only,
# per PI_KIOSK_BRINGUP.md phase 4). Pass your own to override, e.g.:
#   ./run_kiosk.sh                       # distance only
#   ./run_kiosk.sh --mock                # synthetic data, no hardware
#   ./run_kiosk.sh                       # (all sensors) — when other phases come online
#
# Env vars forwarded to the Node server: MOCK, PORT, INGEST_TOKEN.

set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
cd "$ROOT"

if [ "$#" -gt 0 ]; then
  SIDECAR_ARGS=("$@")
else
  SIDECAR_ARGS=("--no-pir" "--no-breath" "--no-touch")
fi

# Prefer the project venv; fall back to whatever python is on PATH (the
# Pi README's `pip install -e .` flow can target system Python).
if [ -x "$ROOT/python/.venv/bin/python" ]; then
  PY="$ROOT/python/.venv/bin/python"
else
  PY="python3"
fi

prefix() {
  awk -v tag="$1" '{ print "[" tag "] " $0; fflush(); }'
}

PIDS=()
cleanup() {
  trap - INT TERM EXIT
  for pid in "${PIDS[@]+"${PIDS[@]}"}"; do
    kill "$pid" 2>/dev/null || true
  done
  wait 2>/dev/null || true
}
trap cleanup INT TERM EXIT

(cd "$ROOT/server" && exec node src/index.js 2>&1) | prefix "node" &
PIDS+=($!)

(cd "$ROOT/python" && exec "$PY" -m ambient_kiosk "${SIDECAR_ARGS[@]}" 2>&1) | prefix "py  " &
PIDS+=($!)

# Block until either pipeline ends; cleanup trap kills the survivor.
wait -n
