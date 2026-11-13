#!/bin/bash
# Removes Norrlog from this Mac. Double-click it in Finder, or run it in a
# terminal. EverQuest's own logs are never touched.
#
# Ships in the disk image as "Uninstall Norrlog.command", and inside the app
# at Norrlog.app/Contents/Resources/uninstall.command. Adapted from Bard's.

set -u

data="$HOME/Library/Application Support/norrlog"
config="$HOME/Library/Application Support/eqlparser-gui"
state="$HOME/Library/Saved Application State/io.github.hammackj.norrlog.savedState"

echo "Uninstall Norrlog"
echo

# The app first, politely, then the daemon, which has no window to ask. It
# saves as it goes, so stopping it loses nothing.
if pgrep -x norrlog >/dev/null; then
  echo "Quitting Norrlog..."
  osascript -e 'tell application "Norrlog" to quit' >/dev/null 2>&1
  sleep 2
  pkill -x norrlog 2>/dev/null
fi
pkill -x norrlogd 2>/dev/null

removed=0
for app in "/Applications/Norrlog.app" "$HOME/Applications/Norrlog.app"; do
  if [ -d "$app" ]; then
    if rm -rf "$app" 2>/dev/null || sudo rm -rf "$app"; then
      echo "Removed $app"
      removed=1
    else
      echo "Could not remove $app; drag it to the Trash instead."
    fi
  fi
done
[ "$removed" = 1 ] || echo "Norrlog.app was not found in Applications."

echo
echo "Norrlog's own data is kept unless you remove it here: fight history"
echo "and state in"
echo "  $data"
echo "and settings in"
echo "  $config"
read -r -p "Delete them too? [y/N] " answer
case "$answer" in
  [yY]*)
    rm -rf "$data" "$config" "$state"
    echo "Deleted."
    ;;
  *)
    echo "Kept; a reinstall picks up where you left off."
    ;;
esac

echo
echo "Done. You can close this window."
