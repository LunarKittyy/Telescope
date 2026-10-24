#!/usr/bin/env bash
# Telescope launcher for Linux.
# Run this once to set up a Telescope-owned virtual environment and install
# Python dependencies into it, then it launches the app.
# Usage: ./start.sh

set -e
cd "$(dirname "$0")"

echo "Telescope"
echo "========"

# The pinned NumPy and pyee versions need Python 3.12 or newer.
MIN_PY="3.12"

# True if the interpreter given as $1 is 3.12 or newer.
py_ok() {
    "$1" -c 'import sys; sys.exit(0 if sys.version_info >= (3, 12) else 1)' 2>/dev/null
}

# Prints the interpreter's major.minor version, or "unknown".
py_version() {
    "$1" -c 'import sys; print("%d.%d" % sys.version_info[:2])' 2>/dev/null || echo unknown
}

# Explains how to get a newer Python and exits. $1 is the interpreter, $2 its version, $3 an optional venv to delete.
need_newer_python() {
    echo ""
    echo "Telescope needs Python $MIN_PY or newer, but $1 is Python $2."
    echo "Install a newer Python first. Options:"
    echo "  Ubuntu 22.04 (ships 3.10):"
    echo "    sudo add-apt-repository ppa:deadsnakes/ppa"
    echo "    sudo apt update"
    echo "    sudo apt install python3.12 python3.12-venv"
    echo "  Debian 12 ships 3.11, Debian 13 has 3.13."
    echo "  Ubuntu 24.04+, Fedora 39+ and Arch already ship 3.12 or newer."
    echo "python3.12, python3.13 and python3.14 are picked up automatically when installed."
    if [ -n "$3" ]; then
        echo ""
        echo "Then delete the old environment and run ./start.sh again:"
        echo "  rm -rf \"$3\""
    fi
    exit 1
}

# A venv under the app's own XDG data directory instead of installing into
# the active system/user Python: keeps Telescope's dependency versions
# isolated from (and un-clobbered by) whatever else is on this machine, and
# from a system Python upgrade breaking the app out from under the user.
DATA_HOME="${XDG_DATA_HOME:-$HOME/.local/share}"
VENV_DIR="$DATA_HOME/Telescope/venv"

# Sets PYTHON_BIN to the newest suitable interpreter (so a python3.13 next to an old python3 is picked)
# and OLD_VERSION to the version of the first one that's too old.
find_python() {
    PYTHON_BIN=""
    OLD_VERSION=""
    for candidate in python3.14 python3.13 python3.12 python3; do
        if command -v "$candidate" &>/dev/null; then
            if py_ok "$candidate"; then
                PYTHON_BIN="$candidate"
                return
            fi
            OLD_VERSION="${OLD_VERSION:-$(py_version "$candidate")}"
        fi
    done
}

PY="$VENV_DIR/bin/python3"
# An old venv would fail later at pip, so check its interpreter before anything else.
if [ -x "$PY" ] && ! py_ok "$PY"; then
    OLD_VENV_VERSION="$(py_version "$PY")"
    find_python
    if [ -z "$PYTHON_BIN" ]; then
        echo ""
        echo "Telescope's existing environment ($VENV_DIR) was made with Python $OLD_VENV_VERSION."
        need_newer_python "that environment's Python" "$OLD_VENV_VERSION" "$VENV_DIR"
    fi
    # Telescope made this venv itself, so it can make it again with the newer Python
    echo "Telescope's environment was made with Python $OLD_VENV_VERSION. Setting it up again with $PYTHON_BIN..."
    rm -rf "$VENV_DIR"
fi
if [ ! -x "$PY" ]; then
    find_python
    if [ -z "$PYTHON_BIN" ]; then
        if [ -n "$OLD_VERSION" ]; then
            need_newer_python "python3" "$OLD_VERSION"
        fi
        echo ""
        echo "Python $MIN_PY or newer is required but not found. Install it with:"
        echo "  sudo apt install python3 python3-venv                # Debian 13+ / Ubuntu 24.04+ (22.04: deadsnakes PPA)"
        echo "  sudo dnf install python3                             # Fedora / RHEL"
        echo "  sudo pacman -S python                                # Arch"
        exit 1
    fi

    echo "Setting up Telescope's Python environment (first run only)..."
    "$PYTHON_BIN" -m venv "$VENV_DIR"
    "$PY" -m pip install --quiet --disable-pip-version-check --upgrade pip || true
fi

# Only when the requirements changed (first run, or an update), so a normal launch needs no network.
# A failed install still launches: main.py says what's missing, and an update that broke it gets rolled back.
PIP_ARGS=(-r requirements.txt)
if [ -f constraints.txt ]; then
    PIP_ARGS+=(-c constraints.txt)
fi
NODEPS_ARGS=(--no-deps -r requirements-nodeps.txt)
if [ -f constraints.txt ]; then
    NODEPS_ARGS+=(-c constraints.txt)
fi
STAMP="$VENV_DIR/.telescope-requirements"
WANTED="$(cat requirements.txt requirements-nodeps.txt constraints.txt 2>/dev/null | cksum)"
if [ "$(cat "$STAMP" 2>/dev/null)" != "$WANTED" ]; then
    echo "Checking dependencies..."
    # aiortc goes in on its own, without its dependencies: see requirements-nodeps.txt
    if "$PY" -m pip install --quiet --disable-pip-version-check "${PIP_ARGS[@]}" \
        && { [ ! -f requirements-nodeps.txt ] || "$PY" -m pip install --quiet --disable-pip-version-check "${NODEPS_ARGS[@]}"; }; then
        echo "$WANTED" > "$STAMP"
    else
        echo "Couldn't install Telescope's dependencies; trying to start anyway."
    fi
fi

echo "Launching..."
exec "$PY" main.py "$@"
