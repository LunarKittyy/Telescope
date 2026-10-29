"""Records what the Python app does for Notes, Name tag and Desk mode, for tests/cards_parity.rs in the app crate to compare the Rust
plugins with.

    cd desktop && QT_QPA_PLATFORM=offscreen python ../rust/crates/effects/tests/fixtures/make_cards_fixtures.py

Writes next to this file: cards_configs.json (what get_config answers after set_config), nametag.json with the layout of each tag
and its rendered tile as nametag_NN.rgba (the tag's colour times its alpha, then its alpha), and desk.json with the desk picture
(desk_frame.rgba) put through each desk setting as desk_NN.rgba. Pictures are raw RGBA, 4 bytes a pixel, sizes in the json.
"""
import json
import sys
from pathlib import Path

import cv2
import numpy as np

HERE = Path(__file__).resolve().parent
DESKTOP = HERE.parents[4] / "desktop"
sys.path.insert(0, str(DESKTOP))

from telescope import name_tag as nt  # noqa: E402
from telescope.plugins import desk  # noqa: E402
from telescope.view import View, to_full  # noqa: E402

DW, DH = 240, 135
CORNERS = [(83, 15), (158, 21), (167, 128), (74, 122)]  # a sheet seen at an angle, in px


_APP = []


def _app():
    from PyQt6.QtWidgets import QApplication
    if not _APP:
        _APP.append(QApplication.instance() or QApplication([]))
    return _APP[0]


def rgba(bgr: np.ndarray) -> bytes:
    out = np.dstack([bgr[..., ::-1], np.full(bgr.shape[:2], 255, np.uint8)])
    return np.ascontiguousarray(out).tobytes()


def desk_frame() -> np.ndarray:
    """A dark desk with a white, unevenly lit sheet at CORNERS and a black bar and some lines of ink across it."""
    sheet = np.full((200, 150, 3), 240, np.uint8)
    sheet[:, :] = (np.linspace(150, 245, 150)[None, :, None] * np.ones((200, 1, 3))).astype(np.uint8)
    sheet[95:105, 20:130] = 0
    for y in range(20, 180, 14):
        sheet[y:y + 3, 15:100] = 60
    matrix = cv2.getPerspectiveTransform(np.float32([[0, 0], [150, 0], [150, 200], [0, 200]]), np.float32(CORNERS))
    frame = np.full((DH, DW, 3), 40, np.uint8)
    mask = cv2.warpPerspective(np.full((200, 150), 255, np.uint8), matrix, (DW, DH))
    frame[mask > 0] = cv2.warpPerspective(sheet, matrix, (DW, DH))[mask > 0]
    return frame


def full(points):
    return tuple(to_full(View(), x / DW, y / DH) for x, y in points)


def desks() -> dict:
    frame = desk_frame()
    (HERE / "desk_frame.rgba").write_bytes(rgba(frame))
    cases = [
        ("filled", dict(shape="fill")),
        ("kept in its shape", dict()),
        ("a4", dict(shape="a4")),
        ("whitened", dict(cleanup="contrast")),
        ("black and white, filled", dict(shape="fill", cleanup="bw")),
        ("stretched", dict(stretch=(120, 80))),
        ("a square", dict(shape="square", cleanup="contrast")),
    ]
    out = []
    for i, (name, extra) in enumerate(cases):
        settings = desk.Settings(on=True, corners=full(CORNERS), **extra)
        warp = desk.Warp()
        got = desk.straighten(frame, settings, warp)
        file = f"desk_{i:02d}.rgba"
        (HERE / file).write_bytes(rgba(got))
        matrix, rect = warp.get(settings, DW, DH)
        out.append({"name": name, "file": file, "settings": {"corners": [list(p) for p in settings.corners], "cleanup": settings.cleanup,
                                                              "shape": settings.shape, "stretch": list(settings.stretch)},
                    "rect": list(rect), "matrix": matrix.tolist(), "on_preview": warp.on_preview.tolist()})
    corners = desk.order_corners([(x, y) for x, y in CORNERS])
    return {"width": DW, "height": DH, "frame": "desk_frame.rgba", "cases": out,
            "aspect": desk.sheet_aspect(corners, (DW / 2, DH / 2), desk._FOCAL * DW)}


def tags() -> dict:
    _app()
    base = dict(name="Luna Kozankiewicz", pronouns="she/her", second="Telescope developer", families=nt.app_families())
    cases = [
        ("the default", 640, 360, {}),
        ("a pill in the middle", 640, 360, dict(style="pill", position="bottom_centre")),
        ("light, large, top right", 640, 360, dict(style="light", position="top_right", size="large")),
        ("a shadow, bottom right", 640, 360, dict(style="shadow", position="bottom_right")),
        ("only a small name", 640, 360, dict(pronouns="", second="", size="small")),
        ("a name and pronouns", 640, 360, dict(second="")),
        ("a long name", 640, 360, dict(name="Wolfeschlegelsteinhausenbergerdorff " * 3, second="")),
        ("full hd", 1920, 1080, {}),
        ("a tall canvas", 1080, 1920, dict(position="bottom_centre")),
    ]
    out = []
    for i, (name, w, h, extra) in enumerate(cases):
        spec = nt.Spec(**{**base, **extra})
        lay = nt.layout(spec, w, h)
        tag = nt.render(spec, w, h)
        piece = lambda p: None if p is None else {"text": p[0], "x": p[2].x(), "baseline": p[2].y()}  # noqa: E731
        file = f"nametag_{i:02d}.rgba"
        th, tw = tag.ink.shape[:2]
        tile = np.dstack([tag.ink[..., ::-1], 255 - tag.keep[..., 0]])
        (HERE / file).write_bytes(np.ascontiguousarray(tile).tobytes())
        out.append({
            "name": name, "width": w, "height": h,
            "spec": {k: getattr(spec, k) for k in ("name", "pronouns", "second", "style", "position", "size")},
            "box": [lay.box.x(), lay.box.y(), lay.box.width(), lay.box.height()],
            "name_at": piece(lay.name), "pronouns_at": piece(lay.pronouns), "second_at": piece(lay.second),
            "stripe": None if lay.stripe is None else [lay.stripe.x(), lay.stripe.y(), lay.stripe.width(), lay.stripe.height()],
            "radius": lay.radius, "em": lay.em, "tile": {"x": tag.x, "y": tag.y, "width": tw, "height": th, "file": file},
        })
    return {"cases": out}


def configs() -> list:
    _app()
    from telescope.plugin import EventBus
    from telescope.plugins.desk import DeskPlugin
    from telescope.plugins.name_tag import NameTagPlugin
    from telescope.plugins.notes import NotesPlugin

    class Host:
        def schedule_save(self):
            pass

        def view(self):
            return View()

        def cancel_pick(self, *a):
            pass

        def set_preview_warp(self, *a):
            pass

    out, panels = [], []

    def run(plugin_cls, name, inputs):
        for cfg in inputs:
            host, bus = Host(), EventBus()
            plugin = plugin_cls()
            plugin.setup(host, bus)
            panels.append(plugin.create_panel())
            try:
                plugin.set_config(json.loads(json.dumps(cfg)))
            except TypeError:
                out.append({"plugin": name, "input": cfg, "output": None})
                continue
            out.append({"plugin": name, "input": cfg, "output": plugin.get_config()})

    run(NotesPlugin, "notes", [
        {},
        {"sets": ["a", 5, None], "current": 9, "size": "huge", "speed": 99, "scroll": "sideways"},
        {"sets": "not a list"},
        {"sets": ["one", "two", "three"], "current": 2, "size": 70, "speed": 7, "scroll": "hand"},
        {"sets": [str(i) for i in range(12)], "current": 8},
        {"sets": ["x"], "current": -1, "size": 3, "speed": 0},
        {"sets": ["x"], "current": 1.0, "size": 99.9, "speed": "4"},
        {"sets": [], "size": None, "speed": [3]},
        {"sets": ["multi\nline\n\ntext", "é" * 40], "current": 1, "size": 16, "speed": 10},
    ])
    run(NameTagPlugin, "name_tag", [
        {},
        {"on": True, "name": "Luna", "pronouns": "she/her", "second": "Host", "style": "pill", "position": "top_right", "size": "large"},
        {"on": "yes", "name": 5, "style": "neon", "position": "middle", "size": "huge", "second": "x" * 500},
        {"on": 1, "name": "n" * 80, "style": "shadow", "position": "bottom_centre", "size": "small"},
        {"size": ["large"]},
        {"style": ["bar"], "position": {"a": 1}, "name": ["x"]},
        {"on": True, "pronouns": "  they/them  ", "second": None},
    ])
    corners = [[0.1, 0.2], [0.9, 0.2], [0.9, 0.8], [0.1, 0.8]]
    run(DeskPlugin, "desk", [
        {},
        {"on": True, "corners": corners, "cleanup": "bw", "shape": "a4", "stretch": [80, 130]},
        {"on": True, "corners": [[0, 0], [1, "x"]], "cleanup": "sepia", "shape": "oval", "stretch": [500, True]},
        {"stretch": [10, 900], "keep_shape": False},
        {"on": True, "corners": [[0.1, 0.5], [0.4, 0.5], [0.6, 0.5], [0.9, 0.5]]},
        {"on": True, "corners": [[0.9, 0.1], [0.1, 0.1], [0.9, 0.9], [0.1, 0.9]], "shape": "square", "cleanup": "contrast"},
        {"on": True, "corners": corners, "stretch": [75.5, 120.4]},
        {"on": 1, "corners": corners},
        {"corners": corners[:3]},
        {"on": True, "corners": [[0.1, 0.2], [0.9, float("nan")], [0.9, 0.8], [0.1, 0.8]]},
        {"shape": "fill", "keep_shape": True},
        {"keep_shape": False},
    ])
    return out


def main():
    _app()
    (HERE / "cards_configs.json").write_text(json.dumps(configs(), indent=1, allow_nan=True))
    (HERE / "nametag.json").write_text(json.dumps(tags(), indent=1))
    (HERE / "desk.json").write_text(json.dumps(desks(), indent=1))


if __name__ == "__main__":
    main()
