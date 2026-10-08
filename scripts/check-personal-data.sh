#!/usr/bin/env bash
# Dev-time personal-data check (shift-left half of the publish content
# guard): scans every tracked, export-shipped file for the local machine's
# personal tokens - home path, username, hostname - so a leak is caught in
# development, not by the publish guard (or worse, by a reader of the
# public repository). Export-ignored files never ship and are skipped.
#
# A hit is fixed by scrubbing the file to synthetic placeholders
# (/home/tester, C:\Users\tester, test-host) - never by excluding the file.
#
# Usage:
#   scripts/check-personal-data.sh        # tracked working tree
#   scripts/check-personal-data.sh <sha>  # a specific commit tree
set -euo pipefail
cd "$(dirname "$0")/.."

TREE="${1:-}"
TARGET=(-- .)
if [[ -n "$TREE" ]]; then
  git rev-parse --verify --quiet "${TREE}^{commit}" >/dev/null || {
    echo "error: not a commit: $TREE" >&2
    exit 1
  }
  TARGET=("$TREE" -- .)
fi

TOKENS=("$HOME" "/home/${HOME##*/}" "C:\\Users\\${HOME##*/}")
# Bare-word tokens (username, hostname) are only meaningful on the machine
# whose identity they carry. On GitHub-hosted runners that identity is
# generic ("runner") and pollutes the scan with CI and dependency vocabulary
# (runner.os in workflows, @vitest/runner in lockfiles), so bare words apply
# on developer machines only; CI keeps the home-path tokens. The publish
# guard derives the full set regardless - it runs on the real machine.
if [[ "${GITHUB_ACTIONS:-}" != "true" ]]; then
  for tok in "$(id -un 2>/dev/null || true)" "$(uname -n 2>/dev/null || true)"; do
    if [[ -n "$tok" && "$tok" != "localhost" && ${#tok} -ge 4 ]]; then
      TOKENS+=("$tok")
    fi
  done
fi

FAIL=0
for tok in "${TOKENS[@]}"; do
  while IFS= read -r path; do
    attr="$(git check-attr export-ignore -- "$path" | awk -F': ' '{print $NF}')"
    [[ "$attr" == "set" ]] && continue
    echo "  $path: $tok"
    FAIL=1
  done < <(git grep -i -F -l -e "$tok" "${TARGET[@]}" || true)
done

if [[ "$FAIL" -ne 0 ]]; then
  echo "PERSONAL DATA: found in tracked, shipped files (listed above)." >&2
  echo "Scrub the files to synthetic placeholders (/home/tester, C:\\Users\\tester, test-host); do not exclude them from the scan." >&2
  exit 1
fi
echo "PERSONAL DATA: clean (tracked, export-shipped files)."
