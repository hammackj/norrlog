#!/usr/bin/env bash
# Build Norrlog's Linux AppImage: one file holding the app and the daemon.
#
#     packaging/appimage/build.sh [OUT_DIR]
#
# Needs appimagetool on PATH, or its path in $APPIMAGETOOL. Builds the release
# binaries first. An AppImage runs on systems whose glibc is at least the
# build machine's, so a release is built on an old distribution (CI uses
# Ubuntu 22.04), not a rolling one.
set -euo pipefail
ROOT=$(cd "$(dirname "$(readlink -f "$0")")/../.." && pwd)
OUT=${1:-$ROOT/target/appimage}
TOOL=${APPIMAGETOOL:-appimagetool}
command -v "$TOOL" >/dev/null || { echo "appimagetool not found; set APPIMAGETOOL" >&2; exit 1; }

cd "$ROOT"
cargo build --release --locked -p norrlog -p norrlogd
VERSION=$(cargo metadata --format-version 1 --no-deps |
    python3 -c 'import json,sys; print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"] == "norrlog"))')

APPDIR=$(mktemp -d)/Norrlog.AppDir
trap 'rm -rf "$(dirname "$APPDIR")"' EXIT
mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/share/applications"
install -m755 target/release/norrlog target/release/norrlogd "$APPDIR/usr/bin/"
install -m755 packaging/appimage/AppRun "$APPDIR/AppRun"
install -m644 packaging/appimage/norrlog.desktop "$APPDIR/norrlog.desktop"
install -m644 packaging/appimage/norrlog.desktop "$APPDIR/usr/share/applications/"
for size in 32 48 64 128 256; do
    d="$APPDIR/usr/share/icons/hicolor/${size}x${size}/apps"
    mkdir -p "$d"
    install -m644 "packaging/eqlparser-$size.png" "$d/norrlog.png"
done
install -m644 packaging/eqlparser-256.png "$APPDIR/norrlog.png"
ln -s norrlog.png "$APPDIR/.DirIcon"

mkdir -p "$OUT"
ARCH=x86_64 VERSION=$VERSION "$TOOL" --no-appstream "$APPDIR" "$OUT/Norrlog-$VERSION-x86_64.AppImage"
echo "$OUT/Norrlog-$VERSION-x86_64.AppImage"
