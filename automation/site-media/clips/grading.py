# Colour grading clip: the card's pucks dragged, the picture graded by the real plugin. python grading.py
import json, math, os, subprocess, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from lib import *

def frames(name): return sorted((SEQ / name).glob("f*.png"))


TEAL, ORANGE, N = (0.40, -0.28), (-0.24, 0.28), 48
CARDS = WORK / "cards-grading"
def render_cards():
    lines = []
    for k in range(2 * N + 1):
        sv = k / N; a, b = min(sv, 1.0), max(0.0, sv - 1.0)
        lines.append({"_props": {"wheel-x": [TEAL[0] * a, 0, ORANGE[0] * b, 0], "wheel-y": [TEAL[1] * a, 0, ORANGE[1] * b, 0]}})
    f = WORK / "_grading-cards.jsonl"; f.write_text("\n".join(json.dumps(l) for l in lines) + "\n")
    subprocess.run([BIN, "scenes", str(CARDS), "--card", "colour_grading", "--config-lines", str(f), "--name", "g", "--scale", "2"], check=True)
def grading():
    if not (CARDS / "2x" / "g-096.png").exists():
        render_cards()
    crop = (32, 32, 926, 1100)
    PW = 1228
    scale = (W - PW - 56) / (crop[2] - crop[0])
    cw, chh = round((crop[2] - crop[0]) * scale), round((crop[3] - crop[1]) * scale)
    cx0, cy0 = PW + 28, (H - chh) // 2
    def to_screen(lx, ly):
        return cx0 + (lx - crop[0]) * scale, cy0 + (ly - crop[1]) * scale
    UNIT = 122.0
    lift_c, gain_c = (267, 486), (267, 884)
    reset = to_screen(756, 88)
    cache = {}
    def card_at(s):
        k = int(round(clamp(s, 0, 2) * N))
        if k not in cache:
            im = card(CARDS / "2x" / f"g-{k:03d}.png", crop).resize((cw, chh), Image.LANCZOS)
            a = np.asarray(im).copy()
            n = 150
            a[-n:, :, 3] = (a[-n:, :, 3] * np.linspace(1, 0, n)[:, None] ** 1.5).astype(np.uint8)
            cache[k] = Image.fromarray(a)
        return cache[k]
    paths = frames("grading")
    def sval(t): return ramp(t, 1.3, 3.1) + ramp(t, 3.8, 5.6)
    lines = []
    for i in range(len(paths)):
        s = sval(i / FPS); a, b = min(s, 1.0), max(0.0, s - 1.0)
        reset_k = ramp(i / FPS, 8.05, 8.4)
        lines.append({"wheels": {"lift": [TEAL[0] * a, TEAL[1] * a], "gain": [ORANGE[0] * b, ORANGE[1] * b]}})
    for i, (p, gfull) in enumerate(zip(paths, effect(paths, "colour_grading", lines=lines))):
        t = i / FPS
        f = bgr(p)
        pic = f[:, (W - PW) // 2 - 80:(W - PW) // 2 - 80 + PW]
        gp = gfull[:, (W - PW) // 2 - 80:(W - PW) // 2 - 80 + PW]
        s = sval(t)
        a, b = min(s, 1.0), max(0.0, s - 1.0)
        reset_k = ramp(t, 8.05, 8.4)
        out = np.empty((H, W, 3), np.uint8); out[:] = (22, 18, 15)
        if reset_k > 0:
            gp = cv2.addWeighted(gp, 1 - reset_k, pic, reset_k, 0)
        out[:, :PW] = gp
        over(out, card_at(s), cx0, cy0)
        if reset_k > 0:
            over(out, card_at(0), cx0, cy0, reset_k)
        lift_puck = to_screen(lift_c[0] + TEAL[0] * a * UNIT, lift_c[1] - TEAL[1] * a * UNIT)
        gain_puck = to_screen(gain_c[0] + ORANGE[0] * b * UNIT, gain_c[1] - ORANGE[1] * b * UNIT)
        pa = ramp(t, 0.5, 0.9) * (1 - ramp(t, 8.8, 9.3))
        if t < 1.3:
            k2 = ramp(t, 0.5, 1.2)
            px, py = lerp(cx0 + cw * 0.5, lift_puck[0], k2), lerp(cy0 + chh * 0.95, lift_puck[1], k2)
        elif t < 3.2:
            px, py = lift_puck
        elif t < 3.8:
            k2 = ramp(t, 3.2, 3.75)
            px, py = lerp(lift_puck[0], gain_puck[0], k2), lerp(lift_puck[1], gain_puck[1], k2)
        elif t < 5.7:
            px, py = gain_puck
        elif t < 7.6:
            k2 = ramp(t, 6.9, 7.8)
            px, py = lerp(gain_puck[0], reset[0], k2), lerp(gain_puck[1], reset[1], k2)
        else:
            px, py = reset
        press = clamp((t - 8.0) / 0.45) if 8.0 <= t < 8.45 else 0.0
        pointer(out, px, py, pa, press)
        save("grading", i, out)
grading()
print("grading done")
