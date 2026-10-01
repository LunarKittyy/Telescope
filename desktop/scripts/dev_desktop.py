#!/usr/bin/env python3
"""Run this checkout as a dev desktop client next to the real Telescope: its own config (and so its own pairings and computer identity), log and mic, no menu or sign-in entry, and USB pairing aimed at the dev phone app (com.telescope.dev, from `./gradlew installDev`).

Usage: python scripts/dev_desktop.py [--keep] [Qt options]
  default  a fresh profile, deleted when the app quits
  --keep   reuse one profile across runs, so the dev phone stays paired (delete it with --reset)
"""

import getpass
import os
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

DESKTOP = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(DESKTOP))

from telescope import dev_profile  # noqa: E402


def kept_folder() -> Path:
    return Path(tempfile.gettempdir()) / f"telescope-dev-profile-{getpass.getuser()}"


def main(argv) -> int:
    keep, reset = "--keep" in argv, "--reset" in argv
    rest = [a for a in argv if a not in ("--keep", "--reset")]
    if reset:
        shutil.rmtree(kept_folder(), ignore_errors=True)
        print(f"Removed {kept_folder()}")
        if not keep:
            return 0
    if keep:
        folder = kept_folder()
        folder.mkdir(mode=0o700, exist_ok=True)
    else:
        folder = Path(tempfile.mkdtemp(prefix="telescope-dev-"))
    print(f"Dev profile: {folder}")
    env = dict(os.environ, **{dev_profile.ENV: str(folder)})
    try:
        return subprocess.call([sys.executable, str(DESKTOP / "main.py"), *rest], env=env, cwd=DESKTOP)
    finally:
        if not keep:
            shutil.rmtree(folder, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
