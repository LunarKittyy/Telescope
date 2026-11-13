# Desk mode clip: corners dragged onto the sheet, then the real straightening eased in and out. python desk.py
import json, math, os, subprocess, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from lib import *


# ── 3. Desk mode: corners dragged onto the sheet, then Telescope's own straightening, eased in and out ──
def desk():
    L, N = 10.0, int(10.0 * FPS)
    still = bgr(SEQ / "desk" / "still.png")
    sheet = np.float32(json.loads((SEQ / "desk" / "corners.json").read_text()))
    norm = tuple((float(x) / W, float(y) / H) for x, y in sheet)
    final = next(effect([SEQ / "desk" / "still.png"], "desk", {"corners": [list(p) for p in norm]}))
    white = WORK / "_white.png"; cv2.imwrite(str(white), np.full((H, W, 3), 255, np.uint8))
    mask = next(effect([white], "desk", {"corners": [list(p) for p in norm]})).min(axis=2) > 200
    ys, xs_ = np.where(mask); rx, ry, rw, rh = xs_.min(), ys.min(), xs_.max() - xs_.min() + 1, ys.max() - ys.min() + 1
    print("rect", rx, ry, rw, rh)
    # the corners as the real warp uses them (a hair inside the marks) and where they end up
    src = np.float32([(u * W, v * H) for u, v in norm])
    c = src.mean(axis=0)
    src = (c + (src - c) * 0.985).astype(np.float32)
    dst = np.float32([[rx, ry], [rx + rw, ry], [rx + rw, ry + rh], [rx, ry + rh]])
    start = np.float32([[0.2 * W, 0.2 * H], [0.8 * W, 0.2 * H], [0.8 * W, 0.8 * H], [0.2 * W, 0.8 * H]])
    marks = np.float32(sheet)
    S = 2.0  # the preview's pixels to ours
    drags = [(1.6 + j * 0.72, 1.6 + j * 0.72 + 0.56) for j in range(4)]
    for i in range(N):
        t = i / FPS
        # straightening: in at 4.9..6.3, out at 8.3..9.7
        k = ramp(t, 4.9, 6.3) * (1 - ramp(t, 8.3, 9.7))
        if k <= 0.0005:
            out = still.copy()
        elif k >= 0.9995:
            out = final.copy()
        else:
            d = src + (dst - src) * k
            hm = cv2.getPerspectiveTransform(src, d)
            out = cv2.warpPerspective(still, hm, (W, H), flags=cv2.INTER_LINEAR, borderMode=cv2.BORDER_REPLICATE)
            inside = np.zeros((H, W), np.uint8)
            cv2.fillConvexPoly(inside, np.round(d * 16).astype(np.int32), 255, cv2.LINE_AA, 4)
            fade = ramp(k, 0.0, 0.7)
            back = np.empty_like(out); back[:] = (28, 28, 28)
            m = (inside.astype(np.float32) / 255)[..., None]
            outside = out * (1 - fade) + back * fade
            out = (out * m + outside * (1 - m)).astype(np.uint8)
        # the marking overlay, as the preview draws it
        ov = ramp(t, 1.0, 1.4) * (1 - ramp(t, 4.6, 5.0))
        pts = start.copy()
        for j, (a, b) in enumerate(drags):
            pts[j] = start[j] + (marks[j] - start[j]) * ramp(t, a, b)
        if ov > 0.002:
            layer = out.copy()
            shade = np.full((H, W), 90, np.uint8)
            cv2.fillPoly(shade, [np.round(pts * 16).astype(np.int32)], 0, cv2.LINE_AA, 4)
            a = (shade.astype(np.float32) / 255)[..., None]
            layer = (layer * (1 - a)).astype(np.uint8)
            poly = np.round(pts * 16).astype(np.int32)
            cv2.polylines(layer, [poly], True, (0, 0, 0), int(4 * S), cv2.LINE_AA, 4)
            cv2.polylines(layer, [poly], True, (255, 255, 255), int(2 * S), cv2.LINE_AA, 4)
            for q in pts:
                cc = (int(round(q[0] * 16)), int(round(q[1] * 16)))
                cv2.circle(layer, cc, int(7 * S * 16), (235, 235, 235), -1, cv2.LINE_AA, 4)
                cv2.circle(layer, cc, int(7 * S * 16), (40, 40, 40), int(2 * S), cv2.LINE_AA, 4)
            out = (out * (1 - ov) + layer * ov).astype(np.uint8)
        # the pointer: to each corner, press, drag it onto the sheet
        pa = ramp(t, 1.15, 1.45) * (1 - ramp(t, 4.45, 4.8))
        if pa > 0.002:
            if t < drags[0][0]:
                px, py = start[0] + (np.float32([-120, 90])) * (1 - ramp(t, 1.15, drags[0][0]))
            else:
                px, py = pts[3]
                for j, (a, b) in enumerate(drags):
                    if t < a:
                        prev = marks[j - 1]
                        k2 = ramp(t, drags[j - 1][1] + 0.02, a)
                        px, py = prev + (start[j] - prev) * k2
                        break
                    if t <= b + 0.02:
                        px, py = pts[j]
                        break
            pointer(out, px, py, pa)
        save("desk", i, out)


desk()
print("desk done")
