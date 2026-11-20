#!/usr/bin/env bash
# Rapid deterministic churn simulation for agents and local checks.
# Not part of CI. Override rounds with SOAK_FUZZ_ROUNDS.
set -euo pipefail
cd "$(dirname "$0")/.."

if command -v nix >/dev/null 2>&1; then
  exec nix develop -c cargo test -p reactive-core --test soak fuzz_churn_is_bounded -- --ignored --nocapture
else
  exec cargo test -p reactive-core --test soak fuzz_churn_is_bounded -- --ignored --nocapture
fi
