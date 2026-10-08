#!/usr/bin/env bash
# Validate an exported release tree exactly the way the public CI will.
# Run it on the stage copy produced by a publish dry-run (or any `git
# archive` extraction). A clean checkout has no vendored dependencies: what
# builds on the dev machine can still fail in CI - this script reproduces
# the CI conditions before anything is pushed.
#
# Stages (all by default; pass names to run a subset):
#   rust      - fmt --check, clippy (zero warnings), workspace tests
#   extension - npm ci, wasm bundle, vitest, tsc compile
#   smoke     - headless VS Code suite (xvfb-run; skipped with a warning
#               where xvfb is not available)
#
# Usage:
#   scripts/verify-export.sh /tmp/stage
#   scripts/verify-export.sh rust extension /tmp/stage
set -euo pipefail

die() {
  echo "error: $*" >&2
  exit 1
}

STAGES=()
DIR=""
for arg in "$@"; do
  case "$arg" in
  rust | extension | smoke) STAGES+=("$arg") ;;
  -h | --help)
    sed -n '2,20p' "$0"
    exit 0
    ;;
  *)
    if [[ -z "$DIR" ]]; then
      DIR="$arg"
    else
      die "unexpected argument: $arg"
    fi
    ;;
  esac
done
[[ -n "$DIR" ]] || die "usage: scripts/verify-export.sh [rust|extension|smoke ...] <stage-dir>"
[[ -d "$DIR" ]] || die "stage dir not found: $DIR"
[[ ${#STAGES[@]} -eq 0 ]] && STAGES=(rust extension smoke)

cd "$DIR"
export PATH="$HOME/.cargo/bin:$PATH"

FAIL=0
step() {
  local name="$1"
  shift
  echo "=== $name"
  if "$@"; then
    echo "--- $name: OK"
  else
    echo "--- $name: FAILED" >&2
    FAIL=1
  fi
}

if [[ " ${STAGES[*]} " == *" rust "* ]]; then
  command -v cargo >/dev/null 2>&1 || die "cargo not found (rust stage)"
  step "rust: cargo fmt --check" cargo fmt --check
  step "rust: clippy --workspace --all-targets (zero warnings)" \
    cargo clippy --workspace --all-targets -- -D warnings
  step "rust: workspace tests" cargo test --workspace
fi

if [[ " ${STAGES[*]} " == *" extension "* ]]; then
  command -v npm >/dev/null 2>&1 || die "npm not found (extension stage)"
  cd extensions/vscode
  step "extension: npm ci" npm ci
  step "extension: wasm bundle" npm run build:wasm
  step "extension: vitest" npm test
  step "extension: tsc compile" npm run compile
  cd ../..
fi

if [[ " ${STAGES[*]} " == *" smoke "* ]]; then
  if command -v xvfb-run >/dev/null 2>&1; then
    cd extensions/vscode
    step "smoke: headless VS Code suite" xvfb-run -a npm run test:vscode
    cd ../..
  else
    echo "=== smoke: SKIPPED (xvfb-run not installed; the tests workflow runs it on CI)" >&2
  fi
fi

if [[ "$FAIL" -ne 0 ]]; then
  echo "VERIFY-EXPORT: FAILED (stage: $DIR)" >&2
  echo "A missing pkg-config/fontconfig here reproduces what public CI would hit on a clean checkout." >&2
  exit 1
fi
echo "VERIFY-EXPORT: OK (stage: $DIR; stages: ${STAGES[*]})"
