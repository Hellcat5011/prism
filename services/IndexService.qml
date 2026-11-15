// ─────────────────────────────────────────────────────────────────────────
// IndexService.qml — Long-lived singleton managing file index rebuilding
//
// Features:
//   • Runs `pkexec updatedb` with fixed argv (no shell strings, no credentials)
//   • Pre-flight verification of running updatedb processes and polkit agent
//   • Persists across SettingsWindow open/close lifecycles
//   • Tracks last indexed timestamp via `stat -c %Y` on /var/lib/plocate/plocate.db
//   • Signals indexUpdated() on completion to reset sticky provider errors
//   • Dispatches desktop notification via notify-send on success
// ─────────────────────────────────────────────────────────────────────────
pragma Singleton
import QtQuick
import Quickshell
import Quickshell.Io

Item {
    id: root

    // States: "idle", "checking", "running", "success", "cancelled", "failed", "no_agent", "already_running"
    property string status: "idle"
    property string statusMessage: ""
    property string lastError: ""
    property string lastIndexedTime: ""

    signal indexUpdated()

    property string _stderrBuffer: ""
    property string _statBuffer: ""
    property double _pkexecStartTime: 0

    // Auto-reset terminal status (success, cancelled, failed, no_agent, already_running) to idle after ~5s
    property Timer resetIdleTimer: Timer {
        interval: 5000
        repeat: false
        onTriggered: {
            if (root.status !== "running" && root.status !== "checking") {
                root.status = "idle"
                root.statusMessage = ""
                root.lastError = ""
            }
        }
    }

    // ── Stat Process for Last Indexed Timestamp ─────────────────────────
    property Process statProcess: Process {
        command: ["/bin/stat", "-c", "%Y", "/var/lib/plocate/plocate.db"]
        stdout: SplitParser {
            onRead: data => {
                const line = data.trim()
                if (line.length > 0) {
                    root._statBuffer = line
                }
            }
        }
        onExited: (code) => {
            if (code === 0 && root._statBuffer.length > 0) {
                const ts = parseInt(root._statBuffer, 10)
                if (!isNaN(ts) && ts > 0) {
                    const d = new Date(ts * 1000)
                    root.lastIndexedTime = d.toLocaleDateString(Qt.locale(), Locale.ShortFormat) + " " + d.toLocaleTimeString(Qt.locale(), Locale.ShortFormat)
                    return
                }
            }
            root.lastIndexedTime = ""
        }
    }

    Component.onCompleted: {
        refreshTimestamp()
    }

    Component.onDestruction: {
        if (pkexecProcess.running) {
            pkexecProcess.running = false
        }
    }

    function refreshTimestamp() {
        _statBuffer = ""
        if (!statProcess.running) statProcess.running = true
    }

    // ── Pre-flight Check: updatedb already running ──────────────────────
    property Process checkRunningProcess: Process {
        command: ["/bin/pgrep", "-x", "updatedb|plocate-build"]
        onExited: (code) => {
            if (code === 0) {
                root.status = "already_running"
                root.statusMessage = "Indexing already in progress"
                root.resetIdleTimer.restart()
                return
            }
            // Proceed directly to pkexec without hard-blocking on polkit regex
            root.status = "running"
            root.statusMessage = "Indexing…"
            root._stderrBuffer = ""
            root._pkexecStartTime = Date.now()
            if (!pkexecProcess.running) pkexecProcess.running = true
        }
    }

    // ── Main pkexec Process (fixed argv, no shell string) ───────────────
    property Process pkexecProcess: Process {
        command: ["/bin/pkexec", "updatedb", "--prune-bind-mounts", "no"]
        stderr: SplitParser {
            onRead: data => {
                root._stderrBuffer += data + "\n"
            }
        }
        onExited: (code, exitStatus) => {
            const elapsed = Date.now() - root._pkexecStartTime

            if (code === 0) {
                root.status = "success"
                root.statusMessage = "Index updated"
                root.lastError = ""
                root.refreshTimestamp()
                root.indexUpdated()
                Quickshell.execDetached([
                    "notify-send",
                    "-a", "Prism",
                    "Search Index",
                    "File search index updated successfully.",
                    "-i", "system-search"
                ])
                root.resetIdleTimer.restart()
            } else if (code === 126 || root._stderrBuffer.indexOf("dismissed") !== -1 || root._stderrBuffer.indexOf("canceled") !== -1 || root._stderrBuffer.indexOf("cancelled") !== -1) {
                // Auth dialog dismissed / cancelled by user
                root.status = "cancelled"
                root.statusMessage = "Cancelled"
                root.lastError = ""
                root.resetIdleTimer.restart()
            } else {
                const err = root._stderrBuffer.trim()
                // Fast failure with no prompt indicates no authentication agent was available
                const noAgent = (elapsed < 1500) && (
                    err.indexOf("Not authorized") !== -1 ||
                    err.indexOf("No authentication agent") !== -1 ||
                    err.indexOf("no agent") !== -1 ||
                    err.indexOf("cannot find authentication agent") !== -1
                )

                if (noAgent) {
                    root.status = "no_agent"
                    root.statusMessage = "No polkit authentication agent running"
                    root.lastError = err.length > 0 ? err : "No polkit authentication agent running"
                } else {
                    root.status = "failed"
                    root.statusMessage = "Failed"
                    root.lastError = err.length > 0 ? err : ("Exit code " + code)
                }
                root.resetIdleTimer.restart()
            }
        }
    }

    function rebuildIndex() {
        if (status === "running" || status === "checking") return
        status = "checking"
        statusMessage = "Checking…"
        lastError = ""
        checkRunningProcess.running = true
    }
}
