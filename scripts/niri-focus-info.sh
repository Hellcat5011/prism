#!/usr/bin/env bash
# niri-focus-info.sh — report the focused window and focused output in one call.
#
# Supports Niri, MangoWM, and Hyprland.
# Prints a single line of JSON:
#   {"window": {"id": ..., "app_id": ...} | null, "output": {"name": ...} | null}
#
# Used by EmojiPicker.qml to remember which window should receive the paste and
# which monitor the picker should open on. Never fails: if unreachable both fields are null.

win_json="null"
out_json="null"

# 1. Hyprland
if [ -n "${HYPRLAND_INSTANCE_SIGNATURE:-}" ] || command -v hyprctl >/dev/null 2>&1; then
    raw_win="$(hyprctl activewindow -j 2>/dev/null)" || raw_win=""
    if [ -n "$raw_win" ] && [ "$raw_win" != "{}" ]; then
        win_json="$(printf '%s' "$raw_win" | jq -c 'select(.address != null and .address != "") | {id: .address, app_id: (.class // .initialClass // "")}' 2>/dev/null)" || win_json="null"
        [ -z "$win_json" ] && win_json="null"
    fi
    raw_out="$(hyprctl monitors -j 2>/dev/null)" || raw_out=""
    if [ -n "$raw_out" ]; then
        out_json="$(printf '%s' "$raw_out" | jq -c '([.[] | select(.focused == true)][0] // .[0]) | select(. != null) | {name: .name}' 2>/dev/null)" || out_json="null"
        [ -z "$out_json" ] && out_json="null"
    fi
fi

# 2. MangoWM
if [ "$win_json" = "null" ] && [ "$out_json" = "null" ] && { [ -n "${MANGO_INSTANCE_SIGNATURE:-}" ] || command -v mmsg >/dev/null 2>&1; }; then
    raw_win="$(mmsg get focusing-client 2>/dev/null)" || raw_win=""
    if [ -n "$raw_win" ] && [ "$raw_win" != "null" ]; then
        win_json="$(printf '%s' "$raw_win" | jq -c 'select(. != null) | {id: (.id // .window // .pid // "client"), app_id: (.app_id // .class // "")}' 2>/dev/null)" || win_json="null"
        [ -z "$win_json" ] && win_json="null"
    fi
    raw_out="$(mmsg get all-monitors 2>/dev/null)" || raw_out=""
    if [ -n "$raw_out" ]; then
        out_json="$(printf '%s' "$raw_out" | jq -c '([.[] | select(.focused == true)][0] // .[0]) | select(. != null) | {name: .name}' 2>/dev/null)" || out_json="null"
        [ -z "$out_json" ] && out_json="null"
    fi
fi

# 3. Niri
if [ "$win_json" = "null" ] && [ "$out_json" = "null" ] && { [ -n "${NIRI_SOCKET:-}" ] || command -v niri >/dev/null 2>&1; }; then
    raw_win="$(niri msg --json focused-window 2>/dev/null)" || raw_win=""
    if [ -n "$raw_win" ] && [ "$raw_win" != "null" ]; then
        win_json="$(printf '%s' "$raw_win" | jq -c 'select(. != null) | {id: .id, app_id: (.app_id // "")}' 2>/dev/null)" || win_json="null"
        [ -z "$win_json" ] && win_json="null"
    fi
    raw_out="$(niri msg --json focused-output 2>/dev/null)" || raw_out=""
    if [ -n "$raw_out" ] && [ "$raw_out" != "null" ]; then
        out_json="$(printf '%s' "$raw_out" | jq -c 'select(. != null) | {name: .name}' 2>/dev/null)" || out_json="null"
        [ -z "$out_json" ] && out_json="null"
    fi
fi

printf '{"window":%s,"output":%s}\n' "$win_json" "$out_json"
exit 0
