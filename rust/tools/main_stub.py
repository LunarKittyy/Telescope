#!/usr/bin/env python3
"""The package's main.py. The Python app's updater only accepts a tarball with main.py and telescope/ in it, and anything still
starting `python main.py` lands here, so this hands over to start.sh and does nothing else, unless an update was cut short."""

import os
import shutil
import sys
from pathlib import Path

here = Path(__file__).resolve().parent
start = here / "start.sh"


def old_launcher():
    """The Python app's start.sh runs main.py, which is this file, so handing over to it would only come back here."""
    try:
        return b"telescope-rs" not in start.read_bytes()
    except OSError:
        return True


if old_launcher():
    # The updater replaces main.py before start.sh: finish its swap (or go back), which brings the new start.sh
    argv = None
    try:
        sys.path.insert(0, str(here))
        import update_guard

        argv = update_guard.recover(here)
    except Exception:
        pass
    if argv and Path(argv[0]) != Path(__file__).resolve():
        os.execv(argv[0], argv)
    if old_launcher():
        sys.exit("Telescope's update was cut short and couldn't be finished here. Download the latest Telescope from https://telescope.webcam and unpack it over this folder.")

bash = shutil.which("bash") or "/bin/bash"
os.execv(bash, [bash, str(start), *sys.argv[1:]])
