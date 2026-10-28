#!/usr/bin/env python3
"""Renders every Qt scene of the translation preview to PNG, in English, at 1x and 1.5x, for compare_scenes.py.

    python render_scenes.py OUT                 # OUT/1x/<scene>.png and OUT/1.5x/<scene>.png, real screens only
    python render_scenes.py OUT cool main-wide  # just these scenes
    python render_scenes.py OUT --scale 1 --all # also the pages of texts no screen shows and the search scenes

It reuses the preview's offscreen render workers (desktop/scripts/translation_preview.py), so each scene is built
the same way the translation workbench builds it. 1.5x is Qt's QT_SCALE_FACTOR.
"""

import argparse
import base64
import os
import sys
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

DESKTOP = Path(__file__).resolve().parents[2] / "desktop"
GALLERIES = ("search", "messages")


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("out", type=Path)
    parser.add_argument("scenes", nargs="*", help="scene ids (default: all)")
    parser.add_argument("--scale", type=float, nargs="+", default=[1, 1.5])
    parser.add_argument("--all", action="store_true", help="include the generated text pages and search scenes")
    args = parser.parse_args(argv)
    sys.path.insert(0, str(DESKTOP))
    from scripts import translation_preview as preview
    from scripts import translation_scenes as scenes
    ids = args.scenes or [i for i, s in scenes.SCENES.items() if args.all or s.section not in GALLERIES]
    unknown = [i for i in ids if i not in preview.SCENES]
    if unknown:
        parser.error(f"no such scene: {', '.join(unknown)}")
    failed = []
    for scale in args.scale:
        os.environ["QT_SCALE_FACTOR"] = f"{scale:g}"
        with preview._idle_lock:  # workers started at another scale keep it, so start fresh ones
            for worker in preview._idle:
                worker.kill()
            preview._idle.clear()
        folder = args.out / f"{scale:g}x"
        folder.mkdir(parents=True, exist_ok=True)

        def render(id):
            try:
                result = preview.worker({"lang": "en", "scene": id}, timeout=120)
            except RuntimeError as exc:
                return id, str(exc).strip().splitlines()[-1:] or ["failed"]
            (folder / f"{id}.png").write_bytes(base64.b64decode(result["image"]))
            return id, None

        with ThreadPoolExecutor(min(8, os.cpu_count() or 2)) as pool:
            for n, (id, error) in enumerate(pool.map(render, ids), 1):
                if error:
                    failed.append(f"{scale:g}x {id}: {error[0]}")
                print(f"\r{scale:g}x: {n} of {len(ids)}", end="", flush=True)
        print()
    for line in failed:
        print(f"couldn't render {line}")
    print(f"{len(ids) * len(args.scale) - len(failed)} pictures in {args.out}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
