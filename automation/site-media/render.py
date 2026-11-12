# python render.py PAGE OUTDIR [--stills t1,t2,...] [--fps 30] [--workers 8] [--prefix f]: frames of a seekable page via Chromium (CHROMIUM names a browser to use, else Playwright's own)
import asyncio, sys, os, time, argparse
from playwright.async_api import async_playwright
ap = argparse.ArgumentParser(); ap.add_argument("page"); ap.add_argument("out"); ap.add_argument("--stills"); ap.add_argument("--fps", type=int, default=30); ap.add_argument("--workers", type=int, default=8); ap.add_argument("--prefix", default="f")
a = ap.parse_args(); os.makedirs(a.out, exist_ok=True)
url = "file://" + os.path.abspath(a.page)
async def open_page(p):
    b = await p.chromium.launch(executable_path=os.environ.get("CHROMIUM") or None, args=["--force-color-profile=srgb", "--disable-lcd-text"])
    pg = await b.new_page(viewport={"width": 1920, "height": 1080})
    errs = []; pg.on("pageerror", lambda e: errs.append(str(e))); pg.on("console", lambda m: errs.append(m.text) if m.type == "error" else None)
    await pg.goto(url); await pg.evaluate("window.READY")
    return b, pg, errs
async def worker(p, times):
    b, pg, errs = await open_page(p)
    for name, t in times:
        await pg.evaluate(f"seek({t})")
        await pg.screenshot(path=f"{a.out}/{name}.png")
    await b.close(); return errs
async def main():
    async with async_playwright() as p:
        if a.stills:
            times = [(f"t{float(x):06.2f}", float(x)) for x in a.stills.split(",")]
            print(await worker(p, times)); return
        b, pg, _ = await open_page(p); dur = await pg.evaluate("DURATION"); await b.close()
        n = round(dur * a.fps); print("frames", n, flush=True)
        jobs = [[(f"{a.prefix}{f:05d}", f / a.fps) for f in range(k, n, a.workers)] for k in range(a.workers)]
        t0 = time.time(); res = await asyncio.gather(*[worker(p, j) for j in jobs])
        print("done in", round(time.time() - t0), "s; errors:", [e for r in res for e in r][:5])
asyncio.run(main())
