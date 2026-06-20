#!/usr/bin/env bash
# Telescope launcher for Linux.
# Run this once to install Python dependencies, then it launches the app.
# Usage: ./start.sh

set -e
cd "$(dirname "$0")"

echo "Telescope"
echo "========"

# Check Python 3
if ! command -v python3 &>/dev/null; then
    echo ""
    echo "Python 3 is required but not found. Install it with:"
    echo "  sudo apt install python3 python3-pip     # Debian / Ubuntu"
    echo "  sudo dnf install python3 python3-pip     # Fedora / RHEL"
    echo "  sudo pacman -S python python-pip         # Arch"
    exit 1
fi

# Install dependencies (silently if already up to date)
echo "Checking dependencies..."
python3 -m pip install --quiet -r requirements.txt 2>/dev/null \
    || python3 -m pip install --quiet --user -r requirements.txt

echo "Launching..."
python3 main.py
