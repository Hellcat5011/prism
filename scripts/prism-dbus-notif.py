#!/usr/bin/env python3
"""
Prism FreeDesktop Notifications D-Bus Service.
Claims org.freedesktop.Notifications on the session bus and bridges
incoming notifications to prism-ctl.
"""

import os
import shutil
import signal
import sys
import subprocess
import dbus
import dbus.service
import dbus.mainloop.glib
from gi.repository import GLib

PRISM_CTL = shutil.which("prism-ctl") or os.path.expanduser("~/.local/bin/prism-ctl")

# Popen() children were never waited on, so every notification left a zombie prism-ctl.
# Ignoring SIGCHLD makes the kernel reap them automatically.
signal.signal(signal.SIGCHLD, signal.SIG_IGN)

class NotificationServer(dbus.service.Object):
    def __init__(self, bus_name):
        super().__init__(bus_name, "/org/freedesktop/Notifications")
        self.next_id = 1

    @dbus.service.method("org.freedesktop.Notifications", out_signature="as")
    def GetCapabilities(self):
        return ["body", "body-markup", "actions", "icon-static", "persistence"]

    @dbus.service.method(
        "org.freedesktop.Notifications",
        in_signature="susssasa{sv}i",
        out_signature="u"
    )
    def Notify(self, app_name, replaces_id, app_icon, summary, body, actions, hints, expire_timeout):
        notif_id = self.next_id if replaces_id == 0 else replaces_id
        self.next_id += 1

        app = str(app_name) if app_name else "System"
        sum_str = str(summary) if summary else "Notification"
        body_str = str(body) if body else ""

        try:
            subprocess.Popen([
                PRISM_CTL, "notif", "send",
                app, sum_str, body_str
            ])
        except Exception as e:
            sys.stderr.write(f"[prism-dbus-notif] Failed to forward notification: {e}\n")

        return dbus.UInt32(notif_id)

    @dbus.service.method("org.freedesktop.Notifications", in_signature="u", out_signature="")
    def CloseNotification(self, id):
        try:
            subprocess.Popen([PRISM_CTL, "notif", "dismiss", str(id)])
        except Exception:
            pass
        self.NotificationClosed(id, 3)

    @dbus.service.method("org.freedesktop.Notifications", out_signature="ssss")
    def GetServerInformation(self):
        return ("prismd", "Prism", "0.1.0", "1.2")

    @dbus.service.signal("org.freedesktop.Notifications", signature="uu")
    def NotificationClosed(self, id, reason):
        pass

    @dbus.service.signal("org.freedesktop.Notifications", signature="us")
    def ActionInvoked(self, id, action_key):
        pass

def main():
    dbus.mainloop.glib.DBusGMainLoop(set_as_default=True)
    session_bus = dbus.SessionBus()

    try:
        bus_name = dbus.service.BusName(
            "org.freedesktop.Notifications",
            session_bus,
            allow_replacement=True,
            replace_existing=True,
            do_not_queue=True
        )
    except Exception as e:
        sys.stderr.write(f"[prism-dbus-notif] Could not acquire bus name: {e}\n")
        sys.exit(1)

    server = NotificationServer(bus_name)
    loop = GLib.MainLoop()
    try:
        loop.run()
    except KeyboardInterrupt:
        pass

if __name__ == "__main__":
    main()
