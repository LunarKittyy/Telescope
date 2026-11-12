# python shoot.py PAGE OUTDIR [--fps 30] [--still] [--workers 8] [--prefix f]: frames of a seekable tile page (PNG); --still saves still.png and the desk page's corners.json
import asyncio, argparse, json, os, time
from playwright.async_api import async_playwright
ap = argparse.ArgumentParser(); ap.add_argument("page"); ap.add_argument("out"); ap.add_argument("--fps", type=int, default=30)
ap.add_argument("--still", action="store_true"); ap.add_argument("--workers", type=int, default=8); ap.add_argument("--prefix", default="f")
a = ap.parse_args(); os.makedirs(a.out, exist_ok=True)
url = "file://" + os.path.abspath(a.page)
async def open_page(p):
    b = await p.chromium.launch(executable_path=os.environ.get("CHROMIUM") or None, args=["--force-color-profile=srgb", "--disable-lcd-text"])
    pg = await b.new_page(viewport={"width": 1920, "height": 1080})
    errs = []; pg.on("pageerror", lambda e: errs.append(str(e)))
    await pg.goto(url); await pg.wait_for_function("window.READY === true")
    return b, pg, errs
async def worker(p, frames):
    b, pg, errs = await open_page(p)
    for name, t in frames:
        await pg.evaluate(f"seek({t})")
        await pg.screenshot(path=f"{a.out}/{name}.png")
    await b.close(); return errs
async def main():
    async with async_playwright() as p:
        if a.still:
            b, pg, errs = await open_page(p)
            await pg.evaluate("seek(0)"); await pg.screenshot(path=f"{a.out}/still.png")
            extra = await pg.evaluate("window.CORNERS || null")
            if extra: open(f"{a.out}/corners.json", "w").write(json.dumps(extra))
            print("still", errs); await b.close(); return
        b, pg, _ = await open_page(p); dur = await pg.evaluate("DURATION"); await b.close()
        n = round(dur * a.fps)
        jobs = [[(f"{a.prefix}{f:05d}", f / a.fps) for f in range(k, n, a.workers)] for k in range(a.workers)]
        t0 = time.time(); res = await asyncio.gather(*[worker(p, j) for j in jobs])
        print(a.page, "frames", n, "in", round(time.time() - t0), "s; errors:", [e for r in res for e in r][:5])
asyncio.run(main())
