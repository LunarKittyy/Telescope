#!/usr/bin/env bash
# Delete and recreate the rolling "nightly" pre-release so it always sorts to
# the top of the Releases page. GitHub sorts releases by their `created_at`
# timestamp, which is set once at creation and never bumped by later edits -
# so a release that's only ever updated in place (the old behavior here)
# permanently keeps its original position, even though its assets are fresh.
#
# Multiple workflows (linux/windows/apk builds) publish to this same release,
# each only holding its own asset, so a plain delete+recreate would drop
# whatever the other workflows had already uploaded. To avoid that, this
# script downloads any assets it isn't replacing before deleting, and
# re-uploads them alongside the new one. Callers must serialize their calls
# to this script via a shared concurrency group so two invocations can't run
# at once (see the `concurrency:` blocks in the calling workflows).
set -euo pipefail
shopt -s nullglob

ASSET="$1"
KEEP_DIR="nightly-keep"
NOTES_FILE="$(dirname "${BASH_SOURCE[0]}")/NIGHTLY_NOTES.md"

mkdir -p "$KEEP_DIR"

if gh release view nightly >/dev/null 2>&1; then
    while IFS= read -r name; do
        [ "$name" = "$ASSET" ] && continue
        gh release download nightly -p "$name" -D "$KEEP_DIR" --clobber
    done < <(gh release view nightly --json assets --jq '.assets[].name')

    gh release delete nightly --yes --cleanup-tag
fi

gh release create nightly "$ASSET" "$KEEP_DIR"/* \
    --title "Nightly" \
    --notes-file "$NOTES_FILE" \
    --prerelease \
    --target "$GITHUB_SHA"
