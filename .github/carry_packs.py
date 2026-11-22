#!/usr/bin/env python3
"""Keep the last few vision and matting packs on the nightly release, so older nightlies can still download theirs.

Usage: carry_packs.py --old OLD_DIR --out OUT_DIR [--keep 3]
OLD_DIR holds the pack zips and pack-history.json that the release being replaced had (it may be empty or missing).
"""

import argparse
import json
import re
import shutil
from pathlib import Path

HISTORY = "pack-history.json"
PACK = re.compile(r"^Telescope-(vision|matting)-(linux|windows)-[0-9a-f]+\.zip$")


def plan(old_history: list[str], old_files: list[str], new_files: list[str], keep: int) -> list[str]:
    """The pack names to publish, newest first: this build's, then older ones in the order they were kept, at most keep per kind and system."""
    ordered = sorted(new_files) + [n for n in old_history if n not in new_files] + sorted(n for n in old_files if n not in old_history and n not in new_files)
    seen: dict[tuple[str, str], int] = {}
    names: list[str] = []
    for name in ordered:
        match = PACK.match(name)
        if not match or name in names:
            continue
        group = match.groups()
        if seen.get(group, 0) < keep or name in new_files:
            seen[group] = seen.get(group, 0) + 1
            names.append(name)
    return names


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--old", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--keep", type=int, default=3)
    args = parser.parse_args()
    history_file = args.old / HISTORY
    try:
        history = json.loads(history_file.read_text()) if history_file.is_file() else []
    except (OSError, ValueError):
        history = []
    old_files = [p.name for p in args.old.glob("Telescope-*.zip")] if args.old.is_dir() else []
    new_files = [p.name for p in args.out.glob("Telescope-*.zip") if PACK.match(p.name)]
    names = [n for n in plan(history, old_files, new_files, args.keep) if n in new_files or (args.old / n).is_file()]
    for name in names:
        if name not in new_files:
            shutil.copy2(args.old / name, args.out / name)
            print(f"carrying {name}")
    (args.out / HISTORY).write_text(json.dumps(names, indent=1) + "\n")


if __name__ == "__main__":
    main()
