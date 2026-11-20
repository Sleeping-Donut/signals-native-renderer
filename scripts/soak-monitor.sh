#!/usr/bin/env bash
# Manual long-running soak. Not part of CI.
# Usage: scripts/soak-monitor.sh [hours] [interval-seconds]
set -euo pipefail
cd "$(dirname "$0")/.."

hours="${1:-24}"
interval="${2:-3600}"
mkdir -p .soak
log=".soak/soak-$(date +%Y%m%d-%H%M%S).log"

if command -v nix >/dev/null 2>&1; then
  runner=(nix develop -c cargo)
else
  runner=(cargo)
fi

echo "Starting ${hours}h soak monitor; interval ${interval}s"
echo "Log: ${log}"
SOAK_SECONDS=$((hours * 3600)) SOAK_INTERVAL_SECS="$interval" \
  nohup "${runner[@]}" test -p reactive-core --test soak soak_monitor -- --ignored --nocapture \
  >"$log" 2>&1 &
pid=$!
echo "$pid" > .soak/soak.pid
echo "PID: ${pid}"
echo "Stop with: kill \$(cat .soak/soak.pid)"
