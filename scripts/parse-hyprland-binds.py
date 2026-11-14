#!/usr/bin/env python3
"""parse-hyprland-binds.py — parse Hyprland keybindings into JSON for KeybindViewer.

Usage:
    parse-hyprland-binds.py [path/to/hyprland.conf]

Locates config via:
    $HYPRLAND_CONFIG, else $XDG_CONFIG_HOME/hypr/hyprland.conf, else ~/.config/hypr/hyprland.conf

Follows `source = ...` directives recursively.
Prints a JSON array on stdout:
    [{"section": "Shell", "key": "SUPER + Space", "action": "App Launcher"}, ...]
"""
import json
import os
import re
import sys

MAX_SOURCE_DEPTH = 10

MOD_ORDER = {"SUPER": 0, "CTRL": 1, "ALT": 2, "SHIFT": 3}

MOD_NAMES = {
    "super": "SUPER", "mod": "SUPER", "win": "SUPER", "logo": "SUPER",
    "ctrl": "CTRL", "control": "CTRL",
    "alt": "ALT",
    "shift": "SHIFT",
}

KEY_NAMES = {
    "return": "Enter",
    "space": "Space",
    "slash": "/",
    "backslash": "\\",
    "bracketleft": "[",
    "bracketright": "]",
    "minus": "-",
    "equal": "=",
    "plus": "Plus",
    "comma": ",",
    "period": ".",
    "semicolon": ";",
    "apostrophe": "'",
    "grave": "`",
    "escape": "Esc",
    "delete": "Del",
    "tab": "Tab",
    "page_up": "PgUp",
    "page_down": "PgDn",
    "prior": "PgUp",
    "next": "PgDn",
    "print": "PrtSc",
    "left": "Left",
    "right": "Right",
    "up": "Up",
    "down": "Down",
    "mouse:272": "LMB",
    "mouse:273": "RMB",
    "mouse:274": "MMB",
    "mouse_down": "Scroll Down",
    "mouse_up": "Scroll Up",
}

QS_NAMES = {
    "launcher toggle": "App Launcher",
    "launcher open": "Open App Launcher",
    "launcher close": "Close App Launcher",
    "search toggle": "Spotlight Search",
    "search open": "Open Spotlight Search",
    "search close": "Close Spotlight Search",
    "keybinds toggle": "Keybind Viewer",
    "keybinds open": "Open Keybind Viewer",
    "keybinds close": "Close Keybind Viewer",
    "lock lock": "Lock Screen",
    "power toggle": "Power Menu",
    "power open": "Open Power Menu",
    "power close": "Close Power Menu",
    "wallpaper toggle": "Wallpaper Picker",
    "wallpaper open": "Open Wallpaper Picker",
    "colorpicker toggle": "Color Picker",
    "clipboard toggle": "Clipboard History",
    "emoji toggle": "Emoji Picker",
    "notif toggle": "Notification Center",
    "settings toggle": "Settings",
    "desktopedit toggle": "Desktop Edit Mode",
    "bluelight toggle": "Blue Light Filter",
}

DISPATCHER_LABELS = {
    "killactive": ("Close Window", "Windows"),
    "exit": ("Exit Hyprland", "Session & System"),
    "togglefloating": ("Toggle Floating", "Windows"),
    "fullscreen": ("Toggle Fullscreen", "Windows"),
    "togglesplit": ("Toggle Split Direction", "Windows"),
    "cyclenext": ("Focus Next Window", "Windows"),
    "swapnext": ("Swap Next Window", "Windows"),
}

SECTION_ORDER = [
    "Shell", "Applications", "Media & Hardware", "Windows", "Columns",
    "Workspaces", "Monitors", "Screenshots", "Session & System", "Other"
]


def find_config():
    if "HYPRLAND_CONFIG" in os.environ and os.path.isfile(os.environ["HYPRLAND_CONFIG"]):
        return os.environ["HYPRLAND_CONFIG"]
    xdg = os.environ.get("XDG_CONFIG_HOME", os.path.expanduser("~/.config"))
    path = os.path.join(xdg, "hypr", "hyprland.conf")
    if os.path.isfile(path):
        return path
    return os.path.expanduser("~/.config/hypr/hyprland.conf")


def normalize_mods(mods_str):
    raw = [m.strip().lower() for m in re.split(r"[+\s]+", mods_str) if m.strip()]
    cleaned = set()
    for r in raw:
        if r in ("none", ""):
            continue
        if r in MOD_NAMES:
            cleaned.add(MOD_NAMES[r])
    return sorted(list(cleaned), key=lambda x: MOD_ORDER.get(x, 99))


def normalize_key(key_str):
    low = key_str.strip().lower()
    if low in KEY_NAMES:
        return KEY_NAMES[low]
    if len(key_str) == 1:
        return key_str.upper()
    return key_str.strip()


def format_combo(mods_str, key_str):
    mods = normalize_mods(mods_str)
    key = normalize_key(key_str)
    if mods:
        return " + ".join(mods + [key])
    return key


def human_cmd(cmd):
    c = cmd.strip()
    m = re.search(r"\b(?:qs|quickshell)\b.*?\bipc\s+call\s+(\w+)\s+(\w+)", c)
    if m:
        key = m.group(1).lower() + " " + m.group(2).lower()
        if key in QS_NAMES:
            return QS_NAMES[key], "Shell"
        return m.group(1).capitalize() + " → " + m.group(2), "Shell"

    low = c.lower()
    if "killall qs" in low or "pkill qs" in low or ("qs" in low and "& disown" in low):
        return "Restart Shell", "Session & System"
    if "volume.sh" in low:
        if "+5" in c or "+" in c:
            return "Volume +5%", "Media & Hardware"
        if "-5" in c or "-" in c:
            return "Volume -5%", "Media & Hardware"
        if "mute" in low:
            return "Mute Audio", "Media & Hardware"
        return "Volume Control", "Media & Hardware"
    if "brightness" in low:
        return "Adjust Brightness", "Media & Hardware"
    if "playerctl" in low:
        return "Media Control", "Media & Hardware"

    short = c if len(c) <= 50 else c[:47] + "…"
    return "Run: " + short, "Applications"


def humanise_action(dispatcher, arg):
    disp = dispatcher.strip()
    disp_low = disp.lower()
    arg_clean = arg.strip()

    if disp_low in ("exec", "exec-once"):
        return human_cmd(arg_clean)

    if disp_low == "movefocus":
        return f"Focus Window {arg_clean.capitalize()}", "Windows"
    if disp_low == "movewindow":
        return f"Move Window {arg_clean.capitalize()}", "Windows"
    if disp_low == "workspace":
        return f"Switch to Workspace {arg_clean}", "Workspaces"
    if disp_low == "movetoworkspace":
        return f"Move Window to Workspace {arg_clean}", "Workspaces"

    if disp_low in DISPATCHER_LABELS:
        return DISPATCHER_LABELS[disp_low]

    label = " ".join(w.capitalize() for w in disp.replace("_", " ").split())
    if arg_clean:
        label += " " + arg_clean
    return label, "Other"


def parse_hypr_file(filepath, visited=None, depth=0):
    if visited is None:
        visited = set()
    if depth > MAX_SOURCE_DEPTH:
        return []

    real_path = os.path.realpath(filepath)
    if real_path in visited or not os.path.isfile(real_path):
        return []
    visited.add(real_path)

    base_dir = os.path.dirname(real_path)
    rows = []

    try:
        with open(real_path, "r", encoding="utf-8", errors="replace") as f:
            lines = f.readlines()
    except Exception as e:
        print(f"parse-hyprland-binds: failed to read {real_path}: {e}", file=sys.stderr)
        return []

    for line in lines:
        line = line.strip()
        if not line or line.startswith("#"):
            continue

        if " #" in line:
            line = line.split(" #", 1)[0].strip()

        src_match = re.match(r"^source\s*=\s*(.+)$", line)
        if src_match:
            inc_target = src_match.group(1).strip()
            inc_target = os.path.expanduser(inc_target)
            if not os.path.isabs(inc_target):
                inc_target = os.path.normpath(os.path.join(base_dir, inc_target))
            rows.extend(parse_hypr_file(inc_target, visited, depth + 1))
            continue

        bind_match = re.match(r"^(bind[a-z]*)\s*=\s*(.+)$", line)
        if not bind_match:
            continue

        val = bind_match.group(2).strip()
        parts = [p.strip() for p in val.split(",", 3)]
        if len(parts) < 3:
            continue

        mods_str = parts[0]
        key_str = parts[1]
        dispatcher = parts[2]
        arg = parts[3] if len(parts) > 3 else ""

        label, section = humanise_action(dispatcher, arg)
        combo = format_combo(mods_str, key_str)

        rows.append({
            "section": section,
            "key": combo,
            "action": label
        })

    return rows


def collect_rows(config_path):
    rows = parse_hypr_file(config_path)
    order = {s: i for i, s in enumerate(SECTION_ORDER)}
    rows.sort(key=lambda r: order.get(r["section"], len(order)))
    return rows


def main(argv):
    path = argv[1] if len(argv) > 1 else find_config()
    if not os.path.isfile(path):
        print("[]")
        print("parse-hyprland-binds: config not found: %s" % path, file=sys.stderr)
        return 2

    rows = collect_rows(path)
    print(json.dumps(rows, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
