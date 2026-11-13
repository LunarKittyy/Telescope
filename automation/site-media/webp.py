# python webp.py [NAME...]: animated WebPs from work/NAME/c%05d.png (30 fps), with no keyframes so still parts of the picture don't shimmer
import os, sys
from pathlib import Path
from PIL import Image
WORK = Path(os.environ.get("SITE_MEDIA_WORK", Path(__file__).resolve().parent / "work"))
QUALITY = {"fun": 90}  # the whole picture moves, which shimmers at 80
SIZE, SRC_FPS, WEBP_FPS = (640, 360), 30, 20
NAMES = ["setup", "pro", "lenses", "framing", "chroma", "desk", "fun", "grading", "one-picture"]
for name in sys.argv[1:] or NAMES:
    folder = WORK / name
    if not folder.is_dir():
        continue
    count = len(list(folder.glob("c*.png")))
    n = round(count * WEBP_FPS / SRC_FPS)
    frames = [Image.open(folder / f"c{min(count - 1, round(i * SRC_FPS / WEBP_FPS)):05d}.png").convert("RGB").resize(SIZE, Image.LANCZOS) for i in range(n)]
    out = folder / "out"
    out.mkdir(exist_ok=True)
    # kmin/kmax past the clip's length: every frame only re-encodes what changed since the last one
    frames[0].save(out / f"{name}-anim.webp", save_all=True, append_images=frames[1:], duration=round(1000 / WEBP_FPS),
                   loop=0, quality=QUALITY.get(name, 80), method=6, kmin=n + 1, kmax=n + 2)
    print(name, n, "frames", os.path.getsize(out / f"{name}-anim.webp") // 1024, "KB", flush=True)
