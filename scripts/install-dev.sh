#!/usr/bin/env bash
# Builds heyListen from this checkout, installs it as /Applications/heyListen.app and restarts
# it. For trying local changes in the real app. Refuses while a meeting is being recorded.
set -euo pipefail
cd "$(dirname "$0")/.."

if pgrep -f "__record" > /dev/null; then
    echo "A meeting is being recorded. Stop it first, then run this again." >&2
    exit 1
fi

scripts/bundle-macos.sh

pkill -f "heyListen.app/Contents/MacOS/heylisten-tray" || true
rm -rf /Applications/heyListen.app
ditto target/heyListen.app /Applications/heyListen.app
open /Applications/heyListen.app
echo "Installed and started /Applications/heyListen.app"
