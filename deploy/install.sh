#!/usr/bin/env bash
# install.sh — Build and install the Prism Native Rust Desktop Shell
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
RUST_TREE="$REPO_DIR"
BIN_DIR="${HOME}/.local/bin"
SYSTEMD_USER_DIR="${HOME}/.config/systemd/user"
SHARE_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/prism"

echo "========================================================"
echo "    Installing Prism Native Rust Desktop Shell          "
echo "========================================================"

mkdir -p "$BIN_DIR"
mkdir -p "$SYSTEMD_USER_DIR"

echo "[1/5] Compiling Rust binaries (release mode)..."
cargo build --release --manifest-path "$RUST_TREE/Cargo.toml"

echo "[2/5] Installing binaries to $BIN_DIR..."
cp -f "$RUST_TREE/target/release/prism-ctl" "$BIN_DIR/prism-ctl"
cp -f "$RUST_TREE/target/release/prism-lock" "$BIN_DIR/prism-lock"
cp -f "$RUST_TREE/target/release/prism-phoned" "$BIN_DIR/prism-phoned"
cp -f "$RUST_TREE/target/release/prism-rec" "$BIN_DIR/prism-rec"
cp -f "$RUST_TREE/target/release/prismd" "$BIN_DIR/prismd"
cp -f "$RUST_TREE/target/release/prism-ui" "$BIN_DIR/prism-ui"

chmod +x "$BIN_DIR/prism-ctl" "$BIN_DIR/prism-lock" "$BIN_DIR/prismd" "$BIN_DIR/prism-ui" "$BIN_DIR/prism-phoned" "$BIN_DIR/prism-rec"
echo "Installed to $BIN_DIR: prism-ctl, prism-lock, prismd, prism-ui, prism-phoned, prism-rec"

# Also install to /usr/local/bin for system-level services (greetd Cage kiosk)
if [ -w /usr/local/bin ]; then
    cp -f "$RUST_TREE/target/release/prism-ctl" /usr/local/bin/prism-ctl
    cp -f "$RUST_TREE/target/release/prism-lock" /usr/local/bin/prism-lock
    cp -f "$RUST_TREE/target/release/prismd" /usr/local/bin/prismd
    cp -f "$RUST_TREE/target/release/prism-ui" /usr/local/bin/prism-ui
    echo "Installed to /usr/local/bin for greetd and kiosk use."
elif command -v sudo >/dev/null 2>&1; then
    sudo cp -f "$RUST_TREE/target/release/prism-ctl" "$RUST_TREE/target/release/prism-lock" "$RUST_TREE/target/release/prismd" "$RUST_TREE/target/release/prism-ui" /usr/local/bin/ 2>/dev/null || true
fi

echo "[3/5] Installing helper scripts to $SHARE_DIR/scripts..."
# Keybind parsers and the wallpaper depth generator; prismd and prism-ui look here.
mkdir -p "$SHARE_DIR/scripts"
install -m644 "$REPO_DIR"/scripts/*.py "$SHARE_DIR/scripts/"
install -m755 "$REPO_DIR"/scripts/*.sh "$SHARE_DIR/scripts/"
# Replaced by the notification server in prismd.
rm -f "$SHARE_DIR/scripts/prism-dbus-notif.py"

echo "[4/5] Installing systemd user service..."
cp -f "$SCRIPT_DIR/prismd.service" "$SYSTEMD_USER_DIR/prismd.service"
if command -v systemctl >/dev/null 2>&1; then
    systemctl --user daemon-reload || true
    echo "Service installed to $SYSTEMD_USER_DIR/prismd.service"
    echo "Enable it once with: systemctl --user enable --now prismd"
fi

echo "[5/5] Compositor Integration Instructions:"
echo "To switch completely from Quickshell to the native Rust shell:"
echo " - MangoWM:  add 'source $SCRIPT_DIR/prism-mango.conf' to ~/.config/mango/config.conf"
echo " - Niri:     add 'include \"$SCRIPT_DIR/prism-niri.kdl\"' to ~/.config/niri/config.kdl"
echo " - Hyprland: add 'source = $SCRIPT_DIR/prism-hyprland.conf' to ~/.config/hypr/hyprland.conf"
echo ""
echo "Lock screen and greeter (run these yourself; they change system files):"
echo " - PAM service for unlocking (otherwise 'login' is used):"
echo "     sudo install -Dm644 $SCRIPT_DIR/pam.d/prism /etc/pam.d/prism"
echo " - Greeter binary and greetd on cage (no temporary compositor):"
echo "     sudo install -m755 $RUST_TREE/target/release/prism-lock /usr/local/bin/prism-lock"
echo "     sudo cp /etc/greetd/config.toml /etc/greetd/config.toml.bak"
echo "     sudo install -m644 $SCRIPT_DIR/greetd/config.toml /etc/greetd/config.toml"
echo "   Test before rebooting: from a TTY run 'sudo systemctl restart greetd'."
echo " - Check the lock screen accepts your password without locking anything:"
echo "     prism-lock --check-auth"
echo ""
echo "Installation completed successfully!"
