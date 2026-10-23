#!/usr/bin/env bash
# Builds heyListen from this checkout, installs it as /Applications/heyListen.app and restarts
# it. For trying local changes in the real app. Refuses while a meeting is being recorded.
set -euo pipefail
cd "$(dirname "$0")/.."

# The recorder's state file names its process; a live one means a meeting is being recorded.
# (Matching process names would also match any shell whose command mentions the recorder.)
state="$HOME/Library/Application Support/heylisten/recorder.json"
pid=$(sed -n 's/.*"pid": *\([0-9]*\).*/\1/p' "$state" 2>/dev/null || true)
if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
    echo "A meeting is being recorded. Stop it first, then run this again." >&2
    exit 1
fi

scripts/bundle-macos.sh

pkill -f "heyListen.app/Contents/MacOS/heylisten-tray" || true
rm -rf /Applications/heyListen.app
ditto target/heyListen.app /Applications/heyListen.app
open /Applications/heyListen.app
echo "Installed and started /Applications/heyListen.app"
