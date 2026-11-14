#!/bin/bash
set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_DIR="$(dirname "$SCRIPT_DIR")"
TARGET_DIR="$HOME/.config/quickshell/prism"

echo "================================================="
echo "                Prism Installer                  "
echo "================================================="
echo ""

# 1. Sanity check: Ensure we are inside the cloned repo
if [ ! -d "$REPO_DIR/deploy" ] || [ ! -f "$REPO_DIR/shell.qml" ]; then
    echo "ERROR: Install script must be run from within the cloned repository." >&2
    exit 1
fi

# 2. Dependency Diagnostics & Installation
echo "Checking dependencies..."
MISSING_PACMAN_PKGS=()
MISSING_AUR_PKGS=()

check_dep() {
    local cmd="$1"
    local desc="$2"
    local pacman_pkg="$3"
    local is_aur="${4:-false}"
    local required="${5:-false}"

    if command -v "$cmd" >/dev/null 2>&1; then
        echo "  [✓] $cmd ($desc)"
        return 0
    else
        if [ "$required" = "true" ]; then
            echo "  [✗] $cmd ($desc) - REQUIRED"
        else
            echo "  [ ] $cmd ($desc) - optional"
        fi

        if [ -n "$pacman_pkg" ]; then
            if [ "$is_aur" = "true" ]; then
                MISSING_AUR_PKGS+=("$pacman_pkg")
            else
                MISSING_PACMAN_PKGS+=("$pacman_pkg")
            fi
        fi
        return 0
    fi
}

# Check Qt6 5Compat module (required for Qt5Compat QML imports)
if pacman -Q qt6-5compat >/dev/null 2>&1 || [ -d /usr/lib/qt6/qml/Qt5Compat ]; then
    echo "  [✓] qt6-5compat (Qt6 compatibility module)"
else
    echo "  [✗] qt6-5compat (Qt6 compatibility module) - REQUIRED"
    MISSING_PACMAN_PKGS+=("qt6-5compat")
fi

# Core
if ! command -v quickshell >/dev/null 2>&1 && ! command -v qs >/dev/null 2>&1; then
    echo "  [✗] quickshell / qs (Quickshell runtime) - REQUIRED"
    MISSING_AUR_PKGS+=("quickshell")
else
    echo "  [✓] quickshell (Quickshell runtime)"
fi

# Compositor check (supports Niri, MangoWM, and Hyprland)
FOUND_WMS=()
if command -v niri >/dev/null 2>&1; then
    echo "  [✓] niri compositor"
    FOUND_WMS+=("niri")
else
    echo "  [ ] niri compositor"
fi
if command -v mango >/dev/null 2>&1 || command -v mangowm >/dev/null 2>&1; then
    echo "  [✓] mangowm compositor"
    FOUND_WMS+=("mangowm")
else
    echo "  [ ] mangowm compositor"
fi
if command -v Hyprland >/dev/null 2>&1; then
    echo "  [✓] hyprland compositor"
    FOUND_WMS+=("hyprland")
else
    echo "  [ ] hyprland compositor"
fi

if [ ${#FOUND_WMS[@]} -eq 0 ]; then
    echo "  [✗] No supported Wayland compositor found (niri, mangowm, or hyprland) - REQUIRED"
    MISSING_PACMAN_PKGS+=("niri")
fi
check_dep "greetd" "greetd display manager" "greetd" false true
check_dep "jq" "JSON processor" "jq" false true
check_dep "python3" "Python runtime" "python" false true
check_dep "wl-paste" "Wayland clipboard utility" "wl-clipboard" false false
check_dep "wtype" "Emoji picker auto-paste (virtual keyboard)" "wtype" false false

# Theming & Wallpaper
check_dep "matugen" "Material You color generation" "matugen-bin" true false
check_dep "magick" "ImageMagick thumbnail scaling" "imagemagick" false false

if ! command -v awww >/dev/null 2>&1 && ! command -v swww >/dev/null 2>&1; then
    echo "  [ ] awww / swww (Wallpaper daemon) - optional"
    MISSING_AUR_PKGS+=("awww-git (or swww)")
else
    echo "  [✓] wallpaper daemon ($(command -v awww >/dev/null 2>&1 && echo "awww" || echo "swww"))"
fi

# Media & Hardware
check_dep "playerctl" "MPRIS player controls" "playerctl" false false
check_dep "pactl" "Audio sink management" "libpulse" false false
check_dep "bluetoothctl" "Bluetooth status" "bluez-utils" false false
check_dep "ffmpeg" "Video thumbnails" "ffmpeg" false false
check_dep "brightnessctl" "Laptop screen brightness" "brightnessctl" false false
check_dep "ddcutil" "External monitor DDC/CI brightness" "ddcutil" false false

# Recommended fonts for shell and lockscreen
if fc-list : family 2>/dev/null | grep -qi "inter"; then
    echo "  [✓] Inter font"
else
    echo "  [ ] Inter font (clean UI typography) - recommended"
    MISSING_PACMAN_PKGS+=("inter-font")
fi

if fc-list : family 2>/dev/null | grep -qi "nerd font"; then
    echo "  [✓] Nerd Font icons"
else
    echo "  [ ] Nerd Font (lockscreen symbols & shell icons) - recommended"
    MISSING_PACMAN_PKGS+=("ttf-cascadia-code-nerd")
fi

echo ""

# Prompt to install missing official packages
if [ ${#MISSING_PACMAN_PKGS[@]} -gt 0 ]; then
    if command -v pacman >/dev/null 2>&1; then
        echo "The following missing packages can be installed via pacman:"
        for pkg in "${MISSING_PACMAN_PKGS[@]}"; do
            echo "  - $pkg"
        done
        echo ""
        read -rp "Would you like to install them now with 'sudo pacman -S --needed'? [Y/n] " install_confirm
        install_confirm="${install_confirm:-y}"
        if [[ "$install_confirm" =~ ^[Yy]$ ]]; then
            sudo pacman -S --needed "${MISSING_PACMAN_PKGS[@]}"
            echo "Packages installed successfully."
        else
            echo "Skipping pacman package installation."
        fi
    else
        echo "Notice: Missing packages (${MISSING_PACMAN_PKGS[*]}), but pacman was not found."
    fi
    echo ""
fi

# Display AUR packages notice if any
if [ ${#MISSING_AUR_PKGS[@]} -gt 0 ]; then
    echo "Note: The following package(s) can be installed from the AUR:"
    for pkg in "${MISSING_AUR_PKGS[@]}"; do
        echo "  - $pkg"
    done
    echo "Example: yay -S ${MISSING_AUR_PKGS[*]}"
    echo ""
fi

# 3. Copy live config (Excluding internal/dev artifacts)
if [ "$REPO_DIR" != "$TARGET_DIR" ]; then
    echo "Deploying live config to $TARGET_DIR..."
    mkdir -p "$TARGET_DIR"
    rsync -a \
        --exclude 'deploy' \
        --exclude 'deploy-install' \
        --exclude 'scratch' \
        --exclude 'evidence' \
        --exclude 'reports' \
        --exclude '.git' \
        --exclude '.gitignore' \
        --exclude '__pycache__' \
        --exclude 'assets' \
        --exclude 'README.md' \
        "$REPO_DIR/" "$TARGET_DIR/"
else
    echo "Config directory is current repository: $TARGET_DIR"
fi

# 4. Ensure scripts and helpers have execute permissions
echo "Setting executable permissions on scripts and binaries..."
chmod +x "$TARGET_DIR"/scripts/*.sh "$TARGET_DIR"/scripts/*.py 2>/dev/null || true

GAMMA_BIN="$TARGET_DIR/helpers/gamma-ctl/target/release/gamma-ctl"
if [ -f "$GAMMA_BIN" ]; then
    chmod +x "$GAMMA_BIN"
    echo "  [✓] gamma-ctl helper binary verified."
else
    echo "  [!] gamma-ctl binary not found."
    if command -v cargo >/dev/null 2>&1; then
        echo "      Building gamma-ctl helper with cargo..."
        (cd "$TARGET_DIR/helpers/gamma-ctl" && cargo build --release)
        chmod +x "$GAMMA_BIN" 2>/dev/null || true
    else
        echo "      Cargo is not installed. Blue light filter will be unavailable until gamma-ctl is compiled."
    fi
fi

# 5. Initialize cache and runtime state directories
echo "Initializing cache and state directories..."
mkdir -p "$HOME/.cache/quickshell-clipboard"
mkdir -p "${XDG_CACHE_HOME:-$HOME/.cache}/Prism/thumbs"
mkdir -p "${XDG_CACHE_HOME:-$HOME/.cache}/Prism/wallpaper-depth"
mkdir -p "${XDG_STATE_HOME:-$HOME/.local/state}/prism/idle-dim"
mkdir -p "$HOME/Pictures/Screenshots"

# Ensure backward-compatibility symlink exists
mkdir -p "$HOME/.config/quickshell"
if [ "$TARGET_DIR" != "$HOME/.config/quickshell/xeon-shell-niri" ]; then
    ln -sfn "$TARGET_DIR" "$HOME/.config/quickshell/xeon-shell-niri" 2>/dev/null || true
fi

# 6. Matugen configuration
if command -v matugen >/dev/null 2>&1; then
    MATUGEN_DIR="$HOME/.config/matugen"
    MATUGEN_TEMPLATES="$MATUGEN_DIR/templates"
    MATUGEN_CONF="$MATUGEN_DIR/config.toml"

    mkdir -p "$MATUGEN_TEMPLATES"
    if [ -f "$REPO_DIR/deploy/matugen/qs-colors.json" ]; then
        cp -n "$REPO_DIR/deploy/matugen/qs-colors.json" "$MATUGEN_TEMPLATES/qs-colors.json" 2>/dev/null || true
    fi

    if [ -f "$MATUGEN_CONF" ]; then
        if ! grep -q '\[templates\.prism\]' "$MATUGEN_CONF"; then
            cp -n "$MATUGEN_CONF" "$MATUGEN_CONF.bak-before-prism" 2>/dev/null || true
            echo "Adding a Matugen template for Prism to $MATUGEN_CONF..."
            cat >> "$MATUGEN_CONF" << 'EOF'

[templates.prism]
input_path = '~/.config/matugen/templates/qs-colors.json'
output_path = '~/.config/quickshell/prism/data/colors.json'
EOF
        fi
    else
        echo "Creating default Matugen config in $MATUGEN_CONF..."
        cat > "$MATUGEN_CONF" << 'EOF'
[config]
caching = false

[global]
backend = "quantiles"
mode = "dark"

[templates.prism]
input_path = '~/.config/matugen/templates/qs-colors.json'
output_path = '~/.config/quickshell/prism/data/colors.json'
EOF
    fi
fi

# 7. Settings migration (supports legacy Xeon Shell configs)
PRISM_CONF_DIR="$HOME/.config/Prism"
PRISM_CONF="$PRISM_CONF_DIR/prism.conf"
OLD_XEON_NIRI_CONF="$HOME/.config/Xeon Shell Niri/xeon-shell-niri.conf"
OLD_XEON_HYPR_CONF="$HOME/.config/Xeon Shell/xeon-shell.conf"

mkdir -p "$PRISM_CONF_DIR"
if [ ! -f "$PRISM_CONF" ]; then
    if [ -f "$OLD_XEON_NIRI_CONF" ]; then
        echo "Migrating settings from '$OLD_XEON_NIRI_CONF' to '$PRISM_CONF'..."
        cp "$OLD_XEON_NIRI_CONF" "$PRISM_CONF"
    elif [ -f "$OLD_XEON_HYPR_CONF" ]; then
        echo "Migrating settings from '$OLD_XEON_HYPR_CONF' to '$PRISM_CONF'..."
        cp "$OLD_XEON_HYPR_CONF" "$PRISM_CONF"
    fi
fi

# 8. Privileged section (Prism Greeter & Hardware Privileges)
echo ""
echo "-------------------------------------------------"
echo "System Greeter & Hardware Setup (requires sudo, OPTIONAL):"
echo "  - Setup custom Quickshell Greeter for greetd"
echo "  - Configure user/group permissions & PAM authentication"
echo "  - Configure i2c permissions for external monitor dimming"
echo ""
echo "WARNING: the greeter is SYSTEM-WIDE. This configures /etc/greetd/config.toml"
echo "(a .bak copy is kept) and /etc/greetd/quickshell-greeter/."
echo "You can choose which compositor (Niri, MangoWM, or Hyprland) hosts the greeter."
echo "Logging in to any session works from the greeter regardless of host compositor."
echo "-------------------------------------------------"
if ! command -v greetd >/dev/null 2>&1; then
    echo "NOTICE: 'greetd' is not installed yet. You can still set up the files,"
    echo "but you will need to install 'greetd' with your package manager."
    echo ""
fi
read -rp "Proceed with the greeter and hardware setup? [y/N] " confirm
confirm="${confirm:-n}"
if [[ ! "$confirm" =~ ^[Yy]$ ]]; then
    echo "Skipping system greeter setup."
    echo ""
    echo "================================================="
    echo "Prism user configuration is complete!"
    echo "Run the desktop shell with:"
    echo "  qs -c prism"
    echo "================================================="
    exit 0
fi

# Select greeter host compositor
echo ""
echo "Select the compositor to host the greetd login screen:"
echo "  1) Niri (default)"
echo "  2) MangoWM"
echo "  3) Hyprland"
read -rp "Selection [1-3] (default: 1): " wm_choice
wm_choice="${wm_choice:-1}"

case "$wm_choice" in
    2)
        SELECTED_WM="mango"
        GREETER_SESSION_CMD="mango -c /etc/greetd/mango-greet.conf"
        ;;
    3)
        SELECTED_WM="hyprland"
        GREETER_SESSION_CMD="Hyprland --config /etc/greetd/hyprland-greet.conf"
        ;;
    *)
        SELECTED_WM="niri"
        GREETER_SESSION_CMD="niri --config /etc/greetd/niri-greet.kdl"
        ;;
esac

# Validate sudo credentials once
sudo -v

# Keep-alive loop and cleanup
cleanup() {
    if [ -n "${KEEP_ALIVE_PID:-}" ]; then
        kill "$KEEP_ALIVE_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT

# Run keep-alive in background
(while true; do sudo -n true; sleep 60; kill -0 "$$" || exit; done 2>/dev/null) &
KEEP_ALIVE_PID=$!

echo "Setting up greeter permissions and state directory..."
if ! getent group greeter-sync >/dev/null; then
    sudo groupadd greeter-sync
fi
sudo usermod -aG greeter-sync "$USER"

if ! id greeter >/dev/null 2>&1; then
    sudo useradd --system --no-create-home --shell /usr/bin/nologin greeter
fi
sudo usermod -aG greeter-sync greeter
sudo usermod -aG video greeter
sudo usermod -aG input greeter

# Setup DDC/CI i2c permissions if i2c group exists
if getent group i2c >/dev/null; then
    sudo usermod -aG i2c "$USER" 2>/dev/null || true
fi
sudo modprobe i2c-dev 2>/dev/null || true

sudo mkdir -p /var/lib/greetd/quickshell-greeter
sudo chown root:greeter-sync /var/lib/greetd/quickshell-greeter
sudo chmod 2775 /var/lib/greetd/quickshell-greeter

# Pre-flight check for new group
if ! groups | grep -q '\bgreeter-sync\b'; then
    echo "NOTE: 'greeter-sync' group is not active in this shell session yet."
fi

echo "Deploying greeter files..."
sudo mkdir -p /etc/greetd/quickshell-greeter
sudo rsync -a --exclude '__pycache__' "$REPO_DIR/deploy/etc/greetd/quickshell-greeter/" /etc/greetd/quickshell-greeter/
sudo rsync -a "$REPO_DIR/deploy/etc/greetd/niri-greet.kdl" /etc/greetd/niri-greet.kdl
sudo rsync -a "$REPO_DIR/deploy/etc/greetd/mango-greet.conf" /etc/greetd/mango-greet.conf
sudo rsync -a "$REPO_DIR/deploy/etc/greetd/hyprland-greet.conf" /etc/greetd/hyprland-greet.conf

if [ -f /etc/greetd/config.toml ]; then
    sudo cp /etc/greetd/config.toml /etc/greetd/config.toml.bak
    echo "Backed up existing config.toml to config.toml.bak"
fi

sudo tee /etc/greetd/config.toml >/dev/null << EOF
# greetd configuration for Xeon Shell's Quickshell greeter.

[terminal]
vt = 1

[default_session]
command = "$GREETER_SESSION_CMD"
user = "greeter"
EOF

echo ""
echo "Please review PAM configuration diff (if it exists):"
if [ -f /etc/pam.d/quickshell ]; then
    diff -u /etc/pam.d/quickshell "$REPO_DIR/deploy/pam.d/quickshell" || true
else
    echo "(New file: /etc/pam.d/quickshell)"
fi
read -rp "Install PAM config? [y/N] " confirm_pam
if [[ "$confirm_pam" =~ ^[Yy]$ ]]; then
    [ -f "$REPO_DIR/deploy/pam.d/quickshell" ] || { echo "missing pam source"; exit 1; }
    sudo cp "$REPO_DIR/deploy/pam.d/quickshell" /etc/pam.d/quickshell
    echo "Installed /etc/pam.d/quickshell"
fi

# Print autostart + keybind guidance
echo ""
echo "=== IMPORTANT: Desktop Autostart & Keybinds ==="
case "${SELECTED_WM:-niri}" in
    mango)
        echo "For MangoWM, add the following to ~/.config/mango/config.conf:"
        echo "  source = ~/.config/mango/prism-mango.conf"
        echo "(or copy from $REPO_DIR/deploy/prism-mango.conf)"
        ;;
    hyprland)
        echo "For Hyprland, add the following to ~/.config/hypr/hyprland.conf:"
        echo "  source = ~/.config/hypr/prism-hyprland.conf"
        echo "(or copy from $REPO_DIR/deploy/prism-hyprland.conf)"
        ;;
    *)
        echo "For Niri, add the following to ~/.config/niri/config.kdl:"
        echo "  include \"prism-niri.kdl\""
        echo "(or copy from $REPO_DIR/deploy/prism-niri.kdl)"
        ;;
esac
echo ""
echo "All compositor configuration snippets are available in $REPO_DIR/deploy/:"
echo "  - Niri:     $REPO_DIR/deploy/prism-niri.kdl"
echo "  - MangoWM:  $REPO_DIR/deploy/prism-mango.conf"
echo "  - Hyprland: $REPO_DIR/deploy/prism-hyprland.conf"
echo "==============================================="
echo ""

echo "Running initial greeter wallpaper sync..."
sudo -u "$USER" -g greeter-sync bash "$TARGET_DIR/scripts/sync-greeter-wallpaper.sh" || true

echo "Writing default greeter config..."
sudo -u "$USER" -g greeter-sync bash -c 'printf "{\n  \"lockscreenAlignment\": \"left\",\n  \"rememberLastUser\": false\n}\n" > /var/lib/greetd/quickshell-greeter/config-snapshot.json && chmod 664 /var/lib/greetd/quickshell-greeter/config-snapshot.json'

echo ""
echo "================================================="
echo "Installation complete!"
echo ""
echo "To enable the custom Prism login greeter:"
echo "  sudo systemctl enable greetd"
echo ""
echo "Note: If this is your first time setting up greeter-sync or i2c,"
echo "please log out or reboot your system for new group permissions to take effect."
echo ""
echo "Start Prism using:"
echo "  qs -c prism"
echo "================================================="
