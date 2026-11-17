#!/usr/bin/env bash
# install.sh — Build and install Prism for the current user
#
#   ./deploy/install.sh             build, install to ~/.local/bin, restart prismd if it runs
#   ./deploy/install.sh --greeter   also install prism-lock to /usr/local/bin for the greetd
#                                   greeter (asks for your password through sudo)
#
# Nothing else outside your home directory is changed: the remaining system steps (PAM
# service, greetd config) are printed at the end for you to run.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN_DIR="${HOME}/.local/bin"
SYSTEMD_USER_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
SHARE_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/prism"
BINARIES=(prismd prism-ui prism-ctl prism-lock prism-phoned prism-rec)

GREETER=0
for arg in "$@"; do
    case "$arg" in
        --greeter) GREETER=1 ;;
        -h|--help) sed -n '2,10p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "Unknown option: $arg (see --help)" >&2; exit 2 ;;
    esac
done

echo "========================================================"
echo "    Installing Prism (Rust Wayland desktop shell)       "
echo "========================================================"

echo "[1/6] Checking build dependencies..."
missing=()
command -v cargo >/dev/null 2>&1 || missing+=("cargo (Rust 1.77 or newer)")
if command -v pkg-config >/dev/null 2>&1; then
    # pkg-config module and what to install when it is missing.
    deps=(
        "wayland-client:wayland"
        "xkbcommon:libxkbcommon"
        "cairo:cairo"
        "pangocairo:pango"
        "gdk-pixbuf-2.0:gdk-pixbuf2"
        "gdk-3.0:gtk3"
        "dbus-1:dbus"
        "libavcodec:ffmpeg"
        "libavformat:ffmpeg"
        "libavutil:ffmpeg"
        "libswscale:ffmpeg"
        "libswresample:ffmpeg"
        "libpipewire-0.3:pipewire (development files)"
    )
    for d in "${deps[@]}"; do
        pkg-config --exists "${d%%:*}" || missing+=("${d%%:*} (${d#*:})")
    done
else
    missing+=("pkg-config")
fi
[ -f /usr/include/security/pam_appl.h ] || pkg-config --exists pam 2>/dev/null || missing+=("PAM headers (pam / libpam-dev)")
# bindgen (FFmpeg and PipeWire bindings) needs libclang at build time.
# (grep reads all of ldconfig's output: with pipefail, `grep -q` would make the pipeline fail.)
if ! ldconfig -p 2>/dev/null | grep 'libclang\.so' >/dev/null && [ -z "${LIBCLANG_PATH:-}" ]; then
    missing+=("libclang (clang)")
fi
if [ ${#missing[@]} -gt 0 ]; then
    echo "Missing build dependencies:" >&2
    printf '  - %s\n' "${missing[@]}" >&2
    echo "Install them with your package manager (names are the Arch ones) and run this again." >&2
    exit 1
fi

echo "[2/6] Compiling (release)..."
cargo build --release --manifest-path "$REPO_DIR/Cargo.toml"

echo "[3/6] Installing binaries to $BIN_DIR..."
mkdir -p "$BIN_DIR"
for b in "${BINARIES[@]}"; do
    # install(1) writes a new file, so binaries that are running are replaced safely.
    install -m755 "$REPO_DIR/target/release/$b" "$BIN_DIR/$b"
done
echo "Installed: ${BINARIES[*]}"
case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *) echo "Note: $BIN_DIR is not in your PATH; compositor keybinds call prism-ctl from there." ;;
esac
for b in prismd prism-ui prism-ctl; do
    if [ -e "/usr/local/bin/$b" ]; then
        echo "Note: an old /usr/local/bin/$b exists (earlier installers put it there); remove it with"
        echo "      sudo rm /usr/local/bin/$b   so it can never shadow the one in $BIN_DIR."
    fi
done

echo "[4/6] Installing helper scripts to $SHARE_DIR/scripts..."
# Keybind parsers and the wallpaper depth generator; prismd and prism-ui look here.
mkdir -p "$SHARE_DIR/scripts"
install -m644 "$REPO_DIR"/scripts/*.py "$SHARE_DIR/scripts/"
install -m755 "$REPO_DIR"/scripts/*.sh "$SHARE_DIR/scripts/"
# Replaced by the notification server in prismd.
rm -f "$SHARE_DIR/scripts/prism-dbus-notif.py"

echo "[5/6] Installing the systemd user service..."
mkdir -p "$SYSTEMD_USER_DIR"
install -m644 "$SCRIPT_DIR/prismd.service" "$SYSTEMD_USER_DIR/prismd.service"
if command -v systemctl >/dev/null 2>&1; then
    systemctl --user daemon-reload || true
    if systemctl --user is-active --quiet prismd; then
        # Apps run in their own scopes (app.slice), so restarting prismd never closes them.
        systemctl --user restart prismd
        echo "prismd was running: restarted with the new build."
    else
        echo "Enable it once with: systemctl --user enable --now prismd"
    fi
fi

echo "[6/6] Greeter binary..."
if [ "$GREETER" = 1 ]; then
    echo "Installing prism-lock to /usr/local/bin for the greetd greeter (sudo)..."
    sudo install -m755 "$REPO_DIR/target/release/prism-lock" /usr/local/bin/prism-lock
    echo "Installed /usr/local/bin/prism-lock."
elif [ -e /usr/local/bin/prism-lock ] && ! cmp -s "$REPO_DIR/target/release/prism-lock" /usr/local/bin/prism-lock; then
    echo "The greeter's /usr/local/bin/prism-lock is from an older build; update it with:"
    echo "  $0 --greeter"
else
    echo "Skipped (use --greeter to install prism-lock for greetd)."
fi

echo ""
echo "Setup (once):"
echo " - Compositor: keybinds, and for MangoWM the layer rules (blur only behind cards):"
echo "     MangoWM:  add 'source=$SCRIPT_DIR/prism-mango.conf' to ~/.config/mango/config.conf"
echo "     Niri:     add 'include \"$SCRIPT_DIR/prism-niri.kdl\"' to ~/.config/niri/config.kdl"
echo "     Hyprland: add 'source = $SCRIPT_DIR/prism-hyprland.conf' to ~/.config/hypr/hyprland.conf"
echo "   Do not also start prismd from the compositor config: systemd runs it."
echo " - Theme: add the template in $SCRIPT_DIR/matugen/prism-colors.json to"
echo "   ~/.config/matugen/config.toml as [templates.prism-rust], output ~/.config/prism/colors.json."
echo " - Phone sync (KDE Connect app): allow UDP 1716 and TCP 1714-1764 through your firewall,"
echo "   e.g. 'sudo ufw allow 1714:1764/udp && sudo ufw allow 1714:1764/tcp'."
echo ""
echo "Lock screen and greeter (run these yourself; they change system files):"
echo " - PAM service for unlocking (otherwise 'login' is used):"
echo "     sudo install -Dm644 $SCRIPT_DIR/pam.d/prism /etc/pam.d/prism"
echo " - Check the lock screen accepts your password without locking anything:"
echo "     prism-lock --check-auth"
echo " - Greeter: install prism-lock for greetd ($0 --greeter), then greetd on cage:"
echo "     sudo cp /etc/greetd/config.toml /etc/greetd/config.toml.bak"
echo "     sudo install -m644 $SCRIPT_DIR/greetd/config.toml /etc/greetd/config.toml"
echo "   Test before rebooting: from a TTY run 'sudo systemctl restart greetd'."
echo ""
echo "Installation completed successfully!"
