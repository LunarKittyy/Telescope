"""Records Background and Privacy blur's pictures as SHA-256 in vision_cards.json. Frames and masks come from the same formulas on both sides, in R, G, B."""
import hashlib
import json
import sys
from pathlib import Path

import cv2
import numpy as np

HERE = Path(__file__).resolve().parent
DESKTOP = HERE.parents[4] / "desktop"
sys.path.insert(0, str(DESKTOP))

from telescope import vision  # noqa: E402
from telescope.people import Track  # noqa: E402
from telescope.pictures import soften  # noqa: E402
from telescope.plugin import EventBus  # noqa: E402
from telescope.plugins import background as bg  # noqa: E402
from telescope.plugins import privacy_blur as pb  # noqa: E402
from telescope.view import View  # noqa: E402

BLUR8 = [(96, 54, 0.6), (96, 54, 1.1), (240, 135, 2.0), (106, 60, 3.2), (160, 90, 6.5), (7, 5, 1.8), (3, 1, 1.2)]
BLUR32 = [(240, 135, 1.1), (106, 60, 3.2), (119, 67, 6.5), (53, 30, 0.8), (40, 22, 1.6)]
SOFTEN = [(320, 180, 1), (320, 180, 3), (322, 181, 8), (640, 360, 10)]
WIDE, SMALL = (900, 300), (40, 30)
BACKGROUND = [
    (320, 180, {"mode": "blur", "blur": 1, "edges": 0}),
    (320, 180, {"mode": "blur", "blur": 6, "edges": 3}),
    (320, 180, {"mode": "blur", "blur": 10, "edges": 10}),
    (854, 480, {"mode": "blur", "blur": 4, "edges": 5}),
    (331, 187, {"mode": "blur", "blur": 7, "edges": 2}),
    (320, 180, {"mode": "picture", "edges": 0, "picture": "wide"}),
    (320, 180, {"mode": "picture", "edges": 4, "picture": "small"}),
    (331, 187, {"mode": "picture", "edges": 2, "picture": "wide"}),
]
YOU = {"id": 1, "box": [0.1, 0.2, 0.4, 0.9], "seen": 30.0}
OTHER = {"id": 2, "box": [0.65, 0.3, 0.85, 0.9], "seen": 1.0}
BESIDE = {"id": 3, "box": [0.42, 0.2, 0.6, 0.9], "seen": 1.0}
VIEW = {"zoom": 1.0, "pan_x": 0.0, "pan_y": 0.0, "flip_h": False, "rotation": None, "stretch_x": 1.0}
ZOOMED = dict(VIEW, zoom=2.0, pan_x=0.25, pan_y=-0.5)
TURNED = dict(VIEW, flip_h=True, rotation=None, stretch_x=0.5)
PRIVACY = [
    # (mode, Blur now, a mask, the view, [people each update])
    ("others", False, False, VIEW, [[YOU, OTHER]]),
    ("others", False, True, VIEW, [[YOU, OTHER]]),
    ("background", False, False, VIEW, [[YOU, OTHER]]),
    ("background", False, True, VIEW, [[YOU, OTHER]]),
    ("wait", False, False, VIEW, [[YOU, OTHER]]),
    ("others", False, True, ZOOMED, [[YOU, OTHER]]),
    ("others", False, False, TURNED, [[YOU, OTHER]]),
    ("others", False, True, VIEW, [[YOU, BESIDE]]),
    ("others", False, True, VIEW, [[YOU, OTHER], [OTHER]]),
    ("others", False, False, VIEW, [[YOU, OTHER], [OTHER]]),
    ("background", False, True, VIEW, [[YOU, OTHER], [OTHER]]),
    ("others", True, False, VIEW, []),
    ("others", True, True, VIEW, [[YOU]]),
]
PW, PH = 640, 360


def pattern(w, h):
    """R, G, B."""
    y, x = np.mgrid[0:h, 0:w]
    return np.dstack([(x * 7 + y * 13 + c * 50 + ((x * y) % 31) * 3) % 256 for c in range(3)]).astype(np.uint8)


def soft_mask(w, h):
    x = np.arange(w)
    ramp = np.where(x < w // 4, 0, np.where(x >= w // 2, 255, (x - w // 4) * 255 // max(w // 4, 1)))
    mask = np.tile(ramp, (h, 1))
    mask[: h // 8] //= 2
    return mask.astype(np.uint8)


def sha(array):
    return hashlib.sha256(np.ascontiguousarray(array).tobytes()).hexdigest()


class _Host:
    """What the plugins ask the host for."""

    def __init__(self, mask, view=None):
        self.mask, self.shown_view = mask, view or View()

    def person_mask(self, frame):
        return self.mask

    def vision_state(self):
        return vision.READY

    def schedule_save(self):
        pass

    def want_people(self, owner, on):
        pass

    def tracked_people(self, source_id=None):
        return [], 0

    def view(self):
        return self.shown_view

    def focused_source_id(self):
        return "phone"

    def is_streaming(self):
        return True


def background_cases():
    pictures = {"wide": pattern(*WIDE)[..., ::-1], "small": pattern(*SMALL)[..., ::-1]}
    bg.read_picture = lambda path: pictures.get(path)
    out = []
    for w, h, config in BACKGROUND:
        plugin = bg.BackgroundPlugin()
        plugin.setup(_Host(soft_mask(w, h)), EventBus())
        plugin.create_panel()
        plugin.set_config(config)
        done = plugin.process_frame(pattern(w, h)[..., ::-1])
        out.append({"w": w, "h": h, "config": config, "sha": sha(done[..., ::-1])})
    return out


def privacy_cases():
    clock = [100.0]
    pb.time.monotonic = lambda: clock[0]
    out = []
    for mode, now, masked, view, updates in PRIVACY:
        host, bus = _Host(soft_mask(PW, PH) if masked else None, View(**{k: v for k, v in view.items()})), EventBus()
        plugin = pb.PrivacyBlurPlugin()
        plugin.setup(host, bus)
        plugin.create_panel()
        plugin.set_config({})
        bus.phones_changed.emit(1)
        plugin._switch.setChecked(True)
        plugin._set_mode(mode)
        if now:
            plugin._now_btn.click()
        for update in updates:
            tracks = [Track(t["id"], tuple(t["box"]), (0.5, 0.5), t["seen"], 0.0) for t in update]
            for _ in range(3):
                clock[0] += 1 / 12
                bus.people_tracked.emit("phone", tracks, len(tracks))
        done = plugin.process_frame(pattern(PW, PH)[..., ::-1])
        plugin.shutdown()
        out.append({"mode": mode, "now": now, "mask": masked, "view": view, "updates": updates, "sha": sha(done[..., ::-1])})
    return out


def main():
    from PyQt6.QtWidgets import QApplication
    app = QApplication.instance() or QApplication([])
    recorded = {
        "blur8": [{"w": w, "h": h, "sigma": s, "sha": sha(cv2.GaussianBlur(pattern(w, h), (0, 0), s)),
                   "mask_sha": sha(cv2.GaussianBlur(soft_mask(w, h), (0, 0), s))} for w, h, s in BLUR8],
        "blur32": [{"w": w, "h": h, "sigma": s, "sha": sha(cv2.GaussianBlur(pattern(w, h).astype(np.float32) / 7.0, (0, 0), s))}
                   for w, h, s in BLUR32],
        "soften": [{"w": w, "h": h, "edges": e, "sha": sha(soften(soft_mask(w, h), e))} for w, h, e in SOFTEN],
        "wide": {"w": WIDE[0], "h": WIDE[1]},
        "small": {"w": SMALL[0], "h": SMALL[1]},
        "background": background_cases(),
        "privacy_size": {"w": PW, "h": PH},
        "privacy": privacy_cases(),
    }
    (HERE / "vision_cards.json").write_text(json.dumps(recorded, indent=1) + "\n")
    del app


if __name__ == "__main__":
    main()
