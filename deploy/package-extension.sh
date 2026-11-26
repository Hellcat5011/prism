#!/usr/bin/env bash
# package-extension.sh: build the Prism Downloads Firefox extension for addons.mozilla.org.
#
#   ./deploy/package-extension.sh        writes target/extension/prism-downloads-<version>.xpi
#
# To install it permanently, Firefox needs it signed by Mozilla (free):
#   1. Sign in at https://addons.mozilla.org/developers/ and choose "Submit a New Add-on".
#   2. "On your own" (unlisted) signs it for you only, usually within minutes; "On this site"
#      lists it publicly after a review. Upload the .xpi this script wrote.
#   3. Install the signed .xpi it gives back (drag it onto Firefox, or about:addons > gear >
#      Install Add-on From File). Later versions: raise "version" in extension/manifest.json,
#      run this again and upload the new file.
# Before that, about:debugging > This Firefox > Load Temporary Add-on (extension/manifest.json)
# runs it until Firefox restarts.
set -euo pipefail
REPO_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SRC="$REPO_DIR/extension"
VERSION="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["version"])' "$SRC/manifest.json")"
OUT="$REPO_DIR/target/extension/prism-downloads-$VERSION.xpi"
mkdir -p "$(dirname "$OUT")"
python3 - "$SRC" "$OUT" "$REPO_DIR/LICENSE" <<'PY'
import os, sys, zipfile
src, out, licence = sys.argv[1], sys.argv[2], sys.argv[3]
with zipfile.ZipFile(out, "w", zipfile.ZIP_DEFLATED) as z:
    for root, dirs, files in os.walk(src):
        dirs[:] = sorted(d for d in dirs if not d.startswith("."))
        for f in sorted(files):
            if f.startswith("."):
                continue
            path = os.path.join(root, f)
            z.write(path, os.path.relpath(path, src))
    # GPL-3.0-or-later, like the rest of Prism.
    z.write(licence, "LICENSE")
PY
echo "Wrote $OUT"
