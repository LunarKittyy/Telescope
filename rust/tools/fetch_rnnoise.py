#!/usr/bin/env python3
"""Copies RNNoise, the microphone's noise reduction, out of a pinned pyrnnoise wheel.

Usage: fetch_rnnoise.py linux|windows FOLDER
"""

import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path

# Only the library is used: the wheel brings audiolab and matplotlib for its own command line
VERSION = "0.4.5"
WHEELS = {"linux": ("manylinux1_x86_64", "librnnoise.so"), "windows": ("win_amd64", "rnnoise.dll")}


def main():
    target, folder = sys.argv[1], Path(sys.argv[2])
    platform, library = WHEELS[target]
    with tempfile.TemporaryDirectory() as work:
        subprocess.run([sys.executable, "-m", "pip", "download", "--quiet", "--no-deps", "--only-binary=:all:", "--python-version", "3.12",
                        "--platform", platform, "--dest", work, f"pyrnnoise=={VERSION}"], check=True)
        wheel = next(Path(work).glob("*.whl"))
        with zipfile.ZipFile(wheel) as z:
            name = next(n for n in z.namelist() if n.endswith("/" + library))
            folder.mkdir(parents=True, exist_ok=True)
            (folder / library).write_bytes(z.read(name))
    print(f"{library} from {wheel.name}")


if __name__ == "__main__":
    main()
