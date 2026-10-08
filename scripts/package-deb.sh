#!/usr/bin/env bash
# Package the Linux GUI build (noviewlog-slint, product name noviewlog)
# as a Debian archive: noviewlog-linux-x64.deb.
#
# Plain dpkg-deb staging - no cargo-deb, no maintainer scripts (dpkg
# triggers refresh the desktop database on install and remove). The
# shipped binary name is noviewlog, matching the tar.gz archives.
#
# Depends names are written for Ubuntu 20.04+/Debian; Ubuntu 24.04+
# t64 packages (e.g. libpng16-16t64) keep Provides for the old names,
# so one list resolves everywhere.
#
# Usage:
#   scripts/package-deb.sh [version] [binary]
#     version  deb upstream version, default: latest v* tag without the v
#     binary   GUI binary to ship, default: target/release/noviewlog-slint,
#              falling back to target/release-dev/noviewlog-slint
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

VERSION="${1:-}"
BIN="${2:-}"

if [[ -z "$VERSION" ]]; then
  TAG="$(git describe --tags --abbrev=0 2>/dev/null || true)"
  if [[ "$TAG" =~ ^v[0-9]+(\.[0-9]+){1,2}$ ]]; then
    VERSION="${TAG#v}"
  else
    VERSION="0.0.0-dev"
  fi
fi
if [[ ! "$VERSION" =~ ^[0-9][0-9A-Za-z.+~]*(-[0-9A-Za-z.+~]+)?$ ]]; then
  echo "error: invalid deb version '$VERSION'" >&2
  exit 1
fi

if [[ -z "$BIN" ]]; then
  if [[ -x target/release/noviewlog-slint ]]; then
    BIN=target/release/noviewlog-slint
  elif [[ -x target/release-dev/noviewlog-slint ]]; then
    BIN=target/release-dev/noviewlog-slint
  else
    echo "error: no GUI binary found - build first (cargo build --release -p noviewlog-slint) or pass one: $0 <version> <binary>" >&2
    exit 1
  fi
fi
if [[ ! -x "$BIN" ]]; then
  echo "error: binary is missing or not executable: $BIN" >&2
  exit 1
fi

DESKTOP=packaging/linux/noviewlog.desktop
ICON=crates/noviewlog-slint/ui/assets/icon.png
STAGE="target/deb/noviewlog_${VERSION}_amd64"
OUT="noviewlog-linux-x64.deb"

rm -rf "$STAGE"
install -m 755 -D "$BIN" "$STAGE/usr/bin/noviewlog"
install -m 644 -D "$DESKTOP" "$STAGE/usr/share/applications/noviewlog.desktop"
install -m 644 -D "$ICON" "$STAGE/usr/share/pixmaps/noviewlog.png"
install -m 644 -D LICENSE "$STAGE/usr/share/doc/noviewlog/copyright"

mkdir -p "$STAGE/DEBIAN"
cat > "$STAGE/DEBIAN/control" <<EOF
Package: noviewlog
Version: ${VERSION}-1
Section: utils
Priority: optional
Architecture: amd64
Maintainer: nostalgie <nostalgie@users.noreply.github.com>
Depends: libfontconfig1, libfreetype6, libexpat1, libpng16-16, libbrotli1, libbz2-1.0, zlib1g, libx11-6, libxcursor1, libxrandr2, libxi6, libxrender1, libxkbcommon0, libxkbcommon-x11-0, libwayland-client0, libgl1
Description: NoViewLog - native desktop log viewer
 NoViewLog opens large application and terminal logs instantly and
 filters them live. Native Rust engine, Slint UI, no WebView.
EOF

if command -v desktop-file-validate >/dev/null 2>&1; then
  desktop-file-validate "$STAGE/usr/share/applications/noviewlog.desktop"
fi

dpkg-deb --build --root-owner-group "$STAGE" "$OUT" >/dev/null
echo
dpkg-deb -I "$OUT"
echo
echo "OK: $OUT"
