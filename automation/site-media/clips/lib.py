# Shared helpers for the clip scripts: frames through the desktop app's real plugins (telescope-rs effect), easing, and the pointer.
import json, math, os, subprocess, threading
from pathlib import Path
import cv2
import numpy as np
from PIL import Image, ImageDraw, ImageFilter

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
# the built desktop app (cargo build -p telescope-app in rust/), or whatever TELESCOPE_RS points at
BIN = os.environ.get("TELESCOPE_RS", str(ROOT.parent.parent / "rust" / "target" / "debug" / "telescope-rs"))
# everything generated lives here (frames, card pictures, mp4s), outside git
WORK = Path(os.environ.get("SITE_MEDIA_WORK", ROOT / "work"))
SEQ = WORK / "seq"
W, H, FPS = 1920, 1080, 30

clamp = lambda v, a=0.0, b=1.0: min(b, max(a, v))
io5 = lambda t: t * t * t * (t * (6 * t - 15) + 10)
def ramp(t, a, b, e=io5): return e(clamp((t - a) / (b - a)))
def lerp(a, b, k): return a + (b - a) * k

def bgr(path): return cv2.imread(str(path), cv2.IMREAD_COLOR)

def effect(frames, plugin, config=None, lines=None, w=W, h=H):
    """Yields BGR frames (uint8) of `frames` (PNG paths) changed by the real plugin; `lines`: one settings dict per frame."""
    args = [BIN, "effect", plugin, str(w), str(h)]
    WORK.mkdir(parents=True, exist_ok=True)
    if lines is not None:
        p = WORK / f"_lines-{plugin}.jsonl"
        p.write_text("\n".join(json.dumps(l) for l in lines) + "\n")
        args += ["--config-lines", str(p)]
    else:
        args += ["--config", json.dumps(config or {})]
    # ffmpeg turns the PNG files into the raw frames the effect command reads, so any list of paths works
    dec = subprocess.Popen(["ffmpeg", "-v", "error", "-f", "image2pipe", "-vcodec", "png", "-i", "-", "-f", "rawvideo", "-pix_fmt", "rgba", "-"],
                           stdin=subprocess.PIPE, stdout=subprocess.PIPE)
    eff = subprocess.Popen(args, stdin=dec.stdout, stdout=subprocess.PIPE)
    def feed():
        for p in frames:
            dec.stdin.write(Path(p).read_bytes())
        dec.stdin.close()
    threading.Thread(target=feed, daemon=True).start()
    size = w * h * 4
    while True:
        buf = eff.stdout.read(size)
        if len(buf) < size:
            break
        yield cv2.cvtColor(np.frombuffer(buf, np.uint8).reshape(h, w, 4), cv2.COLOR_RGBA2BGR)
    eff.wait()
    if eff.returncode:
        raise RuntimeError(f"telescope-rs effect {plugin} failed ({eff.returncode})")

def save(name, i, img):
    out = WORK / name
    out.mkdir(parents=True, exist_ok=True)
    cv2.imwrite(str(out / f"c{i:05d}.png"), img)

# a pointer like the desktop's, drawn at 4x and scaled down so its edges stay smooth
_PTR = [(2, 2), (2, 25.5), (7.6, 20.4), (11.4, 29.2), (15.3, 27.5), (11.6, 18.9), (19.2, 18.9)]
def _pointer(scale):
    s = 4 * scale
    im = Image.new("RGBA", (int(26 * s), int(36 * s)), (0, 0, 0, 0))
    sh = Image.new("RGBA", im.size, (0, 0, 0, 0))
    ImageDraw.Draw(sh).polygon([(x * s + 2 * s, y * s + 3 * s) for x, y in _PTR], fill=(0, 0, 0, 110))
    sh = sh.filter(ImageFilter.GaussianBlur(3 * s))
    d = ImageDraw.Draw(im)
    pts = [(x * s, y * s) for x, y in _PTR]
    d.polygon(pts, fill=(255, 255, 255, 255))
    d.line(pts + pts[:1], fill=(17, 17, 17, 255), width=int(1.6 * s), joint="curve")
    out = Image.alpha_composite(sh, im)
    return out.resize((im.width // 4, im.height // 4), Image.LANCZOS)
POINTER = _pointer(1.7)
TIP = (2 * 1.7, 2 * 1.7)

def over(img, rgba, x, y, alpha=1.0):
    """Paste a PIL RGBA picture onto a BGR frame with its top left at x, y."""
    if alpha <= 0.002:
        return img
    a = np.asarray(rgba).astype(np.float32)
    h, w = a.shape[:2]
    x0, y0 = int(round(x)), int(round(y))
    X0, Y0, X1, Y1 = max(0, x0), max(0, y0), min(img.shape[1], x0 + w), min(img.shape[0], y0 + h)
    if X1 <= X0 or Y1 <= Y0:
        return img
    piece = a[Y0 - y0:Y1 - y0, X0 - x0:X1 - x0]
    k = piece[..., 3:4] / 255 * alpha
    dst = img[Y0:Y1, X0:X1].astype(np.float32)
    img[Y0:Y1, X0:X1] = (dst * (1 - k) + piece[..., [2, 1, 0]] * k).astype(np.uint8)
    return img

def pointer(img, x, y, alpha=1.0, press=0.0):
    if press > 0:
        r = int(26 * (0.6 + 0.4 * press))
        ring = Image.new("RGBA", (2 * r + 4, 2 * r + 4), (0, 0, 0, 0))
        ImageDraw.Draw(ring).ellipse((2, 2, 2 * r + 2, 2 * r + 2), fill=(205, 196, 250, int(110 * (1 - press) + 30)))
        over(img, ring, x - r - 2, y - r - 2, alpha * math.sin(math.pi * press))
    return over(img, POINTER, x - TIP[0], y - TIP[1], alpha)

def card(path, crop=None):
    im = Image.open(path).convert("RGBA")
    return im.crop(crop) if crop else im
