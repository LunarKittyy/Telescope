#!/usr/bin/env python3
"""Compares the Rust window's scenes with Qt's, both with only the plugins Rust has ported, and marks the ones that match.

    python scenes.py OUT                       # every scene telescope-rs can draw, at 1x and 1.5x
    python scenes.py OUT connection-ready      # just these
    python scenes.py OUT --mark                # and mark the scenes that match at every scale `ported` in the ledger

A scene matches when, after compare_scenes.py's light blur, no more than 0.5% of pixels differ. Up to 2% it's
close and listed for a person to judge; that's never marked.

Asks `telescope-rs scenes --list` which plugins the Rust scene window loads and which scenes it draws, renders those
scenes with the translation preview's own builders on a Qt window that registers only those plugins (OUT/qt), renders
them from Rust (OUT/rust), then runs compare_scenes.py --reduced (OUT/review/index.html). A full Qt render
(render_scenes.py) is what `verified` is checked against, once every card exists.
"""

import argparse
import base64
import json
import os
import subprocess
import sys
from concurrent.futures import ProcessPoolExecutor
from multiprocessing import get_context
from pathlib import Path

HERE = Path(__file__).resolve().parent
RUST = HERE.parents[2]
DESKTOP = RUST.parent / "desktop"
PARITY = RUST / "parity"
LEDGER = PARITY / "ledger.toml"
BINARY = RUST / "target" / "debug" / "telescope-rs"


def qt_render(id: str, scale: float, plugins: list) -> tuple:
    """(id, PNG bytes or None, error) for one scene on a Qt window with only plugins, in a fresh process."""
    os.environ["QT_QPA_PLATFORM"] = "offscreen"
    os.environ["QT_SCALE_FACTOR"] = f"{scale:g}"
    sys.path.insert(0, str(DESKTOP))
    try:
        import telescope.plugins as builtins
        from scripts import translation_preview as preview
        from PyQt6.QtWidgets import QApplication
        # render() grabs after one round of events, before a WrapLabel's new height has reached the layouts around it
        settle = QApplication.processEvents
        QApplication.processEvents = staticmethod(lambda *args: [settle(*args) for _ in range(5)][-1])
        everything = builtins.builtin_plugins
        # A plugin with a window of its own: its scenes show that window, so it stays out of every other scene's cards
        wanted = plugins + ["composite"] if id.startswith("composite-editor") else plugins
        builtins.builtin_plugins = lambda: [p for p in everything() if p.name in wanted]
        result = preview.render({"lang": "en", "scene": id})
        return id, base64.b64decode(result["image"]), None
    except Exception as exc:  # noqa: BLE001
        return id, None, f"{type(exc).__name__}: {exc}"


def render_qt(ids: list, scales: list, plugins: list, out: Path) -> list:
    failed = []
    for scale in scales:
        folder = out / f"{scale:g}x"
        folder.mkdir(parents=True, exist_ok=True)
        with ProcessPoolExecutor(min(8, os.cpu_count() or 2), mp_context=get_context("spawn"), max_tasks_per_child=1) as pool:
            jobs = [pool.submit(qt_render, id, scale, plugins) for id in ids]
            for n, job in enumerate(jobs, 1):
                id, png, error = job.result()
                if png is None:
                    failed.append(f"{scale:g}x {id}: {error}")
                else:
                    (folder / f"{id}.png").write_bytes(png)
                print(f"\rQt {scale:g}x: {n} of {len(ids)}", end="", flush=True)
        print()
    return failed


def matching(qt: Path, rust: Path, ids: list, scales: list, threshold: float, review: float, tolerance: int) -> tuple:
    """(ids whose Rust render matches the Qt one at every scale by compare_scenes.py's measure, ids that are only close:
    the same size and within review, for a person to judge)."""
    sys.path.insert(0, str(PARITY))
    import compare_scenes
    same, close = [], []
    for id in ids:
        worst = 0.0
        for scale in scales:
            a, b = qt / f"{scale:g}x" / f"{id}.png", rust / f"{scale:g}x" / f"{id}.png"
            if not (a.exists() and b.exists()):
                worst = 1.0
                break
            x, y = compare_scenes.load(a), compare_scenes.load(b)
            share, _ = compare_scenes.difference(x, y, tolerance, float(scale))
            worst = max(worst, share if x.shape == y.shape else 1.0)
        if worst <= threshold:
            same.append(id)
        elif worst <= review:
            close.append(f"{id} ({worst:.2%})")
    return same, close


def mark(ids: list) -> list:
    """Marks ids `ported` in the ledger where they're still todo; the ones it changed."""
    text, changed = LEDGER.read_text(encoding="utf-8"), []
    for id in ids:
        line = f'"{id}" = {{ status = "todo" }}'
        if line in text:
            text = text.replace(line, f'"{id}" = {{ status = "ported", rust = "rust/app/src/scenes.rs" }}')
            changed.append(id)
    LEDGER.write_text(text, encoding="utf-8")
    return changed


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("out", type=Path)
    parser.add_argument("scenes", nargs="*", help="scene ids (default: every scene telescope-rs draws)")
    parser.add_argument("--scale", type=float, nargs="+", default=[1, 1.5])
    parser.add_argument("--mark", action="store_true", help="mark the scenes that match `ported` in the ledger")
    parser.add_argument("--no-build", action="store_true", help="use target/debug/telescope-rs as it is")
    args = parser.parse_args(argv)
    if not args.no_build:
        subprocess.run(["cargo", "build", "-q", "-p", "telescope-app"], cwd=RUST, check=True)
    known = json.loads(subprocess.run([BINARY, "scenes", "--list"], capture_output=True, text=True, check=True).stdout)
    ids = args.scenes or known["scenes"]
    unknown = [i for i in ids if i not in known["scenes"]]
    if unknown:
        parser.error(f"telescope-rs can't draw: {', '.join(unknown)}")
    print(f"Plugins on both sides: {', '.join(known['plugins'])}")
    failed = render_qt(ids, args.scale, known["plugins"], args.out / "qt")
    scales = [f"{s:g}" for s in args.scale]
    rust = subprocess.run([BINARY, "scenes", args.out / "rust", *ids, "--scale", *scales])
    for line in failed:
        print(f"Qt couldn't render {line}")
    subprocess.run([sys.executable, PARITY / "compare_scenes.py", args.out / "qt", args.out / "rust", args.out / "review",
                    "--reduced"])
    same, close = matching(args.out / "qt", args.out / "rust", ids, args.scale, 0.005, 0.02, 24)
    print(f"{len(same)} of {len(ids)} scenes match at every scale" + (f": {', '.join(same)}" if same else ""))
    if close:
        print(f"{len(close)} are close, for a person to judge on the review page (never marked): {', '.join(close)}")
    if args.mark and same:
        changed = mark(same)
        print(f"Marked {len(changed)} scenes ported" + (f": {', '.join(changed)}" if changed else ""))
    return 1 if failed or rust.returncode else 0


if __name__ == "__main__":
    sys.exit(main())
