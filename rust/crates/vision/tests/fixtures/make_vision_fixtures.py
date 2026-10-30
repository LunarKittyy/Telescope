"""What the Python app's vision code makes of fixed frames, for the Rust vision crate's parity tests.

    python make_vision_fixtures.py [--pack <vision pack folder>]

Writes vision.json (hashes of every preprocessing step, which Rust has to match to the byte) and, with --pack, what the real
models find in astronaut.rgb.z (astronaut_mask.u8 and the people in vision.json), which Rust has to match within float tolerance.
astronaut.rgb.z is Buzz Aldrin on the Moon (NASA, public domain) on a grey frame, 384 x 216 RGB, zlib-packed.
"""
import argparse
import hashlib
import json
import sys
import zlib
from pathlib import Path

import cv2
import numpy as np

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE.parents[4] / "desktop"))

from telescope import view, vision  # noqa: E402


def sha(a) -> str:
    return hashlib.sha256(np.ascontiguousarray(a).tobytes()).hexdigest()


def pattern(w: int, h: int) -> np.ndarray:
    """RGB, the same formula the Rust tests use."""
    y, x = np.mgrid[0:h, 0:w].astype(np.int64)
    c = np.arange(3, dtype=np.int64)
    return ((x[..., None] * 7 + y[..., None] * 13 + c * 50 + ((x * y) % 31)[..., None] * 3) % 256).astype(np.uint8)


def noisy(rgb: np.ndarray) -> np.ndarray:
    h, w = rgb.shape[:2]
    y, x = np.mgrid[0:h, 0:w].astype(np.int64)
    c = np.arange(3, dtype=np.int64)
    noise = (x[..., None] * 31 + y[..., None] * 17 + c * 7) % 31 - 15
    return np.clip(rgb.astype(np.int64) + noise, 0, 255).astype(np.uint8)


def astronaut() -> np.ndarray:
    return np.frombuffer(zlib.decompress((HERE / "astronaut.rgb.z").read_bytes()), np.uint8).reshape(216, 384, 3)


def bgr(rgb):
    return np.ascontiguousarray(rgb[..., ::-1])


def rgb_of(b):
    return np.ascontiguousarray(b[..., ::-1])


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--pack", type=Path)
    args = ap.parse_args()
    if not (HERE / "astronaut.rgb.z").exists():
        photo = cv2.imread("/usr/share/plasma/avatars/photos/Astronaut.png")
        frame = np.full((216, 384, 3), 40, np.uint8)
        frame[:, 84:300] = cv2.resize(photo, (216, 216), interpolation=cv2.INTER_AREA)
        (HERE / "astronaut.rgb.z").write_bytes(zlib.compress(rgb_of(frame).tobytes(), 9))
    out = {}
    frames = {f"{w}x{h}": pattern(w, h) for w, h in [(1280, 720), (1920, 1080), (720, 1280), (160, 100)]}
    frames["astronaut"] = astronaut()

    out["model_input"] = {}
    for name, rgb in frames.items():
        small = vision.model_input(bgr(rgb))
        out["model_input"][name] = [small.shape[1], small.shape[0], sha(rgb_of(small))]

    motion = vision._Motion()
    base = frames["1280x720"]
    moved = base.copy()
    moved[300:420, 600:720] = 255
    seq = [motion.moved(bgr(base))]
    out["thumb"] = sha(motion._now)
    motion.used()
    seq += [motion.moved(bgr(noisy(base))), motion.moved(bgr(moved))]
    out["moved"] = [bool(s) for s in seq]
    m2 = vision._Motion()
    m2.moved(bgr(frames["astronaut"]))
    out["thumb_astronaut"] = sha(m2._now)

    out["people_small"] = {}
    for name in ("1920x1080", "1280x720", "720x1280"):
        b = bgr(frames[name])
        h, w = b.shape[:2]
        width = min(vision._PEOPLE_WIDTH, w)
        small = cv2.resize(b, (width, max(1, round(h * width / w))), interpolation=cv2.INTER_AREA)
        out["people_small"][name] = [small.shape[1], small.shape[0], sha(rgb_of(small))]

    i = np.arange(65536, dtype=np.int64)
    a, b = ((i * 37 + 11) % 256).astype(np.uint8), ((i * 101 + 7) % 256).astype(np.uint8)
    out["add_weighted"] = {}
    for carry in (vision._MASK_CARRY, vision._MASK_CARRY_STILL):
        mixed = cv2.addWeighted(a.reshape(256, 256), 1.0 - carry, b.reshape(256, 256), carry, 0)
        out["add_weighted"][str(carry)] = sha(mixed)
    k = np.arange(256, dtype=np.float64)
    f = np.concatenate([(i / 65535.0), k / 255.0, (k + 0.5) / 255.0]).astype(np.float32)
    out["convert_scale_abs"] = sha(cv2.convertScaleAbs(f.reshape(1, -1), alpha=255.0))

    yy, xx = np.mgrid[0:144, 0:256]
    mask = ((xx * 3 + yy * 5) % 256).astype(np.uint8)
    out["mask_up"] = {f"{w}x{h}": sha(cv2.resize(mask, (w, h), interpolation=cv2.INTER_LINEAR))
                      for w, h in [(1280, 720), (384, 216), (720, 1280)]}

    out["turned"] = {}
    for fh, fv, rot in [(False, False, None), (True, False, None), (False, True, cv2.ROTATE_90_CLOCKWISE),
                        (True, True, cv2.ROTATE_90_COUNTERCLOCKWISE), (False, False, cv2.ROTATE_180)]:
        turned = view.transform_frame(bgr(frames["astronaut"]), fh, fv, rot)
        deg = {None: 0, cv2.ROTATE_90_CLOCKWISE: 90, cv2.ROTATE_180: 180, cv2.ROTATE_90_COUNTERCLOCKWISE: 270}[rot]
        out["turned"][f"{int(fh)}{int(fv)}{deg}"] = sha(rgb_of(turned))

    if args.pack:
        backend = vision.MediaPipeBackend(args.pack)
        segment, close = backend.segmenter()
        m = segment(vision.model_input(bgr(frames["astronaut"])), 1)
        close()
        (HERE / "astronaut_mask.u8").write_bytes(m.tobytes())
        out["mask_shape"] = [m.shape[1], m.shape[0]]
        detect, close = backend.pose()
        people = detect(bgr(frames["astronaut"]), 1)
        close()
        out["people"] = [{n: list(p) for n, p in sorted(person.points.items())} for person in people]
        out["mediapipe"] = backend.version
    elif (HERE / "vision.json").exists():
        old = json.loads((HERE / "vision.json").read_text())
        out.update({k: old[k] for k in ("mask_shape", "people", "mediapipe") if k in old})
    (HERE / "vision.json").write_text(json.dumps(out, indent=1, sort_keys=True) + "\n")


if __name__ == "__main__":
    main()
