#!/usr/bin/env bash
# Fetch Ubuntu fontconfig -dev bits into .deps/ for Slint builds without sudo.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
WORKDIR="${ROOT}/.deps"
TMP="${TMPDIR:-/tmp}/noviewlog-slint-deps"
mkdir -p "$WORKDIR" "$TMP"
cd "$TMP"
if [[ ! -f libfontconfig-dev.deb ]]; then
  apt-get download libfontconfig-dev
  mv libfontconfig-dev_*.deb libfontconfig-dev.deb
fi
dpkg-deb -x libfontconfig-dev.deb "$WORKDIR"
mkdir -p "$WORKDIR/pkgconfig" "$WORKDIR/lib"
# Prefer shared system lib — avoid linking the static .a from the -dev package.
ln -sfn /usr/lib/x86_64-linux-gnu/libfontconfig.so.1 "$WORKDIR/lib/libfontconfig.so"
cat > "$WORKDIR/pkgconfig/fontconfig.pc" <<EOF2
prefix=${WORKDIR}/usr
libdir=${WORKDIR}/lib
includedir=\${prefix}/include

Name: Fontconfig
Description: Font configuration library (local .deps)
Version: 2.17.1
Libs: -L\${libdir} -lfontconfig
Cflags: -I\${includedir}
EOF2
echo "OK: PKG_CONFIG_PATH=${WORKDIR}/pkgconfig"
PKG_CONFIG_PATH="${WORKDIR}/pkgconfig" pkg-config --libs --cflags fontconfig
