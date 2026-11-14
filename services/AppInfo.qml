pragma Singleton
import QtQuick
import QtCore
import Quickshell
import Quickshell.Widgets

QtObject {
    // The shell's identity. Kept for display, debug logging, and any
    // other code that wants to reference the canonical names.
    readonly property string name: "prism"
    readonly property string organization: "Prism"

    // The exact path to the settings file. Computed from the standard
    // config location plus a fixed subdirectory and filename, so it
    // never depends on Qt.application.organization or
    // Qt.application.name being set before this is read.
    readonly property url settingsLocation:
        StandardPaths.writableLocation(StandardPaths.ConfigLocation)
        + "/Prism/prism.conf"

    Component.onCompleted: {
        // Set the application identity as a belt-and-braces measure for
        // any other code that reads Qt.application.name or
        // Qt.application.organization.
        Qt.application.name = "prism"
        Qt.application.organization = "Prism"
    }

    function getAppIcon(appClass: string): string {
        if (!appClass || appClass.length === 0) return "application-x-executable";
        let lower = appClass.toLowerCase();
        for (let app of DesktopEntries.applications.values) {
            if (!app.id) continue;
            let appId = app.id.toLowerCase();
            if (appId === lower || appId === lower + ".desktop") {
                return app.icon || appClass;
            }
        }
        for (let app of DesktopEntries.applications.values) {
            if (!app.id) continue;
            let appId = app.id.toLowerCase();
            if (appId.replace(".desktop", "").endsWith(lower)) {
                return app.icon || appClass;
            }
        }
        return appClass;
    }

    function getAppName(appClass: string): string {
        if (!appClass || appClass.length === 0) return "";
        let lower = appClass.toLowerCase();
        for (let app of DesktopEntries.applications.values) {
            if (!app.id) continue;
            let appId = app.id.toLowerCase();
            if (appId === lower || appId === lower + ".desktop") {
                return app.name || appClass;
            }
        }
        return appClass;
    }
}
