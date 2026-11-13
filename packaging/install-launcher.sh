#!/usr/bin/env bash
# Install a desktop launcher for the current user, running the app built in
# this checkout (`cargo build --release` first).
#
# Per-user rather than system-wide: the Exec path points at wherever the repo
# happens to live and only means anything for the person who checked it out.
# No root, nothing outside $HOME. For an install that does not need the
# checkout, build the AppImage instead (packaging/appimage/build.sh).
#
# Safe to re-run: every step overwrites rather than appends, so this doubles as
# the way to fix the launcher after moving the repo. It also removes the old
# eqlparser launcher, which started the Electron app and no longer works.
set -eu

REPO="$(cd "$(dirname "$(readlink -f "$0")")/.." && pwd)"
APPS="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
ICONS="${XDG_DATA_HOME:-$HOME/.local/share}/icons/hicolor"
DESKTOP_DIR="$(xdg-user-dir DESKTOP 2>/dev/null || echo "$HOME/Desktop")"

if [ ! -x "$REPO/target/release/norrlog" ]; then
    echo "note: $REPO/target/release/norrlog is not built yet;" \
        "run 'cargo build --release' before using the launcher." >&2
fi

mkdir -p "$APPS"

# The icon goes in the hicolor theme under its own name, which is what
# `Icon=norrlog` resolves against. Every size is installed so the menu, the
# task switcher and the desktop each pick their own rather than rescaling one.
for size in 32 48 64 128 256; do
    src="$REPO/packaging/norrlog-$size.png"
    [ -f "$src" ] || continue
    dir="$ICONS/${size}x${size}/apps"
    mkdir -p "$dir"
    cp -f "$src" "$dir/norrlog.png"
done
mkdir -p "$ICONS/scalable/apps"
cp -f "$REPO/packaging/norrlog.svg" "$ICONS/scalable/apps/norrlog.svg"

# The Exec path is only knowable at install time.
sed "s|__REPO__|$REPO|g" "$REPO/packaging/norrlog.desktop" > "$APPS/norrlog.desktop"
chmod +x "$APPS/norrlog.desktop"

# The Electron launcher, from before the rename. Only ours: a file of that
# name that does not run eqlparser-launch.sh is left alone.
for old in "$APPS/eqlparser.desktop" "$DESKTOP_DIR/eqlparser.desktop"; do
    if [ -f "$old" ] && grep -q 'eqlparser-launch.sh' "$old"; then
        rm -f "$old"
        echo "Removed the old Electron launcher: $old"
    fi
done

if [ -d "$DESKTOP_DIR" ]; then
    cp -f "$APPS/norrlog.desktop" "$DESKTOP_DIR/norrlog.desktop"
    chmod +x "$DESKTOP_DIR/norrlog.desktop"
    # KDE will not launch a desktop file it does not trust, and there is no
    # way to set that from inside the file itself.
    command -v kioclient5 >/dev/null 2>&1 &&
        kioclient5 --noninteractive exec "$DESKTOP_DIR/norrlog.desktop" >/dev/null 2>&1 || true
    gio set "$DESKTOP_DIR/norrlog.desktop" metadata::trusted true 2>/dev/null || true
fi

command -v update-desktop-database >/dev/null 2>&1 &&
    update-desktop-database "$APPS" >/dev/null 2>&1 || true
command -v gtk-update-icon-cache >/dev/null 2>&1 &&
    gtk-update-icon-cache -qtf "$ICONS" >/dev/null 2>&1 || true

echo "Installed:"
echo "  $APPS/norrlog.desktop"
[ -d "$DESKTOP_DIR" ] && echo "  $DESKTOP_DIR/norrlog.desktop"
echo "  icons under $ICONS"
