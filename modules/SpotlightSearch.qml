// ─────────────────────────────────────────────────────────────────────────
// SpotlightSearch.qml — Standalone Spotlight-style unified search popup
//
// Features:
//   • Centered in the upper third of the screen with fluid height expansion
//   • Three configurable result sections: Applications, Folders, Files
//   • User-defined section ordering (persisted in Config)
//   • Automatic section header display only when 2 or more sections have items
//   • Instant app search sharing SearchProviderApps and Fuzzy.scoreLegacy
//   • Asynchronous debounced plocate + stat classification for folders/files
//   • Stable panel height with anti-shrink guard during in-flight search
//   • Coalesced result merging (single pass per event loop turn)
//   • Pinned search bar at top (y=0) and scrollable results container below
//   • Arrow key navigation across sections, Enter to launch, Shift+Enter
//     to open containing parent directory for folders and files
//   • Exclusive focus on open, dismiss on Escape or backdrop click
// ─────────────────────────────────────────────────────────────────────────
import QtQuick
import QtQuick.Layouts
import QtQuick.Shapes
import Quickshell
import Quickshell.Widgets
import Quickshell.Io
import "../services"

OverlayWindow {
    id: root

    panelWidth:  680
    anchorPos:   "spotlight"
    spotlightTopMargin: 140
    hasBorder:   true

    // External tracker for anti-shrink height avoiding self-referential binding loops
    property int lastHeight: 60

    // Dynamic height calculation derived from actual rendered content
    panelHeight: {
        const headerH = 60
        const q = queryField.text.trim()
        if (q.length === 0) {
            return headerH
        }

        let desiredH = headerH
        if (results.length === 0) {
            // Searching banner or "No matches found"
            desiredH = headerH + 1 + 50
        } else {
            let headerCount = 0
            if (results.some(r => r.section && r.section.length > 0)) {
                const uniqueSecs = new Set(results.map(r => r.section).filter(s => s && s.length > 0))
                headerCount = uniqueSecs.size
            }
            const headersH = headerCount * 26
            const listH = (list && list.contentHeight > 0)
                ? list.contentHeight + 12
                : (results.length * 50 + headersH + 12)
            desiredH = Math.min(480, headerH + 1 + listH)
        }

        // Anti-shrink guard: while file search is actively running, do not let
        // the height shrink below what it was to avoid typing jitter.
        if (filesProvider.searching && desiredH < root.lastHeight && root.lastHeight > headerH) {
            return root.lastHeight
        }

        return desiredH
    }

    // Pointer-burst suppression on Wayland surface appearance
    property bool mouseActivated: false
    Timer { id: mouseReadyTimer; interval: 150 }

    // Search providers
    SearchProviderApps {
        id: appsProvider
        maxApps: 5 // Cap applications at 5 in Spotlight popup
        onResultsUpdated: root.queueUpdate(false)
    }

    SearchProviderFiles {
        id: filesProvider
        onResultsUpdated: root.queueUpdate(false)
        onSearchingChanged: {
            root.lastHeight = root.panelHeight
        }
    }

    // React live to section config changes
    property Connections configConn: Connections {
        target: Config
        function onSearchSectionOrderChanged() { root.queueUpdate(false) }
        function onSearchEnableAppsChanged() { root.applyFilter() }
        function onSearchEnableFoldersChanged() { root.applyFilter() }
        function onSearchEnableFilesChanged() { root.applyFilter() }
    }

    property var results: []
    property int selectedIndex: 0
    property bool _pendingResetSelection: false

    // ── Update Coalescing Timer ─────────────────────────────────────────
    // Coalesce apps, folders, files, and text changes into ONE rebuild per event tick
    Timer {
        id: coalesceTimer
        interval: 0
        repeat: false
        onTriggered: root._rebuildMergedResults()
    }

    function queueUpdate(resetSelection) {
        if (resetSelection) {
            _pendingResetSelection = true
        }
        coalesceTimer.restart()
    }

    // IPC and test helpers
    function setQuery(text) {
        if (!text) {
            queryField.text = ""
            return
        }
        let q = text
        if ((q.startsWith("\"") && q.endsWith("\"") && q.length >= 2) ||
            (q.startsWith("'") && q.endsWith("'") && q.length >= 2)) {
            q = q.substring(1, q.length - 1)
        }
        queryField.text = q
    }

    function getQuery() {
        return queryField.text
    }

    function getResultsInfo() {
        return JSON.stringify(results.map(r => ({
            id: r.id,
            section: r.section || "",
            type: r.type,
            title: r.title,
            subtitle: r.subtitle || "",
            icon: r.icon || ""
        })))
    }

    function getFilesSearching() {
        return filesProvider.searching
    }

    function getFilesError() {
        return filesProvider.errorMessage
    }

    Component.onCompleted: {
        appsProvider.refreshApps()
    }

    // Reset state on open/close
    onShownChanged: {
        if (shown) {
            lastHeight = 60
            queryField.text = ""
            results = []
            selectedIndex = 0
            _pendingResetSelection = false
            appsProvider.refreshApps()
            appsProvider.cancel()
            filesProvider.cancel()
            queryField.forceActiveFocus()
            root.mouseActivated = false
            mouseReadyTimer.restart()
        } else {
            filesProvider.cancel()
            appsProvider.cancel()
            results = []
            _pendingResetSelection = false
            lastHeight = 60
        }
    }

    function applyFilter() {
        const q = queryField.text.trim()
        if (q.length === 0) {
            appsProvider.cancel()
            filesProvider.cancel()
            results = []
            selectedIndex = 0
            _pendingResetSelection = false
            return
        }

        // Instant synchronous app search
        if (Config.searchEnableApps) {
            appsProvider.search(q)
        } else {
            appsProvider.cancel()
        }

        // Debounced asynchronous folder and file search (if either enabled)
        if (Config.searchEnableFiles || Config.searchEnableFolders) {
            filesProvider.search(q)
        } else {
            filesProvider.cancel()
        }

        queueUpdate(true)
    }

    function _rebuildMergedResults() {
        const q = queryField.text.trim()
        if (q.length === 0) {
            results = []
            selectedIndex = 0
            _pendingResetSelection = false
            return
        }

        const appItems = Config.searchEnableApps ? appsProvider.results : []
        const folderItems = Config.searchEnableFolders ? filesProvider.folderResults : []
        const fileItems = Config.searchEnableFiles ? filesProvider.fileResults : []

        let activeSectionsCount = 0
        if (appItems.length > 0) activeSectionsCount++
        if (folderItems.length > 0) activeSectionsCount++
        if (fileItems.length > 0) activeSectionsCount++

        // Section headers are shown only when 2 or more sections have results
        const showHeaders = (activeSectionsCount >= 2)
        const order = Config.getSectionOrder()

        const merged = []
        for (let s = 0; s < order.length; s++) {
            const sec = order[s]
            if (sec === "apps") {
                for (let i = 0; i < appItems.length; i++) {
                    const item = Object.assign({}, appItems[i])
                    item.section = showHeaders ? "Applications" : ""
                    merged.push(item)
                }
            } else if (sec === "folders") {
                for (let j = 0; j < folderItems.length; j++) {
                    const fldItem = Object.assign({}, folderItems[j])
                    fldItem.section = showHeaders ? "Folders" : ""
                    merged.push(fldItem)
                }
            } else if (sec === "files") {
                for (let k = 0; k < fileItems.length; k++) {
                    const fItem = Object.assign({}, fileItems[k])
                    fItem.section = showHeaders ? "Files" : ""
                    merged.push(fItem)
                }
            }
        }

        const shouldReset = _pendingResetSelection
        _pendingResetSelection = false

        if (!shouldReset && results.length > 0 && selectedIndex < results.length) {
            const currentSelectedId = results[selectedIndex] ? results[selectedIndex].id : null
            results = merged
            if (currentSelectedId) {
                let found = -1
                for (let m = 0; m < merged.length; m++) {
                    if (merged[m].id === currentSelectedId) {
                        found = m
                        break
                    }
                }
                if (found !== -1) {
                    selectedIndex = found
                } else {
                    selectedIndex = Math.min(selectedIndex, Math.max(0, merged.length - 1))
                }
            }
        } else {
            results = merged
            selectedIndex = 0
        }

        if (results.length === 0) {
            selectedIndex = 0
        } else {
            selectedIndex = Math.max(0, Math.min(results.length - 1, selectedIndex))
        }
    }

    function moveSelection(delta) {
        if (results.length === 0) return
        let nextIndex = selectedIndex + delta
        if (nextIndex < 0) nextIndex = 0
        if (nextIndex >= results.length) nextIndex = results.length - 1
        selectedIndex = nextIndex
        list.positionViewAtIndex(selectedIndex, ListView.Contain)
    }

    function activateCurrent(event) {
        if (results.length === 0) return
        const idx = Math.max(0, Math.min(results.length - 1, selectedIndex))
        const item = results[idx]
        if (!item || item.type === "info") return

        // While file search is in flight, do not activate stale file/folder results
        if (filesProvider.searching && (item.type === "file" || item.type === "folder")) {
            return
        }

        const isShift = (event && (event.modifiers & Qt.ShiftModifier)) ? true : false
        activate(item, isShift)
    }

    function activate(item, isShift) {
        if (!item) return
        if (item.type === "info") return

        // Guard against activating stale in-flight file/folder results
        if (filesProvider.searching && (item.type === "file" || item.type === "folder")) {
            return
        }

        if (item.type === "file" || item.type === "folder") {
            if (isShift) {
                let parentDir = "/"
                if (item.path) {
                    let lastSlash = item.path.lastIndexOf("/")
                    parentDir = (lastSlash > 0) ? item.path.substring(0, lastSlash) : "/"
                }
                Quickshell.execDetached(["xdg-open", parentDir])
            } else {
                Quickshell.execDetached(["xdg-open", item.path])
            }
        } else if (item.app && typeof item.app.execute === "function") {
            item.app.execute()
        } else if (typeof item.execute === "function") {
            item.execute(isShift)
        }

        root.hide()
    }

    // ── Cursor-blanking overlay ─────────────────────────────────────────
    Loader {
        anchors.fill: parent
        z: 100
        active: !root.mouseActivated
        sourceComponent: MouseArea {
            anchors.fill: parent
            acceptedButtons: Qt.NoButton
            hoverEnabled: true
            cursorShape: Qt.BlankCursor
            onPositionChanged: {
                if (!mouseReadyTimer.running) root.mouseActivated = true
            }
        }
    }

    // ── Pinned Search Input Bar ─────────────────────────────────────────
    Item {
        id: searchBar
        anchors.top: parent.top
        anchors.left: parent.left
        anchors.right: parent.right
        height: 60
        z: 10

        RowLayout {
            anchors.fill: parent
            anchors.leftMargin: 18
            anchors.rightMargin: 18
            spacing: 12

            // Magnifier glyph (Shape)
            Shape {
                width: 20
                height: 20
                Layout.alignment: Qt.AlignVCenter
                layer.enabled: true
                layer.samples: 4
                ShapePath {
                    strokeWidth: 2.2
                    strokeColor: queryField.activeFocus ? Theme.primary : Theme.surfaceVariantText
                    fillColor: "transparent"
                    capStyle: ShapePath.RoundCap
                    PathSvg { path: "M 7 7 m -5 0 a 5 5 0 1 0 10 0 a 5 5 0 1 0 -10 0 M 11 11 L 16 16" }
                }
            }

            TextInput {
                id: queryField
                Layout.fillWidth:    true
                Layout.fillHeight:   true
                verticalAlignment:   TextInput.AlignVCenter
                font.pixelSize:      18
                color:               Theme.surfaceText
                clip:                true

                onTextChanged: root.applyFilter()

                Keys.onEscapePressed: root.hide()
                Keys.onReturnPressed: (event) => root.activateCurrent(event)
                Keys.onEnterPressed:  (event) => root.activateCurrent(event)
                Keys.onDownPressed:   root.moveSelection(1)
                Keys.onUpPressed:     root.moveSelection(-1)

                Text {
                    visible: queryField.text.length === 0
                    text: "Search apps, folders, and files…"
                    color: Theme.surfaceVariantText
                    font: queryField.font
                    anchors.verticalCenter: parent.verticalCenter
                }
            }

            // Subtle searching indicator inside input bar when results already present
            RowLayout {
                visible: filesProvider.searching && root.results.length > 0 && queryField.text.trim().length > 0
                spacing: 6
                Layout.alignment: Qt.AlignVCenter

                Text {
                    text: "searching…"
                    color: Theme.primary
                    font.pixelSize: 12
                    font.italic: true
                }
            }

            // Escape key badge
            Rectangle {
                width: 28
                height: 20
                radius: 4
                color: Qt.rgba(Theme.onPrimaryContainerColor.r, Theme.onPrimaryContainerColor.g, Theme.onPrimaryContainerColor.b, 0.12)
                border.width: 1
                border.color: Qt.rgba(Theme.outlineVariant.r, Theme.outlineVariant.g, Theme.outlineVariant.b, 0.3)
                Layout.alignment: Qt.AlignVCenter

                Text {
                    anchors.centerIn: parent
                    text: "esc"
                    font.pixelSize: 10
                    font.weight: Font.DemiBold
                    color: Theme.surfaceVariantText
                }
            }
        }
    }

    // ── Thin Divider ────────────────────────────────────────────────────
    Rectangle {
        id: divider
        anchors.top: searchBar.bottom
        anchors.left: parent.left
        anchors.right: parent.right
        height: 1
        color: Theme.outlineVariant
        opacity: 0.25
        visible: queryField.text.trim().length > 0
        z: 9
    }

    // ── Results Area / Empty State ──────────────────────────────────────
    Item {
        id: resultsContainer
        anchors.top: divider.bottom
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.bottom: parent.bottom
        visible: queryField.text.trim().length > 0

        // In-flight searching state when 0 results so far
        RowLayout {
            anchors.centerIn: parent
            visible: queryField.text.trim().length > 0 && root.results.length === 0 && filesProvider.searching
            spacing: 8

            Text {
                text: "Searching files and folders…"
                color: Theme.surfaceVariantText
                font.pixelSize: 14
                font.italic: true
                opacity: 0.8
            }
        }

        // Empty state when search completed with no matches
        Text {
            anchors.centerIn: parent
            visible: queryField.text.trim().length > 0 && root.results.length === 0 && !filesProvider.searching
            text: "No matches found"
            color: Theme.surfaceVariantText
            font.pixelSize: 14
            opacity: 0.7
        }

        // Results ListView
        ListView {
            id: list
            anchors.fill: parent
            anchors.margins: 6
            clip: true
            model: root.results
            spacing: 2
            visible: root.results.length > 0

            section.property: "section"
            section.criteria: ViewSection.FullString
            section.delegate: Component {
                Item {
                    width: list.width
                    height: section && section.length > 0 ? 26 : 0
                    visible: section && section.length > 0

                    RowLayout {
                        anchors.fill: parent
                        anchors.leftMargin: 12
                        anchors.rightMargin: 12
                        spacing: 8

                        Text {
                            text: section
                            color: Theme.surfaceVariantText
                            font.pixelSize: 11
                            font.weight: Font.DemiBold
                            font.capitalization: Font.AllUppercase
                            opacity: 0.75
                            Layout.alignment: Qt.AlignVCenter
                        }

                        Item { Layout.fillWidth: true }
                    }
                }
            }

            delegate: Item {
                id: delegateRoot
                width: list.width
                height: 50

                readonly property bool isStale: filesProvider.searching && (modelData.type === "file" || modelData.type === "folder")
                opacity: isStale ? 0.45 : 1.0

                Rectangle {
                    id: pill
                    anchors.fill: parent
                    anchors.margins: 1
                    radius: Theme.radiusSmall
                    color: (index === root.selectedIndex || (root.mouseActivated && hoverArea.containsMouse))
                           ? Qt.rgba(Theme.onPrimaryContainerColor.r, Theme.onPrimaryContainerColor.g, Theme.onPrimaryContainerColor.b, 0.85)
                           : "transparent"

                    RowLayout {
                        anchors.fill: parent
                        anchors.leftMargin: 12
                        anchors.rightMargin: 12
                        spacing: 12

                        IconImage {
                            width: 28
                            height: 28
                            source: {
                                if (modelData.type === "folder") {
                                    return Quickshell.iconPath("folder") || Quickshell.iconPath("inode-directory") || Quickshell.iconPath("folder-open")
                                }
                                return Quickshell.iconPath(modelData.icon) || Quickshell.iconPath(modelData.type === "file" ? "text-x-generic" : "application-x-executable")
                            }
                            Layout.alignment: Qt.AlignVCenter
                        }

                        ColumnLayout {
                            Layout.fillWidth: true
                            Layout.alignment: Qt.AlignVCenter
                            spacing: 1

                            Text {
                                Layout.fillWidth: true
                                text: modelData.title
                                color: (index === root.selectedIndex || (root.mouseActivated && hoverArea.containsMouse))
                                       ? Theme.inversePrimary
                                       : Theme.onPrimaryContainerColor
                                font.pixelSize: 14
                                font.weight: Font.Medium
                                elide: Text.ElideRight
                            }

                            Text {
                                visible: modelData.subtitle && modelData.subtitle.length > 0
                                Layout.fillWidth: true
                                text: modelData.subtitle ?? ""
                                color: (index === root.selectedIndex || (root.mouseActivated && hoverArea.containsMouse))
                                       ? Qt.rgba(Theme.inversePrimary.r, Theme.inversePrimary.g, Theme.inversePrimary.b, 0.7)
                                       : Theme.surfaceVariantText
                                font.pixelSize: 11
                                elide: (modelData.type === "file" || modelData.type === "folder") ? Text.ElideMiddle : Text.ElideRight
                            }
                        }

                        // Action hint pill on selected item
                        Text {
                            visible: index === root.selectedIndex
                            text: {
                                if (delegateRoot.isStale) return "Searching…"
                                if (modelData.type === "folder") return "Shift+↵ Parent  ↵ Open"
                                if (modelData.type === "file") return "Shift+↵ Folder  ↵ Open"
                                return "↵ Open"
                            }
                            color: (index === root.selectedIndex || (root.mouseActivated && hoverArea.containsMouse))
                                   ? Qt.rgba(Theme.inversePrimary.r, Theme.inversePrimary.g, Theme.inversePrimary.b, 0.6)
                                   : Theme.surfaceVariantText
                            font.pixelSize: 10
                            Layout.alignment: Qt.AlignVCenter
                        }
                    }
                }

                MouseArea {
                    id: hoverArea
                    anchors.fill: parent
                    hoverEnabled: true
                    cursorShape: delegateRoot.isStale ? Qt.ArrowCursor : Qt.PointingHandCursor
                    onPositionChanged: {
                        if (!mouseReadyTimer.running && !root.mouseActivated)
                            root.mouseActivated = true
                        if (root.mouseActivated && !delegateRoot.isStale)
                            root.selectedIndex = index
                    }
                    onClicked: (mouse) => {
                        if (delegateRoot.isStale) return
                        root.mouseActivated = true
                        root.selectedIndex = index
                        const isShift = (mouse.modifiers & Qt.ShiftModifier) ? true : false
                        root.activate(modelData, isShift)
                    }
                }
            }
        }
    }
}
