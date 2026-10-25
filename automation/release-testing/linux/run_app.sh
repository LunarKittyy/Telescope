#!/bin/bash
# run_app.sh [args]: start the profile's Telescope under drive.py in the background; PID goes to .profile/app.pid
source "$(dirname "$0")/env.sh"
rm -f "$DRIVE_DIR"/*
cd "${RT_APP:-$RT_PROFILE/apps/Telescope}"
nohup "$XDG_DATA_HOME/Telescope/venv/bin/python3" "$RT_LINUX/drive.py" "$@" > "$RT_PROFILE/app.log" 2>&1 &
echo $! > "$RT_PROFILE/app.pid"
