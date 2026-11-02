#!/usr/bin/env bash
# Telescope launcher for Linux, the package's start.sh: the app's entries and the updater's restart all come through here.
# Usage: ./start.sh

cd "$(dirname "$0")" || exit 1

# A build artifact can lose the program's executable bit on its way into the tarball
[ -x telescope-rs ] || chmod +x telescope-rs

missing() {
    ldd ./telescope-rs 2>/dev/null | grep 'not found'
}

# An entry of the journal's list $1, one name per line
journal_list() {
    tr -d '\n' < .update.json | sed -n "s/.*\"$1\": *\[\([^]]*\)\].*/\1/p" | tr -d '"' | tr ',' '\n' | sed 's/^ *//'
    echo
}

# The new version is on trial and can't run, so its own guard (update_guard.rs) never will: this puts the old one back the same way
roll_back() {
    grep -q '"state": *"trial"' .update.json 2>/dev/null && [ -d .previous ] || return 1
    build=$(sed -n 's/.*"to_build": *\([0-9]*\).*/\1/p' .update.json)
    journal_list added | while read -r name; do
        [ -n "$name" ] && rm -rf "./$name"
    done
    journal_list entries | while read -r name; do
        if [ -n "$name" ] && [ -e ".previous/$name" ]; then
            # A file is replaced by the rename itself, so start.sh is never missing; only a folder has to go first
            if [ -d "./$name" ] && [ ! -L "./$name" ]; then
                rm -rf "./$name"
            fi
            mv -f ".previous/$name" "./$name"
        fi
    done
    [ -n "$build" ] && [ "$build" != 0 ] && printf '{"build":%s}' "$build" > .update-failed
    rm -rf .previous .update-staging .update.json
}

# The system's libva goes first, since it knows where that system's video drivers are. lib/fallback is for a system without one
if missing | grep -q 'libva\|libdrm'; then
    export LD_LIBRARY_PATH="$PWD/lib/fallback${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
fi

# In a folder of its own, since libva's being wanted mustn't put these ahead of the system's
if missing | grep -q 'libfontconfig\|libfreetype\|libexpat'; then
    export LD_LIBRARY_PATH="$PWD/lib/fallback-fonts${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
    [ -e /etc/fonts/fonts.conf ] || export FONTCONFIG_FILE="$PWD/lib/fallback-fonts/fonts.conf"
fi

gone="$(missing)"
if [ -n "$gone" ]; then
    echo "Telescope can't start because this system lacks what it needs:"
    echo "$gone"
    if roll_back; then
        echo "Going back to the version before this update."
        exec ./start.sh "$@"
    fi
    exit 1
fi

if [ ! -f .update.json ]; then
    exec ./telescope-rs "$@"
fi

# A first start that fails shouldn't wait for someone to try again: the next run's guard rolls it back, or this does if the guard never ran
./telescope-rs "$@"
code=$?
if [ "$code" -ne 0 ] && [ -f .update.json ]; then
    if grep -q '"started": *true' .update.json; then
        exec ./telescope-rs
    elif roll_back; then
        exec ./start.sh "$@"
    fi
fi
exit "$code"
