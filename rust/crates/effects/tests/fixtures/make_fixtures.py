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


def screen_scene(w: int, h: int, screen) -> np.ndarray:
    """A BGR green or blue screen with creases, a skin block, a green-lit edge, a grey block and a little noise."""
    rng = np.random.default_rng(5)
    img = np.empty((h, w, 3), np.float32)
    img[:] = screen
    shade = np.linspace(-14, 10, w, dtype=np.float32)[None, :, None]
    img += shade * np.array([0.4, 1.0, 0.4], np.float32)
    img[h // 8:h // 3, w // 10:w // 4] += np.array([-20, 22, -20], np.float32)
    img[h // 4:h * 7 // 8, w // 3:w * 2 // 3] = (120, 150, 210)
    img[h // 3:h // 2, w * 2 // 3 - 4:w * 2 // 3 + 6] = (110, 170, 140)
    img[h * 2 // 3:h * 5 // 6, w * 3 // 4:w * 7 // 8] = (128, 128, 128)
    img[h // 2:h * 3 // 5, w // 3 + 6:w // 3 + 20] = (40, 40, 220)
    img += rng.normal(0, 3, img.shape)
    img = np.clip(img, 0, 255).astype(np.uint8)
    return cv2.GaussianBlur(img, (3, 3), 0.8)


def backdrops() -> dict:
    """Pictures to put behind the person: one smaller than the frame and one a good deal bigger and of another shape."""
    small = np.zeros((30, 40, 3), np.uint8)
    small[..., 0] = np.linspace(0, 255, 40, dtype=np.uint8)[None, :]
    small[..., 1] = np.linspace(255, 0, 30, dtype=np.uint8)[:, None]
    small[..., 2] = 90
    big = np.zeros((300, 240, 3), np.uint8)
    for i in range(0, 300, 20):
        big[i:i + 10, :, 2] = 200
    for j in range(0, 240, 30):
        big[:, j:j + 15, 0] = 180
    big[100:200, 80:160] = (30, 220, 240)
    out = {}
    for name, img in (("backdrop_small.png", small), ("backdrop_big.png", big)):
        cv2.imwrite(str(HERE / name), img)
        out[name] = cv2.imread(str(HERE / name))
    return out


def chroma() -> dict:
    """The Chroma key's picture for a few scenes and settings: the screen swapped for a colour or a picture."""
    from telescope.plugins.chroma_key import Settings, SCREENS, key, key_tables
    pics = backdrops()
    scenes = {"chroma_green.rgba": screen_scene(96, 64, (60, 180, 50)), "chroma_blue.rgba": screen_scene(95, 63, (190, 80, 20)),
              "chroma_wide.rgba": screen_scene(160, 90, (60, 180, 50))}
    for file, scene in scenes.items():
        (HERE / file).write_bytes(rgba(scene))
    cases = [
        ("the default green key on a colour", "chroma_green.rgba", {"screen": "green"}),
        ("a tight key", "chroma_green.rgba", {"screen": "green", "tolerance": 5, "softness": 0, "spill": 0}),
        ("a loose soft key", "chroma_green.rgba", {"screen": "green", "tolerance": 85, "softness": 90, "spill": 100}),
        ("no spill removal", "chroma_green.rgba", {"screen": "green", "spill": 0}),
        ("a picked colour", "chroma_green.rgba", {"screen": "picked", "picked": [70, 175, 60]}),
        ("a blue screen at an odd size", "chroma_blue.rgba", {"screen": "blue"}),
        ("a pale colour behind", "chroma_green.rgba", {"screen": "green", "colour": [230, 220, 200]}),
        ("a small picture behind", "chroma_green.rgba", {"screen": "green", "background": "picture", "picture": "@fixtures/backdrop_small.png"}),
        ("a big picture behind", "chroma_wide.rgba", {"screen": "green", "background": "picture", "picture": "@fixtures/backdrop_big.png"}),
        ("a picture that can't open", "chroma_green.rgba", {"screen": "green", "background": "picture", "picture": "@fixtures/missing.png"}),
    ]
    out = []
    for i, (name, scene, cfg) in enumerate(cases):
        frame = scenes[scene]
        screen = cfg.get("screen", "green")
        picked = cfg.get("picked")
        colour = next(bgr for k, _t, bgr in SCREENS if k == screen) if screen != "picked" else tuple(picked)
        tables = key_tables(colour, cfg.get("tolerance", 40), cfg.get("softness", 30), cfg.get("spill", 60))
        picture = pics.get(Path(cfg.get("picture", "")).name) if cfg.get("background") == "picture" else None
        settings = Settings(True, tables, cfg.get("background", "colour"), tuple(cfg.get("colour", (40, 34, 30))), picture)
        keyed = key(frame, settings)
        file = f"chroma_{i:02d}.rgba"
        (HERE / file).write_bytes(rgba(keyed))
        out.append({"name": name, "scene": scene, "width": frame.shape[1], "height": frame.shape[0], "config": cfg, "file": file})
    from telescope.plugins.chroma_key import chroma_of, keyable
    colours = [[60, 180, 50], [190, 80, 20], [160, 40, 150], [128, 128, 128], [0, 0, 0], [255, 255, 255], [12, 200, 90], [200, 10, 10], [5, 5, 40], [250, 240, 230]]
    probes = [{"bgr": c, "chroma": list(chroma_of(c)), "keyable": keyable(c)} for c in colours]
    tabs = []
    for key_bgr, t, s, sp in (((60, 180, 50), 40, 30, 60), ((190, 80, 20), 90, 0, 100), ((160, 40, 150), 0, 100, 0), ((70, 175, 60), 55, 45, 30)):
        a, cr, cb = key_tables(key_bgr, t, s, sp)
        tabs.append({"key": list(key_bgr), "tolerance": t, "softness": s, "spill": sp, "alpha": a.tobytes().hex(), "cr": cr.tobytes().hex(), "cb": cb.tobytes().hex()})
    return {"cases": out, "probes": probes, "tables": tabs}


def fun() -> dict:
    """The Fun pack's effects that don't depend on chance: what Python's fun_effects make of a few scenes and settings."""
    from telescope import fun_effects as fx
    from telescope.fun_effects import Clones, Look
    plate = cv2.resize(picture(), (320, 180), interpolation=cv2.INTER_LINEAR)
    person = plate.copy()
    person[40:160, 120:200] = (20, 220, 240)
    clone = plate.copy()
    clone[40:160, 20:80] = (240, 60, 40)
    rect = [20, 40, 80, 160]
    mask = np.zeros((180, 320), np.uint8)
    mask[40:160, 20:80] = 255
    clones = Clones.add(None, clone, mask)
    files = {"fun_plate.rgba": plate, "fun_person.rgba": person, "fun_clone.rgba": clone}
    seq = [np.roll(plate, 9 * i, axis=1) for i in range(6)]
    for i, f in enumerate(seq):
        files[f"fun_seq_{i}.rgba"] = f
    dark = np.full((180, 320, 3), 12, np.uint8)
    lights = []
    for i in range(4):
        f = dark.copy()
        f[60:70, 40 + 30 * i:50 + 30 * i] = 255
        f[100:104, 200:230] = (250, 255, 240)
        lights.append(f)
        files[f"fun_light_{i}.rgba"] = f
    for name, img in files.items():
        (HERE / name).write_bytes(rgba(img))
    lines = (((0.2, 0.2), (0.8, 0.8)), ((0.5, 0.5), (0.3, 0.9)))
    heads = (((0.5, 0.3), 0.1),)
    cases = [
        ("the cloak", Look("cloak", plate=plate, edges=3), [person], ["fun_person.rgba"]),
        ("the cloak with hard edges", Look("cloak", plate=plate, edges=0), [person], ["fun_person.rgba"]),
        ("clones", Look("clones", plate=plate, clones=clones, edges=3), [person], ["fun_person.rgba"]),
        ("slit-scan down", Look("slit", spread=12), seq, [f"fun_seq_{i}.rgba" for i in range(6)]),
        ("slit-scan across", Look("slit", spread=12, across=True), seq, [f"fun_seq_{i}.rgba" for i in range(6)]),
        ("light painting", Look("light", fade=5, brightness=6), lights, [f"fun_light_{i}.rgba" for i in range(4)]),
        ("light painting, long trails", Look("light", fade=10, brightness=1), lights, [f"fun_light_{i}.rgba" for i in range(4)]),
        ("a CRT", Look("crt"), [person], ["fun_person.rgba"]),
        ("ascii", Look("ascii"), [person], ["fun_person.rgba"]),
        ("a stick figure on paper", Look("stick", lines=lines, heads=heads), [person], ["fun_person.rgba"]),
        ("a stick figure on the room", Look("stick", plate=plate, lines=lines, heads=heads), [person], ["fun_person.rgba"]),
        ("game boy", Look("game_boy"), [person], ["fun_person.rgba"]),
    ]
    out = []
    for i, (name, look, frames, names) in enumerate(cases):
        runner = fx.Runner()
        shown = None
        for f in frames:
            shown = runner(f, look) if look.effect != "game_boy" else fx.GameBoy()(f, look)
        file = f"fun_{i:02d}.rgba"
        (HERE / file).write_bytes(rgba(shown))
        out.append({"name": name, "effect": look.effect, "frames": names, "file": file, "edges": look.edges, "fade": look.fade,
                    "brightness": look.brightness, "spread": look.spread, "across": look.across, "plate": look.plate is not None,
                    "clones": look.clones is not None, "lines": look.lines, "heads": look.heads})
    return {"width": 320, "height": 180, "clone_rect": rect, "cases": out}


def configs() -> list:
    """Settings as saved, and what each plugin makes of them: set_config(input), then get_config()."""
    from PyQt6.QtWidgets import QApplication
    app = QApplication.instance() or QApplication([])
    from telescope.plugin import EventBus
    from telescope.plugins.chroma_key import ChromaKeyPlugin
    from telescope.plugins.colour_grading import ColourGradingPlugin
    from telescope.plugins.fun_pack import FunPackPlugin
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

        def packets_problem(self):
            return ""

        def change_packets(self, *a):
            pass

        def want_people(self, *a):
            pass

        def vision_state(self):
            return "off"

        def view(self):
            from telescope.view import View
            return View()

        def view_aspect(self):
            return 16 / 9

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
    run(ChromaKeyPlugin, "chroma_key", [
        {},
        {"on": True, "screen": "picked", "picked": [160, 40, 150], "tolerance": 70, "softness": 10, "spill": 0, "background": "colour", "colour": [1, 2, 3], "picture": ""},
        {"screen": "picked", "picked": [128, 128, 128], "tolerance": 500, "colour": "red", "background": "video"},
        {"screen": "blue", "picked": [1.5, 2, 3], "colour": [1, 2, 300], "on": 1, "softness": "12", "spill": None},
        {"picked": [True, 2, 3], "colour": [-1, 0, 0], "tolerance": -4, "softness": 12.7, "spill": "x", "picture": 7},
        {"on": "yes", "screen": "sepia", "background": "picture", "picture": "/nowhere/pic.png"},
        {"screen": "picked", "picked": [0, 0, 255], "colour": [255, 255, 255, 255], "tolerance": [1]},
    ])
    run(FunPackPlugin, "fun_pack", [
        {},
        {"effect": "slit", "spread": 30, "across": True, "fade": 99},
        {"effect": "nonsense", "edges": "x", "smear": 7, "spread": 1.9, "across": 1},
        {"effect": "clones", "edges": 0, "fade": 1, "brightness": 10, "spread": 45, "smear": 3},
        {"effect": "mosh", "across": False, "edges": -5, "brightness": None, "spread": "12"},
        {"effect": 7, "across": "yes", "fade": [1]},
        {"effect": "game_boy", "edges": 4.7, "smear": 1.2},
    ])
    return out


def main():
    (HERE / "frame.rgba").write_bytes(rgba(picture()))
    lut_files()
    manifest = {"width": W, "height": H}
    (HERE / "views.json").write_text(json.dumps({**manifest, "cases": views()}, indent=1, allow_nan=True))
    (HERE / "colours.json").write_text(json.dumps({**manifest, "cases": grades()}, indent=1))
    (HERE / "configs.json").write_text(json.dumps(configs(), indent=1, allow_nan=True))
    (HERE / "chroma.json").write_text(json.dumps(chroma(), indent=1))
    (HERE / "fun.json").write_text(json.dumps(fun(), indent=1))


if __name__ == "__main__":
    main()
