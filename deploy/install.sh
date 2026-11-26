#!/usr/bin/env bash
# install.sh — Build and install Prism for the current user
#
#   ./deploy/install.sh               build, install to ~/.local/bin, restart prismd if it runs
#                                     (from a release download: install its prebuilt binaries;
#                                     from the prism-shell package, as prism-setup: set up your
#                                     user for the package's binaries)
#   ./deploy/install.sh --greeter     also set up the greetd greeter without asking
#   ./deploy/install.sh --no-greeter  never touch the greeter, and do not ask
#   ./deploy/install.sh --yes         answer yes to every question (PATH, OCR, PAM, greeter)
#   ./deploy/install.sh --uninstall   remove everything this script installed (asks about sudo
#                                     parts, and whether to keep your settings and data)
#   ./deploy/install.sh --purge       --uninstall, and delete your settings and data too
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
DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
CONFIG_HOME="${XDG_CONFIG_HOME:-$HOME/.config}"
SHARE_DIR="$DATA_HOME/prism"
BINARIES=(prismd prism-ui prism-ctl prism-native-host prism-lock prism-phoned prism-rec prism-video)
GREETER_BIN=/usr/local/bin/prism-lock
GREETD_CONF=/etc/greetd/config.toml
# Three ways to be run: from the prism-shell pacman package (/usr/share/prism/deploy, binaries in
# /usr/bin: only per-user setup), from a release download (deploy/package-release.sh: binaries
# in bin/, no sources) or from the sources (build first).
SYSTEM=0
PREBUILT=0
if [ "$REPO_DIR" = /usr/share/prism ] && [ -x /usr/bin/prismd ]; then
    SYSTEM=1
    PREBUILT=1
    OUT_DIR=/usr/bin
    BIN_DIR=/usr/bin
    GREETER_BIN=/usr/bin/prism-lock
elif [ -x "$REPO_DIR/bin/prismd" ] && [ ! -f "$REPO_DIR/Cargo.toml" ]; then
    PREBUILT=1
    OUT_DIR="$REPO_DIR/bin"
else
    OUT_DIR="$REPO_DIR/target/release"
fi

GREETER=ask
YES=0
UNINSTALL=0
PURGE=0
for arg in "$@"; do
    case "$arg" in
        --greeter) GREETER=1 ;;
        --no-greeter) GREETER=0 ;;
        -y|--yes) YES=1 ;;
        --uninstall) UNINSTALL=1 ;;
        --purge) UNINSTALL=1; PURGE=1 ;;
        -h|--help) sed -n '2,19p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
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

# The installer's "# Added by the Prism installer" line and the PATH line under it.
strip_path_line() {
    local f="$1" marker="# Added by the Prism installer"
    [ -f "$f" ] && grep -qxF "$marker" "$f" || return 0
    awk -v m="$marker" '{ l[NR] = $0 } END {
        for (i = 1; i <= NR; i++) {
            if (l[i] == m) { i++; continue }
            if (l[i] == "" && l[i + 1] == m) continue
            print l[i]
        }
    }' "$f" > "$f.prism-tmp" && cat "$f.prism-tmp" > "$f" && rm -f "$f.prism-tmp"
    echo "  removed the PATH line from $f"
}

# What a manual install put in your home folder (binaries, service, portal, icons...), not your
# settings and data. Also used when the package replaces a manual install.
remove_user_files() {
    if command -v systemctl >/dev/null 2>&1; then
        systemctl --user disable --now prismd >/dev/null 2>&1 || true
    fi
    for b in "${BINARIES[@]}"; do rm -f "$HOME/.local/bin/$b"; done
    rm -rf "$SHARE_DIR/scripts" "$SHARE_DIR/wallpapers"
    rm -f "$SYSTEMD_USER_DIR/prismd.service" "$SYSTEMD_USER_DIR/xdg-desktop-portal.service.d/prism.conf"
    rmdir "$SYSTEMD_USER_DIR/xdg-desktop-portal.service.d" 2>/dev/null || true
    rm -f "$DATA_HOME/xdg-desktop-portal/portals/prism.portal" \
        "$DATA_HOME/dbus-1/services/org.freedesktop.impl.portal.desktop.prism.service" \
        "$DATA_HOME/icons/hicolor/scalable/apps/prism.svg" \
        "$DATA_HOME/applications/prism-settings.desktop" \
        "$DATA_HOME/man/man1/prism-ctl.1"
    for size in 16 24 32 48 64 128 256 512; do
        rm -f "$DATA_HOME/icons/hicolor/${size}x${size}/apps/prism.png"
        rmdir "$DATA_HOME/icons/hicolor/${size}x${size}/apps" "$DATA_HOME/icons/hicolor/${size}x${size}" 2>/dev/null || true
    done
    rmdir "$DATA_HOME/icons/hicolor/scalable/apps" "$DATA_HOME/icons/hicolor/scalable" 2>/dev/null || true
    if [ -d "$DATA_HOME/icons/hicolor" ]; then
        if [ -z "$(find "$DATA_HOME/icons/hicolor" -type f ! -name icon-theme.cache -print -quit)" ]; then
            # Only Prism's icons were there: the cache the installer made goes too.
            rm -rf "$DATA_HOME/icons/hicolor"
            rmdir "$DATA_HOME/icons" 2>/dev/null || true
        else
            gtk-update-icon-cache -q -t "$DATA_HOME/icons/hicolor" 2>/dev/null || true
        fi
    fi
    # Folders the installer made, when nothing else is in them.
    rmdir "$DATA_HOME/applications" "$DATA_HOME/man/man1" "$DATA_HOME/man" \
        "$DATA_HOME/dbus-1/services" "$DATA_HOME/dbus-1" \
        "$DATA_HOME/xdg-desktop-portal/portals" "$DATA_HOME/xdg-desktop-portal" 2>/dev/null || true
    if [ -d "$DATA_HOME/fonts/prism" ]; then
        rm -rf "$DATA_HOME/fonts/prism"
        fc-cache -f >/dev/null 2>&1 || true
    fi
    if command -v systemctl >/dev/null 2>&1; then
        systemctl --user daemon-reload || true
    fi
}

uninstall() {
    echo "========================================================"
    echo "    Removing Prism                                      "
    echo "========================================================"
    # Read now: prism-ctl goes with the files below.
    local dep_list="" ctl
    for ctl in "$BIN_DIR/prism-ctl" "$OUT_DIR/prism-ctl" "$(command -v prism-ctl 2>/dev/null || true)"; do
        if [ -x "$ctl" ]; then
            dep_list="$("$ctl" deps --packages 2>/dev/null || true)"
            break
        fi
    done
    echo "[1/5] Stopping prismd and removing the files in your home folder..."
    remove_user_files
    # Written by prismd for the Prism Downloads extension.
    rm -f "$HOME/.mozilla/native-messaging-hosts/prism_downloads.json" "$HOME/.librewolf/native-messaging-hosts/prism_downloads.json"
    # Portal choices: only files still as the installer wrote them.
    for f in "$CONFIG_HOME"/xdg-desktop-portal/*-portals.conf; do
        [ -f "$f" ] && cmp -s "$f" "$SCRIPT_DIR/portal/mango-portals.conf" && rm -f "$f" && echo "  removed $f"
    done
    rmdir "$CONFIG_HOME/xdg-desktop-portal" 2>/dev/null || true
    if command -v systemctl >/dev/null 2>&1; then
        systemctl --user try-restart xdg-desktop-portal.service >/dev/null 2>&1 || true
    fi
    local path_lines=0
    for f in "$HOME/.zshenv" "$HOME/.bashrc" "$HOME/.profile" "$HOME/.bash_profile"; do
        [ -f "$f" ] && grep -qxF "# Added by the Prism installer" "$f" && path_lines=1
    done
    [ -e "$CONFIG_HOME/fish/conf.d/prism-path.fish" ] && path_lines=1
    if [ "$path_lines" = 1 ]; then
        # Other programs in ~/.local/bin (pipx, cargo install...) may rely on it: kept by default.
        if ask "The installer added ~/.local/bin to your PATH. Remove that too (other programs there may need it)?" n; then
            for f in "$HOME/.zshenv" "$HOME/.bashrc" "$HOME/.profile" "$HOME/.bash_profile"; do strip_path_line "$f"; done
            rm -f "$CONFIG_HOME/fish/conf.d/prism-path.fish"
        else
            echo "  kept ~/.local/bin in your PATH"
        fi
    fi

    echo "[2/5] Lock screen and laptop files (sudo)..."
    if [ "$SYSTEM" = 0 ] && [ -e /etc/pam.d/prism ] && ask "Remove the lock screen's PAM service /etc/pam.d/prism (sudo)?" y; then
        sudo rm -f /etc/pam.d/prism && echo "  removed /etc/pam.d/prism"
    fi
    if [ -e /etc/udev/rules.d/90-prism-charge-limit.rules ] && ask "Remove the battery charge limit udev rule (sudo)?" y; then
        sudo rm -f /etc/udev/rules.d/90-prism-charge-limit.rules && sudo udevadm control --reload
        echo "  removed /etc/udev/rules.d/90-prism-charge-limit.rules"
    fi
    if grep -qsx '# prism: keyring' /etc/pam.d/greetd && ask "Remove the keyring unlock lines Prism added to /etc/pam.d/greetd (sudo)?" y; then
        sudo sed -i -e '/^# prism: keyring$/,+1d' /etc/pam.d/greetd && echo "  removed them"
    fi

    echo "[3/5] Greeter (sudo)..."
    local greeter_gone=1
    if grep -qs 'prism-lock' "$GREETD_CONF" || { [ -e "$GREETD_CONF" ] && [ ! -r "$GREETD_CONF" ] && sudo grep -qs 'prism-lock' "$GREETD_CONF"; }; then
        local backup
        backup="$(sudo sh -c "ls -t $GREETD_CONF.bak.* 2>/dev/null | head -n 1")"
        if [ -n "$backup" ] && ask "greetd starts the Prism greeter. Put back your earlier greetd config ($backup)?" y; then
            sudo cp "$backup" "$GREETD_CONF" && echo "  restored $GREETD_CONF"
        else
            greeter_gone=0
            echo "  greetd still starts the Prism greeter ($GREETD_CONF), so the greeter's files stay:" >&2
            echo "  without them you could not log in. Set another greeter there (for example" >&2
            echo "  command = \"agreety --cmd /bin/sh\") or switch display manager, then run this again." >&2
        fi
    else
        echo "  greetd does not use the Prism greeter."
    fi
    if [ "$greeter_gone" = 1 ]; then
        if [ "$SYSTEM" = 0 ] && { [ -e /usr/local/bin/prism-lock ] || [ -e /usr/local/bin/prism-video ]; } \
            && ask "Remove the greeter's prism-lock and prism-video from /usr/local/bin (sudo)?" y; then
            sudo rm -f /usr/local/bin/prism-lock /usr/local/bin/prism-video && echo "  removed them"
        fi
        if [ -d /var/lib/greetd/prism ] && ask "Remove the greeter's copy of your wallpaper and settings (/var/lib/greetd/prism, sudo)?" y; then
            sudo rm -rf /var/lib/greetd/prism && echo "  removed /var/lib/greetd/prism"
        fi
    fi

    echo "[4/5] Your settings and data..."
    local data=("$CONFIG_HOME/prism" "${XDG_CACHE_HOME:-$HOME/.cache}/prism" "${XDG_CACHE_HOME:-$HOME/.cache}/Prism"
        "${XDG_STATE_HOME:-$HOME/.local/state}/prism" "$SHARE_DIR" "$HOME/.wa.jpg" "$HOME/.wa-thumb.jpg")
    local present=()
    for d in "${data[@]}"; do [ -e "$d" ] && present+=("$d"); done
    if [ ${#present[@]} -eq 0 ]; then
        echo "  none left."
    else
        printf '  %s\n' "${present[@]}"
        # Never with --yes alone: only --purge, or a "y" typed here.
        local delete=0 reply=""
        if [ "$PURGE" = 1 ]; then
            delete=1
        elif [ -t 0 ]; then
            read -r -p "Delete these too (settings, notes, phone pairing, downloads list, caches)? This cannot be undone. [y/N] " reply || reply=""
            case "$reply" in [yY]*) delete=1 ;; esac
        fi
        if [ "$delete" = 1 ]; then
            rm -rf "${present[@]}" && echo "  deleted"
        else
            echo "  kept (run with --purge to delete them)"
        fi
    fi

    echo "[5/5] Packages..."
    remove_packages "$dep_list"

    echo ""
    echo "Prism is removed. Left for you to undo, if you set them up:"
    echo " - the Prism lines in your compositor config (prism-mango.conf, prism-niri.kdl or prism.lua),"
    echo "   and the [templates.prism-rust] block in ~/.config/matugen/config.toml;"
    echo " - calendar addresses stay in your keyring, and a keyring renamed to \"login\" keeps that name."
    if [ "$SYSTEM" = 1 ] && pacman -Q prism-shell >/dev/null 2>&1; then
        echo "The prism-shell package is still installed:  sudo pacman -Rn prism-shell"
    fi
}

# The installed package of an Arch package name, also as its -bin / -git variant (AUR).
installed_variant() {
    local p="$1" c
    for c in "$p" "${p%-bin}" "$p-bin" "$p-git"; do
        if pacman -Qq "$c" >/dev/null 2>&1; then echo "$c"; return 0; fi
    done
    return 1
}

# Packages that need $1, other than those in the rest of the arguments (removed with it).
needed_by() {
    local pkg="$1" by
    shift
    for by in $(LC_ALL=C pacman -Qi "$pkg" 2>/dev/null | sed -n 's/^Required By *: //p'); do
        [ "$by" = None ] && continue
        [[ " $* " == *" $by "* ]] && continue
        echo "$by"
    done
}

# The prism-shell package (package mode, after asking), then the programs Prism uses that the
# user picks by number. Nothing is removed without an answer typed here: not with --yes,
# --purge or without a terminal. A package that something else still needs is kept.
remove_packages() {
    local dep_list="$1"
    if ! command -v pacman >/dev/null 2>&1; then
        echo "  Not an Arch-based system: no packages are removed."
        return 0
    fi
    local remove_shell=0
    if [ "$SYSTEM" = 1 ] && pacman -Q prism-shell >/dev/null 2>&1 \
        && ask "Remove the prism-shell package (sudo pacman -Rn prism-shell)?" y; then
        remove_shell=1
    fi
    # Its required libraries (FFmpeg, GTK...) are not offered: other programs use them, and
    # "pacman -Qdtq" lists any that nothing needs any more.
    local names=() pkgs=() whats=() name level arch what p found list
    if [ -z "$dep_list" ]; then
        echo "  Cannot list the programs Prism uses (no prism-ctl here); packages are left alone."
    else
        while IFS=$'\t' read -r name level arch what; do
            # Sound, the portal, xdg-open, GLib and fontconfig belong to the desktop, not to Prism.
            [ "$level" = needed ] && continue
            list=""
            for p in $arch; do
                found="$(installed_variant "$p")" || continue
                [[ " $list " == *" $found "* ]] || list="${list:+$list }$found"
            done
            [ -n "$list" ] || continue
            names+=("$name"); pkgs+=("$list"); whats+=("$what ($level)")
        done <<< "$dep_list"
    fi
    for p in greetd cage; do
        if pacman -Qq "$p" >/dev/null 2>&1 && ! grep -qs 'prism-lock' "$GREETD_CONF"; then
            names+=("$p"); pkgs+=("$p"); whats+=("the Prism login screen (optional)")
        fi
    done

    local chosen=()
    if [ ${#names[@]} -eq 0 ]; then
        echo "  None of the programs Prism uses is installed as a package."
    elif [ ! -t 0 ]; then
        echo "  Kept every package (choosing which to remove needs a terminal)."
    else
        echo "  Prism uses these installed packages. Other programs may use them too, so nothing is"
        echo "  removed unless you choose it:"
        local i
        for i in "${!names[@]}"; do
            printf '  %2d) %-28s %s\n' "$((i + 1))" "${pkgs[$i]}" "${whats[$i]}"
        done
        local reply ok n
        while true; do
            read -r -p "  Numbers to remove, separated by spaces, \"all\", or Enter to keep them all: " reply || reply=""
            chosen=()
            ok=1
            if [ "$reply" = all ]; then
                chosen=("${!names[@]}")
            else
                for n in $reply; do
                    if [[ "$n" =~ ^[0-9]+$ ]] && [ "$n" -ge 1 ] && [ "$n" -le ${#names[@]} ]; then
                        [[ " ${chosen[*]} " == *" $((n - 1)) "* ]] || chosen+=("$((n - 1))")
                    else
                        echo "  \"$n\" is not one of the numbers above."
                        ok=0
                    fi
                done
            fi
            [ "$ok" = 1 ] && break
        done
    fi

    # Everything chosen goes in one transaction, so packages that only need each other can go.
    local targets=() keep=()
    for i in "${chosen[@]}"; do
        for p in ${pkgs[$i]}; do targets+=("$p"); done
    done
    local by q
    # Drop the ones something else needs, again until nothing changes (one kept can keep another).
    local changed=1
    while [ "$changed" = 1 ]; do
        changed=0
        for p in "${targets[@]}"; do
            by="$(needed_by "$p" "${targets[@]}" $([ "$remove_shell" = 1 ] && echo prism-shell) | tr '\n' ' ')"
            if [ -n "${by// /}" ]; then
                keep+=("$p (needed by ${by% })")
                local rest=()
                for q in "${targets[@]}"; do [ "$q" = "$p" ] || rest+=("$q"); done
                targets=("${rest[@]}")
                changed=1
                break
            fi
        done
    done
    for p in "${keep[@]}"; do echo "  kept $p"; done

    if [ "$remove_shell" = 1 ]; then
        # pacman shows what it removes and asks again. -n: no .pacsave copies of its files.
        sudo pacman -Rn prism-shell || echo "  prism-shell was not removed." >&2
    fi
    if [ ${#targets[@]} -gt 0 ]; then
        # -s also removes what they alone pulled in; pacman lists it all and asks first.
        if sudo pacman -Rns "${targets[@]}"; then
            echo "  removed ${targets[*]}"
        else
            echo "  The packages were not removed." >&2
        fi
    elif [ ${#chosen[@]} -eq 0 ] && [ ${#names[@]} -gt 0 ] && [ -t 0 ]; then
        echo "  kept them all"
    fi
}

if [ "$UNINSTALL" = 1 ]; then
    uninstall
    exit 0
fi

echo "========================================================"
echo "    Installing Prism (Rust Wayland desktop shell)       "
echo "========================================================"

if [ "$SYSTEM" = 1 ]; then
echo "[1/11] Using the prism-shell package's binaries in /usr/bin..."
# An earlier manual install would run instead: ~/.local/bin comes first in PATH, and its
# prismd.service in ~/.config/systemd/user hides the package's.
old_install=0
for b in "${BINARIES[@]}"; do [ -e "$HOME/.local/bin/$b" ] && old_install=1; done
[ -e "$SYSTEMD_USER_DIR/prismd.service" ] && old_install=1
if [ "$old_install" = 1 ]; then
    echo "  An earlier manual install is in your home folder (~/.local/bin, ~/.config/systemd/user)."
    if ask "  Remove it so the package's Prism runs (your settings stay)?" y; then
        remove_user_files
        echo "  removed"
    else
        echo "  Kept: it runs instead of the package until removed (prism-setup again, or install.sh --uninstall)." >&2
    fi
fi
elif [ "$PREBUILT" = 1 ]; then
echo "[1/11] Checking the libraries the prebuilt binaries need..."
# Built on an up-to-date Arch Linux: an older or newer FFmpeg, PipeWire... shows here.
missing=()
for b in "${BINARIES[@]}"; do
    while read -r lib; do
        [[ " ${missing[*]} " == *" $lib "* ]] || missing+=("$lib")
    done < <(ldd "$OUT_DIR/$b" 2>/dev/null | awk '/not found/ {print $1}')
done
if [ ${#missing[@]} -gt 0 ]; then
    echo "These libraries are missing (or of another version than the release was built with):" >&2
    printf '  - %s\n' "${missing[@]}" >&2
    echo "Update your system (sudo pacman -Syu) and install the missing packages, or build from" >&2
    echo "source: git clone https://github.com/Hellcat5011/prism && ./prism/deploy/install.sh" >&2
    exit 1
fi
else
echo "[1/11] Checking build dependencies..."
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
fi

if [ "$PREBUILT" = 1 ]; then
    echo "[2/11] Using the prebuilt binaries (nothing to compile)."
else
    echo "[2/11] Compiling (release)..."
    echo "      Note: this takes a while, and prism-ui takes longest. Prism is built with link-time"
    echo "      optimisation and one code unit per crate for smaller, faster programs, so Cargo can"
    echo "      sit on \"prism-ui\" for several minutes using a single CPU core. That is normal: it"
    echo "      has not frozen. Prebuilt binaries for Arch Linux are on the GitHub releases page."
    cargo build --release --manifest-path "$REPO_DIR/Cargo.toml"
fi

if [ "$SYSTEM" = 1 ]; then
    echo "[3/11] Binaries: installed by the package."
    echo "[4/11] Helper scripts and the default wallpaper: installed by the package (/usr/share/prism)."
else
echo "[3/11] Installing binaries to $BIN_DIR..."
mkdir -p "$BIN_DIR"
for b in "${BINARIES[@]}"; do
    # install(1) writes a new file, so binaries that are running are replaced safely.
    install -m755 "$OUT_DIR/$b" "$BIN_DIR/$b"
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

echo "[4/11] Installing helper scripts and the default wallpaper to $SHARE_DIR..."
# Keybind parsers and the wallpaper depth generator; prismd and prism-ui look here.
mkdir -p "$SHARE_DIR/scripts"
install -m644 "$REPO_DIR"/scripts/*.py "$SHARE_DIR/scripts/"
install -m755 "$REPO_DIR"/scripts/*.sh "$SHARE_DIR/scripts/"
# Prism's own wallpaper, shown on first start with the built-in wallpaper engine.
install -Dm644 "$REPO_DIR/assets/wallpapers/prism.png" "$SHARE_DIR/wallpapers/prism.png"
# Replaced by the notification server in prismd.
rm -f "$SHARE_DIR/scripts/prism-dbus-notif.py"
fi

echo "[5/11] Installing the systemd user service..."
# The package's unit is /usr/lib/systemd/user/prismd.service.
if [ "$SYSTEM" = 0 ]; then
    mkdir -p "$SYSTEMD_USER_DIR"
    install -m644 "$SCRIPT_DIR/prismd.service" "$SYSTEMD_USER_DIR/prismd.service"
fi
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

echo "[6/11] Desktop portal (screenshots, screen sharing and appearance for apps)..."
# prism-ui serves org.freedesktop.impl.portal.desktop.prism. xdg-desktop-portal reads portal
# descriptions from XDG_DATA_DIRS, so a drop-in adds ~/.local/share for it (no root needed).
# The package installs these under /usr/share (no XDG_DATA_DIRS drop-in needed there).
if [ "$SYSTEM" = 0 ]; then
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
fi
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

echo "[7/11] Copy text (OCR) for the screenshot overlay..."
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

echo "[8/11] Pixel font for the Pixel launcher..."
# Pixelify Sans (SIL Open Font License, deploy/fonts) gives the Pixel launcher design its
# lettering; without it the design uses a blocky Monospace. User fonts only, no root.
FONT_DIR="${XDG_DATA_HOME:-$HOME/.local/share}/fonts/prism"
if fc-list 2>/dev/null | grep -qi "Pixelify Sans"; then
    echo "Pixelify Sans is installed."
elif ask "Install the Pixelify Sans font for the Pixel launcher design (to $FONT_DIR)?" n; then
    mkdir -p "$FONT_DIR"
    install -m644 "$REPO_DIR/deploy/fonts/PixelifySans.ttf" "$REPO_DIR/deploy/fonts/PixelifySans-OFL.txt" "$FONT_DIR/"
    fc-cache -f "$FONT_DIR" >/dev/null 2>&1 || true
    echo "Installed Pixelify Sans."
else
    echo "Skipped: the Pixel launcher uses Monospace (run ./deploy/install.sh again to add the font)."
fi

echo "[9/11] Lock screen PAM service..."
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

echo "[10/11] Greeter..."
if [ "$GREETER" = ask ]; then
    if [ "$SYSTEM" = 1 ] && grep -qs 'prism-lock' "$GREETD_CONF"; then
        # The package updates /usr/bin/prism-lock; only an older manual greeter needs switching.
        if grep -qs "$GREETER_BIN" "$GREETD_CONF"; then
            GREETER=0
            echo "The greeter uses the package's prism-lock."
        elif ask "The greeter runs an older prism-lock from /usr/local/bin. Use the package's instead (sudo)?" y; then
            GREETER=1
        else
            GREETER=0
        fi
    elif [ "$SYSTEM" = 0 ] && [ -e "$GREETER_BIN" ]; then
        # Set up before: only the binaries need to follow the session's build.
        if cmp -s "$OUT_DIR/prism-lock" "$GREETER_BIN"; then
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
        if [ "$SYSTEM" = 0 ]; then
            sudo install -m755 "$OUT_DIR/prism-lock" "$OUT_DIR/prism-video" /usr/local/bin/
            echo "  installed prism-lock and prism-video to /usr/local/bin"
        fi
        # Your session copies the wallpaper, palette and settings here; the greeter reads them.
        if [ ! -d /var/lib/greetd/prism ]; then
            greeter_group=greeter
            getent group greeter >/dev/null 2>&1 || greeter_group="$(id -gn)"
            sudo install -d -m755 -o "$(id -un)" -g "$greeter_group" /var/lib/greetd/prism
            echo "  created /var/lib/greetd/prism"
        fi
        if sudo grep -qs 'prism-lock' "$GREETD_CONF"; then
            # Already the Prism greeter: point it at this prism-lock (package or /usr/local/bin).
            if ! sudo grep -qs "$GREETER_BIN" "$GREETD_CONF"; then
                sudo sed -i "s|/usr/local/bin/prism-lock|$GREETER_BIN|" "$GREETD_CONF"
                echo "  $GREETD_CONF now starts $GREETER_BIN"
                if [ "$SYSTEM" = 1 ] && [ -e /usr/local/bin/prism-lock ] && ask "  Remove the old prism-lock and prism-video from /usr/local/bin (sudo)?" y; then
                    sudo rm -f /usr/local/bin/prism-lock /usr/local/bin/prism-video
                fi
            fi
        else
            if sudo test -e "$GREETD_CONF"; then
                backup="$GREETD_CONF.bak.$(date +%Y%m%d%H%M%S)"
                sudo cp "$GREETD_CONF" "$backup"
                echo "  backed up the old greetd config to $backup"
            fi
            sed "s|/usr/local/bin/prism-lock|$GREETER_BIN|" "$SCRIPT_DIR/greetd/config.toml" | sudo install -Dm644 /dev/stdin "$GREETD_CONF"
            echo "  installed $GREETD_CONF"
        fi
        if ! systemctl is-enabled --quiet greetd 2>/dev/null; then
            echo "  greetd is not enabled. To switch to it, disable your current display manager"
            echo "  (e.g. 'sudo systemctl disable sddm') and run 'sudo systemctl enable greetd'."
        fi
        echo "  Test it before rebooting: from a TTY run 'sudo systemctl restart greetd'."
    fi
fi

echo "[11/11] Checking the programs and services Prism uses..."
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
