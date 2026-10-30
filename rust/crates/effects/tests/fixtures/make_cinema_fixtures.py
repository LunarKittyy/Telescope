"""Records what the Python app's Cinema tools and scopes make, for tests/cinema_parity.rs in the app crate to compare with.

    cd desktop && QT_QPA_PLATFORM=offscreen python ../rust/crates/effects/tests/fixtures/make_cinema_fixtures.py

Writes next to this file: cinema.json (a checksum of every picture and count the scopes and the pixel tools make on the same test
pictures, what the level filter answers to a run of readings, and what the plugin saves after loading odd settings) and
scope_grid_256.bin (the circle, bar targets and skin line OpenCV draws on a 256 vectorscope, as the lit pixels x, y, B, G, R, A).
The pictures aren't stored: the formula in picture() is written out the same way in the Rust test. Pass --dump NAME to write a
scope's picture as raw BGRA next to the script, to see where Rust and Python part.
"""
import json
import sys
import zlib
from pathlib import Path

import cv2
import numpy as np

HERE = Path(__file__).resolve().parent
DESKTOP = HERE.parents[4] / "desktop"
sys.path.insert(0, str(DESKTOP))

from telescope import scopes  # noqa: E402
from telescope.plugins import cinema  # noqa: E402
from telescope.view import View  # noqa: E402

PICTURES = [("small", 96, 64), ("hd", 640, 360), ("wide", 1600, 900), ("tall", 90, 200), ("narrow", 7, 5)]


def picture(w: int, h: int) -> np.ndarray:
    """A BGR test picture from integer arithmetic alone, so Rust can make the same one: ramps, a texture with hard edges, a dark
    block, a bright one and a clipped one."""
    y, x = np.mgrid[0:h, 0:w]
    r = (x * 255) // max(w - 1, 1)
    g = (y * 255) // max(h - 1, 1)
    b = ((x * 3 + y * 5) ^ ((x * y) >> 3)) & 255
    img = np.stack([b, g, r], -1).astype(np.uint8)
    img[h // 8:h // 3, w // 10:w // 3] = (30, 60, 220)
    img[h // 2:h * 3 // 4, w // 2:w * 3 // 4] = (250, 250, 250)
    img[h * 3 // 4:, :w // 4] = (0, 0, 0)
    img[h // 5, :] = 255
    img[:, w // 3 * 2] = (255, 255, 255)
    return img


def crc(a: np.ndarray) -> int:
    return zlib.crc32(np.ascontiguousarray(a).tobytes())


def rows(a: np.ndarray) -> list:
    return [crc(a[i]) for i in range(a.shape[0])]


def image_record(a: np.ndarray) -> dict:
    return {"w": a.shape[1], "h": a.shape[0], "crc": crc(a), "rows": rows(a)}


def pictures() -> dict:
    out = {}
    for name, w, h in PICTURES:
        img = picture(w, h)
        small = scopes.shrink(img)
        luma = scopes.luma(img)
        record = {"w": w, "h": h, "luma": crc(luma), "shrink": {"w": small.shape[1], "h": small.shape[0], "crc": crc(small)},
                  "histogram": scopes.histogram(small).tolist(), "waveform": crc(scopes.waveform(scopes.luma(small))),
                  "parade": crc(scopes.parade(small)), "vectorscope": crc(scopes.vectorscope(small)),
                  "vectorscope_64": crc(scopes.vectorscope(small, 64)),
                  "chroma": [crc(c) for c in scopes.chroma(small)]}
        record["images"] = {n: image_record(scopes.scope_image(n, img)) for n in scopes.SCOPES}
        record["false_colour"] = crc(cinema.false_colour(img))
        record["peaking"] = {str(s): crc(cinema.peaking_mask(img, s).view(np.uint8)) for s in (1, 3, 5, 7, 10)}
        record["zebra"] = {f"{level}:{phase}": crc(cinema.zebra_mask(img, level, phase).view(np.uint8))
                           for level in (70, 95, 100) for phase in (0, 3, 9)}
        record["tint"] = tints(img)
        out[name] = record
    return out


LOOKS = [
    dict(false_colour=True), dict(zebras=True), dict(peaking=True), dict(peaking=True, peaking_colour="green", peaking_level=9),
    dict(zebras=True, peaking=True, peaking_colour="white", zebra_level=80), dict(zebras=True, false_colour=True),
    dict(false_colour=True, peaking=True, peaking_colour="blue", peaking_level=2, zebras=True, zebra_level=100),
    dict(guides="thirds"), dict(),
]


def tints(img: np.ndarray) -> list:
    out = []
    for look in LOOKS:
        layer = cinema.CinemaLayer()
        layer.look = cinema.Look(**look)
        crcs, means = [], []
        for _ in range(3):
            tinted = layer.tint(img.copy())
            crcs.append(crc(tinted))
            means.append([float(m) for m in tinted.mean(axis=(0, 1))])
        # What was judged on a smaller copy is grown back to the frame with OpenCV's own bilinear resize (the IPP one in its wheels),
        # which no plain arithmetic matches to the byte (a level off in a few pixels, and so a mask's edge a pixel off), so a big
        # frame is compared by its mean colour
        out.append({"look": look, "crcs": crcs, "means": means, "grown": bool(img.shape[1] > cinema._WORK_WIDTH and any(
            look.get(k) for k in ("false_colour", "zebras", "peaking")))})
    return out


def level() -> dict:
    views = [("none", {}), ("flip_h", {"flip_h": True}), ("flip_v", {"flip_v": True}), ("both", {"flip_h": True, "flip_v": True}),
             ("cw", {"rotation": cv2.ROTATE_90_CLOCKWISE}), ("r180", {"rotation": cv2.ROTATE_180}),
             ("ccw", {"rotation": cv2.ROTATE_90_COUNTERCLOCKWISE}), ("flip_cw", {"flip_h": True, "rotation": cv2.ROTATE_90_CLOCKWISE}),
             ("flip_v_ccw", {"flip_v": True, "rotation": cv2.ROTATE_90_COUNTERCLOCKWISE})]
    shown = []
    for name, kwargs in views:
        for roll in (-179.0, -90.0, -5.5, 0.0, 3.25, 90.0, 170.0, 180.0):
            shown.append({"view": name, "roll": roll, "shown": cinema.shown_roll(roll, View(**kwargs))})
    runs = []
    rng = np.random.default_rng(7)
    for smooth in (0.3, 0.001, 1.0):
        steps = []
        f = cinema.LevelFilter(smooth_s=smooth)
        t = 0.0
        for i in range(160):
            t += float(rng.choice([1 / 30, 1 / 30, 0.2, 1.5]))
            roll = None if i % 37 == 5 else float(rng.normal(0.0, 3.0)) if i < 100 else float(179.0 + rng.normal(0.0, 4.0))
            if roll is not None and roll > 180:
                roll -= 360
            pitch = None if i == 120 else float(rng.normal(0.0, 4.0)) if i < 60 else float(rng.normal(0.0, 0.5))
            lv = f.update(roll, pitch, t)
            steps.append({"roll": roll, "pitch": pitch, "t": t,
                          "out": None if lv is None else {"roll": lv.roll, "pitch": lv.pitch, "roll_level": lv.roll_level,
                                                          "pitch_level": lv.pitch_level}})
        runs.append({"smooth": smooth, "steps": steps})
    return {"shown_roll": shown, "runs": runs}


class _Host:
    def want_sensors(self, owner, kinds):
        pass

    def add_preview_layer(self, owner, layer):
        pass

    def remove_preview_layer(self, owner):
        pass

    def refresh_preview(self):
        pass

    def schedule_save(self):
        pass

    def sensor_status(self, source_id=None):
        from telescope.sensors import SensorStatus
        return SensorStatus()

    def sensor_reading(self, source_id=None):
        return None

    def focused_source_id(self):
        return "phone"

    def view(self):
        return View()


_APP = []


def configs() -> list:
    from PyQt6.QtWidgets import QApplication
    from telescope.plugin import EventBus
    if not _APP:
        _APP.append(QApplication.instance() or QApplication([]))
    out = []
    cases = [
        {}, {"peaking": True, "scope": "vectorscope", "peaking_colour": "blue", "peaking_level": 9, "zebra_level": 80,
             "scope_size": "large"},
        {"scope": "oscilloscope", "peaking_colour": "plaid", "zebra_level": 3, "peaking_level": "x"}, {"guides": "golden"},
        {"guides": "spiral"}, {"peaking": 1, "zebras": "yes", "false_colour": None, "level": True}, {"level": True, "guides": "4x4"},
        {"peaking_level": 99, "zebra_level": 500}, {"peaking_level": -4, "zebra_level": -4}, {"peaking_level": 7.9, "zebra_level": "88"},
        {"peaking_level": True, "zebra_level": False}, {"scope": "parade", "scope_size": "tiny"}, {"scope": ["histogram"]},
        {"guides": 5, "scope": 3, "peaking_colour": None}, {"zebras": True, "scope": "waveform", "guides": "thirds", "unknown": 1},
        {"peaking_level": 12.0, "zebra_level": 71.5},
    ]
    for cfg in cases:
        plugin = cinema.CinemaPlugin()
        plugin.setup(_Host(), EventBus())
        held = [plugin.create_panel(), plugin.create_settings_sections()]  # the widgets live as long as these do
        try:
            plugin.set_config(cfg)
            out.append({"input": cfg, "output": plugin.get_config()})
        except TypeError:  # an unhashable value (a list) where a name goes: the host then loads the fields that do load
            out.append({"input": cfg, "output": None})
        plugin.shutdown()
    return out


def grid_file() -> bytes:
    grid = scopes._vectorscope_grid(256)
    ys, xs = np.nonzero(grid[..., 3])
    out = bytearray()
    for y, x in zip(ys, xs):
        out += bytes([int(x), int(y), *[int(v) for v in grid[y, x]]])
    return bytes(out)


def main():
    if len(sys.argv) > 2 and sys.argv[1] == "--dump":
        name, which = sys.argv[2], sys.argv[3] if len(sys.argv) > 3 else "small"
        size = next((w, h) for n, w, h in PICTURES if n == which)
        (HERE / f"dump_{name}.bgra").write_bytes(scopes.scope_image(name, picture(*size)).tobytes())
        return
    data = {"pictures": pictures(), "level": level(), "configs": configs(),
            "false_colours": [[upper, None if c is None else list(c), name] for upper, c, name in cinema.FALSE_COLOURS],
            "lut": cinema._FALSE_LUT.reshape(256, 3).tolist(),
            "constants": {"scope_fps": cinema._SCOPE_FPS, "zebra_period": cinema._ZEBRA_PERIOD, "work_width": cinema._WORK_WIDTH,
                          "level_snap": cinema.LEVEL_SNAP, "level_leave": cinema.LEVEL_LEAVE, "pitch_snap": cinema.PITCH_SNAP,
                          "pitch_leave": cinema.PITCH_LEAVE, "smooth_s": cinema._LEVEL_SMOOTH_S, "stale_s": cinema._LEVEL_STALE_S,
                          "unseen_s": cinema._LEVEL_UNSEEN_S},
            "guide_lines": {k: list(v) for k, v in cinema.GUIDE_LINES.items()},
            "sizes": [[k, s] for k, _n, s in cinema.SIZES],
            "peaking_colours": [[k, list(c)] for k, _n, c in cinema.PEAKING_COLOURS]}
    (HERE / "cinema.json").write_text(json.dumps(data))
    (HERE / "scope_grid_256.bin").write_bytes(grid_file())


if __name__ == "__main__":
    main()
