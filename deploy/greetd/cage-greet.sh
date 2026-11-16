#!/usr/bin/env bash
# cage-greet.sh — Run Prism greeter directly in a single-window Cage kiosk compositor
#
# Cage is a Wayland kiosk compositor that displays a single fullscreen application
# and terminates cleanly as soon as that application exits.
set -euo pipefail

if ! command -v cage >/dev/null 2>&1; then
    echo "Error: 'cage' is not installed. Install cage or use niri-greet.kdl / mango-greet.conf." >&2
    exit 1
fi

exec cage -s -- /usr/local/bin/prism-lock --greeter
