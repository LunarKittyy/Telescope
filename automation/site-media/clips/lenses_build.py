# Builds scenes/lenses/img from the Camera card windows (ultra-wide, main, tele): the top of each window with the video stage cleared. python lenses_build.py
import os, sys
from PIL import Image, ImageDraw
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from lib import WORK, ROOT

OUT = ROOT / "scenes" / "lenses" / "img"
OUT.mkdir(parents=True, exist_ok=True)
for i, name in enumerate(["l-ultra", "l-main", "l-tele"]):
    im = Image.open(WORK / "lenses-win" / "2x" / f"l-{i:03d}.png").convert("RGB").crop((0, 0, 2600, 1800))
    # the Get set up card sits where the page puts the live picture, so paint the stage with its own background
    ImageDraw.Draw(im).rectangle((870, 162, 1708, 1800), fill=im.getpixel((1600, 190)))
    im.save(OUT / f"{name}.png")
print("built", len(os.listdir(OUT)))
