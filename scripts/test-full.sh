#!/usr/bin/env bash
# Periodic FULL test run (internal fast-tests policy): workspace-wide
# clippy + fast tier, plus the slow tier (-- --ignored) with --full.
#
# Run periodically, not per task: before a release; when declaring
# audit-cycle convergence; after a batch of >=5 merged PRs or a cross-crate
# refactor; when main is red for reasons a scoped run does not explain.
#
# Usage:
#   scripts/test-full.sh          # clippy + fast tier, whole workspace
#   scripts/test-full.sh --full   # + slow tier (-- --ignored)
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
# Agent/CI shells may export NO_COLOR=1; crossterm then strips SGR and
# color-asserting slow-tier TUI tests fail. Full suite needs real colors.
unset NO_COLOR || true

FULL=0
[ "${1:-}" = "--full" ] && FULL=1

echo "=== cargo clippy --workspace --all-targets (must be zero warnings)"
warnings=$(cargo clippy --workspace --all-targets 2>&1 | grep -cE '^warning:|^error' || true)
if [ "$warnings" -ne 0 ]; then
    echo "clippy: $warnings warning(s)/error(s)" >&2
    exit 1
fi

echo "=== cargo test --workspace (fast tier)"
cargo test --workspace

if [ "$FULL" -eq 1 ]; then
    # Serial across packages (-j1) and within each harness (--test-threads=1):
    # ConPTY e2e suites share the real system clipboard and contend for PTY
    # slots; parallel harnesses flake on "TUI must reach the running state".
    echo "=== slow tier (-j1 -- --ignored --test-threads=1)"
    cargo test --workspace -j1 -- --ignored --test-threads=1
fi

echo "FULL TESTS: OK"
