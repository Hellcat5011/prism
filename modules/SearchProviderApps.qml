// ─────────────────────────────────────────────────────────────────────────
// SearchProviderApps.qml — Applications search provider
//
// Reads Quickshell's DesktopEntries singleton and provides ranked,
// fuzzy-scored application results.
// ─────────────────────────────────────────────────────────────────────────
import QtQuick
import Quickshell
import "../services"

QtObject {
    id: root

    readonly property string providerId: "apps"
    readonly property string name: "Applications"
    property bool searching: false
    property string errorMessage: ""
    property int maxApps: 0
    property var allApps: []
    property var results: []

    signal resultsUpdated()

    Component.onCompleted: {
        refreshApps()
    }

    function refreshApps() {
        const apps = []
        for (const entry of DesktopEntries.applications.values) {
            if (!entry.noDisplay) apps.push(entry)
        }
        apps.sort((a, b) => a.name.localeCompare(b.name))
        allApps = apps
    }

    function _createResult(app) {
        return {
            id: "app:" + (app.id || app.name),
            section: "Applications",
            type: "app",
            title: app.name,
            subtitle: app.genericName || "",
            icon: app.icon || "application-x-executable",
            app: app,
            execute: function(isShift) {
                if (app && typeof app.execute === "function") {
                    app.execute()
                }
            }
        }
    }

    function search(query) {
        if (allApps.length === 0) {
            refreshApps()
        }
        const q = (query || "").trim()
        if (q.length === 0) {
            const list = (maxApps > 0) ? allApps.slice(0, maxApps) : allApps
            results = list.map(a => _createResult(a))
            resultsUpdated()
            return
        }

        const scoredApps = []
        for (let i = 0; i < allApps.length; i++) {
            const app = allApps[i]
            let maxScore = -1

            const nameScore = Fuzzy.scoreLegacy(q, app.name)
            if (nameScore > maxScore) maxScore = nameScore

            if (app.genericName) {
                const gScore = Fuzzy.scoreLegacy(q, app.genericName)
                if (gScore !== -1 && (gScore * 0.8) > maxScore) {
                    maxScore = gScore * 0.8
                }
            }

            if (app.keywords) {
                for (let k = 0; k < app.keywords.length; k++) {
                    const kScore = Fuzzy.scoreLegacy(q, app.keywords[k])
                    if (kScore !== -1 && (kScore * 0.5) > maxScore) {
                        maxScore = kScore * 0.5
                    }
                }
            }

            if (maxScore !== -1) {
                scoredApps.push({ app: app, score: maxScore })
            }
        }

        scoredApps.sort((a, b) => b.score - a.score)
        const finalScored = (maxApps > 0) ? scoredApps.slice(0, maxApps) : scoredApps
        results = finalScored.map(e => _createResult(e.app))
        resultsUpdated()
    }

    function cancel() {
        results = []
        resultsUpdated()
    }
}

