#!/usr/bin/env bash
# Release-mode budget measurements. Not part of CI.
set -euo pipefail
cd "$(dirname "$0")/.."

if command -v nix >/dev/null 2>&1; then
  exec nix develop -c cargo test --release -p view --test perf_budgets perf_budgets -- --ignored --nocapture
else
  exec cargo test --release -p view --test perf_budgets perf_budgets -- --ignored --nocapture
fi
