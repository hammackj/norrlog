#!/usr/bin/env bash
# Launch the EQL Parser desktop app.
#
# A launcher rather than a bare `npm start` in the .desktop file: started from
# a menu there is no terminal to read, so a missing dependency or an unbuilt
# daemon would show as a window that never appears. Each of those is checked
# for and reported in a dialog instead.
set -u

REPO="$(cd "$(dirname "$(readlink -f "$0")")/.." && pwd)"
GUI="$REPO/gui"
DAEMON="$REPO/target/release/eqlparserd"

# Whatever the desktop offers. Falls back to stderr when run from a terminal.
fail() {
    if command -v kdialog >/dev/null 2>&1; then
        kdialog --title "EQL Parser" --error "$1"
    elif command -v zenity >/dev/null 2>&1; then
        zenity --error --title="EQL Parser" --text="$1"
    else
        printf 'EQL Parser: %s\n' "$1" >&2
    fi
    exit 1
}

command -v npm >/dev/null 2>&1 ||
    fail "npm was not found on PATH.\n\nThe desktop app is an Electron app and needs Node installed."

[ -x "$DAEMON" ] ||
    fail "The parser daemon has not been built yet.\n\nRun this in $REPO:\n\n    cargo build --release"

[ -d "$GUI/node_modules" ] ||
    fail "The app's dependencies are not installed.\n\nRun this in $GUI:\n\n    npm install"

cd "$GUI" || fail "Could not enter $GUI"

# exec so the launcher does not linger as a parent of the app; the desktop
# tracks the Electron process itself.
exec npm start
