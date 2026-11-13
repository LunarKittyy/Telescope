# Chroma key clip: the real key on the green screen, wiped across to the swapped picture and back. python chroma.py [FRAME_COUNT]
import os, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from lib import *
L = 9.0
frames = sorted((SEQ / "chroma").glob("f*.png"))
if len(sys.argv) > 1:
    frames = frames[:int(sys.argv[1])]
cfg = {"screen": "green", "tolerance": 60, "softness": 30, "spill": 60, "background": "picture", "picture": str(SEQ / "skyline" / "still.png")}
xs = np.arange(W, dtype=np.float32)
for i, (p, keyed) in enumerate(zip(frames, effect(frames, "chroma_key", cfg))):
    t = i / 30
    f = cv2.imread(str(p), cv2.IMREAD_COLOR)
    k = ramp(t, 1.6, 3.6) * (1 - ramp(t, 6.2, 8.2))
    edge = lerp(-80, W + 80, k)
    m = np.clip((edge - xs) / 60 + 0.5, 0, 1)[None, :, None]
    out = (keyed * m + f * (1 - m)).astype(np.uint8)
    if 0.001 < k < 0.999:
        line = np.clip(1 - np.abs(xs - int(edge)) / 2.0, 0, 1)[None, :, None] * 0.55
        out = (out * (1 - line) + 255 * line).astype(np.uint8)
    save("chroma", i, out)
print("chroma done", i + 1)
