#!/usr/bin/env bash
# Telescope launcher for Linux, the package's start.sh: the app's entries and the updater's restart all come through here.
# Usage: ./start.sh

cd "$(dirname "$0")" || exit 1

# A build artifact can lose the program's executable bit on its way into the tarball
[ -x telescope-rs ] || chmod +x telescope-rs

missing() {
    ldd ./telescope-rs 2>/dev/null | grep 'not found'
}

# The system's libva goes first, since it knows where that system's video drivers are. lib/fallback is for a system without one
if missing | grep -q 'libva\|libdrm'; then
    export LD_LIBRARY_PATH="$PWD/lib/fallback${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
fi

gone="$(missing)"
if [ -n "$gone" ]; then
    echo "Telescope can't start because this system lacks what it needs:"
    echo "$gone"
    if [ -d .previous ]; then
        echo "The version before this update is still in $PWD/.previous."
    fi
    exit 1
fi

exec ./telescope-rs "$@"
