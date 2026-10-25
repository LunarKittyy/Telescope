#!/bin/bash
source "$(dirname "$0")/env.sh"
[ -f "$RT_PROFILE/app.pid" ] && kill "$(cat "$RT_PROFILE/app.pid")" 2>/dev/null; rm -f "$RT_PROFILE/app.pid"
