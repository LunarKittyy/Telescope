#!/usr/bin/env python3
"""What each plugin and host service costs per frame, on synthetic frames through the real frame path.

Builds the window offscreen with every built-in plugin, as the app does, then times each frame step the stream worker
would run (the view, the plugins in registration order, the colour pipeline), the worker around them, the preview's
GUI side and Several cameras in one picture. The vision model is a stand-in that costs nothing, so Background and the
other vision users show only their own work; the model's real cost is in Copy diagnostics.

Usage: python scripts/bench_frames.py [--frames 60] [--size 1920x1080] [--only fun_pack] [--json out.json] [--seconds 3] [--profile]
"""

import argparse
import json
import os
import statistics
import sys
import tempfile
import time
import tracemalloc
from pathlib import Path

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")
_HOME = tempfile.mkdtemp(prefix="telescope-bench-")  # never the real settings
os.environ["XDG_CONFIG_HOME"] = os.path.join(_HOME, "xdg")
os.environ["APPDATA"] = os.path.join(_HOME, "appdata")
sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import cv2  # noqa: E402
import numpy as np  # noqa: E402

BUDGETS = (("30 fps", 1000 / 30), ("60 fps", 1000 / 60))


# ── Synthetic input ────────────────────────────────────────────────────────

def frames(w: int, h: int, n: int) -> list:
    """n moving frames: a gradient room with a person-shaped blob that walks, plus some texture for edges."""
    rng = np.random.default_rng(0)
    yy, xx = np.mgrid[0:h, 0:w].astype(np.float32)
    base = np.dstack([(xx / w * 200 + 30), (yy / h * 180 + 40), ((xx + yy) / (w + h) * 160 + 50)]).astype(np.uint8)
    noise = rng.integers(0, 24, (h, w, 3), np.uint8)
    base = cv2.add(base, noise)
    out = []
    for i in range(n):
        f = base.copy()
        cx = int(w * (0.35 + 0.3 * (i % 30) / 30))
        cv2.ellipse(f, (cx, int(h * 0.62)), (w // 9, h // 3), 0, 0, 360, (60, 90, 170), -1)
        cv2.circle(f, (cx, int(h * 0.28)), h // 9, (80, 120, 200), -1)
        cv2.putText(f, f"frame {i}", (40, 80), cv2.FONT_HERSHEY_SIMPLEX, 2, (255, 255, 255), 3)
        out.append(f)
    return out


class FakeBackend:
    """Stands in for MediaPipe: an ellipse for the person mask, one person standing in the middle."""
    version = "bench"

    def segmenter(self):
        def segment(frame, _ts):
            h, w = frame.shape[:2]
            mask = np.zeros((h, w), np.uint8)
            cv2.ellipse(mask, (w // 2, int(h * 0.6)), (w // 8, h // 3), 0, 0, 360, 255, -1)
            return mask
        return segment, lambda: None

    def pose(self):
        from telescope.vision import Person

        def detect(_frame, _ts):
            pts = {"nose": (0.5, 0.3, 0.99), "left_shoulder": (0.42, 0.45, 0.99), "right_shoulder": (0.58, 0.45, 0.99),
                   "left_hip": (0.45, 0.75, 0.9), "right_hip": (0.55, 0.75, 0.9)}
            return [Person(pts)]
        return detect, lambda: None


# ── Building the window ────────────────────────────────────────────────────

class _Held:
    def __init__(self, *_a, **_k): pass
    def show(self, *_a): pass
    def stop(self): pass
    def start(self): pass
    def set_watched(self, _w): pass
    def lookup(self, _pid): return []


class _MicBackend:
    pick_name = "Bench mic"
    def problem(self): return None
    def prepare(self): return ""
    def teardown(self): pass


def build_window():
    from PyQt6.QtWidgets import QApplication

    import telescope.app as app_module
    import telescope.plugins.connection as connection_module
    import telescope.plugins.setup as setup_plugin
    import telescope.vcam as vcam
    from telescope.on_air import OnAirServer
    from telescope.plugins import builtin_plugins
    from telescope.plugins.onboarding import OnboardingPlugin
    from telescope.plugins.updates import UpdatesPlugin
    from telescope.theme import apply_theme

    app = QApplication.instance() or QApplication([])
    apply_theme(app)
    vcam.locked_size = lambda *_: None
    OnAirServer.__init__.__defaults__ = (0, "127.0.0.1")
    app_module.QSystemTrayIcon.isSystemTrayAvailable = staticmethod(lambda: False)
    connection_module.LanDiscovery = lambda *a, **k: _Held()
    connection_module.stop_adb_server = lambda: None
    setup_plugin.uc_registered_name = lambda: None
    setup_plugin.uc_in_app_folder = lambda: False
    connection_module.ConnectionPlugin._spawn_resolve = lambda self, *a: None
    connection_module.ConnectionPlugin.ensure_virtual_camera = lambda self, interactive=True: True
    OnboardingPlugin.check_virtual_camera = lambda self: None
    UpdatesPlugin._maybe_auto_check = lambda self: None

    win = app_module.TelescopeWindow()
    for plugin in builtin_plugins():
        if plugin.name == "wait_screen":
            plugin._screen, plugin._watch_cls, plugin._extra_screen = _Held(), _Held, _Held
        elif plugin.name == "microphone":
            plugin._backend = _MicBackend()
        elif plugin.name == "shortcuts":
            plugin._backend_factory = lambda: None
        win.register_plugin(plugin)
    win.apply_saved_config()
    app.processEvents()
    win._vision._find, win._vision._there = FakeBackend, lambda: True
    win._vision.load()
    for _ in range(100):
        if win._vision.state() == "ready":
            break
        time.sleep(0.01)
    win._colour._inline = True  # bake on this thread, so the first frame already has its LUT
    return app, win


# ── Timing ─────────────────────────────────────────────────────────────────

class Clock:
    def __init__(self, n: int, profile: bool = False):
        self.n = n
        self.seq = 0
        self.profile = profile

    def run(self, step, inputs: list, key) -> dict:
        """Median and 90th percentile ms of step over inputs, and the peak memory it allocated beyond its input."""
        from telescope import vision
        times = []
        for i in range(self.n + 5):
            frame = inputs[i % len(inputs)]
            self.seq += 1
            token = vision.current_frame.set((key, self.seq))
            try:
                t0 = time.perf_counter()
                step(frame)
                dt = time.perf_counter() - t0
            finally:
                vision.current_frame.reset(token)
            if i >= 5:  # the first few warm caches up
                times.append(dt * 1000)
        frame = inputs[0]
        self.seq += 1
        token = vision.current_frame.set((key, self.seq))
        tracemalloc.start()
        try:
            base = tracemalloc.get_traced_memory()[0]
            step(frame)
            peak = tracemalloc.get_traced_memory()[1] - base
        finally:
            tracemalloc.stop()
            vision.current_frame.reset(token)
        times.sort()
        return {"ms": statistics.median(times), "p90": times[int(len(times) * 0.9) - 1],
                "calls": self._profile(step, inputs, key) if self.profile else [],
                "copies": peak / inputs[0].nbytes if isinstance(inputs[0], np.ndarray) else 0.0}


    def _profile(self, step, inputs: list, key) -> list:
        """The costliest calls inside step, as lines to print."""
        import cProfile
        import pstats
        from telescope import vision
        prof = cProfile.Profile()
        for i in range(self.n):
            self.seq += 1
            token = vision.current_frame.set((key, self.seq))
            try:
                prof.runcall(step, inputs[i % len(inputs)])
            finally:
                vision.current_frame.reset(token)
        stats = pstats.Stats(prof).stats
        top = sorted(stats.items(), key=lambda kv: -kv[1][2])[:6]
        lines = []
        for (path, line, fn), (_cc, calls, tottime, _cum, _callers) in top:
            if tottime / self.n * 1000 >= 0.05:
                where = f"{Path(path).name}:{line}" if line else ""
                lines.append(f"{tottime / self.n * 1000:6.2f} ms  {calls / self.n:4.1f}x  {fn} {where}")
        return lines


def fmt(row: dict) -> str:
    shares = "  ".join(f"{row['ms'] / b * 100:5.1f}%" for _n, b in BUDGETS)
    return f"{row['ms']:7.2f} ms  p90 {row['p90']:6.2f}  {shares}  {row['copies']:4.1f} frames alloc"


def report(name: str, row: dict, results: dict):
    results[name] = row
    print(f"  {name:<44} {fmt(row)}")
    for line in row.get("calls", ()):
        print(f"      {line}")


def header(title: str):
    print(f"\n{title}")
    print(f"  {'':<44} {'median':>10}  {'':>10}  " + "  ".join(f"{n:>6}" for n, _b in BUDGETS) + "  memory")


# ── The scenarios ──────────────────────────────────────────────────────────

def heavy_settings(win, tmp: Path, w: int, h: int) -> dict:
    """{label: (plugin name, how to switch it to that setting)}; each is undone with set_config(defaults)."""
    picture = tmp / "behind.png"
    cv2.imwrite(str(picture), cv2.GaussianBlur(np.random.default_rng(1).integers(0, 255, (1080, 1920, 3), np.uint8),
                                               (0, 0), 8))
    from telescope.plugins.privacy_blur import _Blur

    def blur(mode, away=False):
        def go(p):
            p._blur = _Blur(mode, True, (0.4, 0.15, 0.6, 0.95), ((0.05, 0.2, 0.25, 0.95), (0.75, 0.2, 0.95, 0.95)),
                            False, away)
        return go

    def cfg(c):
        return lambda p: p.set_config(c)

    out = {
        "transforms: zoom 2, rotate 90, stretch": ("transforms", cfg({"zoom": 2.0, "rotation": "90°", "stretch_x": 1.3})),
        "background: blur 6 (default strength)": ("background", cfg({"mode": "blur"})),
        "background: blur 10, edges 10": ("background", cfg({"mode": "blur", "blur": 10, "edges": 10})),
        "background: picture": ("background", cfg({"mode": "picture", "picture": str(picture)})),
        "chroma key: on, colour": ("chroma_key", cfg({"on": True})),
        "chroma key: on, picture": ("chroma_key", cfg({"on": True, "background": "picture", "picture": str(picture)})),
        "privacy blur: all but you": ("privacy_blur", blur("all")),
        "privacy blur: others": ("privacy_blur", blur("others")),
        "privacy blur: others, you away": ("privacy_blur", blur("others", away=True)),
        "colour grading: sliders (per channel)": ("colour_grading", cfg({"on": True, "contrast": 1.2})),
        "colour grading: full grade (3D LUT)": ("colour_grading", cfg({"on": True, "temperature": 20, "saturation": 1.3,
                                                                         "wheels": {"lift": [0.1, 0.05]}})),
        "desk: on": ("desk", cfg({"on": True, "corners": [[0.2, 0.2], [0.8, 0.25], [0.85, 0.85], [0.15, 0.8]]})),
        "desk: on, whiten": ("desk", cfg({"on": True, "corners": [[0.2, 0.2], [0.8, 0.25], [0.85, 0.85], [0.15, 0.8]],
                                          "cleanup": "whiten"})),
        "desk: on, black and white": ("desk", cfg({"on": True, "corners": [[0.2, 0.2], [0.8, 0.25], [0.85, 0.85],
                                                                           [0.15, 0.8]], "cleanup": "bw"})),
        "name tag: on": ("name_tag", cfg({"on": True, "name": "Sam Example", "pronouns": "they/them",
                                          "second": "Telescope"})),
        "qr reader: on": ("qr_reader", cfg({"on": True})),
    }
    from PyQt6.QtWidgets import QApplication
    from telescope import vision
    from telescope.plugins.fun_pack import EFFECTS
    room = frames(w, h, 16)

    def taken(effect, grabs):
        def go(p):
            p.set_config({"effect": effect})
            for i, what in enumerate(grabs):
                p._grab = what
                token = vision.current_frame.set(("taking", i))
                try:
                    p.process_frame(room[i * 5])
                finally:
                    vision.current_frame.reset(token)
                QApplication.processEvents()
        return go
    for effect, _label in EFFECTS[1:]:
        if effect != "clones":  # nothing to show before one is taken
            out[f"fun pack: {effect}"] = ("fun_pack", cfg({"effect": effect}))
    out["fun pack: cloak"] = ("fun_pack", taken("cloak", ["plate"]))
    out["fun pack: clones, three taken"] = ("fun_pack", taken("clones", ["plate", "clone", "clone", "clone"]))
    out["fun pack: stick"] = ("fun_pack", taken("stick", ["plate"]))  # without people found, only the copy
    return out


def run(args):
    from telescope.plugin import TelescopePlugin
    from telescope.stream import StreamWorker
    w, h = (int(v) for v in args.size.split("x"))
    app, win = build_window()
    cv2.setNumThreads(2)  # as main.py runs it
    clock = Clock(args.frames, args.profile)
    inputs = frames(w, h, 30)
    results = {"size": [w, h], "cpus": os.cpu_count()}
    key = StreamWorker("bench://cam", None, None, 30)  # what vision and the colour pipeline tell streams apart by
    plugins = {p.name: p for p in win._plugins}
    steps = [("view", win._view)] + [(p.name, p) for p in sorted(win._plugins, key=lambda p: p.sees_final_frame)
                                      if type(p).process_frame is not TelescopePlugin.process_frame]
    steps.append(("colour", win._colour))

    header(f"Idle: every frame step at its default settings ({w}x{h})")
    for name, p in steps:
        if not args.only or args.only in name:
            report(f"{name} (idle)", clock.run(p.process_frame, inputs, key), results)

    if not args.only:
        header("Host services on their own")
        win._vision.want_people("bench", True)
        report("vision: person_mask (model not counted)", clock.run(win._vision.person_mask, inputs, key), results)
        report("view: feeding people tracking", clock.run(win._view.process_frame, inputs, key), results)
        win._vision.want_people("bench", False)

    header("Each plugin at a heavy setting (the others idle)")
    for label, (name, switch) in heavy_settings(win, Path(_HOME), w, h).items():
        p = plugins.get(name)
        if p is None or (args.only and args.only not in label and args.only not in name):
            continue
        defaults = p.get_config()
        switch(p)
        app.processEvents()
        win._colour.settle()
        # Transforms and Colour grading only hand their settings to the view and the colour pipeline
        step = {"transforms": win._view, "colour_grading": win._colour}.get(name, p).process_frame
        try:
            report(label, clock.run(step, inputs, key), results)
        finally:
            p.set_config(defaults)
            if name == "privacy_blur":
                from telescope.plugins.privacy_blur import _Blur
                p._blur = _Blur()
            app.processEvents()

    if not args.only:
        session_section(win, app, clock, inputs, results, plugins, args.seconds)
        worker_section(win, clock, inputs, key, results, w, h)
        preview_section(win, app, clock, inputs, key, results, plugins)
        composite_section(clock, inputs, results, w, h)
        decode_section(clock, inputs, results, w, h)

    if args.json:
        Path(args.json).write_text(json.dumps(results, indent=1))
    win._shut_down()
    return results


SESSION = {
    "transforms": {"zoom": 1.5},
    "background": {"mode": "blur"},
    "colour_grading": {"on": True, "temperature": 20, "saturation": 1.3, "wheels": {"lift": [0.1, 0.05]}},
    "name_tag": {"on": True, "name": "Sam Example"},
    "cinema": {"peaking": True, "zebras": True, "false_colour": True, "scope": "waveform"},
}


def session_section(win, app, clock, inputs, results, plugins, seconds: float):
    """Several plugins on at once through the worker, then the same on a thread while the GUI thread runs a 5 ms timer:
    how late it fires shows whether frame work holds the GUI up (the GIL, or work done on that thread)."""
    import threading
    from PyQt6.QtCore import QTimer
    from telescope.stream import StreamWorker
    header("A session: " + ", ".join(SESSION) + ", with the preview showing")
    saved = {}
    for name, cfg in SESSION.items():
        if name in plugins:
            saved[name] = plugins[name].get_config()
            plugins[name].set_config(cfg)
    pv = plugins.get("preview")
    if pv is not None:
        pv._streaming = True
    is_camera_on, win.is_camera_on = win.is_camera_on, lambda: True
    app.processEvents()
    win._colour.settle()
    try:
        worker = StreamWorker("bench://cam", None, None, 30, frame_pipeline=win._flow.pipeline(live=True))
        seq = iter(range(1, 10**9))

        def publish(f):
            worker._publish(next(seq), f)
            app.processEvents()  # the preview's frame reaches the GUI thread
        report("worker._publish and the preview's GUI side", clock.run(publish, inputs, worker), results)

        for fps in (30, 60):
            stop, late, last = threading.Event(), [], [time.perf_counter()]

            def pump():
                due = time.perf_counter()
                while not stop.is_set():
                    worker._publish(next(seq), inputs[next(seq) % len(inputs)])
                    due += 1 / fps
                    time.sleep(max(0.0, due - time.perf_counter()))

            def tick():
                now = time.perf_counter()
                late.append((now - last[0]) * 1000 - 5)
                last[0] = now
            timer = QTimer()
            timer.setInterval(5)
            timer.timeout.connect(tick)
            timer.start()
            thread = threading.Thread(target=pump, daemon=True)
            thread.start()
            end = time.perf_counter() + seconds
            while time.perf_counter() < end:
                app.processEvents()
                time.sleep(0.001)
            stop.set()
            thread.join()
            timer.stop()
            late.sort()
            row = {"median": late[len(late) // 2], "p99": late[int(len(late) * 0.99)], "max": late[-1]}
            results[f"GUI timer lateness at {fps} fps"] = row
            print(f"  GUI 5 ms timer late, frames at {fps} fps:    median {row['median']:.2f} ms  p99 {row['p99']:.2f}"
                  f"  max {row['max']:.1f}")
    finally:
        win.is_camera_on = is_camera_on
        for name, cfg in saved.items():
            plugins[name].set_config(cfg)
        app.processEvents()


def worker_section(win, clock, inputs, key, results, w, h):
    from telescope.stream import StreamWorker, _vcam_format
    from telescope import vcam
    header("The stream worker: resize, every step, publish; then the virtual camera side")
    for label, steps in (("idle pipeline", win._flow.pipeline(live=True)), ("no plugins", [])):
        worker = StreamWorker("bench://cam", None, None, 30, frame_pipeline=steps)
        seq = iter(range(1, 10**9))
        report(f"worker._publish, {label}", clock.run(lambda f: worker._publish(next(seq), f), inputs, worker), results)
    worker = StreamWorker("bench://cam", 1280, 720, 30, frame_pipeline=win._flow.pipeline(live=True))
    seq = iter(range(1, 10**9))
    report("worker._publish, Stream output at 720p", clock.run(lambda f: worker._publish(next(seq), f), inputs, worker),
           results)
    _fmt, to_cam = _vcam_format(w, h)
    report("vcam: fit_frame (same size)", clock.run(lambda f: vcam.fit_frame(f, w, h), inputs, key), results)
    report("vcam: fit_frame (portrait into landscape)",
           clock.run(lambda f: vcam.fit_frame(f, w, h), [np.ascontiguousarray(f.transpose(1, 0, 2)) for f in inputs[:4]],
                     key), results)
    if to_cam is not None:
        report("vcam: BGR to I420", clock.run(to_cam, inputs, key), results)


def preview_section(win, app, clock, inputs, key, results, plugins):
    header("Preview: the worker's copy, then the GUI thread's tint, QImage and paint")
    pv = plugins.get("preview")
    if pv is None:
        return
    from telescope.plugins.preview import _fit_within
    size = (640, 360)
    report("preview worker: _fit_within to the card", clock.run(lambda f: _fit_within(f, size), inputs, key), results)
    small = [_fit_within(f, size) for f in inputs[:8]]
    pv._preview_lbl.resize(*size)
    pv._preview_lbl.show()
    pv._streaming, pv._active = True, True
    win.is_camera_on = lambda: True
    report("preview GUI: _show_frame", clock.run(pv._show_frame, small, key), results)
    report("preview GUI: label paint", clock.run(lambda _f: pv._preview_lbl.grab(), small, key), results)
    cin = plugins.get("cinema")
    if cin is not None:
        defaults = cin.get_config()
        cin.set_config({"peaking": True, "zebras": True, "false_colour": True, "scope": "waveform"})
        app.processEvents()
        report("preview GUI: overlays.tint, all Cinema marks", clock.run(win._overlays.tint, small, key), results)
        pv._show_frame(small[0])
        report("preview GUI: label paint with scope", clock.run(lambda _f: pv._preview_lbl.grab(), small, key), results)
        from telescope import scopes
        for name in scopes.SCOPES:
            report(f"cinema scope (analyse thread): {name}",
                   clock.run(lambda f, n=name: scopes.scope_image(n, f), small, key), results)
        cin.set_config(defaults)
        app.processEvents()


def composite_section(clock, inputs, results, w, h):
    from telescope.composite import Compositor, Layout, grid
    header("Several cameras in one picture (Compositor.render, frames numbered as they'd come)")
    for n in (2, 3, 4):
        comp = Compositor()
        streams = [(f"s{i}", f"S{i}", w / h) for i in range(n)]
        comp.prepare(Layout(tiles=grid(streams, w / h)), (w, h))
        count = iter(range(1, 10**9))

        def render(f, n=n):
            k = next(count)
            comp.render({f"s{i}": (f, k * n + i) for i in range(n)})
        report(f"{n} streams, every frame new", clock.run(render, inputs, None), results)
        same = {f"s{i}": (inputs[i], i) for i in range(n)}
        report(f"{n} streams, others unchanged", clock.run(lambda f: comp.render(dict(same, s0=(f, None))), inputs, None),
               results)


def decode_section(clock, inputs, results, w, h):
    header("Decoding (the reader thread, before any plugin)")
    from telescope.mjpeg_reader import MjpegReader
    jpegs = [cv2.imencode(".jpg", f, [cv2.IMWRITE_JPEG_QUALITY, 80])[1].tobytes() for f in inputs[:8]]
    report("MJPEG decode", clock.run(lambda j: MjpegReader.decode(j), jpegs, None) | {"copies": 1.0}, results)
    try:
        import av
        from telescope.h264_reader import new_decoder
        enc = av.CodecContext.create("libx264", "w")
        enc.width, enc.height, enc.pix_fmt = w, h, "yuv420p"
        enc.options = {"preset": "ultrafast", "tune": "zerolatency"}
        enc.framerate = 30
        data = []
        for f in inputs:
            for pkt in enc.encode(av.VideoFrame.from_ndarray(f, format="bgr24")):
                data.append(bytes(pkt))
        dec = new_decoder()
        dec.feed(b"".join(data[:1]))
        units = data[1:]
        it = iter(range(10**9))

        def feed(_f):
            dec.feed(units[next(it) % len(units)])
        report("H.264 decode + BGR (software)", clock.run(feed, inputs, None) | {"copies": 1.0}, results)
    except Exception as exc:  # noqa: BLE001
        print(f"  H.264 skipped: {type(exc).__name__}: {exc}")


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--frames", type=int, default=60)
    ap.add_argument("--size", default="1920x1080")
    ap.add_argument("--only", default="")
    ap.add_argument("--json", default="")
    ap.add_argument("--seconds", type=float, default=3.0, help="how long the GUI thread is watched at each rate")
    ap.add_argument("--profile", action="store_true", help="list each row's costliest calls under it")
    run(ap.parse_args())


if __name__ == "__main__":
    main()
