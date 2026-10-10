#!/usr/bin/env python3
"""Windows CI: an unzipped copy moving to the installed one, for real.

Copies the built bundle into a fake Downloads folder (with a file of the user's beside it), runs the real
install_with_setup() against the real TelescopeSetup.exe, checks it found the install through the setup's uninstall
entry and asked the installed copy to tidy up, then runs the installed copy's clean-up and checks only Telescope's
files went. Uninstalls at the end.

Usage (from desktop/): python scripts/windows_move_check.py <TelescopeSetup.exe> <bundle folder>
"""

import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

from telescope import updates  # noqa: E402


def main():
    setup, bundle = Path(sys.argv[1]).resolve(), Path(sys.argv[2]).resolve()
    problems = []
    downloads = Path(tempfile.mkdtemp(prefix="Downloads-"))
    old = downloads / "Telescope"
    shutil.copytree(bundle, old)
    (downloads / "holiday.jpg").write_bytes(b"not Telescope's")

    result = updates.install_with_setup(setup, old)  # the real setup, subprocess and registry
    installed = updates.installed_location()
    print("Installed at", installed)
    print("Relaunch with", result.relaunch)
    if installed is None or not (installed / "TelescopeDesktop.exe").is_file():
        problems.append("the setup's uninstall entry didn't lead to TelescopeDesktop.exe")
    elif result.relaunch != [str(installed / "TelescopeDesktop.exe"), "--after-update", "--moved-from", str(old)]:
        problems.append(f"unexpected relaunch command {result.relaunch}")
    if not old.is_dir():
        problems.append("the unzipped copy went before the installed one started")

    if not problems:
        if not updates.remove_unzipped_copy(old, installed, tries=4, pause=0.5):
            problems.append("the clean-up didn't remove the unzipped copy")
        if old.exists():
            problems.append(f"the unzipped folder is still there: {sorted(p.name for p in old.iterdir())}")
        if not (downloads / "holiday.jpg").is_file():
            problems.append("the clean-up removed a file that isn't Telescope's")
        if not (installed / "TelescopeDesktop.exe").is_file():
            problems.append("the clean-up touched the installed copy")

    if installed is not None and (installed / "unins000.exe").is_file():
        subprocess.run([str(installed / "unins000.exe"), "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"])
        for _ in range(60):  # the uninstaller hands over to a copy of itself and returns
            if not installed.exists():
                break
            time.sleep(1)

    for p in problems:
        print(f"::error::{p}")
    if problems:
        sys.exit(1)
    print("Moving an unzipped copy to the installed one works.")


if __name__ == "__main__":
    main()
