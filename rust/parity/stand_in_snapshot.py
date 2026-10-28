#!/usr/bin/env python3
"""Stands in for `telescope-rs config-snapshot` until the real one exists: prints the fixture's expected.json,
changed by any --set path=json first, so compare_config.py can be tried on both a match and a difference.

    python stand_in_snapshot.py [--set saved.plugin_configs.setup.language='"de"'] config-snapshot FOLDER
"""

import argparse
import json
import sys
from pathlib import Path


def main(argv=None) -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--set", action="append", default=[], metavar="PATH=JSON")
    parser.add_argument("command", choices=["config-snapshot"])
    parser.add_argument("folder", type=Path)
    args = parser.parse_args(argv)
    data = json.loads((args.folder / "expected.json").read_text(encoding="utf-8"))
    for change in args.set:
        path, _, value = change.partition("=")
        *parents, last = path.split(".")
        target = data
        for part in parents:
            target = target.setdefault(part, {}) if isinstance(target, dict) else {}
        if isinstance(target, dict):
            target[last] = json.loads(value)
    print(json.dumps(data, indent=2, sort_keys=True, ensure_ascii=False))
    return 0


if __name__ == "__main__":
    sys.exit(main())
