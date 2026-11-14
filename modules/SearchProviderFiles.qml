// ─────────────────────────────────────────────────────────────────────────
// SearchProviderFiles.qml — System-wide file and folder search provider
//
// Features:
//   • Queries up to 2000 plocate candidates with debounced execution
//   • Candidate pre-ranking: exact basename > prefix > $HOME > length > alpha
//   • Batched stat classification of top 300 candidates (folder vs file)
//   • Generation tokens preventing stale async results from overwriting newer ones
//   • Separate folderResults and fileResults with configurable result caps
//   • Automatic recovery from missing database via IndexService and proactive checks
// ─────────────────────────────────────────────────────────────────────────
import QtQuick
import Quickshell
import Quickshell.Io
import "../services"

QtObject {
    id: root

    readonly property string providerId: "files"
    readonly property string name: "Files"

    readonly property int plocateLimit: 2000
    readonly property int statBatchLimit: 300

    property bool searching: false
    property string errorMessage: ""
    property bool plocateInstalled: true
    property bool databaseMissing: false

    property var folderResults: []
    property var fileResults: []
    property alias results: root.fileResults
    property string currentQuery: ""

    signal resultsUpdated()

    // ── Listen for IndexService rebuild completions ─────────────────
    property Connections indexConn: Connections {
        target: IndexService
        function onIndexUpdated() {
            root.resetDatabaseMissing()
        }
    }

    function resetDatabaseMissing() {
        root.databaseMissing = false
        root.errorMessage = ""
    }

    // ── Proactive Check: test if database file restored externally ──
    property Process checkDbProcess: Process {
        command: ["test", "-f", "/var/lib/plocate/plocate.db"]
        onExited: (code) => {
            if (code === 0) {
                root.resetDatabaseMissing()
            }
        }
    }

    // ── Check if plocate binary is available ─────────────────────────
    property Process checkBinaryProcess: Process {
        command: ["which", "plocate"]
        onExited: (code) => {
            root.plocateInstalled = (code === 0)
            if (code !== 0) {
                root.errorMessage = "plocate is not installed"
            }
        }
    }

    Component.onCompleted: {
        checkBinaryProcess.running = true
    }

    // ── Input Debounce Timer (~150 ms) ──────────────────────────────
    property Timer debounceTimer: Timer {
        interval: 150
        repeat: false
        onTriggered: root._executeSearch()
    }

    // ── Query Generation & Batched Stat ──────────────────────────────
    property int _queryGeneration: 0
    property int _plocateGeneration: 0
    property int _statGeneration: 0
    property bool _plocateKilled: false
    property bool _statKilled: false
    property var _buffer: []
    property string _stderrBuffer: ""
    property var _statBuffer: []

    function _hasGlobChars(q) {
        if (!q) return false
        return /[*?[\]\\]/.test(q)
    }

    function _escapePlocateGlob(q) {
        if (!q) return ""
        return q.replace(/([\\*?\[\]])/g, "\\$1")
    }

    function _getLongestLiteralSegment(q) {
        if (!q) return ""
        const segments = q.split(/[*?[\]\\]+/)
        let longest = ""
        for (let i = 0; i < segments.length; i++) {
            if (segments[i].length > longest.length) {
                longest = segments[i]
            }
        }
        return longest
    }

    property Process plocateProcess: Process {
        stdout: SplitParser {
            onRead: data => {
                const line = data.endsWith("\r") ? data.slice(0, -1) : data
                if (line.length > 0) {
                    root._buffer.push(line)
                }
            }
        }
        stderr: SplitParser {
            onRead: data => {
                root._stderrBuffer += data + "\n"
            }
        }
        onExited: (code, status) => {
            // Ignore results from killed processes or stale query generations
            if (root._plocateKilled || root._plocateGeneration !== root._queryGeneration) {
                root._plocateKilled = false
                return
            }

            if (code !== 0 && root._stderrBuffer.length > 0) {
                const err = root._stderrBuffer
                if (err.indexOf("No such file or directory") !== -1 || err.indexOf("plocate.db") !== -1) {
                    root.searching = false
                    root.databaseMissing = true
                    root.errorMessage = "plocate database not found"
                    root.fileResults = [root._createInfoItem("plocate database not found", "Run 'sudo updatedb' or Settings → Search → Rebuild file index")]
                    root.folderResults = []
                    root.resultsUpdated()
                    return
                } else {
                    root.errorMessage = "plocate error: " + err.trim()
                }
            } else if (code === 0) {
                root.databaseMissing = false
                root.errorMessage = ""
            }

            root._classifyCandidates()
        }
    }

    property Process statProcess: Process {
        stdout: SplitParser {
            onRead: data => {
                const line = data.endsWith("\r") ? data.slice(0, -1) : data
                if (line.length > 0) {
                    root._statBuffer.push(line)
                }
            }
        }
        onExited: (code, status) => {
            // Ignore results from killed processes or stale query generations
            if (root._statKilled || root._statGeneration !== root._queryGeneration) {
                root._statKilled = false
                return
            }

            // Tolerating missing paths from stale db; partial stdout is accepted
            root._onStatFinished()
        }
    }

    function _getContainingFolder(path) {
        if (!path) return "/"
        const lastSlash = path.lastIndexOf("/")
        if (lastSlash <= 0) return "/"
        return path.substring(0, lastSlash)
    }

    function _iconForPath(path) {
        if (!path) return "text-x-generic"
        const lastSlash = path.lastIndexOf("/")
        const fileName = (lastSlash !== -1) ? path.substring(lastSlash + 1) : path

        const dotIdx = fileName.lastIndexOf(".")
        if (dotIdx === -1 || dotIdx === 0) {
            return "text-x-generic"
        }
        const ext = fileName.substring(dotIdx + 1).toLowerCase()

        // Images
        if (["png", "jpg", "jpeg", "svg", "webp", "gif", "bmp", "ico", "tiff", "avif"].includes(ext)) {
            return "image-x-generic"
        }
        // Audio
        if (["mp3", "flac", "wav", "ogg", "m4a", "opus", "aac", "wma"].includes(ext)) {
            return "audio-x-generic"
        }
        // Video
        if (["mp4", "mkv", "avi", "mov", "webm", "flv", "wmv", "m4v"].includes(ext)) {
            return "video-x-generic"
        }
        // Documents
        if (ext === "pdf") {
            return "application-pdf"
        }
        if (["doc", "docx", "odt", "rtf"].includes(ext)) {
            return "x-office-document"
        }
        if (["xls", "xlsx", "ods", "csv"].includes(ext)) {
            return "x-office-spreadsheet"
        }
        if (["ppt", "pptx", "odp"].includes(ext)) {
            return "x-office-presentation"
        }
        // Archives
        if (["zip", "tar", "gz", "bz2", "xz", "7z", "rar", "zst", "tgz"].includes(ext)) {
            return "package-x-generic"
        }
        // Code / Scripts / Config
        if (["txt", "md", "json", "qml", "js", "ts", "py", "sh", "bash", "c", "cpp", "h", "hpp", "rs", "go", "java", "html", "css", "kdl", "conf", "yaml", "yml", "toml"].includes(ext)) {
            return "text-x-generic"
        }
        // Executables / Binaries
        if (["bin", "so", "run", "desktop", "exe"].includes(ext)) {
            return "application-x-executable"
        }
        return "text-x-generic"
    }

    function _createFolderResult(path) {
        const lastSlash = path.lastIndexOf("/")
        const folderName = (lastSlash !== -1) ? path.substring(lastSlash + 1) : path

        return {
            id: "folder:" + path,
            section: "Folders",
            type: "folder",
            title: folderName,
            subtitle: path,
            icon: "folder",
            path: path,
            execute: function(isShift) {
                if (isShift) {
                    const parentDir = root._getContainingFolder(path)
                    Quickshell.execDetached(["xdg-open", parentDir])
                } else {
                    Quickshell.execDetached(["xdg-open", path])
                }
            }
        }
    }

    function _createResult(path) {
        const lastSlash = path.lastIndexOf("/")
        const fileName = (lastSlash !== -1) ? path.substring(lastSlash + 1) : path
        const iconName = _iconForPath(path)

        return {
            id: "file:" + path,
            section: "Files",
            type: "file",
            title: fileName,
            subtitle: path,
            icon: iconName,
            path: path,
            execute: function(isShift) {
                if (isShift) {
                    const parentDir = root._getContainingFolder(path)
                    Quickshell.execDetached(["xdg-open", parentDir])
                } else {
                    Quickshell.execDetached(["xdg-open", path])
                }
            }
        }
    }

    function _createInfoItem(title, subtitle) {
        return {
            id: "info:" + title,
            section: "Files",
            type: "info",
            title: title,
            subtitle: subtitle,
            icon: "dialog-warning",
            path: "",
            execute: function(isShift) {}
        }
    }

    function search(query) {
        debounceTimer.stop()
        root._queryGeneration++
        if (plocateProcess.running) {
            root._plocateKilled = true
            plocateProcess.running = false
        }
        if (statProcess.running) {
            root._statKilled = true
            statProcess.running = false
        }

        const q = (query || "").trim()
        currentQuery = q
        errorMessage = ""

        // If both folder and file search are disabled, do no work
        if (!Config.searchEnableFiles && !Config.searchEnableFolders) {
            cancel()
            return
        }

        if (q.length < 2) {
            cancel()
            return
        }

        if (!plocateInstalled) {
            errorMessage = "plocate is not installed"
            fileResults = [_createInfoItem("plocate is not installed", "Install plocate and run 'sudo updatedb'")]
            folderResults = []
            resultsUpdated()
            return
        }

        if (databaseMissing) {
            checkDbProcess.running = true
            errorMessage = "plocate database not found"
            fileResults = [_createInfoItem("plocate database not found", "Run 'sudo updatedb' or Settings → Search → Rebuild file index")]
            folderResults = []
            resultsUpdated()
            return
        }

        // Keep previous results visible while searching = true to prevent height jitter
        searching = true
        debounceTimer.restart()
    }

    function cancel() {
        debounceTimer.stop()
        root._queryGeneration++
        if (plocateProcess.running) {
            root._plocateKilled = true
            plocateProcess.running = false
        }
        if (statProcess.running) {
            root._statKilled = true
            statProcess.running = false
        }
        searching = false
        folderResults = []
        fileResults = []
        _buffer = []
        _stderrBuffer = ""
        _statBuffer = []
        currentQuery = ""
        resultsUpdated()
    }

    function _executeSearch() {
        if (currentQuery.length < 2 || (!Config.searchEnableFiles && !Config.searchEnableFolders)) {
            searching = false
            return
        }

        root._queryGeneration++
        if (plocateProcess.running) {
            root._plocateKilled = true
            plocateProcess.running = false
        }
        if (statProcess.running) {
            root._statKilled = true
            statProcess.running = false
        }

        root._plocateGeneration = root._queryGeneration
        root._plocateKilled = false
        _buffer = []
        _stderrBuffer = ""

        // If the query contains * ? [ ] \, pass "*<escaped>*" so plocate performs a
        // case-insensitive substring match against basenames
        let plocateArg = currentQuery
        if (root._hasGlobChars(currentQuery)) {
            const escaped = root._escapePlocateGlob(currentQuery)
            plocateArg = "*" + escaped + "*"
        }

        plocateProcess.command = ["plocate", "-i", "-l", root.plocateLimit.toString(), "-b", plocateArg]
        plocateProcess.running = true
    }

    function _classifyCandidates() {
        if (root._plocateGeneration !== root._queryGeneration) {
            return
        }

        const rawPaths = root._buffer.slice()
        const home = Quickshell.env("HOME") || ""
        const includeHidden = Config.searchIncludeHidden
        const qLower = currentQuery.toLowerCase()
        const hasGlob = root._hasGlobChars(currentQuery)

        // Include standard home user directories if their name matches query
        const stdDirs = ["Desktop", "Documents", "Downloads", "Music", "Pictures", "Videos", "Templates", "Public"]
        if (includeHidden) stdDirs.push(".config")
        for (let s = 0; s < stdDirs.length; s++) {
            const dir = stdDirs[s]
            if (dir.toLowerCase().indexOf(qLower) !== -1) {
                const fullP = home + "/" + dir
                if (!rawPaths.includes(fullP)) {
                    rawPaths.push(fullP)
                }
            }
        }

        // Filter hidden paths and ensure case-insensitive substring match against literal query
        const filtered = []
        for (let i = 0; i < rawPaths.length; i++) {
            const p = rawPaths[i]
            if (!includeHidden && p.indexOf("/.") !== -1) {
                continue
            }
            if (hasGlob) {
                const lastSlash = p.lastIndexOf("/")
                const base = (lastSlash !== -1 ? p.substring(lastSlash + 1) : p).toLowerCase()
                if (base.indexOf(qLower) === -1) {
                    continue
                }
            }
            filtered.push(p)
        }

        // Pre-stat ranking: exact basename > prefix basename > $HOME > shorter > alphabetical
        filtered.sort((a, b) => {
            const lastSlashA = a.lastIndexOf("/")
            const baseA = (lastSlashA !== -1 ? a.substring(lastSlashA + 1) : a).toLowerCase()
            const lastSlashB = b.lastIndexOf("/")
            const baseB = (lastSlashB !== -1 ? b.substring(lastSlashB + 1) : b).toLowerCase()

            // 1. Exact basename match (case-insensitive)
            const exactA = (baseA === qLower) ? 0 : 1
            const exactB = (baseB === qLower) ? 0 : 1
            if (exactA !== exactB) return exactA - exactB

            // 2. Basename prefix match
            const prefixA = baseA.startsWith(qLower) ? 0 : 1
            const prefixB = baseB.startsWith(qLower) ? 0 : 1
            if (prefixA !== prefixB) return prefixA - prefixB

            // 3. Paths under $HOME
            const homeA = (home.length > 0 && a.startsWith(home)) ? 0 : 1
            const homeB = (home.length > 0 && b.startsWith(home)) ? 0 : 1
            if (homeA !== homeB) return homeA - homeB

            // 4. Shorter paths
            if (a.length !== b.length) return a.length - b.length

            // 5. Alphabetical
            return a.localeCompare(b)
        })

        // Top 300 candidates classified in a single batched stat call
        const topCandidates = filtered.slice(0, root.statBatchLimit)
        if (topCandidates.length === 0) {
            root.folderResults = []
            root.fileResults = []
            root.searching = false
            root.resultsUpdated()
            return
        }

        root._statBuffer = []
        root._statGeneration = root._queryGeneration
        root._statKilled = false

        // Batched stat call: fixed argv list, no shell string, tolerates missing files
        // Use -L to dereference symlinks so symlinks to directories count as folders
        statProcess.command = ["stat", "-L", "--printf=%F\t%n\n", "--"].concat(topCandidates)
        statProcess.running = true
    }

    function _onStatFinished() {
        // Discard stale stat results if a newer query has superseded this generation
        if (root._statGeneration !== root._queryGeneration) {
            return
        }

        root.searching = false

        const folders = []
        const files = []

        for (let i = 0; i < root._statBuffer.length; i++) {
            const line = root._statBuffer[i]
            const tabIdx = line.indexOf("\t")
            if (tabIdx === -1) continue

            const typeStr = line.substring(0, tabIdx)
            const p = line.substring(tabIdx + 1)
            if (!p || p.length === 0) continue

            if (typeStr === "directory") {
                folders.push(root._createFolderResult(p))
            } else {
                files.push(root._createResult(p))
            }
        }

        const maxFolders = Config.searchMaxFolderResults || 10
        const maxFiles = Config.searchMaxFileResults || 20

        root.folderResults = Config.searchEnableFolders ? folders.slice(0, maxFolders) : []
        root.fileResults = Config.searchEnableFiles ? files.slice(0, maxFiles) : []
        root.resultsUpdated()
    }
}
