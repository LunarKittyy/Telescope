# Site media

The tooling behind the clips on the website and in the README: setup, pro, lenses, framing, chroma, desk, fun, grading and one-picture. None of them are screen recordings. Each one is a small web page that can be seeked to any moment, photographed frame by frame in Chromium, then encoded. The app's own windows and cards come from `telescope-rs scenes`, and the effects (chroma key, desk straightening, colour grading) run through the real plugins with `telescope-rs effect`, so a clip shows what the current build does. The cat is drawn on a canvas in `scenes/common/feed.js`. There's no stock or face footage.

## You need

- The desktop app built in `rust/` (`cargo build -p telescope-app`), or `TELESCOPE_RS=/path/to/telescope-rs`. Run the scenes headless, never as a window on your desktop.
- Python 3 with `playwright`, `Pillow`, `numpy` and `opencv-python`, and a Chromium for Playwright (`playwright install chromium`, or `CHROMIUM=/path/to/chromium`).
- ffmpeg with libx264, libwebp and libaom-av1.

Run everything from this folder. Frames and other generated files go in `work/` (or `SITE_MEDIA_WORK`), which git ignores. Finished clips go to `site/media` under the same names as before: `NAME.mp4`, `NAME.webp`, `NAME-poster.webp`, plus `NAME.avif` for the README.

## Making a clip

Every clip ends the same way. Render the page to `work/NAME/c%05d.png` and encode it:

```
python render.py scenes/PAGE/index.html work/NAME --prefix c
clips/encode.sh NAME CRF WEBPQ POSTERFRAME
```

I used CRF 21 to 26 and WebP quality 50 to 70, depending on how much the picture moves. Look at the poster frame before you pick it. `python webp.py` makes the plain animated WebPs if you want those too.

**setup.** The app windows and the Add phone dialog:

```
../../rust/target/debug/telescope-rs scenes work/setup-win --setup-clip --scale 2
cp work/setup-win/2x/*.png scenes/setup/img/
python render.py scenes/setup/index.html work/setup --prefix c
```

The phone's own screens are the three small pictures in `scenes/setup/img` (`p-home`, `p-paired`, `p-scan-clock`). They're crops of the Android app on an emulator, so redo them if the phone app's home screen changes.

**pro and lenses.** The Camera card with a pretend phone attached, drawn once per state:

```
python clips/states.py pro work/pro-map.json > work/pro-states.jsonl
telescope-rs scenes work/pro-win --card camera_control --window --card-width 1300 --config-lines work/pro-states.jsonl --name p --scale 2
python clips/pro_build.py
python render.py scenes/pro/index.html work/pro --prefix c

python clips/states.py lenses > work/lenses-states.jsonl
telescope-rs scenes work/lenses-win --card camera_control --window --card-width 1300 --config-lines work/lenses-states.jsonl --name l --scale 2
python clips/lenses_build.py
python render.py scenes/lenses/index.html work/lenses --prefix c
```

**chroma, desk, grading.** These take a rendered tile page as their picture and then use the real plugins on it:

```
python shoot.py scenes/tiles/shot_chroma.html work/seq/chroma --prefix f
python shoot.py scenes/tiles/shot_skyline.html work/seq/skyline --still
python clips/chroma.py

python shoot.py scenes/tiles/desk.html work/seq/desk --still
python clips/desk.py

python shoot.py scenes/tiles/shot_grading.html work/seq/grading
python clips/grading.py
```

`chroma.py`, `desk.py` and `grading.py` write `work/NAME/c%05d.png` directly, so skip `render.py` and go to `encode.sh`.

**framing.** Just the page, no effect: `python shoot.py scenes/tiles/shot_framing.html work/framing --prefix c`.

**fun and one-picture.** Not scripted yet. They need the Fun looks and the several-cameras layouts, and `telescope-rs effect` only does chroma key, desk and colour grading. `scenes/tiles/shot_fun.html` and `shot_camA.html` are the pictures underneath them.

**The README banner.** `python banner.py WINDOW.png OUT_DIR PHONE.png` writes `banner.png` and `banner@2x.png` into `OUT_DIR`. `WINDOW.png` is a 1600x900 window picture at 2x from `telescope-rs scenes`, `PHONE.png` a 1080x2400 phone screenshot. The logo and tagline are cut from the current `docs/assets/banner.png`, so write to a different folder first and copy it over once you like it. No window or phone pictures are kept in the repo.

## What's in here

- `scenes/`: the pages (`setup`, `lenses`, `pro`, `tiles`), the shared cat in `common/`, and `fonts/`.
- `clips/`: the scripts that use the app (`lib.py` has the shared bits) and `encode.sh`.
- `render.py`, `shoot.py`: frames of a page, `window.READY`, `seek(t)` and `DURATION` being what a page has to offer.

Generated pictures such as `scenes/pro/card/`, `scenes/lenses/img/` and the app windows in `scenes/setup/img/` aren't committed.

## Fonts and pictures

All fonts are free to redistribute under the SIL Open Font License 1.1 and come from Google Fonts: Bricolage Grotesque, Instrument Sans, Inter, JetBrains Mono and Caveat.

The three phone pictures are crops of Telescope's own Android app with the emulator's clock showing 9:41. Everything else (the cat, the call window, the stars) is drawn in code, and the names in the scenes (Pixel 9 Pro, Studio PC, Sam) are made up.
