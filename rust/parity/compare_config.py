#!/usr/bin/env python3
"""Holds the Rust app's settings handling to what Python recorded for each config fixture.

Runs `telescope-rs config-snapshot <fixture folder>` on every folder in desktop/tests/fixtures/configs and compares
what it prints with that folder's expected.json. Only the settings the ledger marks ported or verified are compared,
plus the config-level parts that every port has to get right from the start: whether the file was set aside, which
plugins came up, the top-level fields, which devices are kept and the sections of plugins that aren't built in.

    python compare_config.py --binary ../target/debug/telescope-rs
    python compare_config.py --binary "python3 stand_in_snapshot.py" --all   # every setting, ported or not

Exits 1 when anything differs or the binary fails.
"""

import argparse
import json
import shlex
import subprocess
import sys
import tomllib
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
FIXTURES = REPO / "desktop" / "tests" / "fixtures" / "configs"
LEDGER = HERE / "ledger.toml"
DONE = ("ported", "verified")
MISSING = object()


def ported_settings(ledger: dict) -> tuple:
    """The (scope, plugin, key) settings marked ported or verified, and every built-in plugin with settings."""
    done, plugins = set(), set()
    for key, entry in ledger.get("setting", {}).items():
        scope, plugin, name = key.split(".", 2)
        plugins.add(plugin)
        if entry.get("status") in DONE:
            done.add((scope, plugin, name))
    return done, plugins


def show(value) -> str:
    return "(missing)" if value is MISSING else json.dumps(value, ensure_ascii=False, sort_keys=True)


def compare(expected: dict, actual: dict, done: set, builtin: set, everything: bool = False) -> list:
    """Every difference between two snapshots, as "path: expected X, got Y", within what's compared."""
    diffs = []

    def same(path, want, got):
        if want != got:
            diffs.append(f"{path}: expected {show(want)}, got {show(got)}")

    def plugin_configs(path, want, got, scope):
        want, got = want if isinstance(want, dict) else {}, got if isinstance(got, dict) else {}
        for plugin in sorted(set(want) | set(got)):
            w, g = want.get(plugin, MISSING), got.get(plugin, MISSING)
            if plugin not in builtin:  # a plugin that isn't installed: its section is kept as it was
                same(f"{path}.{plugin}", w, g)
                continue
            w, g = w if isinstance(w, dict) else {}, g if isinstance(g, dict) else {}
            for key in sorted(set(w) | set(g)):
                if everything or (scope, plugin, key) in done:
                    same(f"{path}.{plugin}.{key}", w.get(key, MISSING), g.get(key, MISSING))

    for field in ("set_aside", "plugins"):
        same(field, expected.get(field, MISSING), actual.get(field, MISSING))
    want, got = expected.get("saved"), actual.get("saved")
    if not isinstance(want, dict) or not isinstance(got, dict):
        same("saved", want, got)
        return diffs
    for field in sorted(set(want) | set(got)):
        if field not in ("plugin_configs", "devices"):
            same(f"saved.{field}", want.get(field, MISSING), got.get(field, MISSING))
    plugin_configs("saved.plugin_configs", want.get("plugin_configs"), got.get("plugin_configs"), "global")
    want_dev, got_dev = want.get("devices") or {}, got.get("devices") or {}
    same("saved.devices (which ones)", sorted(want_dev), sorted(got_dev))
    for device in sorted(set(want_dev) & set(got_dev)):
        w, g = want_dev[device], got_dev[device]
        same(f"saved.devices.{device} (fields)", sorted(w), sorted(g))
        plugin_configs(f"saved.devices.{device}.plugin_configs", w.get("plugin_configs"), g.get("plugin_configs"),
                       "device")
    return diffs


def snapshot(binary: list, folder: Path) -> dict:
    result = subprocess.run(binary + ["config-snapshot", str(folder)], capture_output=True, text=True, timeout=60)
    if result.returncode != 0:
        raise RuntimeError(f"exit code {result.returncode}: {result.stderr.strip()[-2000:]}")
    try:
        return json.loads(result.stdout)
    except ValueError as exc:
        raise RuntimeError(f"didn't print JSON ({exc}): {result.stdout[:300]!r}")


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--binary", required=True, help="telescope-rs, or a command that stands in for it")
    parser.add_argument("--ledger", type=Path, default=LEDGER)
    parser.add_argument("--fixtures", type=Path, default=FIXTURES)
    parser.add_argument("--all", action="store_true", help="compare every setting, not just the ported ones")
    parser.add_argument("names", nargs="*", help="only these fixtures")
    args = parser.parse_args(argv)
    with args.ledger.open("rb") as f:
        done, builtin = ported_settings(tomllib.load(f))
    binary = shlex.split(args.binary)
    folders = sorted(p for p in args.fixtures.iterdir() if p.is_dir() and (not args.names or p.name in args.names))
    failed = 0
    for folder in folders:
        expected = json.loads((folder / "expected.json").read_text(encoding="utf-8"))
        try:
            diffs = compare(expected, snapshot(binary, folder), done, builtin, args.all)
        except (RuntimeError, OSError, subprocess.TimeoutExpired) as exc:
            diffs = [f"config-snapshot failed: {exc}"]
        failed += bool(diffs)
        print(f"{folder.name}: {'differs' if diffs else 'same'}")
        for line in diffs:
            print(f"  {line}")
    scope = "every setting" if args.all else f"{len(done)} ported settings plus the config-level parts"
    print(f"\n{len(folders) - failed} of {len(folders)} fixtures match ({scope})")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
