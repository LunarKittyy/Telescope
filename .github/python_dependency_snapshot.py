#!/usr/bin/env python3
"""Print a GitHub dependency snapshot of the exact versions pinned in rust/tools/vision-pack-requirements.txt (the vision pack's)."""

import argparse
import datetime
import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
MANIFEST = "rust/tools/vision-pack-requirements.txt"


def normalize(name: str) -> str:
    return re.sub(r"[-_.]+", "-", name).lower()


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--sha", required=True)
    parser.add_argument("--ref", required=True)
    parser.add_argument("--job", required=True)
    args = parser.parse_args()

    resolved = {}
    for line in (ROOT / MANIFEST).read_text().splitlines():
        line = line.split("#", 1)[0].strip()
        if not line:
            continue
        name, version = (part.strip() for part in line.split("==", 1))
        key = normalize(name)
        resolved[key] = {"package_url": f"pkg:pypi/{key}@{version}", "relationship": "direct", "scope": "runtime"}

    snapshot = {
        "version": 0,
        "sha": args.sha,
        "ref": args.ref,
        "job": {"correlator": "python-constraints", "id": args.job},
        "detector": {
            "name": "telescope-constraints",
            "version": "1",
            "url": "https://github.com/LunarKittyy/Telescope/blob/master/.github/python_dependency_snapshot.py",
        },
        "scanned": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
        "manifests": {MANIFEST: {"name": MANIFEST, "file": {"source_location": MANIFEST}, "resolved": resolved}},
    }
    print(json.dumps(snapshot, indent=2))


if __name__ == "__main__":
    main()
