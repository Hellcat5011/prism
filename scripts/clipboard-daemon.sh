#!/usr/bin/env bash

if ! command -v wl-paste >/dev/null 2>&1; then
    echo "clipboard-daemon.sh: wl-paste (wl-clipboard) not found. Clipboard history disabled." >&2
    exit 0
fi

# Kill existing QuickShell clipboard watchers to avoid duplicates
pkill -f "wl-paste -t text/plain --watch .*clipboard-add.sh text" 2>/dev/null || true
pkill -f "wl-paste -t image/png --watch .*clipboard-add.sh image" 2>/dev/null || true
pkill -f "clipboard-prune.sh" 2>/dev/null || true

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

guard=()
command -v setpriv >/dev/null 2>&1 && guard=(setpriv --pdeathsig TERM)

cleanup() {
    trap - EXIT INT TERM HUP
    kill $(jobs -p) 2>/dev/null || true
}
trap cleanup EXIT INT TERM HUP

"${guard[@]}" wl-paste -t text/plain --watch "$SCRIPT_DIR/clipboard-add.sh" text &
"${guard[@]}" wl-paste -t image/png --watch "$SCRIPT_DIR/clipboard-add.sh" image &

# Start the 24-hour auto-expiry pruner in the background
"${guard[@]}" "$SCRIPT_DIR/clipboard-prune.sh" &

wait
