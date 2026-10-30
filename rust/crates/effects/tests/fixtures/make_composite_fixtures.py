"""Records what the Python app does for Several cameras, for tests/composite_parity.rs in the app crate to compare the Rust
plugin with.

    cd desktop && QT_QPA_PLATFORM=offscreen python ../rust/crates/effects/tests/fixtures/make_composite_fixtures.py

Writes next to this file: composite.json with the test streams (composite_src_N.rgba), each layout drawn from them by the
Compositor (composite_NN.rgba), and what the plugin's get_config answers after set_config for odd settings. Pictures are raw
RGBA, 4 bytes a pixel, sizes in the json.
"""
import json
import sys
from pathlib import Path

import cv2
import numpy as np

HERE = Path(__file__).resolve().parent
DESKTOP = HERE.parents[4] / "desktop"
sys.path.insert(0, str(DESKTOP))

from telescope import composite  # noqa: E402
from telescope.composite import Compositor, Layout, Tile, layout_dict  # noqa: E402

_APP = []


def _app():
    from PyQt6.QtWidgets import QApplication
    if not _APP:
        _APP.append(QApplication.instance() or QApplication([]))
    return _APP[0]


def rgba(bgr: np.ndarray) -> bytes:
    out = np.dstack([bgr[..., ::-1], np.full(bgr.shape[:2], 255, np.uint8)])
    return np.ascontiguousarray(out).tobytes()


def stream(w: int, h: int, seed: int) -> np.ndarray:
    """A BGR picture with ramps, saturated blocks, thin lines and a little noise, so scaling and cropping show."""
    rng = np.random.default_rng(seed)
    x = np.linspace(0, 1, w, dtype=np.float32)[None, :]
    y = np.linspace(0, 1, h, dtype=np.float32)[:, None]
    img = np.stack([255 * (1 - x) * (0.4 + 0.6 * y), 255 * y + 0 * x, 255 * x + 0 * y], -1)
    img[h // 8:h // 3, w // 8:w // 3] = (40, 60, 220)
    img[h // 2:3 * h // 4, w // 2:5 * w // 6] = (60, 200, 70)
    for i in range(0, w, max(3, w // 20)):
        cv2.line(img, (i, 0), (w - 1 - i, h - 1), (255, 255, 255), 1)
    img += rng.normal(0, 5, img.shape)
    return np.clip(img, 0, 255).astype(np.uint8)


SOURCES = {"a": (320, 180, 1), "b": (90, 160, 2), "c": (200, 150, 3), "d": (64, 36, 4)}


def renders(frames: dict) -> list:
    streams = [(sid, sid.upper(), w / h) for sid, (w, h, _seed) in SOURCES.items()]
    custom = (
        Tile("a", "", 0.0, 0.0, 1.0, 1.0, zoom=1.7, cx=0.3, cy=0.6),
        Tile("c", "", 0.55, 0.08, 0.4, 0.5, border=30, border_colour="#ffcc00", radius=120),
        Tile("d", "", 0.05, 0.6, 0.3, 0.35, radius=60, crop_l=0.1, crop_t=0.2, crop_r=0.05, crop_b=0.1, stretch=True),
        Tile("b", "", 0.36, 0.3, 0.15, 0.6, border=12, border_colour="#2080ff", radius=40, zoom=1.3, cx=0.8, cy=0.1),
    )
    cases = [
        ("side by side", Layout(tiles=composite.side_by_side(streams[:2], 16 / 9)), (192, 108)),
        ("big and small", Layout(tiles=composite.big_and_small(streams, 16 / 9)), (192, 108)),
        ("a grid", Layout(tiles=composite.grid(streams, 16 / 9)), (192, 108)),
        ("borders, corners, crops and zooms", Layout(tiles=custom, background="#336699"), (192, 108)),
        ("the same at an odd size", Layout(tiles=custom, background="#336699"), (97, 55)),
        ("growing a small stream with big corners", Layout(tiles=(
            Tile("d", "", 0.1, 0.1, 0.8, 0.8, border=40, border_colour="#ffffff", radius=200),
            Tile("a", "", 0.6, 0.55, 0.35, 0.4, radius=80)), background="#000000"), (400, 225)),
        ("an upright picture", Layout(tiles=composite.big_and_small([streams[1], streams[0], streams[2]], 9 / 16)), (90, 160)),
        ("a stream that isn't running", Layout(tiles=(Tile("a", "", 0.0, 0.0, 0.6, 1.0), Tile("gone", "", 0.5, 0.2, 0.5, 0.5))),
         (160, 90)),
    ]
    out = []
    for i, (name, layout, size) in enumerate(cases):
        comp = Compositor()
        comp.prepare(layout, size)
        picture = comp.render({sid: (frame, None) for sid, frame in frames.items()})
        file = f"composite_{i:02d}.rgba"
        (HERE / file).write_bytes(rgba(picture))
        out.append({"name": name, "layout": layout_dict(layout), "width": size[0], "height": size[1], "file": file})
    return out


class _Host:
    def stream_frames(self, copy=False):
        return []

    def stream_frame(self, source_id, copy=False):
        return None

    def frame_source(self):
        return None

    def stream_on_camera(self, slot=0):
        return None

    def canvas_size(self):
        return (None, None)

    def is_streaming_from(self, source_id):
        return False

    def schedule_save(self):
        pass


def configs() -> list:
    from telescope.plugin import EventBus
    from telescope.plugins.composite import CompositePlugin
    _app()
    good = {"name": "Talk", "background": "#202020", "picture": "", "tiles": [
        {"source_id": "phone:1", "name": "Pixel", "x": 0.0, "y": 0.0, "w": 1.0, "h": 1.0},
        {"source_id": "browser:x", "name": "Laptop", "x": 0.7, "y": 0.7, "w": 0.25, "h": 0.25, "zoom": 1.5, "cx": 0.4,
         "cy": 0.6, "border": 4, "border_colour": "#FF8800", "radius": 24, "crop_l": 0.1, "stretch": True}]}
    inputs = [
        {},
        {"on": True, "layout": good, "layouts": [good]},
        {"on": "yes", "layout": "nope", "layouts": "nope"},
        {"on": 1, "layout": {"name": 5, "background": "#12345", "picture": 7, "tiles": [
            {"source_id": "a", "x": "0.5", "y": -2, "w": 3, "h": 0.01, "zoom": 0.2, "cx": 2, "cy": None, "border": 99.7,
             "radius": 250.9, "border_colour": "white", "crop_l": 0.5, "crop_r": 0.5, "crop_t": 1.2, "crop_b": 0.3,
             "stretch": 1, "name": "x‮y\u0007<z>"},
            {"source_id": 5}, "junk", {"source_id": "b" * 250, "w": 0.3, "x": 0.9}]},
         "layouts": [{"name": "  A  "}, {"name": "A"}, {"name": "<>"}, {"name": "B" * 60, "tiles": [{"source_id": "q"}]},
                     None, {"name": "C", "background": "#ABCDEF"}]},
        {"on": False, "layout": {"tiles": [{"source_id": c} for c in "abcdef"]},
         "layouts": [{"name": f"L{i}"} for i in range(35)]},
    ]
    out = []
    for cfg in inputs:
        plugin = CompositePlugin()
        plugin.setup(_Host(), EventBus())
        try:
            plugin.set_config(cfg)
            out.append({"input": cfg, "output": plugin.get_config()})
        finally:
            plugin.shutdown()
    return out


def main():
    frames = {}
    sources = []
    for sid, (w, h, seed) in SOURCES.items():
        frames[sid] = stream(w, h, seed)
        file = f"composite_src_{sid}.rgba"
        (HERE / file).write_bytes(rgba(frames[sid]))
        sources.append({"source_id": sid, "width": w, "height": h, "file": file})
    data = {"sources": sources, "cases": renders(frames), "configs": configs()}
    (HERE / "composite.json").write_text(json.dumps(data, indent=1) + "\n")


if __name__ == "__main__":
    main()
