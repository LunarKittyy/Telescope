#!/bin/bash
# drv.sh 'python code' [tenths of a second to wait]: run code inside the running app and print its output
source "$(dirname "$0")/env.sh"
rm -f "$DRIVE_DIR/out.txt"; printf '%s\n' "$1" > "$DRIVE_DIR/in.tmp" && mv "$DRIVE_DIR/in.tmp" "$DRIVE_DIR/in.py"
for i in $(seq 1 ${2:-100}); do [ -f "$DRIVE_DIR/out.txt" ] && { cat "$DRIVE_DIR/out.txt"; exit 0; }; sleep 0.1; done; echo "(no answer)"
