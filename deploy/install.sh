#!/usr/bin/env bash
# install.sh — Build and install the Prism Native Rust Desktop Shell
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
RUST_TREE="$REPO_DIR"
BIN_DIR="${HOME}/.local/bin"
SYSTEMD_USER_DIR="${HOME}/.config/systemd/user"

echo "========================================================"
echo "    Installing Prism Native Rust Desktop Shell          "
echo "========================================================"

mkdir -p "$BIN_DIR"
mkdir -p "$SYSTEMD_USER_DIR"

echo "[1/4] Compiling Rust binaries (release mode)..."
cargo build --release --manifest-path "$RUST_TREE/Cargo.toml"

echo "[2/4] Installing binaries to $BIN_DIR..."
cp -f "$RUST_TREE/target/release/prism-ctl" "$BIN_DIR/prism-ctl"
cp -f "$RUST_TREE/target/release/prism-lock" "$BIN_DIR/prism-lock"
cp -f "$RUST_TREE/target/release/prismd" "$BIN_DIR/prismd"
cp -f "$RUST_TREE/target/release/prism-ui" "$BIN_DIR/prism-ui"

chmod +x "$BIN_DIR/prism-ctl" "$BIN_DIR/prism-lock" "$BIN_DIR/prismd" "$BIN_DIR/prism-ui"
echo "Installed to $BIN_DIR: prism-ctl, prism-lock, prismd, prism-ui"

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

echo "[3/4] Installing systemd user service..."
cp -f "$SCRIPT_DIR/prismd.service" "$SYSTEMD_USER_DIR/prismd.service"
if command -v systemctl >/dev/null 2>&1; then
    systemctl --user daemon-reload || true
    echo "Service installed to $SYSTEMD_USER_DIR/prismd.service"
fi

echo "[4/4] Compositor Integration Instructions:"
echo "To switch completely from Quickshell to the native Rust shell:"
echo " - MangoWM:  add 'source $SCRIPT_DIR/prism-mango.conf' to ~/.config/mango/config.conf"
echo " - Niri:     add 'include \"$SCRIPT_DIR/prism-niri.kdl\"' to ~/.config/niri/config.kdl"
echo " - Hyprland: add 'source = $SCRIPT_DIR/prism-hyprland.conf' to ~/.config/hypr/hyprland.conf"
echo ""
echo "Installation completed successfully!"
