#!/usr/bin/env python3
"""Print a GitHub dependency snapshot of the exact desktop versions pinned in desktop/constraints.txt."""

import argparse
import datetime
import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DESKTOP = ROOT / "desktop"
NAME = re.compile(r"^\s*([A-Za-z0-9][A-Za-z0-9._-]*)")


def normalize(name: str) -> str:
    return re.sub(r"[-_.]+", "-", name).lower()


def requirement_names(path: Path) -> set[str]:
    names = set()
    for line in path.read_text().splitlines():
        match = NAME.match(line)
        if match:
            names.add(normalize(match.group(1)))
    return names


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--sha", required=True)
    parser.add_argument("--ref", required=True)
    parser.add_argument("--job", required=True)
    args = parser.parse_args()

    runtime = requirement_names(DESKTOP / "requirements.txt")
    # pyinstaller is installed next to requirements-dev.txt by the build workflows
    development = requirement_names(DESKTOP / "requirements-dev.txt") | {"pyinstaller"}

    resolved = {}
    for line in (DESKTOP / "constraints.txt").read_text().splitlines():
        line = line.split("#", 1)[0].strip()
        if not line:
            continue
        name, version = (part.strip() for part in line.split("==", 1))
        key = normalize(name)
        package = {"package_url": f"pkg:pypi/{key}@{version}", "relationship": "indirect"}
        if key in runtime:
            package.update(relationship="direct", scope="runtime")
        elif key in development:
            package.update(relationship="direct", scope="development")
        resolved[key] = package

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
        "manifests": {
            "desktop/constraints.txt": {
                "name": "desktop/constraints.txt",
                "file": {"source_location": "desktop/constraints.txt"},
                "resolved": resolved,
            }
        },
    }
    print(json.dumps(snapshot, indent=2))


if __name__ == "__main__":
    main()
