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
