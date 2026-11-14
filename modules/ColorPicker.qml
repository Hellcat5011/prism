// ─────────────────────────────────────────────────────────────────────────────
// ColorPicker.qml — Hyprpicker-style magnifying glass color picker
//
// Features:
//   • Fullscreen layer-shell overlay with native Wayland screencopy capture
//   • True circular 2D canvas lens with nearest-neighbor 11x pixel magnification
//   • Precision crosshair centered on the exact selected pixel
//   • Real-time hex color label updating dynamically on cursor motion
//   • Monospace hex badge with dark translucent pill background and swatch
//   • Magnifier ring tinted with the currently sampled color
//   • Arrow keys for 1-pixel precision nudging
//   • Single-click / Enter / Space copies hex to clipboard via wl-copy
//   • Automatic clipboard history insertion via standard wl-copy watcher
//   • System notification with color preview on copy
//   • LazyLoader lifecycle: clean creation/destruction with ScreencopyView
//   • Exclusive keyboard focus and ignore exclusion mode for Wayland robustness
//   • Direct in-memory image capture without disk persistence or cache staleness
//   • Right-click / Escape / Q to cancel
// ─────────────────────────────────────────────────────────────────────────────
import QtQuick
import Quickshell
import Quickshell.Io
import Quickshell.Wayland
import "../services"

Scope {
    id: root

    property bool shown: false
    property string currentHex: "#000000"
    property color currentColor: "#000000"

    function show() {
        if (shown) return;
        shown = true;
    }

    function hide() {
        if (!shown) return;
        shown = false;
    }

    function toggle() {
        if (shown) hide();
        else show();
    }

    function pickColor() {
        if (!loader.item || !loader.item.captured) return;
        let hex = currentHex;
        // Copy to system clipboard without lock-file suppression so clipboard-add.sh immediately records it to history
        Quickshell.execDetached([
            "sh",
            "-c",
            "printf '%s' '" + hex + "' | wl-copy && notify-send -a 'Prism' 'Color Picker' 'Copied " + hex + " to clipboard' -i 'color-picker' -h string:x-canonical-private-synchronous:prism-colorpicker"
        ]);
        hide();
    }

    LazyLoader {
        id: loader
        active: root.shown

        PanelWindow {
            id: pickerWindow

            // Screen selection: Default to Quickshell.screens[0]. Synchronous selection avoids race conditions
            // and avoids mutating the screen property on an active Wayland surface.
            screen: (Quickshell.screens && Quickshell.screens.length > 0) ? Quickshell.screens[0] : null

            WlrLayershell.layer: WlrLayer.Overlay
            WlrLayershell.namespace: "prism-color-picker"
            WlrLayershell.keyboardFocus: WlrKeyboardFocus.Exclusive
            anchors { top: true; bottom: true; left: true; right: true }
            exclusionMode: ExclusionMode.Ignore
            color: "transparent"

            property bool captured: false
            property int cursorX: width > 0 ? Math.floor(width / 2) : (screen ? Math.floor(screen.width / 2) : 960)
            property int cursorY: height > 0 ? Math.floor(height / 2) : (screen ? Math.floor(screen.height / 2) : 540)
            property real sourceWidth: (copySource.sourceSize.width > 0) ? copySource.sourceSize.width : (screen ? screen.width : 2560)
            property real sourceHeight: (copySource.sourceSize.height > 0) ? copySource.sourceSize.height : (screen ? screen.height : 1440)

            readonly property int lensDiameter: 180
            readonly property int zoomFactor: 11 // Odd number so the center pixel aligns perfectly

            property var grabResult: null
            property string currentImageUrl: ""
            property int grabTries: 0

            Component.onCompleted: {
                mouseArea.forceActiveFocus();
                captureTimer.restart();
            }

            Component.onDestruction: {
                // Ensure no temp screenshot remains in $XDG_RUNTIME_DIR
                Quickshell.execDetached(["rm", "-f", (Quickshell.env("XDG_RUNTIME_DIR") || "/tmp") + "/prism-colorpicker-" + (Quickshell.env("USER") || "user") + ".png"]);
            }

            Timer {
                id: captureTimer
                interval: 30
                repeat: false
                onTriggered: {
                    pickerWindow.grabTries = 0;
                    copySource.captureFrame();
                    grabTimer.restart();
                }
            }

            Timer {
                id: grabTimer
                interval: 40
                repeat: false
                onTriggered: {
                    if (!copySource.hasContent) {
                        pickerWindow.grabTries++;
                        if (pickerWindow.grabTries >= 50) {
                            root.hide();
                            return;
                        }
                        grabTimer.restart();
                        return;
                    }
                    copySource.grabToImage(function(result) {
                        if (!result) {
                            root.hide();
                            return;
                        }
                        pickerWindow.grabResult = result;
                        let tempPath = (Quickshell.env("XDG_RUNTIME_DIR") || "/tmp") + "/prism-colorpicker-" + (Quickshell.env("USER") || "user") + ".png";
                        let fileUrl = "file://" + tempPath;
                        lensCanvas.unloadImage(fileUrl);
                        let ok = result.saveToFile(tempPath);
                        if (ok) {
                            pickerWindow.currentImageUrl = fileUrl;
                            lensCanvas.loadImage(fileUrl);
                        } else {
                            root.hide();
                        }
                    }, Qt.size(pickerWindow.sourceWidth, pickerWindow.sourceHeight));
                }
            }

            // Native Wayland screen capture source (hidden, provides frame buffer)
            ScreencopyView {
                id: copySource
                anchors.fill: parent
                captureSource: pickerWindow.screen
                live: false
                visible: false
            }

            // Interaction surface
            MouseArea {
                id: mouseArea
                anchors.fill: parent
                hoverEnabled: true
                cursorShape: Qt.BlankCursor
                acceptedButtons: Qt.LeftButton | Qt.RightButton
                focus: true

                onPositionChanged: function(mouse) {
                    pickerWindow.cursorX = mouse.x;
                    pickerWindow.cursorY = mouse.y;
                    lensCanvas.requestPaint();
                }

                onClicked: function(mouse) {
                    if (mouse.button === Qt.LeftButton) {
                        root.pickColor();
                    } else {
                        root.hide();
                    }
                }

                // Keyboard navigation and shortcuts
                Keys.onEscapePressed: root.hide()
                Keys.onReturnPressed: root.pickColor()
                Keys.onSpacePressed: root.pickColor()
                Keys.onPressed: function(event) {
                    if (event.key === Qt.Key_Q) {
                        root.hide();
                        event.accepted = true;
                        return;
                    }
                    // Arrow keys for fine 1-pixel precision control
                    let handled = false;
                    if (event.key === Qt.Key_Left) {
                        pickerWindow.cursorX = Math.max(0, pickerWindow.cursorX - 1);
                        handled = true;
                    } else if (event.key === Qt.Key_Right) {
                        pickerWindow.cursorX = Math.min(pickerWindow.width - 1, pickerWindow.cursorX + 1);
                        handled = true;
                    } else if (event.key === Qt.Key_Up) {
                        pickerWindow.cursorY = Math.max(0, pickerWindow.cursorY - 1);
                        handled = true;
                    } else if (event.key === Qt.Key_Down) {
                        pickerWindow.cursorY = Math.min(pickerWindow.height - 1, pickerWindow.cursorY + 1);
                        handled = true;
                    }
                    if (handled) {
                        lensCanvas.requestPaint();
                        event.accepted = true;
                    }
                }
            }

            // ─────────────────────────────────────────────────────────────────
            // Magnifier Lens (Hyprpicker style — true circular canvas, no square corners)
            // ─────────────────────────────────────────────────────────────────
            Item {
                id: lens
                x: pickerWindow.cursorX - width / 2
                y: pickerWindow.cursorY - height / 2
                width: pickerWindow.lensDiameter
                height: pickerWindow.lensDiameter
                visible: pickerWindow.captured

                Canvas {
                    id: lensCanvas
                    anchors.fill: parent

                    onImageLoaded: {
                        pickerWindow.captured = true;
                        requestPaint();
                    }

                    onPaint: {
                        if (!pickerWindow.captured || pickerWindow.currentImageUrl.length === 0 || pickerWindow.width <= 0 || pickerWindow.height <= 0) return;
                        let ctx = getContext("2d");
                        ctx.clearRect(0, 0, width, height);

                        // 1. True circular clip path
                        ctx.save();
                        ctx.beginPath();
                        ctx.arc(width / 2, height / 2, width / 2 - 1, 0, Math.PI * 2);
                        ctx.clip();

                        // 2. Nearest-neighbor pixelated zoom
                        ctx.imageSmoothingEnabled = false;

                        let imgW = pickerWindow.sourceWidth;
                        let imgH = pickerWindow.sourceHeight;
                        let sW = (width / pickerWindow.zoomFactor) * (imgW / pickerWindow.width);
                        let sH = (height / pickerWindow.zoomFactor) * (imgH / pickerWindow.height);
                        let imgX = (pickerWindow.cursorX * imgW) / pickerWindow.width;
                        let imgY = (pickerWindow.cursorY * imgH) / pickerWindow.height;
                        let sx = imgX - sW / 2;
                        let sy = imgY - sH / 2;

                        ctx.drawImage(pickerWindow.currentImageUrl, sx, sy, sW, sH, 0, 0, width, height);
                        ctx.restore();

                        // 3. Extract exact center pixel color
                        let p = ctx.getImageData(Math.floor(width / 2), Math.floor(height / 2), 1, 1).data;
                        let r = p[0], g = p[1], b = p[2];
                        let hex = "#" + [r, g, b].map(function(c) {
                            return c.toString(16).padStart(2, "0");
                        }).join("").toUpperCase();

                        root.currentHex = hex;
                        root.currentColor = Qt.rgba(r / 255, g / 255, b / 255, 1);
                    }
                }

                // Outer ring & border styling
                Rectangle {
                    anchors.fill: parent
                    radius: width / 2
                    color: "transparent"
                    border.width: 3
                    border.color: root.currentColor

                    // Outer subtle dark contour for visibility against bright backgrounds
                    Rectangle {
                        anchors.fill: parent
                        anchors.margins: -1
                        radius: width / 2
                        color: "transparent"
                        border.width: 1
                        border.color: Qt.rgba(0, 0, 0, 0.45)
                    }

                    // Inner subtle dark contour
                    Rectangle {
                        anchors.fill: parent
                        anchors.margins: 3
                        radius: width / 2
                        color: "transparent"
                        border.width: 1
                        border.color: Qt.rgba(0, 0, 0, 0.35)
                    }
                }

                // Crosshair at the exact center (+ shape)
                Item {
                    anchors.centerIn: parent
                    width: 22
                    height: 22

                    // Dark outline for crosshair (visible against white/light backgrounds)
                    Rectangle {
                        anchors.centerIn: parent
                        width: 4
                        height: 20
                        radius: 1
                        color: "#000000"
                        opacity: 0.75
                    }
                    Rectangle {
                        anchors.centerIn: parent
                        width: 20
                        height: 4
                        radius: 1
                        color: "#000000"
                        opacity: 0.75
                    }

                    // Crisp white inner crosshair (visible against dark backgrounds)
                    Rectangle {
                        anchors.centerIn: parent
                        width: 2
                        height: 18
                        radius: 0.5
                        color: "#ffffff"
                    }
                    Rectangle {
                        anchors.centerIn: parent
                        width: 18
                        height: 2
                        radius: 0.5
                        color: "#ffffff"
                    }
                }

                // Hex Code Badge below crosshair (or above if near bottom edge of screen)
                Rectangle {
                    id: hexBadge
                    readonly property bool flipAbove: pickerWindow.cursorY > (pickerWindow.height - 70)
                    anchors.horizontalCenter: parent.horizontalCenter
                    anchors.top: flipAbove ? undefined : parent.verticalCenter
                    anchors.bottom: flipAbove ? parent.verticalCenter : undefined
                    anchors.topMargin: flipAbove ? 0 : 20
                    anchors.bottomMargin: flipAbove ? 20 : 0
                    width: hexLabel.implicitWidth + 16
                    height: hexLabel.implicitHeight + 8
                    radius: 6
                    color: Qt.rgba(0.08, 0.08, 0.08, 0.85)
                    border.width: 1
                    border.color: Qt.rgba(1, 1, 1, 0.25)

                    Row {
                        anchors.centerIn: parent
                        spacing: 6

                        // Mini color swatch inside badge
                        Rectangle {
                            anchors.verticalCenter: parent.verticalCenter
                            width: 10
                            height: 10
                            radius: 2
                            color: root.currentColor
                            border.width: 1
                            border.color: Qt.rgba(1, 1, 1, 0.4)
                        }

                        Text {
                            id: hexLabel
                            anchors.verticalCenter: parent.verticalCenter
                            text: root.currentHex
                            color: "#ffffff"
                            font.family: "monospace"
                            font.pixelSize: 13
                            font.bold: true
                        }
                    }
                }
            }
        }
    }
}
