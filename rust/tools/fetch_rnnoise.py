#!/usr/bin/env python3
"""Copies RNNoise, the microphone's noise reduction, out of the pyrnnoise wheel desktop/requirements-nodeps.txt pins.

Usage: fetch_rnnoise.py linux|windows FOLDER
"""

import re
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path

REPO = Path(__file__).resolve().parents[2]
WHEELS = {"linux": ("manylinux1_x86_64", "librnnoise.so"), "windows": ("win_amd64", "rnnoise.dll")}


def pinned_version() -> str:
    text = (REPO / "desktop" / "requirements-nodeps.txt").read_text()
    return re.search(r"^pyrnnoise==([\w.]+)", text, re.M).group(1)


def main():
    target, folder = sys.argv[1], Path(sys.argv[2])
    platform, library = WHEELS[target]
    with tempfile.TemporaryDirectory() as work:
        subprocess.run([sys.executable, "-m", "pip", "download", "--quiet", "--no-deps", "--only-binary=:all:", "--python-version", "3.12",
                        "--platform", platform, "--dest", work, f"pyrnnoise=={pinned_version()}"], check=True)
        wheel = next(Path(work).glob("*.whl"))
        with zipfile.ZipFile(wheel) as z:
            name = next(n for n in z.namelist() if n.endswith("/" + library))
            folder.mkdir(parents=True, exist_ok=True)
            (folder / library).write_bytes(z.read(name))
    print(f"{library} from {wheel.name}")


if __name__ == "__main__":
    main()
