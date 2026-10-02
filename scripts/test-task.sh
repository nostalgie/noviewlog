#!/usr/bin/env bash
# Task-scoped test run (internal fast-tests policy, "Task scope" section).
#
# Detects the crates changed on the current branch vs origin/main (or takes
# crate names as arguments) and runs, per crate: fast-tier tests + clippy
# (zero warnings), plus the known slow-tier e2e for the surfaces the crate
# owns (TUI mouse/PTY -> conpty_mouse_selection).
#
# Usage:
#   scripts/test-task.sh                 # auto-detect from branch diff
#   scripts/test-task.sh noviewlog-tui   # force specific crates
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"

if [ $# -gt 0 ]; then
    CRATES=("$@")
else
    mapfile -t CRATES < <(
        git diff --name-only origin/main...HEAD 2>/dev/null |
            grep -oE 'crates/[^/]+' | sort -u | sed 's#crates/##'
    )
    if [ "${#CRATES[@]}" -eq 0 ]; then
        echo "no crates changed on this branch (vs origin/main); pass crate names as arguments" >&2
        exit 1
    fi
fi

echo "task scope: ${CRATES[*]}"

if [ -n "$(git status --porcelain)" ]; then
    echo "NOTE: working tree has uncommitted changes — testing them as-is" >&2
fi

FAIL=0
for crate in "${CRATES[@]}"; do
    echo "=== cargo test -p $crate (fast tier)"
    cargo test -p "$crate" || FAIL=1
done

for crate in "${CRATES[@]}"; do
    echo "=== cargo clippy -p $crate --all-targets (must be zero warnings)"
    warnings=$(cargo clippy -p "$crate" --all-targets 2>&1 | grep -cE '^warning:|^error' || true)
    if [ "$warnings" -ne 0 ]; then
        echo "clippy: $warnings warning(s)/error(s) in $crate" >&2
        FAIL=1
    fi
done

# Slow-tier e2e owned by the crate surfaces the task may have touched.
if printf '%s\n' "${CRATES[@]}" | grep -qx 'noviewlog-tui'; then
    echo "=== TUI mouse/PTY surface: conpty_mouse_selection e2e"
    cargo test -p noviewlog-tui --test conpty_mouse_selection -- --ignored --test-threads=1 || FAIL=1
fi

if [ "$FAIL" -ne 0 ]; then
    echo "TASK TESTS: FAILED" >&2
    exit 1
fi
echo "TASK TESTS: OK (${CRATES[*]})"
