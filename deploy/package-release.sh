#!/usr/bin/env bash
# package-release.sh: build Prism and pack a release for Arch Linux (and Arch-based systems).
#
#   ./deploy/package-release.sh            build, then write target/dist/prism-<version>-x86_64.tar.gz
#   ./deploy/package-release.sh --no-build pack the binaries already in target/release
#
# The archive holds the binaries in bin/ and the files deploy/install.sh needs, without the
# sources: unpacked, `./deploy/install.sh` installs the prebuilt binaries instead of compiling.
# They link to the system's FFmpeg, PipeWire, GTK and so on, so build on an up-to-date Arch
# (.github/workflows/release.yml does, for every v* tag). A .sha256 file is written next to it.
set -euo pipefail
REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BINARIES=(prismd prism-ui prism-ctl prism-native-host prism-lock prism-phoned prism-rec prism-video)

BUILD=1
for arg in "$@"; do
    case "$arg" in
        --no-build) BUILD=0 ;;
        -h|--help) sed -n '2,10p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "Unknown option: $arg (see --help)" >&2; exit 2 ;;
    esac
done

VERSION="$(sed -n '/^\[workspace.package\]/,/^\[/s/^version *= *"\(.*\)"/\1/p' "$REPO_DIR/Cargo.toml")"
ARCH="$(uname -m)"
NAME="prism-$VERSION-$ARCH"
DIST="$REPO_DIR/target/dist"
STAGE="$DIST/$NAME"

if [ "$BUILD" = 1 ]; then
    echo "Building (prism-ui takes the longest: link-time optimisation)..."
    cargo build --release --locked --manifest-path "$REPO_DIR/Cargo.toml"
fi

rm -rf "$STAGE"
mkdir -p "$STAGE/bin"
for b in "${BINARIES[@]}"; do
    install -m755 "$REPO_DIR/target/release/$b" "$STAGE/bin/$b"
done
# What install.sh reads besides the binaries.
cp -r "$REPO_DIR/deploy" "$STAGE/"
mkdir -p "$STAGE/scripts" "$STAGE/assets"
cp "$REPO_DIR"/scripts/*.py "$REPO_DIR"/scripts/*.sh "$STAGE/scripts/"
cp -r "$REPO_DIR/assets/logo" "$REPO_DIR/assets/wallpapers" "$STAGE/assets/"
cp -r "$REPO_DIR/man" "$STAGE/"
cp "$REPO_DIR/LICENSE" "$REPO_DIR/README.md" "$STAGE/"
cat > "$STAGE/INSTALL.txt" <<EOF
Prism $VERSION, prebuilt for Arch Linux ($ARCH).

    ./deploy/install.sh

installs these binaries to ~/.local/bin without compiling. If it reports missing libraries,
update the system (sudo pacman -Syu) or build from source:
https://github.com/Hellcat5011/prism

Prism is free software under the GNU General Public License, version 3 or later (LICENSE).
EOF

tar -C "$DIST" -czf "$DIST/$NAME.tar.gz" "$NAME"
(cd "$DIST" && sha256sum "$NAME.tar.gz" > "$NAME.tar.gz.sha256")
rm -rf "$STAGE"
echo "Wrote $DIST/$NAME.tar.gz"
