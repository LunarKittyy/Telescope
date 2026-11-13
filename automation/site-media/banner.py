# python banner.py WINDOW.png OUT_DIR PHONE.png: the README banner, banner.png and banner@2x.png in OUT_DIR. WINDOW is a 1600x900 window picture at 2x (telescope-rs scenes), PHONE a 1080x2400 phone screenshot. The logo and tagline are cut from docs/assets/banner.png.
import os, sys
from PIL import Image, ImageDraw, ImageFilter, ImageFont
main, outdir = sys.argv[1], sys.argv[2]
REPO = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
old = Image.open(os.path.join(REPO, 'docs', 'assets', 'banner.png')).convert('RGB')
BG = (15, 18, 22)
W, H = 1280, 640
SS = 2
bg = Image.new('RGBA', (W, H), BG + (255,))
ov = Image.new('RGBA', (W * SS, H * SS), (0, 0, 0, 0)); d = ImageDraw.Draw(ov)
cx, cy = 880, 300
for r, a in [(300, 14), (372, 9), (452, 6)]:
    d.ellipse([(cx - r) * SS, (cy - r) * SS, (cx + r) * SS, (cy + r) * SS], outline=(170, 156, 245, a), width=3)
bg = Image.alpha_composite(bg, ov.resize((W, H), Image.LANCZOS)).convert('RGB')
# logo and tagline cut from the old banner, with one stray star painted out and a feathered right edge
old_c = old.copy(); ImageDraw.Draw(old_c).rectangle([180, 287, 192, 297], fill=BG)
box = (0, 196, 520, 450)
crop = old_c.crop(box)
mask = Image.new('L', crop.size, 255); md = ImageDraw.Draw(mask)
for x in range(490, 520):
    md.line([(x, 0), (x, crop.height)], fill=int(255 * (520 - x) / 30))
bg.paste(crop, (box[0], box[1]), mask)
win = Image.open(main).convert('RGB')
ww = 640; wh = round(win.height * ww / win.width)
win = win.resize((ww * 2, wh * 2), Image.LANCZOS)
rad = 20
m = Image.new('L', win.size, 0); ImageDraw.Draw(m).rounded_rectangle([0, 0, win.width - 1, win.height - 1], rad, fill=255)
wx, wy = 508, 128
canvas = bg.convert('RGBA').resize((W * 2, H * 2), Image.LANCZOS)
def shadow(canvas, box, r, off, blur, alpha):
    sh = Image.new('RGBA', canvas.size, (0, 0, 0, 0))
    x0, y0, x1, y1 = box
    ImageDraw.Draw(sh).rounded_rectangle([x0, y0 + off, x1, y1 + off], r, fill=(0, 0, 0, alpha))
    return Image.alpha_composite(canvas, sh.filter(ImageFilter.GaussianBlur(blur)))
canvas = shadow(canvas, (wx*2, wy*2, wx*2 + win.width, wy*2 + win.height), rad, 24, 26, 150)
bd = Image.new('RGBA', canvas.size, (0, 0, 0, 0))
ImageDraw.Draw(bd).rounded_rectangle([wx * 2 - 2, wy * 2 - 2, wx * 2 + win.width + 1, wy * 2 + win.height + 1], rad + 2, fill=(255, 255, 255, 26))
canvas = Image.alpha_composite(canvas, bd)
canvas.paste(win, (wx * 2, wy * 2), m)
# phone: the whole 1080x2400 screen, true 9:20, bezel around it
PH_SCREEN = sys.argv[3]
src = Image.open(PH_SCREEN).convert('RGB')
pw = 144; ph = round(pw * 2400 / 1080)
px, py = 1072, 246
pic = src.resize((pw * 2, ph * 2), Image.LANCZOS)
prad = 30; bez = 5
pm = Image.new('L', pic.size, 0); ImageDraw.Draw(pm).rounded_rectangle([0, 0, pic.width - 1, pic.height - 1], prad, fill=255)
canvas = shadow(canvas, (px*2 - bez*2, py*2 - bez*2, (px + pw)*2 + bez*2, (py + ph)*2 + bez*2), prad + bez*2, 26, 24, 180)
ed = Image.new('RGBA', canvas.size, (0, 0, 0, 0)); de = ImageDraw.Draw(ed)
de.rounded_rectangle([px*2 - bez*2 - 2, py*2 - bez*2 - 2, (px + pw)*2 + bez*2 + 1, (py + ph)*2 + bez*2 + 1], prad + bez*2 + 2, fill=(70, 74, 82, 255))
de.rounded_rectangle([px*2 - bez*2, py*2 - bez*2, (px + pw)*2 + bez*2 - 1, (py + ph)*2 + bez*2 - 1], prad + bez*2, fill=(8, 9, 11, 255))
canvas = Image.alpha_composite(canvas, ed)
canvas.paste(pic, (px * 2, py * 2), pm)
canvas = canvas.convert('RGB')
canvas.resize((W, H), Image.LANCZOS).save(f'{outdir}/banner.png')
canvas.save(f'{outdir}/banner@2x.png')
