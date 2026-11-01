# Stress test

Runs the Rust desktop on a recorded phone stream, alone and beside game-like loads, confined to a mid-tier CPU budget, and measures what comes out of the virtual camera. It's here to show whether Telescope holds up with OBS, a browser and a heavy game running, and what frame dropping and shader effects change once they exist.

## Running it

Build first, or let the script do it:

```
cargo build --release -p telescope-app                 # in rust/
cargo build --release                                  # in rust/tools/stress/loads/
```

Set `CARGO_TARGET_DIR` for the script too if you build with one. Then run one of these:

```
python3 stress.py --clip clip.h264 --vision-pack FOLDER --out now.json                     # everything, about 18 minutes
python3 stress.py --clip clip.h264 --vision-pack FOLDER --quick --compare baseline.json    # 6 cells, about 3 minutes
python3 stress.py --clip clip.h264 --cells crt/gpu,crt/idle --seconds 25
```

A cell is `scenario/load`. Scenarios are `plain`, `colour`, `crt`, `vhs`, `desk`, `blur`, and `none` for the load with no Telescope. Loads are `idle`, `gpu`, `cpu`, `obs` and `all`. `--vision-pack` is a downloaded pack's folder, the one named after its hash; without it `blur` does nothing. If an effect costs no more than plain you get a warning, because it probably never switched on.

`--compare old.json` lists what moved in each cell. `--python` runs the Python desktop through `dev_desktop.py --keep`, and `--no-hw-decode` the software decoder. `--real-phone --profile FOLDER` streams from the phone paired in that folder's settings instead of the clip, for a check on the real path. Two of those runs won't match frame for frame. `--keep-logs` keeps each cell's app log, delivery log and raw timestamps.

It needs Linux, the virtual camera with nothing else reading it (`/dev/video11` unless you pass `--device`), `kwin_wayland`, `dbus-run-session` and `ffmpeg` with libx264. A Telescope already using that camera will spoil the run.

## The clip

`record_clip.py` records `/v1/video.h264` from a paired phone over USB while the digital crop sweeps around, which moves the whole picture and leaves the sensor noise in. Look at a frame afterwards, because a lens that sees nothing gives a black clip and nobody tells you.

Keep it out of git. The fake phone serves it at 30 fps over TLS through the same path as a real phone.

## What runs where

The app and the loads run in a nested virtual KWin on a private bus (nothing shows on the real desktop), confined with CPU affinity to 6 threads on 3 physical cores (`--cpus`). The fake phone, the camera reader and this script stay on the other cores because they stand for the phone and the measuring tool, which shouldn't suffer the load they measure.

| Load | Stated level |
|---|---|
| `gpu` | wgpu headless at 2560x1440 in Rgba16Float, 20 texture reads per pixel, 3000 MB of textures, two frames in flight. 100% of the 3D engine, a frame holds the card for about 21 ms. |
| `cpu` | One thread per budget thread at 100% duty, each doing hashing, pointer chasing through 8 MB, float maths and sorting. |
| `obs` | ffmpeg encoding 1080p60 with libx264 veryfast at 6 Mbps, with noise added so each of the 60 frames costs what a new frame costs. |
| `all` | All three together. |

The CPU budget is real. **The GPU has no cap**: limiting it takes root, and this tool doesn't change system settings. What stands in for a slower card is the frame length, since a game holding a mid-tier card at 47 fps blocks the queue about as long as this one does. A real mid-tier card would also have less left for Telescope's own work, which means the GPU cells flatter it.

v4l2loopback 0.15 serves one capture at a time. That's why the camera reader takes the frames and passes them to ffmpeg through a pipe, dropping any ffmpeg hasn't taken. The load is the encoding, and the reader pays for the copy on its own core.

With no Telescope running, `none/obs` encodes the clip instead, which adds a software H.264 decode to the load.

## The table

| Column | Meaning |
|---|---|
| `fps` | Different frames per second at the camera, by a hash of the picture. Resends of the same frame don't count. |
| `p50` to `worst` | Time between different frames in ms, stamped as they come off the camera. |
| `stut` | Intervals over 1.5 times the median. Even dropping holds this near zero at any frame rate. Watch this one first. |
| `lat50`, `lat99` | Ms from a packet being read off the socket to its frame reaching the camera, from `TELESCOPE_STRESS_LOG`. Python has no such log. |
| `cpu%` | Percent of one core, including worker threads that came and went. |
| `3d%`, `dec%` | Telescope's share of the 3D and compute engines and of the video engine, from `/proc/PID/fdinfo`. amdgpu counts decoding under `enc`. |
| `mem` | Peak resident memory in MB. VRAM is in the JSON. |
| `gpuL`, `cpuL`, `obsL` | Each load's own rate, as a share of its rate with no Telescope. On `none` rows, the rate itself. |

A `!` on a row means a warning follows: something else used over 60% of a core, or the GPU was busy with no GPU load. The idle `plain` cell runs three times spread across the matrix, and the spread is printed at the end. A spread wider than `SPREAD_LIMITS` gets a warning, because small differences mean nothing in a run like that.

The JSON also has per-thread CPU, per-engine GPU, jitter, the time spent in the camera step, how evenly packets arrived and how busy the budget was.

## What it can't show

- A person. The clip has none, so Background blur and Desk mode run on an empty mask, and the cost of drawing around a real person isn't in the numbers.
- The window on a real display. The preview draws into a virtual output.
- A real game's mix of graphics, compute and copy work, or a real browser's. These are stand-ins at a fixed level.
- OBS itself. `obs` has the encoder's CPU cost, not OBS's GPU compositing.
- Latency before the socket or after the camera, or frames lost between the phone and the socket.
