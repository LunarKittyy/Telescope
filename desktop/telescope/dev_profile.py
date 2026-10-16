"""A dev profile (scripts/dev_desktop.py): with TELESCOPE_DEV_PROFILE set to a folder, this copy keeps its config and log there, runs next to the real Telescope, and pairs with the dev phone app (com.telescope.dev) without touching the real one's settings, menu entry, sign-in entry or mic."""

import os
from pathlib import Path
from typing import Optional

ENV = "TELESCOPE_DEV_PROFILE"
PHONE_PACKAGE = "com.telescope.dev"


def folder() -> Optional[Path]:
    value = os.environ.get(ENV)
    return Path(value) if value else None


def active() -> bool:
    return folder() is not None


PLUGINS_ENV = "TELESCOPE_DEV_PLUGINS"
SUPERVISED_ENV = "TELESCOPE_DEV_SUPERVISED"
RESUME_ENV = "TELESCOPE_DEV_RESUME"
RESTART_EXIT = 75  # dev_desktop.py starts this copy again
RESTART_STREAMING = 76  # the same, and it starts streaming again once it's up


def plugin_folders() -> list:
    """Plugins dev_desktop.py --plugin loads straight from the folder they're written in (only in a dev profile)."""
    raw = os.environ.get(PLUGINS_ENV, "") if active() else ""
    return [Path(p) for p in raw.split(os.pathsep) if p]


def supervised() -> bool:
    """Started by dev_desktop.py, which starts it again when it exits with RESTART_EXIT, instead of it relaunching
    itself (which would leave the terminal and delete a throwaway profile)."""
    return active() and os.environ.get(SUPERVISED_ENV) == "1"


def resume_stream() -> bool:
    """The copy before this one was streaming when it restarted."""
    return supervised() and os.environ.get(RESUME_ENV) == "1"


def files_stamp(folders) -> tuple:
    """What changes when a file in folders is saved (path, size, time), leaving out caches and hidden folders."""
    stamp = []
    for folder in folders:
        for root, dirs, names in os.walk(folder):
            dirs[:] = sorted(d for d in dirs if d != "__pycache__" and not d.startswith("."))
            for name in sorted(names):
                if name.startswith(".") or name.endswith((".pyc", ".swp", "~")):
                    continue
                try:
                    st = os.stat(os.path.join(root, name))
                except OSError:
                    continue  # saved by renaming over it: the next look sees the new one
                stamp.append((root, name, st.st_size, st.st_mtime_ns))
    return tuple(stamp)
