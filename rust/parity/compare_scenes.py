#!/usr/bin/env python3
"""Compares the Rust app's scene renders with the Qt ones and writes a side-by-side review page for the ones that differ.

Both folders hold <scale>x/<scene id>.png, as render_scenes.py writes them for Qt. Both pictures are first blurred a
little (a Gaussian of --blur logical pixels, so 1.5 pixels at 1.5x), so how Qt and Slint antialias glyph edges drops
out while a missing button or a line out of place still shows; then a pixel counts as
different when a channel is off by more than --tolerance, and a scene matches when no more than --threshold of its
pixels differ and its size is the same. One that doesn't goes on the page: within --review (2%) it's marked for a
person to look at, and nothing marks it automatically.

    python compare_scenes.py QT_DIR RUST_DIR REVIEW_DIR [--threshold 0.005] [--review 0.02] [--tolerance 24] [--blur 1]

A scene the ledger marks verified fails the run when it differs or has no Rust render; the rest are only reported,
since most scenes aren't ported yet. With --reduced, QT_DIR holds renders of a Qt window with only the plugins Rust has
ported (rust/crates/shell/parity/scenes.py makes them) and ported scenes have to match too. Opens REVIEW_DIR/index.html for the side-by-side view.
"""

import argparse
import html
import os
import sys
import tomllib
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parent
LEDGER = HERE / "ledger.toml"
DONE = ("ported", "verified")  # what has to match a --reduced render; a full one only checks verified


def load(path: Path):
    """The picture as an RGBA array (height, width, 4)."""
    from PyQt6.QtGui import QImage
    image = QImage(str(path)).convertToFormat(QImage.Format.Format_RGBA8888)
    if image.isNull():
        raise ValueError(f"can't read {path}")
    data = image.constBits().asstring(image.sizeInBytes())
    return np.frombuffer(data, np.uint8).reshape(image.height(), image.bytesPerLine() // 4, 4)[:, :image.width()]


def save(array, path: Path):
    from PyQt6.QtGui import QImage
    array = np.ascontiguousarray(array)
    QImage(array.data, array.shape[1], array.shape[0], array.shape[1] * 4, QImage.Format.Format_RGBA8888).save(str(path))


def blurred(picture, sigma: float):
    """picture under a Gaussian blur of sigma pixels, applied along each axis (edges repeat)."""
    radius = max(1, int(round(sigma * 3)))
    x = np.arange(-radius, radius + 1)
    kernel = np.exp(-x * x / (2 * sigma * sigma))
    kernel /= kernel.sum()
    out = picture.astype(np.float32)
    for axis in (0, 1):
        pad = [(0, 0)] * out.ndim
        pad[axis] = (radius, radius)
        padded = np.pad(out, pad, mode="edge")
        out = sum(k * np.take(padded, range(i, i + out.shape[axis]), axis=axis) for i, k in enumerate(kernel))
    return out


def difference(qt, rust, tolerance: int, sigma: float = 1.0) -> tuple:
    """The share of pixels that differ (over the larger of the two sizes, after a blur of sigma pixels, none at 0) and a
    picture marking them in red."""
    height, width = max(qt.shape[0], rust.shape[0]), max(qt.shape[1], rust.shape[1])
    a = np.zeros((height, width, 4), np.int16)
    b = np.zeros((height, width, 4), np.int16)
    a[:qt.shape[0], :qt.shape[1]] = qt
    b[:rust.shape[0], :rust.shape[1]] = rust
    if sigma > 0:
        off = np.abs(blurred(a, sigma) - blurred(b, sigma)).max(axis=2) > tolerance
    else:
        off = np.abs(a - b).max(axis=2) > tolerance
    marked = (a // 3 + 170).clip(0, 255).astype(np.uint8)  # the Qt picture faded, differences in red on top
    marked[..., 3] = 255
    marked[off] = (220, 30, 30, 255)
    return float(off.mean()), marked


def page(rows: list, missing: list, out: Path, threshold: float) -> str:
    def img(path, label):
        rel = html.escape(os.path.relpath(path, out))
        return f'<figure><figcaption>{label}</figcaption><a href="{rel}"><img src="{rel}" loading="lazy"></a></figure>'
    cards = []
    for r in rows:
        badge = '<span class="must">ported</span>' if r["ported"] else ""
        if r["share"] <= r["review"] and r["qt_size"] == r["rust_size"]:
            badge += '<span class="look">close: a person decides</span>'
        size = "" if r["qt_size"] == r["rust_size"] else f' · size {r["qt_size"]} vs {r["rust_size"]}'
        cards.append(f'<section><h2>{html.escape(r["id"])} <small>{r["scale"]} · {r["share"]:.2%} differ{size}</small>'
                     f'{badge}</h2><div class="row">{img(r["qt"], "Qt")}{img(r["rust"], "Rust")}'
                     f'{img(r["diff"], "Difference")}</div></section>')
    gone = "".join(f"<li>{html.escape(m)}</li>" for m in missing)
    return f"""<!doctype html><html lang="en"><head><meta charset="utf-8"><title>Scene review</title>
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>
:root {{ --bg: #f6f6f4; --fg: #1c1c1a; --muted: #6b6b66; --card: #fff; --line: #ddd; --must: #b3261e; }}
@media (prefers-color-scheme: dark) {{ :root {{ --bg: #161615; --fg: #ececea; --muted: #9a9a94; --card: #20201f;
  --line: #333; --must: #f2b8b5; }} }}
body {{ background: var(--bg); color: var(--fg); font: 15px/1.45 system-ui, sans-serif; margin: 0 16px 48px; }}
h1 {{ font-size: 22px; margin: 24px 0 4px; }} p {{ color: var(--muted); margin: 0 0 20px; }}
section {{ background: var(--card); border: 1px solid var(--line); border-radius: 8px; padding: 12px 14px; margin: 14px 0; }}
h2 {{ font-size: 16px; margin: 0 0 10px; }} small {{ color: var(--muted); font-weight: normal; }}
.must {{ color: var(--must); font-size: 12px; margin-left: 8px; }}
.look {{ color: var(--muted); font-size: 12px; margin-left: 8px; }}
.row {{ display: flex; gap: 12px; overflow-x: auto; align-items: flex-start; }}
figure {{ margin: 0; flex: 1 1 0; min-width: 0; }} figcaption {{ color: var(--muted); font-size: 13px; }}
img {{ max-width: 100%; max-height: 70vh; border: 1px solid var(--line); }}
</style></head><body>
<h1>Scene review</h1>
<p>{len(rows)} renders differ by more than {threshold:.2%} of their pixels (after a light blur), {sum(1 for r in rows if r["share"] <= r["review"] and r["qt_size"] == r["rust_size"])} of them only a little. {len(missing)} ported scenes have no Rust render.</p>
{"<h2>Ported but not rendered by Rust</h2><ul>" + gone + "</ul>" if missing else ""}
{"".join(cards)}
</body></html>
"""


def main(argv=None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("qt", type=Path)
    parser.add_argument("rust", type=Path)
    parser.add_argument("review", type=Path)
    parser.add_argument("--threshold", type=float, default=0.005, help="share of pixels that may differ (0.005 = 0.5%%)")
    parser.add_argument("--review", dest="close", type=float, default=0.02, help="up to this share a scene that doesn't match is close, for a person to judge")
    parser.add_argument("--tolerance", type=int, default=24, help="how far a channel may be off before a pixel differs")
    parser.add_argument("--blur", type=float, default=1.0, help="Gaussian blur in logical pixels before comparing, 0 for none")
    parser.add_argument("--ledger", type=Path, default=LEDGER)
    parser.add_argument("--reduced", action="store_true", help="QT_DIR has only the ported plugins: ported scenes must match")
    args = parser.parse_args(argv)
    os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")
    from PyQt6.QtGui import QGuiApplication
    app = QGuiApplication.instance() or QGuiApplication([])  # noqa: F841
    with args.ledger.open("rb") as f:
        must = DONE if args.reduced else ("verified",)
        ported = {k for k, v in tomllib.load(f).get("scene", {}).items() if v.get("status") in must}
    args.review.mkdir(parents=True, exist_ok=True)
    rows, missing, compared, absent = [], [], 0, 0
    for qt_path in sorted(args.qt.glob("*x/*.png")):
        scale, id = qt_path.parent.name, qt_path.stem
        rust_path = args.rust / scale / qt_path.name
        if not rust_path.exists():
            absent += 1
            if id in ported:
                missing.append(f"{scale} {id}")
            continue
        qt, rust = load(qt_path), load(rust_path)
        share, marked = difference(qt, rust, args.tolerance, args.blur * float(scale.rstrip("x")))
        compared += 1
        if share > args.threshold or qt.shape != rust.shape:
            diff_path = args.review / scale / qt_path.name
            diff_path.parent.mkdir(parents=True, exist_ok=True)
            save(marked, diff_path)
            rows.append({"id": id, "scale": scale, "share": share, "ported": id in ported, "review": args.close, "qt": qt_path.resolve(),
                         "rust": rust_path.resolve(), "diff": diff_path.resolve(),
                         "qt_size": f"{qt.shape[1]}x{qt.shape[0]}", "rust_size": f"{rust.shape[1]}x{rust.shape[0]}"})
    rows.sort(key=lambda r: (not r["ported"], -r["share"]))
    (args.review / "index.html").write_text(page(rows, missing, args.review.resolve(), args.threshold), encoding="utf-8")
    failing = [r for r in rows if r["ported"]]
    print(f"{compared} renders compared, {len(rows)} differ ({len(failing)} of them ported), "
          f"{absent} not rendered by Rust ({len(missing)} of them ported)")
    for r in failing:
        print(f"  differs: {r['scale']} {r['id']} ({r['share']:.2%})")
    for m in missing:
        print(f"  no Rust render: {m}")
    print(f"Review page: {args.review / 'index.html'}")
    return 1 if failing or missing else 0


if __name__ == "__main__":
    sys.exit(main())
