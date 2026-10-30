"""Records what telescope/motion.py does on made-up pictures, for motion_parity.rs to compare core::motion with.

    cd desktop && python ../rust/crates/core/tests/fixtures/make_motion_fixtures.py

Writes motion.json next to this file: for each scenario test_motion.py has, which zones moved after every frame and every event it
sent, plus checksums of the small grey copy and of the change picture on two frames. The pictures are the tests' (a grey room with
a little texture and a bright square) with the texture made by a hash of the pixel's place, so Rust can make the same ones.
"""
import json
import sys
from pathlib import Path

import numpy as np

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[4] / "desktop"))

from telescope import motion, vision  # noqa: E402
from telescope.motion import Detector, Motion, grey, moving_zones, thresholds  # noqa: E402

W, H = 640, 360
LEFT, RIGHT = (0.0, 0.0, 0.4, 1.0), (0.6, 0.0, 1.0, 1.0)
NOISE = (((np.arange(H * W * 3, dtype=np.uint64) * np.uint64(2654435761)) >> np.uint64(7)) % np.uint64(7)).astype(np.int64) - 3


def scene(brightness=100, square_x=None, size=60):
    frame = np.clip(brightness + NOISE.reshape(H, W, 3), 0, 255).astype(np.uint8)
    if square_x is not None:
        frame[150:150 + size, square_x:square_x + size] = 230
    return frame


def fnv(data: bytes) -> str:
    h = 0xcbf29ce484222325
    for b in data:
        h = ((h ^ b) * 0x100000001b3) & 0xFFFFFFFFFFFFFFFF
    return f"{h:016x}"


class Run:
    def __init__(self, sensitivity=5):
        self.events, self.trace = [], []
        self.m = Motion(lambda key, owner, zones: self.events.append([key, owner, zones]))
        self.m.watch("trap", [LEFT, RIGHT], sensitivity=sensitivity)

    def feed(self, frame, t):
        here = vision.current_frame.set(("s", 0))
        try:
            self.m.feed(frame, now=t)
        finally:
            vision.current_frame.reset(here)
        self.trace.append(self.m.moving("s", "trap"))

    def result(self):
        return {"events": self.events, "trace": self.trace}


out = {}

r = Run()
t = 0.0
for _ in range(20):
    r.feed(scene(), t)
    t += 0.1
for x in range(400, 600, 20):
    r.feed(scene(square_x=x), t)
    t += 0.1
for _ in range(30):
    r.feed(scene(), t)
    t += 0.1
out["square_in_and_out"] = r.result()

r = Run()
t = 0.0
for i in range(300):
    r.feed(scene(brightness=60 + i * 0.4), t)
    t += 0.1
out["brightness_ramp"] = r.result()

r = Run()
t = 0.0
for _ in range(10):
    r.feed(scene(), t)
    t += 0.1
for _ in range(100):
    r.feed(scene(square_x=500), t)
    t += 0.1
out["left_behind"] = r.result()

r = Run(sensitivity=8)
t = 0.0
wall = scene(brightness=200)
for _ in range(10):
    r.feed(wall, t)
    t += 0.1
coat = wall.copy()
coat[:, :210] = 100
for _ in range(10):
    r.feed(coat, t)
    t += 0.1
out["dark_coat"] = r.result()

r = Run()
r.feed(scene(), 0.0)
r.feed(scene(), 0.1)
r.feed(scene(square_x=500), 30.0)
r.feed(scene(square_x=500), 30.1)
out["after_a_gap"] = r.result()

r = Run()
for i in range(5):
    r.feed(scene(square_x=100), i * 0.1)
r.m.reset(motion.LENS_SETTLE_S, now=0.45)
r.feed(scene(square_x=100), 0.5)
for i in range(6, 30):
    r.feed(scene(square_x=400, size=120), i * 0.1)
out["lens_switch"] = r.result()

r = Run()
for i in range(5):
    r.feed(scene(), i * 0.1)
close = scene()
close[60:, 40:600] = 230
r.feed(close, 0.5)
out["filling_the_picture"] = r.result()

r = Run()
for i in range(5):
    r.feed(scene(brightness=60), i * 0.1)
for i in range(5, 10):
    r.feed(255 - scene(brightness=60), i * 0.1)
out["a_cut"] = r.result()

r = Run()
for i in range(5):
    r.feed(scene(), i * 0.1)
r.feed(scene(square_x=500), 0.5)
r.m.watch("trap", [LEFT, RIGHT])
r.trace.append(r.m.moving("s", "trap"))
r.m.reset(now=0.55)
r.feed(scene(), 0.6)
out["watching_again"] = r.result()

detector = Detector()
first = grey(scene())
detector.change(first, 0.0)
second = grey(scene(square_x=500, size=12))
change = detector.change(second, 0.1)
small = (0.75, 0.38, 0.85, 0.5)
out["detector"] = {
    "grey": fnv(first.tobytes()), "grey_square": fnv(second.tobytes()), "change": fnv(change.tobytes()), "shape": list(change.shape),
    "small_zones": sorted(moving_zones(change, (small, LEFT), *thresholds(5))),
    "right_at_lowest": sorted(moving_zones(change, (RIGHT,), *thresholds(1))),
}
(HERE / "motion.json").write_text(json.dumps(out) + "\n")
print({k: (len(v["events"]) if "events" in v else v) for k, v in out.items() if k != "detector"})
