#!/usr/bin/env bash
# install.sh — Build and install Prism for the current user
#
#   ./deploy/install.sh               build, install to ~/.local/bin, restart prismd if it runs
#   ./deploy/install.sh --greeter     also set up the greetd greeter without asking
#   ./deploy/install.sh --no-greeter  never touch the greeter, and do not ask
#   ./deploy/install.sh --yes         answer yes to every question (PATH, OCR, PAM, greeter)
#
# Run from a terminal, it asks before changing anything outside ~/.local: adding ~/.local/bin
# to your shell's PATH, installing the lock screen's PAM service (sudo), and setting up the
# greeter (sudo: prism-lock in /usr/local/bin, /var/lib/greetd/prism and the greetd config,
# the old one backed up).
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"
BIN_DIR="${HOME}/.local/bin"
SYSTEMD_USER_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
SHARE_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/prism"
BINARIES=(prismd prism-ui prism-ctl prism-native-host prism-lock prism-phoned prism-rec prism-video)

GREETER=ask
YES=0
for arg in "$@"; do
    case "$arg" in
        --greeter) GREETER=1 ;;
        --no-greeter) GREETER=0 ;;
        -y|--yes) YES=1 ;;
        -h|--help) sed -n '2,12p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "Unknown option: $arg (see --help)" >&2; exit 2 ;;
    esac
done

# ask "question" y|n: the default is taken for an empty answer, and without a terminal.
ask() {
    local hint="[y/N]" reply
    [ "$2" = y ] && hint="[Y/n]"
    if [ "$YES" = 1 ]; then return 0; fi
    if [ ! -t 0 ]; then [ "$2" = y ]; return; fi
    read -r -p "$1 $hint " reply || reply=""
    case "${reply:-$2}" in [yY]*) return 0 ;; *) return 1 ;; esac
}

echo "========================================================"
echo "    Installing Prism (Rust Wayland desktop shell)       "
echo "========================================================"

echo "[1/10] Checking build dependencies..."
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

echo "[2/10] Compiling (release)..."
cargo build --release --manifest-path "$REPO_DIR/Cargo.toml"

echo "[3/10] Installing binaries to $BIN_DIR..."
mkdir -p "$BIN_DIR"
for b in "${BINARIES[@]}"; do
    # install(1) writes a new file, so binaries that are running are replaced safely.
    install -m755 "$REPO_DIR/target/release/$b" "$BIN_DIR/$b"
done
echo "Installed: ${BINARIES[*]}"

# prism-ctl must be found by name: from a terminal, and from the compositor's keybinds.
# (Some distributions add ~/.local/bin to PATH at login only if it already existed then.)
add_to_path() {
    local line='export PATH="$HOME/.local/bin:$PATH"' shell rc
    shell="$(basename "${SHELL:-bash}")"
    case "$shell" in
        zsh) rc="$HOME/.zshenv" ;;
        fish) rc="" ;;
        *) rc="$HOME/.bashrc" ;;
    esac
    # Login shells (which start the compositor from a TTY) read ~/.profile or ~/.bash_profile.
    for f in "$rc" "$HOME/.profile" "$HOME/.bash_profile"; do
        [ -n "$f" ] || continue
        [ "$f" = "$rc" ] || [ -f "$f" ] || continue
        grep -qs '\.local/bin' "$f" && grep -qs 'PATH' "$f" && continue
        printf '\n# Added by the Prism installer\n%s\n' "$line" >> "$f"
        echo "  added to $f"
    done
    if [ "$shell" = fish ] || [ -d "${XDG_CONFIG_HOME:-$HOME/.config}/fish" ]; then
        local conf="${XDG_CONFIG_HOME:-$HOME/.config}/fish/conf.d/prism-path.fish"
        if [ ! -e "$conf" ]; then
            mkdir -p "$(dirname "$conf")"
            echo "fish_add_path -g $BIN_DIR  # Added by the Prism installer" > "$conf"
            echo "  added $conf"
        fi
    fi
}
case ":$PATH:" in
    *":$BIN_DIR:"*) ;;
    *)
        echo "$BIN_DIR is not in your PATH: prism-ctl (terminal and compositor keybinds) would not be found."
        if ask "Add it to your shell's PATH?" y; then
            add_to_path
            echo "  Open a new terminal (log out and in for the compositor) to use it."
        else
            echo "  Skipped: add $BIN_DIR to PATH yourself."
        fi
        ;;
esac
for b in prismd prism-ui prism-ctl; do
    if [ -e "/usr/local/bin/$b" ]; then
        echo "Note: an old /usr/local/bin/$b exists (earlier installers put it there); remove it with"
        echo "      sudo rm /usr/local/bin/$b   so it can never shadow the one in $BIN_DIR."
    fi
done

echo "[4/10] Installing helper scripts to $SHARE_DIR/scripts..."
# Keybind parsers and the wallpaper depth generator; prismd and prism-ui look here.
mkdir -p "$SHARE_DIR/scripts"
install -m644 "$REPO_DIR"/scripts/*.py "$SHARE_DIR/scripts/"
install -m755 "$REPO_DIR"/scripts/*.sh "$SHARE_DIR/scripts/"
# Replaced by the notification server in prismd.
rm -f "$SHARE_DIR/scripts/prism-dbus-notif.py"

echo "[5/10] Installing the systemd user service..."
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

echo "[6/10] Desktop portal (screenshots, screen sharing and appearance for apps)..."
# prism-ui serves org.freedesktop.impl.portal.desktop.prism. xdg-desktop-portal reads portal
# descriptions from XDG_DATA_DIRS, so a drop-in adds ~/.local/share for it (no root needed).
DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
CONFIG_HOME="${XDG_CONFIG_HOME:-$HOME/.config}"
# App icon (hicolor "prism") and the Prism Settings launcher entry.
install -Dm644 "$REPO_DIR/assets/logo/prism-app.svg" "$DATA_HOME/icons/hicolor/scalable/apps/prism.svg"
for size in 16 24 32 48 64 128 256 512; do
    install -d "$DATA_HOME/icons/hicolor/${size}x${size}/apps"
    rsvg-convert -w "$size" -h "$size" "$REPO_DIR/assets/logo/prism-app.svg" -o "$DATA_HOME/icons/hicolor/${size}x${size}/apps/prism.png" 2>/dev/null || true
done
gtk-update-icon-cache -q -t "$DATA_HOME/icons/hicolor" 2>/dev/null || true
install -Dm644 "$SCRIPT_DIR/prism-settings.desktop" "$DATA_HOME/applications/prism-settings.desktop"
# man finds ~/.local/share/man through ~/.local/bin on PATH.
install -Dm644 "$REPO_DIR/man/prism-ctl.1" "$DATA_HOME/man/man1/prism-ctl.1"
# prismd registers prism-native-host (the Prism Downloads Firefox extension's helper) with the
# installed browsers itself when it starts (dl.rs register_native_host).
install -Dm644 "$SCRIPT_DIR/portal/prism.portal" "$DATA_HOME/xdg-desktop-portal/portals/prism.portal"
install -Dm644 "$SCRIPT_DIR/portal/org.freedesktop.impl.portal.desktop.prism.service" "$DATA_HOME/dbus-1/services/org.freedesktop.impl.portal.desktop.prism.service"
install -Dm644 "$SCRIPT_DIR/portal/xdg-desktop-portal-prism.conf" "$SYSTEMD_USER_DIR/xdg-desktop-portal.service.d/prism.conf"
# Which portal does what, per desktop (XDG_CURRENT_DESKTOP); an existing file is left alone.
desktop="$(echo "${XDG_CURRENT_DESKTOP:-mango}" | cut -d: -f1 | tr '[:upper:]' '[:lower:]')"
portals_conf="$CONFIG_HOME/xdg-desktop-portal/$desktop-portals.conf"
if [ ! -e "$portals_conf" ]; then
    install -Dm644 "$SCRIPT_DIR/portal/mango-portals.conf" "$portals_conf"
    echo "Prism handles Screenshot and ScreenCast ($portals_conf)."
elif ! grep -q '=prism' "$portals_conf"; then
    echo "Note: $portals_conf exists; to let Prism share screens and take screenshots for apps, set"
    echo "      org.freedesktop.impl.portal.Screenshot=prism and org.freedesktop.impl.portal.ScreenCast=prism"
fi
if [ -e "$portals_conf" ] && ! grep -q 'Settings=prism' "$portals_conf"; then
    echo "Note: for apps to follow Prism's dark/light mode and accent colour, add to $portals_conf:"
    echo "      org.freedesktop.impl.portal.Settings=prism;gtk"
fi
if command -v systemctl >/dev/null 2>&1; then
    systemctl --user daemon-reload || true
    systemctl --user try-restart xdg-desktop-portal.service || true
fi

echo "[7/10] Copy text (OCR) for the screenshot overlay..."
# The overlay's "Copy text" mode reads text with tesseract (system package, needs root).
if command -v tesseract >/dev/null 2>&1; then
    echo "tesseract is installed."
else
    ocr_pkgs=""
    if command -v pacman >/dev/null 2>&1; then
        ocr_pkgs="tesseract tesseract-data-eng"; ocr_cmd="sudo pacman -S --needed $ocr_pkgs"
    elif command -v apt-get >/dev/null 2>&1; then
        ocr_pkgs="tesseract-ocr tesseract-ocr-eng"; ocr_cmd="sudo apt-get install -y $ocr_pkgs"
    elif command -v dnf >/dev/null 2>&1; then
        ocr_pkgs="tesseract tesseract-langpack-eng"; ocr_cmd="sudo dnf install -y $ocr_pkgs"
    fi
    if [ -z "$ocr_pkgs" ]; then
        echo "Copy text needs tesseract with English data; install it with your package manager."
    elif ask "Copy text needs tesseract. Install $ocr_pkgs (sudo)?" y; then
        $ocr_cmd || echo "Installing tesseract failed; Copy text stays unavailable until it is installed."
    else
        echo "Skipped: Copy text needs tesseract (install later with: $ocr_cmd)."
    fi
fi

echo "[8/10] Lock screen PAM service..."
# prism-lock unlocks through /etc/pam.d/prism, else the "login" service. Prism's file includes
# system-local-login (Arch and derivatives); without that file it would refuse every password.
PAM_OK=0
if [ -e /etc/pam.d/prism ]; then
    PAM_OK=1
    echo "Installed (/etc/pam.d/prism)."
elif [ ! -e /etc/pam.d/system-local-login ]; then
    echo "Skipped: this system has no /etc/pam.d/system-local-login, so the lock screen uses 'login'."
elif ask "Install the lock screen's PAM service /etc/pam.d/prism (sudo; otherwise 'login' is used)?" y; then
    sudo install -Dm644 "$SCRIPT_DIR/pam.d/prism" /etc/pam.d/prism && PAM_OK=1
    echo "Installed /etc/pam.d/prism."
else
    echo "Skipped: the lock screen uses the 'login' PAM service."
fi

# Battery charge limit: a udev rule makes the attribute writable for wheel (laptops only).
if ls /sys/class/power_supply/BAT*/charge_control_end_threshold >/dev/null 2>&1 && [ ! -e /etc/udev/rules.d/90-prism-charge-limit.rules ]; then
    if ask "This laptop can stop charging early. Let Prism set the charge limit without a password (udev rule, sudo)?" y; then
        sudo install -Dm644 "$SCRIPT_DIR/udev/90-prism-charge-limit.rules" /etc/udev/rules.d/90-prism-charge-limit.rules \
            && sudo udevadm control --reload && sudo udevadm trigger --subsystem-match=power_supply --action=change
    fi
fi

echo "[9/10] Greeter..."
GREETER_BIN=/usr/local/bin/prism-lock
GREETD_CONF=/etc/greetd/config.toml
if [ "$GREETER" = ask ]; then
    if [ -e "$GREETER_BIN" ]; then
        # Set up before: only the binaries need to follow the session's build.
        if cmp -s "$REPO_DIR/target/release/prism-lock" "$GREETER_BIN"; then
            GREETER=0
            echo "The greeter is up to date."
        elif ask "The greeter ($GREETER_BIN) is from an older build. Update it (sudo)?" y; then
            GREETER=1
        else
            GREETER=0
        fi
    elif ask "Also install the Prism login screen (greetd greeter under cage; needs sudo)?" n; then
        GREETER=1
    else
        GREETER=0
        echo "Skipped (run again with --greeter to set it up later)."
    fi
fi
if [ "$GREETER" = 1 ]; then
    missing=()
    command -v greetd >/dev/null 2>&1 || [ -x /usr/bin/greetd ] || missing+=(greetd)
    command -v cage >/dev/null 2>&1 || missing+=(cage)
    if [ ${#missing[@]} -gt 0 ]; then
        echo "The greeter needs: ${missing[*]}. Install it and run this again with --greeter." >&2
    else
        echo "Setting up the greeter (sudo)..."
        sudo install -m755 "$REPO_DIR/target/release/prism-lock" "$REPO_DIR/target/release/prism-video" /usr/local/bin/
        echo "  installed prism-lock and prism-video to /usr/local/bin"
        # Your session copies the wallpaper, palette and settings here; the greeter reads them.
        if [ ! -d /var/lib/greetd/prism ]; then
            greeter_group=greeter
            getent group greeter >/dev/null 2>&1 || greeter_group="$(id -gn)"
            sudo install -d -m755 -o "$(id -un)" -g "$greeter_group" /var/lib/greetd/prism
            echo "  created /var/lib/greetd/prism"
        fi
        if ! sudo grep -qs 'prism-lock' "$GREETD_CONF"; then
            if sudo test -e "$GREETD_CONF"; then
                backup="$GREETD_CONF.bak.$(date +%Y%m%d%H%M%S)"
                sudo cp "$GREETD_CONF" "$backup"
                echo "  backed up the old greetd config to $backup"
            fi
            sudo install -Dm644 "$SCRIPT_DIR/greetd/config.toml" "$GREETD_CONF"
            echo "  installed $GREETD_CONF"
        fi
        if ! systemctl is-enabled --quiet greetd 2>/dev/null; then
            echo "  greetd is not enabled. To switch to it, disable your current display manager"
            echo "  (e.g. 'sudo systemctl disable sddm') and run 'sudo systemctl enable greetd'."
        fi
        echo "  Test it before rebooting: from a TTY run 'sudo systemctl restart greetd'."
    fi
fi

echo "[10/10] Checking the programs and services Prism uses..."
# The list is prism_common::deps (also: prism-ctl deps, and the setup guide's last page).
DEPS_NEEDED=0
deps_out=$("$BIN_DIR/prism-ctl" deps --quiet) || DEPS_NEEDED=1
if [ -z "$deps_out" ]; then
    echo "  Everything is installed."
else
    echo "$deps_out" | sed 's/^/  /'
    deps_cmd=$(echo "$deps_out" | sed -n 's/^  *\(sudo pacman -S .*\)$/\1/p')
    if [ -n "$deps_cmd" ] && ask "Install the missing packages now ($deps_cmd)?" n; then
        $deps_cmd || echo "  The packages were not installed; see above." >&2
    elif [ "$DEPS_NEEDED" = 1 ]; then
        echo "  Some of these are needed: parts of Prism will not work until they are installed." >&2
    fi
    echo "  The setup guide lists them again; 'prism-ctl deps' checks at any time."
fi

echo ""
echo "Setup (once):"
echo " - Compositor: keybinds, and for MangoWM the layer rules (blur only behind cards):"
echo "     MangoWM:  add 'source=$SCRIPT_DIR/prism-mango.conf' to ~/.config/mango/config.conf"
echo "     Niri:     add 'include \"$SCRIPT_DIR/prism-niri.kdl\"' to ~/.config/niri/config.kdl"
echo "     Hyprland: 'ln -s $SCRIPT_DIR/prism-hyprland.lua ~/.config/hypr/prism.lua', then add"
echo "               'require(\"prism\")' to ~/.config/hypr/hyprland.lua"
echo "   Do not also start prismd from the compositor config: systemd runs it."
echo " - Theme: add the template in $SCRIPT_DIR/matugen/prism-colors.json to"
echo "   ~/.config/matugen/config.toml as [templates.prism-rust], output ~/.config/prism/colors.json."
echo " - Phone sync (KDE Connect app): allow UDP 1716 and TCP 1714-1764 through your firewall,"
echo "   e.g. 'sudo ufw allow 1714:1764/udp && sudo ufw allow 1714:1764/tcp'."
if [ "$PAM_OK" = 0 ] && [ -e /etc/pam.d/system-local-login ]; then
    echo " - Lock screen PAM service (otherwise 'login' is used):"
    echo "     sudo install -Dm644 $SCRIPT_DIR/pam.d/prism /etc/pam.d/prism"
fi
echo " - Check the lock screen accepts your password without locking anything:"
echo "     prism-lock --check-auth"
echo ""
echo "Installation completed successfully!"
