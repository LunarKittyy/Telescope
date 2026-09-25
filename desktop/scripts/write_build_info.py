#!/usr/bin/env python3
"""Stamp a release build: writes telescope/_build.py, which telescope/version.py reads.

Usage: python scripts/write_build_info.py --build 123 --channel nightly --commit <sha>
"""

import argparse
from pathlib import Path

DESKTOP = Path(__file__).resolve().parent.parent


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--build", type=int, required=True)
    parser.add_argument("--channel", choices=("stable", "nightly", "dev"), required=True)
    parser.add_argument("--commit", default="")
    args = parser.parse_args()
    version = (DESKTOP.parent / "VERSION").read_text().strip()
    (DESKTOP / "telescope" / "_build.py").write_text(
        f"VERSION = {version!r}\nBUILD = {args.build}\nCHANNEL = {args.channel!r}\nCOMMIT = {args.commit!r}\n")
    print(f"Telescope {version} {args.channel} {args.build}")


if __name__ == "__main__":
    main()
