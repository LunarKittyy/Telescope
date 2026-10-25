#!/bin/bash
# fetch_nightly.sh [tag]: download a release's assets to $NIGHTLY_DIR/n<build> (default: downloads/ next to this script)
# Point NIGHTLY_DIR at a folder your test VM shares, so the same files reach Windows
set -e
TAG="${1:-nightly}"; TMP="$(mktemp -d)"
gh release download "$TAG" -D "$TMP"  # the repo of the checkout you run it in
BUILD="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["build"])' "$TMP/manifest.json")"
DEST="${NIGHTLY_DIR:-$(cd "$(dirname "$0")" && pwd)/downloads}/n$BUILD"; mkdir -p "$DEST"; mv "$TMP"/* "$DEST"; rmdir "$TMP"
cd "$DEST" && python3 - <<'PY'
import hashlib, json
m = json.load(open("manifest.json"))
for a in m["assets"]:
    # Old names (Telescope-windows.zip) point at a current file, for updaters from before the rename
    f = a["url"].rsplit("/", 1)[-1]
    got = hashlib.sha256(open(f, "rb").read()).hexdigest()
    print(("ok  " if got == a["sha256"] else "BAD ") + a["name"] + ("" if f == a["name"] else f" ({f})"))
PY
echo "$DEST"
