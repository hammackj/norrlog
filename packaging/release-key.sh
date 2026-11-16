#!/bin/sh
# Make the release signing key: run once, by whoever publishes releases.
#
#   packaging/release-key.sh [DIR]
#
# Writes DIR/norrlog-release.key (secret; default ~/.minisign) and prints the
# public key. Then:
#   1. Put the public key (the second line of the .pub) in PUBLIC_KEY in
#      crates/norrlog/src/update.rs, and commit it. Builds made before that
#      carry no key and never install an update.
#   2. Store the secret as the repository's MINISIGN_KEY secret:
#        gh secret set MINISIGN_KEY < DIR/norrlog-release.key
#   3. Keep a copy of the secret somewhere safe. If it is lost, releases
#      signed with a new key are refused by every copy already installed,
#      which then has to be updated by hand once.
#
# No password (-W): the key lives in a CI secret, which a password typed
# into the same secret would not protect. Needs minisign
# (pacman -S minisign, apt install minisign, brew install minisign).
set -eu
dir=${1:-"$HOME/.minisign"}
mkdir -p "$dir"
umask 077
minisign -G -W -s "$dir/norrlog-release.key" -p "$dir/norrlog-release.pub"
echo
echo "Public key (for PUBLIC_KEY in crates/norrlog/src/update.rs):"
sed -n 2p "$dir/norrlog-release.pub"
