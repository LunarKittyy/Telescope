#!/usr/bin/env python3
# Records what cv2's drawing does for crates/effects/src/cv_draw_tests.rs: python3 rust/tools/record_cv_draw.py
import pathlib
import random

import cv2
import numpy as np

OUT = pathlib.Path(__file__).parents[1] / "crates/effects/tests/fixtures/cv_draw.txt"


def fnv(data: bytes) -> str:
    h = 0xcbf29ce484222325
    for b in data:
        h = ((h ^ b) * 0x100000001b3) & 0xffffffffffffffff
    return f"{h:016x}"


def main():
    rng = random.Random(7)
    lines = []
    for n in range(600):
        channels = rng.choice((1, 3, 4))
        w, h = rng.choice(((64, 48), (97, 31), (200, 120), (33, 90)))
        back = rng.choice((0, 40, 232, 255))
        colour = [rng.randrange(256) for _ in range(4)]
        shape = (h, w) if channels == 1 else (h, w, channels)
        image = np.full(shape, back, np.uint8)
        cv_colour = colour[0] if channels == 1 else tuple(colour[:channels])
        head = f"{w} {h} {channels} {back} {' '.join(map(str, colour))}"
        if n % 3 == 0:
            a = (rng.randrange(-30, w + 30), rng.randrange(-30, h + 30))
            b = (rng.randrange(-30, w + 30), rng.randrange(-30, h + 30))
            thick = rng.choice((1, 2, 3, 4, 5, 8, 11))
            cv2.line(image, a, b, cv_colour, thick, cv2.LINE_AA)
            lines.append(f"line {head} {a[0]} {a[1]} {b[0]} {b[1]} {thick} {fnv(image.tobytes())}")
        elif n % 3 == 1:
            c = (rng.randrange(-10, w + 10), rng.randrange(-10, h + 10))
            r = rng.choice((1, 2, 3, 5, 9, 12, 16, 30, 60, 115))
            thick = rng.choice((1, 2, 3, 4, 7))
            cv2.circle(image, c, r, cv_colour, thick, cv2.LINE_AA)
            lines.append(f"circle {head} {c[0]} {c[1]} {r} {thick} {fnv(image.tobytes())}")
        else:
            pixels = rng.randrange(4, 40)
            c = rng.choice(" .:-=+*#%@")
            scale = cv2.getFontScaleFromHeight(cv2.FONT_HERSHEY_PLAIN, pixels, 1)
            (tw, _th), _base = cv2.getTextSize(c, cv2.FONT_HERSHEY_PLAIN, scale, 1)
            org = (rng.randrange(-4, w), rng.randrange(0, h + 4))
            cv2.putText(image, c, org, cv2.FONT_HERSHEY_PLAIN, scale, cv_colour, 1, cv2.LINE_AA)
            lines.append(f"text {head} {pixels} {org[0]} {org[1]} {ord(c)} {tw} {fnv(image.tobytes())}")
    OUT.write_text("\n".join(lines) + "\n")


main()
