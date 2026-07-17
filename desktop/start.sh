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

if [ ! -x "$VENV_DIR/bin/python3" ]; then
    echo "Setting up Telescope's Python environment (first run only)..."
    python3 -m venv "$VENV_DIR"
fi

echo "Checking dependencies..."
"$VENV_DIR/bin/python3" -m pip install --quiet --upgrade pip
PIP_ARGS=(-r requirements.txt)
if [ -f constraints.txt ]; then
    PIP_ARGS+=(-c constraints.txt)
fi
"$VENV_DIR/bin/python3" -m pip install --quiet "${PIP_ARGS[@]}"

echo "Launching..."
"$VENV_DIR/bin/python3" main.py
