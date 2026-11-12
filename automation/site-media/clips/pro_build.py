# Builds scenes/pro/ inputs from the Camera card windows: per-state window crops (card/uNNN.png) and states.js with the pointer path. python pro_build.py
import json, math, os, sys
from pathlib import Path
from PIL import Image, ImageDraw
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from lib import WORK, ROOT
from states import ISO, KEL, DIO, seg, io5, clamp
SRC = ROOT / "scenes" / "pro"
FPS, DUR = 60, 10.5
HOLDS = [(1.45, 4.15, "iso"), (4.6, 7.3, "k"), (7.8, 9.85, "d")]
REST = (1230, 560)
TRACK = (925, 1205)
def knobs(s):
    return {"iso": (1097, 262), "k": (TRACK[0] + (s["k"] - 2000) / 8000 * (TRACK[1] - TRACK[0]), 428.5),
            "d": (TRACK[0] + 6 + s["d"] / 10 * (TRACK[1] - TRACK[0] - 12), 542)}
def state(t): return {"iso": math.exp(seg(t, ISO)), "k": seg(t, KEL), "d": seg(t, DIO)}
frames_map = json.load(open(WORK / "pro-map.json"))
out = []
for f in range(round(DUR * FPS)):
    t = f / FPS; s = state(t); kn = knobs(s); pos = None
    for g, r, name in HOLDS:
        if g <= t <= r: pos = kn[name]
    if pos is None:
        prev_t, prev_p, next_t, next_p = 0, REST, DUR, REST
        for g, r, name in HOLDS:
            if r < t: prev_t, prev_p = r, kn[name]
            if g > t: next_t, next_p = g, kn[name]; break
        move_end = next_t - 0.08 if next_t < DUR else min(DUR - 0.6, prev_t + 1.0)
        move_start = max(prev_t + 0.12, move_end - 0.8)
        k = io5((t - move_start) / max(0.01, move_end - move_start))
        bend = 22 * math.sin(math.pi * k)
        dx, dy = next_p[0] - prev_p[0], next_p[1] - prev_p[1]; l = math.hypot(dx, dy) or 1
        pos = (prev_p[0] + dx * k - dy / l * bend, prev_p[1] + dy * k + dx / l * bend)
    press = 0.0
    for g, r, _ in HOLDS:
        press = max(press, clamp((t - g) / 0.08) * (1 - clamp((t - r) / 0.08)))
    out.append({**{k: round(v, 3) for k, v in s.items()}, "cx": round(pos[0], 2), "cy": round(pos[1], 2), "press": round(press, 3), "u": frames_map[f]})
(SRC / "states.js").write_text("window.STATES = " + json.dumps(out) + ";\n")
# the window pictures, cropped to the page's view with the video stage cleared for the live picture
(SRC / "card").mkdir(exist_ok=True)
n = max(frames_map) + 1
for i in range(n):
    im = Image.open(WORK / "pro-win" / "2x" / f"p-{i:03d}.png").convert("RGB").crop((0, 0, 2600, 1800))
    bg = im.getpixel((1600, 190))
    ImageDraw.Draw(im).rectangle((870, 162, 1708, 1800), fill=bg)
    im.crop((872, 96, 872 + 1712, 96 + 1120)).save(SRC / "card" / f"u{i:03d}.png")
print("built", n)
