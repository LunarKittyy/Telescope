"""How far the shell's renders are from Qt's, whole and by region, while only some cards are ported.

    python region_diff.py QT RUST [--scale 1]

For each scene in both folders: the share of pixels more than 24 off in any channel (compare_scenes.py's tolerance),
the mean difference, and the same for the regions that are the shell's own (header, footer, the settings page).
"""

import argparse
import sys
from pathlib import Path

from PIL import Image, ImageChops, ImageStat

REGIONS = {
    "shell-wide": {"header": (0, 0, 1300, 60), "footer": (0, -40, 1300, None), "camera card": (872, 76, 1278, 597)},
    "shell-medium": {"header": (0, 0, 900, 60), "footer": (0, -40, 900, None)},
    "shell-narrow": {"header": (0, 0, 560, 60), "footer": (0, -40, 560, None)},
    "shell-settings-plugins": {"page": (236, 0, 940, 680), "tab column": (0, 0, 236, 60)},
}


def measure(a: Image.Image, b: Image.Image) -> tuple:
    diff = ImageChops.difference(a, b)
    over = sum(1 for px in zip(*[iter(diff.tobytes())] * 3) if max(px) > 24)
    return 100 * over / (diff.width * diff.height), sum(ImageStat.Stat(diff).mean) / 3


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("qt", type=Path)
    parser.add_argument("rust", type=Path)
    parser.add_argument("--scale", default="1")
    args = parser.parse_args(argv)
    for qt_png in sorted((args.qt / f"{args.scale}x").glob("*.png")):
        rust_png = args.rust / f"{args.scale}x" / qt_png.name
        if not rust_png.exists():
            continue
        qt, rust = Image.open(qt_png).convert("RGB"), Image.open(rust_png).convert("RGB")
        size = "" if qt.size == rust.size else f"  (sizes {qt.size} vs {rust.size})"
        w, h = min(qt.width, rust.width), min(qt.height, rust.height)
        share, mean = measure(qt.crop((0, 0, w, h)), rust.crop((0, 0, w, h)))
        print(f"{qt_png.stem}: {share:.1f}% of pixels differ, mean {mean:.1f}/255{size}")
        s = float(args.scale)
        for name, (x0, y0, x1, y1) in REGIONS.get(qt_png.stem, {}).items():
            box = [round(v * s) if v is not None else None for v in (x0, y0, x1, y1)]
            box = [box[0], box[1] if box[1] >= 0 else qt.height + box[1], box[2], box[3] or qt.height]
            rb = [box[0], box[1] - qt.height + rust.height if y0 < 0 else box[1], box[2], (box[3] - qt.height + rust.height) if y0 < 0 else box[3]]
            share, mean = measure(qt.crop(box), rust.crop(rb))
            print(f"    {name}: {share:.1f}%, mean {mean:.1f}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
