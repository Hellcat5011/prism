pragma Singleton
import QtQuick
import Quickshell

QtObject {
    id: root

    readonly property string type: {
        if (Quickshell.env("MANGO_INSTANCE_SIGNATURE")) return "mango"
        if (Quickshell.env("HYPRLAND_INSTANCE_SIGNATURE")) return "hyprland"
        if (Quickshell.env("NIRI_SOCKET")) return "niri"
        if (Quickshell.env("SWAYSOCK")) return "sway"
        return "unknown"
    }

    readonly property string displayName: {
        switch (type) {
            case "mango": return "MangoWM"
            case "hyprland": return "Hyprland"
            case "niri": return "Niri"
            case "sway": return "Sway"
            default: return "Wayland"
        }
    }

    readonly property bool isNiri: type === "niri"
    readonly property bool isMango: type === "mango"
    readonly property bool isHyprland: type === "hyprland"

    function quitCommand() {
        switch (type) {
            case "mango":
                return "mmsg quit"
            case "hyprland":
                return "hyprctl dispatch exit"
            case "niri":
                return "niri msg action quit --skip-confirmation"
            case "sway":
                return "swaymsg exit"
            default:
                return "loginctl terminate-session self"
        }
    }
}

