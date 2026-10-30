"""Records what telescope/view.py works out, for view_parity.rs to compare core::view with.

    cd desktop && python ../rust/crates/core/tests/fixtures/make_view_fixtures.py

Writes view.json next to this file: how the zoom is split between the phone and this computer (split_zoom) over the lens and
zoom cases test_transforms.py uses, the points mapped between the picture as shown and the phone's frame, and the lens boxes.
"""
import json
import sys
from pathlib import Path

import cv2

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[4] / "desktop"))

from telescope.view import (  # noqa: E402
    PhoneZoomCaps, inverse_map, lens_boxes, lens_step, pan_for, shown_to_view, split_zoom, view_to_shown, zoom_detent,
)

ROTATIONS = {None: None, 90: cv2.ROTATE_90_CLOCKWISE, 180: cv2.ROTATE_180, 270: cv2.ROTATE_90_COUNTERCLOCKWISE}
CAPS = {
    "none": None,
    "tele": PhoneZoomCaps(ratio_max=10.0, crop_max=4.0, freeform=True, lens_zooms=(3.7,)),
    "centre_only": PhoneZoomCaps(ratio_max=10.0, crop_max=4.0, freeform=False, lens_zooms=(3.7,)),
    "crop_only": PhoneZoomCaps(ratio_max=1.0, crop_max=2.0, freeform=True),
    "one_lens": PhoneZoomCaps(ratio_max=10.0, crop_max=4.0, freeform=True),
    "small": PhoneZoomCaps(1.5, 1.0, False),
}
ZOOMS = [(1.0, (0, 0)), (2.0, (0, 0)), (3.0, (1, 0)), (5.0, (-0.4, 0.8)), (2.5, (1, -1)), (4.5, (0, 0)), (5.0, (0.3, -0.2)),
         (3.8, (1, 0)), (3.8, (0, 0)), (4.5, (0.0526, 0)), (2.0, (0.9, 0.1)), (7.5, (0.2, 0.5))]


def caps_json(caps):
    return None if caps is None else {"ratio_max": caps.ratio_max, "crop_max": caps.crop_max, "freeform": caps.freeform,
                                      "lens_zooms": list(caps.lens_zooms)}


def splits():
    out = []
    for name, caps in CAPS.items():
        for zoom, pan in ZOOMS:
            for current in (1.0, 3.7):
                s = split_zoom(zoom, *pan, caps, current)
                phone = None if s.phone is None else [s.phone.ratio, s.phone.crop, s.phone.x, s.phone.y]
                out.append({"caps": name, "zoom": zoom, "pan": list(pan), "current": current, "phone": phone, "desktop": list(s.desktop)})
    return out


def points():
    out = []
    for rotation in ROTATIONS:
        for flip_h in (False, True):
            for flip_v in (False, True):
                for zoom, pan in ((1.0, (0.0, 0.0)), (2.0, (0.0, 0.0)), (2.5, (0.6, -0.4)), (2.5, (0.4, -0.7))):
                    for stretch in ((1.0, 1.0), (1.5, 1.0), (0.5, 2.0)):
                        for u, v in ((0.1, 0.2), (0.5, 0.5), (0.9, 0.35), (-0.2, 1.3), (0.3, 0.6)):
                            args = (zoom, pan[0], pan[1], flip_h, flip_v, ROTATIONS[rotation], *stretch)
                            out.append({
                                "rotation": rotation, "flip_h": flip_h, "flip_v": flip_v, "zoom": zoom, "pan": list(pan),
                                "stretch": list(stretch), "at": [u, v],
                                "to_view": list(shown_to_view(u, v, *args)),
                                "to_shown": list(view_to_shown(u, v, *args)),
                                "inverse": list(inverse_map(u, v, 1600, 900, *args)),
                            })
    return out


def boxes():
    out = []
    for name in ("none", "tele"):
        for zoom, pan_x, pan_y, rotation in ((1.0, 0.0, 0.0, None), (2.0, 1.0, 0.0, None), (2.0, 1.0, 0.0, 90), (3.0, -0.5, 0.5, 270)):
            found = lens_boxes(CAPS[name], zoom, pan_x, pan_y, rotation=ROTATIONS[rotation])
            out.append({"caps": name, "zoom": zoom, "pan": [pan_x, pan_y], "rotation": rotation,
                        "boxes": [[label, *box] for label, *box in found]})
    return out


data = {
    "caps": {name: caps_json(caps) for name, caps in CAPS.items()},
    "splits": splits(),
    "points": points(),
    "lens_boxes": boxes(),
    "pan_for": [[centre, zoom, pan_for(centre, zoom)] for centre, zoom in ((0.5, 2.0), (0.75, 2.0), (0.9, 2.0), (0.6, 1.0), (0.2, 3.0))],
    "zoom_detent": [[old, new, marks, zoom_detent(old, new, marks)]
                    for old, new, marks in ((290, 320, [300, 700]), (300, 330, [300, 700]), (800, 650, [300, 700]), (700, 640, [300, 700]), (150, 170, [300]))],
    "lens_step": [[lens, lens_step(lens)] for lens in (1.51, 2.0, 3.7, 3.9, 5.0, 6.3)],
    "unknown_size": list(inverse_map(1.2, -0.1, 0, 0, zoom=2.0)),
}
(HERE / "view.json").write_text(json.dumps(data) + "\n")
print(len(data["splits"]), "splits,", len(data["points"]), "points")
