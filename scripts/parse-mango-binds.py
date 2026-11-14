#!/usr/bin/env python3
"""parse-mango-binds.py — parse MangoWM keybindings into JSON for KeybindViewer.

Usage:
    parse-mango-binds.py [path/to/config.conf]

Locates config via:
    $MANGO_CONFIG, else $XDG_CONFIG_HOME/mango/config.conf, else ~/.config/mango/config.conf

Follows `source = ...` and `source_optional = ...` directives recursively.
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
    "btn_left": "LMB",
    "btn_right": "RMB",
    "btn_middle": "MMB",
    "btn_side": "Side Button",
    "btn_extra": "Extra Button",
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
    "killclient": ("Close Window", "Windows"),
    "quit": ("Exit MangoWM", "Session & System"),
    "toggleoverview": ("Toggle Overview", "Windows"),
    "sleep_toggle_monitor": ("Toggle Monitor Power", "Session & System"),
    "reload_config": ("Reload Configuration", "Session & System"),
    "togglefloating": ("Toggle Floating", "Windows"),
    "togglemaximizescreen": ("Toggle Maximize", "Windows"),
    "togglefullscreen": ("Toggle Fullscreen", "Windows"),
    "togglefakefullscreen": ("Toggle Fake Fullscreen", "Windows"),
    "toggleglobal": ("Toggle Pin on All Workspaces", "Windows"),
    "minimized": ("Minimize Window", "Windows"),
    "restore_minimized": ("Restore Minimized Window", "Windows"),
    "toggle_scratchpad": ("Toggle Scratchpad", "Windows"),
    "togglejump": ("Jump to Previous Window", "Windows"),
    "switch_proportion_preset": ("Cycle Column Width", "Columns"),
    "centerwin": ("Center Window in Column", "Columns"),
    "togglegaps": ("Toggle Gaps", "Windows"),
    "switch_layout": ("Switch Layout", "Windows"),
    "dwindle_toggle_split_direction": ("Toggle Split Direction", "Windows"),
    "tag_special_tag": ("Move to Special Workspace", "Workspaces"),
    "viewtoleft_have_client": ("Switch to Left Active Workspace", "Workspaces"),
    "viewtoright_have_client": ("Switch to Right Active Workspace", "Workspaces"),
}

SECTION_ORDER = [
    "Shell", "Applications", "Media & Hardware", "Windows", "Columns",
    "Workspaces", "Monitors", "Screenshots", "Session & System", "Other"
]


def find_config():
    if "MANGO_CONFIG" in os.environ and os.path.isfile(os.environ["MANGO_CONFIG"]):
        return os.environ["MANGO_CONFIG"]
    xdg = os.environ.get("XDG_CONFIG_HOME", os.path.expanduser("~/.config"))
    path = os.path.join(xdg, "mango", "config.conf")
    if os.path.isfile(path):
        return path
    return os.path.expanduser("~/.config/mango/config.conf")


def normalize_mods(mods_str):
    raw = [m.strip().lower() for m in re.split(r"[+\s]+", mods_str) if m.strip()]
    cleaned = set()
    for r in raw:
        if r in ("none", ""):
            continue
        if r in MOD_NAMES:
            cleaned.add(MOD_NAMES[r])
    return sorted(list(cleaned), key=lambda x: MOD_ORDER.get(x, 99))


def normalize_key(key_str, directive):
    low = key_str.strip().lower()
    if directive == "axisbind":
        if low == "up":
            return "Scroll Up"
        if low == "down":
            return "Scroll Down"
        return "Scroll " + key_str.strip()

    if low in KEY_NAMES:
        return KEY_NAMES[low]
    if len(key_str) == 1:
        return key_str.upper()
    return key_str.strip()


def format_combo(mods_str, key_str, directive):
    mods = normalize_mods(mods_str)
    key = normalize_key(key_str, directive)
    if mods:
        return " + ".join(mods + [key])
    return key


def human_cmd(cmd):
    c = cmd.strip()
    # Check for QuickShell IPC calls
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
    if "set-brightness.sh" in low:
        if "+" in c:
            return "Brightness +5%", "Media & Hardware"
        if "-" in c:
            return "Brightness -5%", "Media & Hardware"
        return "Adjust Brightness", "Media & Hardware"
    if "brightnessctl" in low:
        if "+" in c:
            return "Brightness +10%", "Media & Hardware"
        if "-" in c:
            return "Brightness -10%", "Media & Hardware"
        return "Adjust Brightness", "Media & Hardware"
    if "playerctl" in low:
        if "play-pause" in low:
            return "Play / Pause", "Media & Hardware"
        if "next" in low:
            return "Next Track", "Media & Hardware"
        if "prev" in low or "previous" in low:
            return "Previous Track", "Media & Hardware"
        if "stop" in low:
            return "Stop Playback", "Media & Hardware"
        return "Media Control", "Media & Hardware"
    if "ghostty" in low:
        return "Terminal (Ghostty)", "Applications"
    if "firefox" in low:
        return "Web Browser (Firefox)", "Applications"
    if "nautilus" in low:
        return "File Manager (Nautilus)", "Applications"
    if "rofi" in low:
        return "Application Launcher (Rofi)", "Applications"
    if "orca" in low:
        return "Screen Reader (Orca)", "Applications"
    if "pavucontrol" in low:
        return "Audio Settings", "Applications"
    if "blueman" in low:
        return "Bluetooth Manager", "Applications"
    if "grim" in low or "slurp" in low or "screenshot" in low:
        return "Take Screenshot", "Screenshots"

    short = c if len(c) <= 50 else c[:47] + "…"
    return "Run: " + short, "Applications"


def humanise_action(dispatcher, args_list, cmd):
    disp = dispatcher.strip()
    disp_low = disp.lower()

    if disp_low in ("spawn", "spawn_shell"):
        return human_cmd(cmd)

    # Directional focus
    if disp_low == "focusdir":
        arg = args_list[0].lower() if args_list else ""
        return f"Focus Window {arg.capitalize()}", "Windows"

    # Focus stack
    if disp_low == "focusstack":
        arg = args_list[0].lower() if args_list else "next"
        return f"Focus {arg.capitalize()} in Stack", "Windows"

    # Move client / window
    if disp_low == "move_client":
        arg = args_list[0].lower() if args_list else ""
        return f"Move Window {arg.capitalize()}", "Windows"

    # Focus monitor
    if disp_low == "focusmon":
        arg = args_list[0].lower() if args_list else ""
        return f"Focus Monitor {arg.capitalize()}", "Monitors"

    # Tag monitor (move client to monitor)
    if disp_low == "tagmon":
        arg = args_list[0].lower() if args_list else ""
        return f"Move to Monitor {arg.capitalize()}", "Monitors"

    # Workspace view / tag
    if disp_low == "viewtoleft":
        return "Switch to Left Workspace", "Workspaces"
    if disp_low == "viewtoright":
        return "Switch to Right Workspace", "Workspaces"
    if disp_low == "tagtoleft":
        return "Move Window to Left Workspace", "Workspaces"
    if disp_low == "tagtoright":
        return "Move Window to Right Workspace", "Workspaces"
    if disp_low == "view":
        tag = args_list[0] if args_list else ""
        return f"Switch to Workspace {tag}", "Workspaces"
    if disp_low == "tag":
        tag = args_list[0] if args_list else ""
        return f"Move Window to Workspace {tag}", "Workspaces"

    # Scroller layout
    if disp_low == "scroller_stack":
        arg = args_list[0].lower() if args_list else ""
        return f"Stack Column {arg.capitalize()}", "Columns"
    if disp_low == "set_proportion":
        val = args_list[0] if args_list else "1.0"
        return f"Set Column Width ({val})", "Columns"

    # Window resize & move
    if disp_low == "resizewin":
        w = args_list[0] if len(args_list) > 0 else ""
        h = args_list[1] if len(args_list) > 1 else ""
        if w and w != "+0" and w != "-0" and w != "0":
            return f"Resize Window Width ({w}px)", "Windows"
        if h and h != "+0" and h != "-0" and h != "0":
            return f"Resize Window Height ({h}px)", "Windows"
        return "Resize Window", "Windows"

    if disp_low == "movewin":
        x = args_list[0] if len(args_list) > 0 else ""
        y = args_list[1] if len(args_list) > 1 else ""
        if x and x != "+0" and x != "-0" and x != "0":
            return f"Move Window Horizontally ({x}px)", "Windows"
        if y and y != "+0" and y != "-0" and y != "0":
            return f"Move Window Vertically ({y}px)", "Windows"
        return "Move Window", "Windows"

    # Gaps
    if disp_low == "incgaps":
        val = args_list[0] if args_list else "1"
        return f"Adjust Gaps ({val})", "Windows"

    # Mouse actions
    if disp_low == "moveresize":
        mode = args_list[0].lower() if args_list else ""
        if "move" in mode:
            return "Move Window (Drag)", "Windows"
        if "resize" in mode:
            return "Resize Window (Drag)", "Windows"
        return "Move / Resize (Drag)", "Windows"

    if disp_low in DISPATCHER_LABELS:
        return DISPATCHER_LABELS[disp_low]

    # Fallback formatting
    label = " ".join(w.capitalize() for w in disp.replace("_", " ").split())
    if args_list:
        label += " " + " ".join(args_list)
    return label, "Other"


def parse_mango_file(filepath, visited=None, depth=0):
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
        print(f"parse-mango-binds: failed to read {real_path}: {e}", file=sys.stderr)
        return []

    for line in lines:
        line = line.strip()
        if not line or line.startswith("#"):
            continue

        # Strip trailing comments if separated by space
        # (Be careful not to strip # inside strings or commands unless standard # comment)
        if " #" in line:
            line = line.split(" #", 1)[0].strip()

        # Check for source includes
        src_match = re.match(r"^(?:source|source_optional)\s*=\s*(.+)$", line)
        if src_match:
            inc_target = src_match.group(1).strip()
            inc_target = os.path.expanduser(inc_target)
            if not os.path.isabs(inc_target):
                inc_target = os.path.normpath(os.path.join(base_dir, inc_target))
            rows.extend(parse_mango_file(inc_target, visited, depth + 1))
            continue

        # Check for bind directives
        bind_match = re.match(r"^(bindl|bind|mousebind|axisbind)\s*=\s*(.+)$", line)
        if not bind_match:
            continue

        directive = bind_match.group(1)
        val = bind_match.group(2).strip()

        parts = val.split(",", 3)
        if len(parts) < 3:
            continue

        mods_str = parts[0].strip()
        key_str = parts[1].strip()
        dispatcher = parts[2].strip()
        rest = parts[3].strip() if len(parts) > 3 else ""

        if dispatcher in ("spawn", "spawn_shell"):
            cmd = rest
            args_list = []
        else:
            cmd = ""
            args_list = [a.strip() for a in rest.split(",") if a.strip()]

        label, section = humanise_action(dispatcher, args_list, cmd)
        combo = format_combo(mods_str, key_str, directive)

        rows.append({
            "section": section,
            "key": combo,
            "action": label
        })

    return rows


def collect_rows(config_path):
    rows = parse_mango_file(config_path)
    order = {s: i for i, s in enumerate(SECTION_ORDER)}
    rows.sort(key=lambda r: order.get(r["section"], len(order)))
    return rows


def main(argv):
    path = argv[1] if len(argv) > 1 else find_config()
    if not os.path.isfile(path):
        print("[]")
        print("parse-mango-binds: config not found: %s" % path, file=sys.stderr)
        return 2

    rows = collect_rows(path)
    print(json.dumps(rows, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
