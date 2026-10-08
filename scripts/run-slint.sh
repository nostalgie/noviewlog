#!/usr/bin/env bash
# Build/run the Slint UI (default product) without system libfontconfig-dev.
# Profile release-dev: opt-level 3 (debug pegs a core on PTY flood) without fat
# LTO, so small edits do not relink the whole graph. Publish still uses
# --release (see scripts/publish-slint-windows.sh).
# Do not set RUSTFLAGS here: appending -L changes the rustc fingerprint and
# forces a full rebuild when this script and a plain `cargo` disagree.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
if [[ ! -f "${ROOT}/.deps/pkgconfig/fontconfig.pc" ]]; then
  bash "${ROOT}/scripts/setup-slint-deps.sh"
fi
cd "$ROOT"
exec cargo run --profile release-dev -p noviewlog-slint -- "$@"
