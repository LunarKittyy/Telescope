#!/bin/bash
# install.sh Telescope-linux.tar.gz: unpack into the profile's apps/Telescope, let start.sh build the venv, then quit the app it opens
set -e
source "$(dirname "$0")/env.sh"
rm -rf "$RT_PROFILE/apps/Telescope" && mkdir -p "$RT_PROFILE/apps/Telescope"
tar -xzf "$1" -C "$RT_PROFILE/apps/Telescope"
cd "$RT_PROFILE/apps/Telescope"
setsid ./start.sh > "$RT_PROFILE/first_run.log" 2>&1 & PGID=$!
# Built once the app writes its log line, which means the venv is ready
for i in $(seq 200); do grep -q "Telescope" /tmp/telescope-$USER/telescope.log 2>/dev/null && [ -x "$XDG_DATA_HOME/Telescope/venv/bin/python3" ] && pgrep -g $PGID -f "[m]ain.py" >/dev/null && break; sleep 3; done
kill -- -$PGID 2>/dev/null || true
echo "installed in $RT_PROFILE/apps/Telescope (start.sh output in $RT_PROFILE/first_run.log)"
