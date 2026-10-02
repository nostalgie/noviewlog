#!/usr/bin/env bash
# Native Windows MSVC publish for noviewlog-slint → dist/noviewlog-slint-win-x64/
#
# From PowerShell (preferred): .\scripts\publish-slint-windows.ps1
# From Git Bash when bash is not in PATH:
#   & "C:\Program Files\Git\bin\bash.exe" scripts/publish-slint-windows.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="$ROOT/dist/noviewlog-slint-win-x64"

is_windows_host() {
    case "$(uname -s 2>/dev/null || echo unknown)" in
        MINGW*|MSYS*|CYGWIN*) return 0 ;;
    esac
    # Git Bash / some environments report Windows_NT via OSTYPE or env
    if [[ "${OS:-}" == "Windows_NT" ]]; then
        return 0
    fi
    return 1
}

if ! is_windows_host; then
    echo "error: publish-slint-windows.sh requires a native Windows host (MSVC Rust toolchain)." >&2
    echo "  Linux→Windows GUI cross-compile is not supported in v1." >&2
    echo "  On Windows: install rustup MSVC target, then re-run this script." >&2
    echo "  From PowerShell: \"C:\\Program Files\\Git\\bin\\bash.exe\" scripts/publish-slint-windows.sh" >&2
    echo "  Or use a windows-latest CI job with the same commands." >&2
    exit 1
fi

cd "$ROOT"

# Static CRT: the dist exe must run on a pristine Windows install with no
# VC Redist. Publish-only flag; never set this for release-dev (fingerprint churn).
export RUSTFLAGS='-C target-feature=+crt-static'

echo "==> Building noviewlog-slint (release, static CRT)..."
cargo build --release -p noviewlog-slint

SRC_EXE="$ROOT/target/release/noviewlog-slint.exe"
if [[ ! -f "$SRC_EXE" ]]; then
    echo "error: expected $SRC_EXE not found after cargo build" >&2
    exit 1
fi

echo "==> Staging $OUT_DIR ..."
rm -rf "$OUT_DIR"
mkdir -p "$OUT_DIR"
cp -f "$SRC_EXE" "$OUT_DIR/NoViewLog.exe"

cat > "$OUT_DIR/README.txt" <<'EOF'
NoViewLog (Slint) — Windows x64

Requirements: Windows 10 or later, x64.
Self-contained: the CRT is statically linked — no VC Redist or other installs.

Run:
  NoViewLog.exe
  NoViewLog.exe -- app.log
  NoViewLog.exe -- npm run dev

Copy this entire folder if you move the app; the engine is linked into NoViewLog.exe
(no separate noviewlog_core.dll).
EOF

echo "==> Headless smoke (process start) ..."
if ! "$OUT_DIR/NoViewLog.exe" --version >/dev/null 2>&1; then
    # App may not implement --version; a failed start with no GUI is still OK for CI.
    # Launch with no args, give it a moment, then kill if still running.
    "$OUT_DIR/NoViewLog.exe" &
    SMOKE_PID=$!
    sleep 2
    if kill -0 "$SMOKE_PID" 2>/dev/null; then
        echo "  OK: NoViewLog.exe started (pid $SMOKE_PID)"
        kill "$SMOKE_PID" 2>/dev/null || true
        wait "$SMOKE_PID" 2>/dev/null || true
    else
        echo "error: NoViewLog.exe exited immediately after launch" >&2
        exit 1
    fi
else
    echo "  OK: NoViewLog.exe --version"
fi

SIZE="$(du -sh "$OUT_DIR" | cut -f1)"
echo ""
echo "Slint Windows distribution ready."
echo "  Output: $OUT_DIR"
echo "  Size:   $SIZE"
echo ""
echo "Run:"
echo "  NoViewLog.exe"
