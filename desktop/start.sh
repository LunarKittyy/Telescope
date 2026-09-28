#!/usr/bin/env bash
# Telescope launcher for Linux.
# Run this once to set up a Telescope-owned virtual environment and install
# Python dependencies into it, then it launches the app.
# Usage: ./start.sh

set -e
cd "$(dirname "$0")"

echo "Telescope"
echo "========"

# Check Python 3
if ! command -v python3 &>/dev/null; then
    echo ""
    echo "Python 3 is required but not found. Install it with:"
    echo "  sudo apt install python3 python3-pip python3-venv     # Debian / Ubuntu"
    echo "  sudo dnf install python3 python3-pip                  # Fedora / RHEL"
    echo "  sudo pacman -S python python-pip                      # Arch"
    exit 1
fi

# A venv under the app's own XDG data directory instead of installing into
# the active system/user Python: keeps Telescope's dependency versions
# isolated from (and un-clobbered by) whatever else is on this machine, and
# from a system Python upgrade breaking the app out from under the user.
DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
VENV_DIR="$DATA_HOME/Telescope/venv"

PY="$VENV_DIR/bin/python3"
if [ ! -x "$PY" ]; then
    echo "Setting up Telescope's Python environment (first run only)..."
    python3 -m venv "$VENV_DIR"
    "$PY" -m pip install --quiet --disable-pip-version-check --upgrade pip || true
fi

# Only when the requirements changed (first run, or an update), so a normal launch needs no network.
# A failed install still launches: main.py says what's missing, and an update that broke it gets rolled back.
PIP_ARGS=(-r requirements.txt)
if [ -f constraints.txt ]; then
    PIP_ARGS+=(-c constraints.txt)
fi
STAMP="$VENV_DIR/.telescope-requirements"
WANTED="$(cat requirements.txt constraints.txt 2>/dev/null | cksum)"
if [ "$(cat "$STAMP" 2>/dev/null)" != "$WANTED" ]; then
    echo "Checking dependencies..."
    if "$PY" -m pip install --quiet --disable-pip-version-check "${PIP_ARGS[@]}"; then
        echo "$WANTED" > "$STAMP"
    else
        echo "Couldn't install Telescope's dependencies; trying to start anyway."
    fi
fi

echo "Launching..."
exec "$PY" main.py "$@"
