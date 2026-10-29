"""Records what the Python app does on the same frames, for tests/effects_parity.rs in the app crate to compare the Rust effects with.

    cd desktop && QT_QPA_PLATFORM=offscreen python ../rust/crates/effects/tests/fixtures/make_fixtures.py

Writes next to this file: frame.rgba (the test picture), views.json with one RGBA picture per view (Transforms, as the window's
view crops, flips, turns and stretches), colours.json with one RGBA picture per grade (Colour grading, baked and applied as the
colour pipeline does), configs.json (what get_config answers after set_config, for settings that must carry over) and the LUT
files the grades use. Pictures are raw RGBA, 4 bytes a pixel, sizes in the json.
"""
import json
import sys
from pathlib import Path

import cv2
import numpy as np

HERE = Path(__file__).resolve().parent
DESKTOP = HERE.parents[4] / "desktop"
sys.path.insert(0, str(DESKTOP))

from telescope import colour  # noqa: E402
from telescope.colour import ColourStage, bake, lattice, write_cube  # noqa: E402
from telescope.view import ROTATIONS, apply_stretch, apply_zoom, transform_frame  # noqa: E402

W, H = 96, 64


def picture() -> np.ndarray:
    """A BGR picture with smooth ramps, saturated blocks, thin lines and a little noise, so every part of an effect shows."""
    rng = np.random.default_rng(11)
    x = np.linspace(0, 1, W, dtype=np.float32)[None, :]
    y = np.linspace(0, 1, H, dtype=np.float32)[:, None]
    r = 255 * x + 0 * y
    g = 255 * y + 0 * x
    b = 255 * (1 - x) * (0.5 + 0.5 * y)
    img = np.stack([b, g, r], -1)
    img[8:24, 8:30] = (40, 60, 220)
    img[8:24, 34:56] = (60, 200, 70)
    img[8:24, 60:86] = (220, 90, 40)
    img[40:56, 10:50] = (200, 200, 200)
    img[44:52, 54:90] = (30, 30, 30)
    for i in range(0, W, 7):
        cv2.line(img, (i, 0), (W - 1 - i, H - 1), (255, 255, 255), 1)
    img += rng.normal(0, 4, img.shape)
    return np.clip(img, 0, 255).astype(np.uint8)


def rgba(bgr: np.ndarray) -> bytes:
    out = np.dstack([bgr[..., ::-1], np.full(bgr.shape[:2], 255, np.uint8)])
    return np.ascontiguousarray(out).tobytes()


def views() -> list:
    cases = [
        ("a view that changes nothing", {}),
        ("zoomed in the middle", {"zoom": 2.0}),
        ("zoomed and panned top left", {"zoom": 2.0, "pan_x": -1.0, "pan_y": -1.0}),
        ("zoomed and panned bottom right", {"zoom": 2.0, "pan_x": 1.0, "pan_y": 1.0}),
        ("a fractional zoom", {"zoom": 1.37, "pan_x": 0.3, "pan_y": -0.6}),
        ("zoomed in a lot", {"zoom": 6.0, "pan_x": 0.2, "pan_y": 0.4}),
        ("mirrored", {"flip_h": True}),
        ("upside down", {"flip_v": True}),
        ("mirrored and upside down", {"flip_h": True, "flip_v": True}),
        ("turned a quarter", {"rotation": "90 CW"}),
        ("turned a half", {"rotation": "180"}),
        ("turned three quarters", {"rotation": "90 CCW"}),
        ("squished sideways", {"stretch_x": 0.6}),
        ("stretched sideways", {"stretch_x": 1.6}),
        ("squished up and down", {"stretch_y": 0.7}),
        ("stretched and squished", {"stretch_x": 1.3, "stretch_y": 0.8}),
        ("a quarter turn with a zoom and a flip", {"rotation": "90 CW", "zoom": 2.0, "pan_x": 0.5, "pan_y": -0.5, "flip_h": True}),
        ("three quarters with a stretch", {"rotation": "90 CCW", "stretch_x": 0.7, "stretch_y": 1.2, "flip_v": True}),
        ("everything at once", {"rotation": "180", "zoom": 3.0, "pan_x": -0.7, "pan_y": 0.3, "flip_h": True, "flip_v": True,
                                  "stretch_x": 1.2, "stretch_y": 0.9}),
    ]
    frame = picture()
    out = []
    for i, (name, view) in enumerate(cases):
        rot = ROTATIONS[view.get("rotation", "None")]
        shown = apply_zoom(frame, view.get("zoom", 1.0), view.get("pan_x", 0.0), view.get("pan_y", 0.0))
        shown = transform_frame(shown, view.get("flip_h", False), view.get("flip_v", False), rot)
        shown = apply_stretch(shown, view.get("stretch_x", 1.0), view.get("stretch_y", 1.0))
        file = f"view_{i:02d}.rgba"
        (HERE / file).write_bytes(rgba(shown))
        out.append({"name": name, "view": view, "width": shown.shape[1], "height": shown.shape[0], "file": file})
    return out


LUT = "@fixtures/Look.cube"
INVERT = "@fixtures/Invert.cube"
SHAPER = "@fixtures/Shaper.cube"


def lut_files():
    warm = lattice(17).copy()
    warm = np.stack([warm[:, 0] ** 0.8, warm[:, 1] * 0.9 + warm[:, 2] * 0.1, np.sqrt(warm[:, 2])], 1)
    write_cube(HERE / "Look.cube", warm.reshape(17, 17, 17, 3), "A warm look")
    write_cube(HERE / "Invert.cube", (1 - lattice(5)).reshape(5, 5, 5, 3), "Invert")
    ramp = np.linspace(0, 1, 9)
    rows = "\n".join(f"{v ** 2.2:.6f} {v ** 1.8:.6f} {v:.6f}" for v in ramp)
    (HERE / "Shaper.cube").write_text(f'TITLE "A 1D curve"\nLUT_1D_SIZE 9\n{rows}\n', encoding="utf-8")


def grades() -> list:
    cases = [
        ("neutral", {}),
        ("warmer", {"temperature": 40}),
        ("cooler and greener", {"temperature": -35, "tint": 30}),
        ("contrast", {"contrast": 1.4}),
        ("contrast around another pivot", {"contrast": 0.7, "pivot": 0.6}),
        ("saturation up", {"saturation": 1.6}),
        ("saturation down", {"saturation": 0.3}),
        ("shadows and highlights", {"shadows": 60, "highlights": -40}),
        ("lift gamma gain offset", {"masters": {"lift": 0.08, "gamma": 0.3, "gain": 1.2, "offset": -0.03}}),
        ("a gain that clips", {"masters": {"lift": 0.0, "gamma": 0.0, "gain": 1.6, "offset": 0.0}}),
        ("wheels", {"wheels": {"lift": [0.2, 0.1], "gamma": [-0.3, 0.4], "gain": [0.1, -0.5], "offset": [0.5, 0.0]}}),
        ("a tone curve", {"curves": {"all": [[0, 0], [0.3, 0.2], [0.7, 0.85], [1, 1]]}}),
        ("curves per channel", {"curves": {"r": [[0, 0.1], [0.5, 0.45], [1, 1]], "b": [[0, 0], [0.6, 0.8], [1, 0.9]]}}),
        ("hue vs sat", {"hue_sat": [[0, 0.5], [0.1667, 0.9], [0.3333, 0.1], [0.5, 0.5], [0.6667, 0.5], [0.8333, 0.5]]}),
        ("hue vs hue", {"hue_hue": [[0, 0.58], [0.1667, 0.5], [0.3333, 0.45], [0.5, 0.5], [0.6667, 0.5], [0.8333, 0.5]]}),
        ("a 3D LUT file", {"lut": LUT}),
        ("a 3D LUT at 40% strength", {"lut": INVERT, "lut_strength": 0.4}),
        ("a 1D LUT file", {"lut": SHAPER}),
        ("a grade and a LUT", {"contrast": 1.2, "saturation": 1.2, "temperature": 15, "lut": LUT, "lut_strength": 0.7}),
        ("everything", {"temperature": 20, "tint": -10, "contrast": 1.15, "pivot": 0.5, "saturation": 1.25, "shadows": 25, "highlights": 15,
                         "masters": {"lift": 0.03, "gamma": 0.1, "gain": 1.1, "offset": 0.01},
                         "wheels": {"lift": [0.1, 0.0], "gamma": [0.0, 0.2], "gain": [-0.2, 0.1], "offset": [0.0, 0.0]},
                         "curves": {"all": [[0, 0], [0.5, 0.55], [1, 1]]},
                         "hue_sat": [[0, 0.55], [0.1667, 0.5], [0.3333, 0.5], [0.5, 0.5], [0.6667, 0.5], [0.8333, 0.5]]}),
    ]
    from PyQt6.QtWidgets import QApplication
    app = QApplication.instance() or QApplication([])
    from telescope.plugin import EventBus
    from telescope.plugins.colour_grading import OWNER, ColourGradingPlugin

    class Host:
        def __init__(self):
            self.stages = {}

        def set_colour_stage(self, owner, stage, order=0, source_id=None):
            self.stages[owner] = stage

        def colour_split(self, owner, position):
            pass

        def schedule_save(self):
            pass

    frame = picture()
    out = []
    for i, (name, cfg) in enumerate(cases):
        host, bus = Host(), EventBus()
        plugin = ColourGradingPlugin()
        plugin.setup(host, bus)
        plugin.create_panel()
        real = json.loads(json.dumps(cfg).replace("@fixtures", str(HERE)))
        plugin.set_config(real)
        stage = host.stages.get(OWNER)
        lut = bake([stage]) if stage is not None else None
        shown = lut.apply(frame) if lut is not None else frame
        file = f"colour_{i:02d}.rgba"
        (HERE / file).write_bytes(rgba(shown))
        out.append({"name": name, "config": cfg, "file": file, "kind": lut.kind if lut is not None else "none"})
    return out


def configs() -> list:
    """Settings as saved, and what each plugin makes of them: set_config(input), then get_config()."""
    from PyQt6.QtWidgets import QApplication
    app = QApplication.instance() or QApplication([])
    from telescope.plugin import EventBus
    from telescope.plugins.colour_grading import ColourGradingPlugin
    from telescope.plugins.transforms import TransformsPlugin

    class Host:
        def __init__(self):
            self.vc_view = {}

        def set_colour_stage(self, *a, **k):
            pass

        def colour_split(self, *a):
            pass

        def schedule_save(self):
            pass

        def set_view(self, **changes):
            self.vc_view.update(changes)

        def focused_source_id(self):
            return None

        def is_streaming(self):
            return False

        def is_streaming_from(self, _source_id):
            return False

    out = []

    def run(plugin_cls, name, inputs):
        for cfg in inputs:
            host, bus = Host(), EventBus()
            plugin = plugin_cls()
            plugin.setup(host, bus)
            plugin.create_panel()
            if not isinstance(cfg, dict):
                continue
            plugin.set_config(json.loads(json.dumps(cfg)))
            out.append({"plugin": name, "input": cfg, "output": plugin.get_config()})

    run(ColourGradingPlugin, "colour_grading", [
        {},
        {"temperature": "hot", "contrast": 99, "wheels": {"lift": [5, 5]}, "masters": {"gain": None}},
        {"curves": {"all": [[0.2, 0.2], [1, 1]], "r": "x"}, "hue_sat": [[0, 0.1], "bad", [0.5]], "wheels": [1, 2]},
        {"lut": 7, "lut_strength": "x", "on": "maybe"},
        {"hue_sat": [[0.0, 0.5], [0.5, 0.2], [1.0, 0.5], [0.5, 0.3]]},
        {"on": False, "temperature": 25, "tint": -40.5, "contrast": 1.25, "pivot": 0.5, "saturation": 0.75, "shadows": 10, "highlights": -10},
        {"wheels": {"lift": [0.2, -0.3], "gamma": [0.0, 0.0], "gain": [0.6, 0.9], "offset": [-1, 0]}, "masters": {"lift": 0.123456, "gain": 1.234}},
        {"curves": {"all": [[0, 0], [0.4, 0.5], [1, 1]], "g": [[0, 0.2], [1, 0.9]]}, "hue_hue": [[0.0, 0.6], [0.5, 0.5]]},
        {"curves": {"all": [[0, 0], [0.5, 0.5]]}},
        {"lut_strength": 0.37, "lut": "/nowhere/Missing.cube"},
    ])
    run(TransformsPlugin, "transforms", [
        {},
        {"flip_h": True, "flip_v": True, "rotation": "180", "zoom": 3.0, "pan_x": 0.4, "pan_y": -0.6, "stretch_x": 1.5, "stretch_y": 0.75},
        {"zoom": 1.15}, {"zoom": 2.03}, {"zoom": 2.3}, {"zoom": 4.1},
        {"zoom": 2.0, "pan_x": float("nan"), "pan_y": 7},
        {"zoom": "big"},
        {"stretch_x": "junk", "stretch_y": 9},
        {"zoom": 2.0, "pan_x": 0.75, "pan_y": -0.5},
        {"zoom": 1.0, "pan_x": 0.75, "pan_y": -0.5},
        {"rotation": "90 CW", "flip_h": 1, "zoom": 12.34567},
        {"rotation": "sideways", "stretch_x": 0.333, "stretch_y": 1.999},
        {"zoom": 50, "pan_x": -9, "pan_y": 0.123456},
    ])
    return out


def main():
    (HERE / "frame.rgba").write_bytes(rgba(picture()))
    lut_files()
    manifest = {"width": W, "height": H}
    (HERE / "views.json").write_text(json.dumps({**manifest, "cases": views()}, indent=1, allow_nan=True))
    (HERE / "colours.json").write_text(json.dumps({**manifest, "cases": grades()}, indent=1))
    (HERE / "configs.json").write_text(json.dumps(configs(), indent=1, allow_nan=True))


if __name__ == "__main__":
    main()
