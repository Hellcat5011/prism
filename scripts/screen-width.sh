#!/usr/bin/env bash
# screen-width.sh — print the pixel width of the focused or widest output.
#
# Supports Niri, MangoWM, Hyprland, and generic wlroots (wlr-randr).
# Falls back to empty string (exit 0) so callers apply their default:
#     SCREEN_W="$(screen-width.sh)"; SCREEN_W="${SCREEN_W:-1920}"

w=""

# 1. Hyprland
if [ -n "${HYPRLAND_INSTANCE_SIGNATURE:-}" ] || command -v hyprctl >/dev/null 2>&1; then
    w="$(hyprctl monitors -j 2>/dev/null \
        | jq -r '([.[] | select(.focused == true).width // empty][0]) // ([.[].width] | max // empty)' 2>/dev/null)" || w=""
fi

# 2. MangoWM
if [ -z "$w" ] && { [ -n "${MANGO_INSTANCE_SIGNATURE:-}" ] || command -v mmsg >/dev/null 2>&1; }; then
    w="$(mmsg get all-monitors 2>/dev/null \
        | jq -r '([.[] | select(.focused == true).width // empty][0]) // ([.[] | .width // empty] | max // empty)' 2>/dev/null)" || w=""
fi

# 3. Niri
if [ -z "$w" ] && { [ -n "${NIRI_SOCKET:-}" ] || command -v niri >/dev/null 2>&1; }; then
    w="$(niri msg --json focused-output 2>/dev/null \
        | jq -r 'select(. != null and .current_mode != null) | .modes[.current_mode].width // empty' 2>/dev/null)" || w=""
    if [ -z "$w" ]; then
        w="$(niri msg --json outputs 2>/dev/null \
            | jq -r '[.[] | select(.current_mode != null) | .modes[.current_mode].width] | max // empty' 2>/dev/null)" || w=""
    fi
fi

# 4. Generic wlroots fallback via wlr-randr
if [ -z "$w" ] && command -v wlr-randr >/dev/null 2>&1; then
    w="$(wlr-randr --json 2>/dev/null \
        | jq -r '[.[].modes[] | select(.current == true).width] | max // empty' 2>/dev/null)" || w=""
fi

printf '%s' "$w"
exit 0
