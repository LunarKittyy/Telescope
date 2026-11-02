#!/usr/bin/env python3
"""Runs Telescope on a recorded phone stream, idle and under game-like loads, and measures what reaches the virtual camera.
Prints a table and writes JSON that --compare can diff. README.md beside this file has the details.

  stress.py --clip CLIP.h264 [--quick] [--out result.json] [--compare old.json]
  stress.py --clip CLIP.h264 --cells plain/idle,crt/gpu,none/all
  stress.py --real-phone --profile FOLDER_WITH_A_PAIRED_PHONE

The app and the loads run on a private bus in a nested virtual KWin, confined to --cpus threads. The fake phone, the
camera reader and this script stay on the other cores, so they measure the load without suffering it.
"""

import argparse
import json
import math
import os
import shutil
import signal
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
RUST = HERE.parents[1]
REPO = RUST.parent
TICK = os.sysconf("SC_CLK_TCK")
PAGE_KB = os.sysconf("SC_PAGESIZE") / 1024

SCENARIOS = {
    "plain": {},
    "colour": {"colour_grading": {"on": True, "wheels": {"lift": [0.1, 0.05], "gain": [-0.1, 0.1]},
                                  "masters": {"gamma": 0.15, "gain": 1.1}}},
    "colour3d": {"colour_grading": {"on": True, "saturation": 1.3, "masters": {"gamma": 0.15, "gain": 1.1}}},
    "crt": {"fun_pack": {"effect": "crt"}},
    "vhs": {"fun_pack": {"effect": "vhs"}},
    "desk": {"desk": {"on": True, "corners": [[0.2, 0.15], [0.8, 0.2], [0.85, 0.85], [0.15, 0.8]]}},
    "blur": {"background": {"mode": "blur"}},
}
LOADS = ["idle", "gpu", "cpu", "obs", "all"]
QUICK = ["plain/idle", "crt/idle", "blur/idle", "none/all", "plain/all", "crt/all"]
SHOWN_THREADS = 6
# Warn above these: percent of one core used by nobody we started, and percent of the 3D engine busy with no GPU load running
NOISE_CPU = 60
NOISE_GPU = 20
# How far apart the idle repeats may be before the run is called noisy, as a share of their mean
SPREAD_LIMITS = {"fps": 0.02, "p99_ms": 0.25, "latency_p99_ms": 0.25, "cpu_pct": 0.15}
TARGET_DIR = os.environ.get("CARGO_TARGET_DIR")


def percentile(values, share: float) -> float:
    if not values:
        return math.nan
    ordered = sorted(values)
    return ordered[min(len(ordered) - 1, int(share * len(ordered)))]


def rounded(value, digits=1):
    return None if value is None or (isinstance(value, float) and math.isnan(value)) else round(value, digits)


def cpu_set(count: int) -> tuple:
    """count threads on as few physical cores as it takes, away from CPU 0, as a mid-tier 6-thread budget would have them. Returns them and the rest."""
    online = sorted(os.sched_getaffinity(0))
    cores = {}
    for cpu in online:
        text = Path(f"/sys/devices/system/cpu/cpu{cpu}/topology/thread_siblings_list").read_text().strip()
        cores.setdefault(text, []).append(cpu)
    chosen = []
    for siblings in sorted(cores.values(), key=lambda s: -s[0]):
        chosen += siblings
        if len(chosen) >= count:
            break
    chosen = sorted(chosen[:count])
    return chosen, [c for c in online if c not in chosen] or online


def graphics_card():
    return next(iter(sorted(Path("/sys/class/drm").glob("card*/device/gpu_busy_percent"))), None)


def read_number(path) -> float:
    try:
        return float(Path(path).read_text())
    except (OSError, ValueError):
        return math.nan


def card_sensors() -> dict:
    """amdgpu's sensors, readable without root: watts in the card's own rolling average, and core and memory clocks in MHz."""
    card = graphics_card()
    folder = next(iter(sorted(card.parent.glob("hwmon/hwmon*"))), None) if card else None
    if not folder:
        return {}
    return {"gpu_w": read_number(folder / "power1_average") / 1e6, "core_mhz": read_number(folder / "freq1_input") / 1e6, "memory_mhz": read_number(folder / "freq2_input") / 1e6}


def package_joules():
    """(joules so far, where the counter wraps) of the CPU package from RAPL, None where that isn't readable."""
    folder = Path("/sys/class/powercap/intel-rapl:0")
    used, wraps = read_number(folder / "energy_uj"), read_number(folder / "max_energy_range_uj")
    return None if math.isnan(used) or math.isnan(wraps) else (used / 1e6, wraps / 1e6)


def die_with_parent():
    # PR_SET_PDEATHSIG: nothing lingers if this script is killed
    import ctypes
    ctypes.CDLL(None).prctl(1, signal.SIGTERM)


def start(cmd, cpus, env=None, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, cwd=None) -> subprocess.Popen:
    def setup():
        die_with_parent()
        os.sched_setaffinity(0, cpus)
    return subprocess.Popen(cmd, env=env, stdout=stdout, stderr=stderr, stdin=subprocess.DEVNULL, preexec_fn=setup, cwd=cwd)


def stop(process, grace=4.0):
    """Ends exactly this process, by its PID."""
    if process is None or process.poll() is not None:
        return
    process.terminate()
    try:
        process.wait(grace)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait()


def children_of() -> dict:
    out = {}
    for name in os.listdir("/proc"):
        if name.isdigit():
            try:
                ppid = int(Path(f"/proc/{name}/stat").read_text().rsplit(")", 1)[1].split()[1])
                out.setdefault(ppid, []).append(int(name))
            except (OSError, IndexError, ValueError):
                pass
    return out


def tree(root: int) -> list:
    children, out, todo = children_of(), [], [root]
    while todo:
        pid = todo.pop()
        out.append(pid)
        todo += children.get(pid, [])
    return out


def process_ticks(pids) -> int:
    """CPU ticks of these processes, including threads that have already exited, which per-thread numbers miss."""
    total = 0
    for pid in pids:
        try:
            fields = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
            total += int(fields[11]) + int(fields[12])
        except (OSError, IndexError, ValueError):
            pass
    return total


def thread_ticks(pids) -> dict:
    """Ticks of CPU per (pid, tid, name), by each thread that's alive."""
    out = {}
    for pid in pids:
        try:
            for tid in os.listdir(f"/proc/{pid}/task"):
                text = Path(f"/proc/{pid}/task/{tid}/stat").read_text()
                name = text[text.index("(") + 1:text.rindex(")")]
                fields = text.rsplit(")", 1)[1].split()
                out[(pid, tid, name)] = int(fields[11]) + int(fields[12])
        except (OSError, IndexError, ValueError):
            pass
    return out


def gpu_stats(pids) -> dict:
    """Busy nanoseconds per engine and bytes of VRAM from each process's DRM file descriptors, counting each client once."""
    clients = {}
    for pid in pids:
        try:
            names = os.listdir(f"/proc/{pid}/fdinfo")
        except OSError:
            continue
        for fd in names:
            try:
                text = Path(f"/proc/{pid}/fdinfo/{fd}").read_text()
            except OSError:
                continue
            if "drm-client-id" not in text:
                continue
            fields = dict(line.split(":", 1) for line in text.splitlines() if ":" in line)
            clients[fields["drm-client-id"].strip()] = fields
    busy, vram = {}, 0
    for fields in clients.values():
        for key, value in fields.items():
            if key.startswith("drm-engine-") and not key.startswith("drm-engine-capacity"):
                engine = key[len("drm-engine-"):]
                busy[engine] = busy.get(engine, 0) + int(value.split()[0])
            elif key in ("drm-memory-vram", "drm-total-vram"):
                vram = max(vram, 0) + int(value.split()[0]) * 1024
    return {"busy": busy, "vram": vram}


def memory_kb(pids) -> int:
    total = 0
    for pid in pids:
        try:
            total += int(Path(f"/proc/{pid}/statm").read_text().split()[1]) * PAGE_KB
        except (OSError, IndexError, ValueError):
            pass
    return int(total)


def cpu_times() -> dict:
    """Busy ticks of every CPU, all together under None."""
    out = {}
    for line in Path("/proc/stat").read_text().splitlines():
        if not line.startswith("cpu"):
            break
        name, *fields = line.split()
        values = [int(v) for v in fields]
        out[None if name == "cpu" else int(name[3:])] = sum(values) - values[3] - values[4]
    return out


class Session:
    """A nested virtual KWin on a private bus, for as long as the run lasts."""

    def __init__(self, folder: Path, cpus):
        self.socket = f"telescope-stress-{os.getpid()}"
        bus_file = folder / "bus"
        script = folder / "session.sh"
        script.write_text(f'#!/bin/sh\necho "$DBUS_SESSION_BUS_ADDRESS" > "$1"\nexec kwin_wayland --virtual --no-lockscreen '
                          f'--no-global-shortcuts --socket {self.socket} --width 1920 --height 1080\n')
        script.chmod(0o755)
        env = {k: v for k, v in os.environ.items() if k not in ("DISPLAY", "WAYLAND_DISPLAY", "DBUS_SESSION_BUS_ADDRESS")}
        log = (folder / "kwin.log").open("w")
        self.process = start(["dbus-run-session", "--", str(script), str(bus_file)], cpus, env=env, stdout=log, stderr=log)
        runtime = Path(os.environ.get("XDG_RUNTIME_DIR", f"/run/user/{os.getuid()}"))
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline and not (bus_file.exists() and bus_file.read_text().strip() and (runtime / self.socket).exists()):
            time.sleep(0.1)
        if not (runtime / self.socket).exists():
            stop(self.process)
            sys.exit(f"The nested KWin didn't start, see {folder / 'kwin.log'}")
        self.bus = bus_file.read_text().strip()

    def pids(self):
        return [p for p in tree(self.process.pid) if p != self.process.pid]

    def close(self):
        for pid in self.pids():
            try:
                os.kill(pid, signal.SIGTERM)
            except OSError:
                pass
        stop(self.process)


class Cell:
    """One scenario under one load: starts everything, measures over the window, stops everything."""

    def __init__(self, name: str, args, session: Session, folder: Path, mid_tier, spare):
        self.name, self.args, self.session, self.mid_tier, self.spare = name, args, session, mid_tier, spare
        self.scenario, self.load = name.split("#")[0].split("/")
        self.folder = folder
        folder.mkdir(parents=True)
        self.loads = ["gpu", "cpu", "obs"] if self.load == "all" else [] if self.load == "idle" else ["gpu"] if self.load in ("split", "burst") else [self.load]
        self.processes = []
        self.app = self.phone = None
        self.obs_lines = []
        self.fifo = folder / "obs.fifo"
        self.result = {"cell": self.name, "scenario": self.scenario, "load": self.load}

    def budget(self):
        return self.args.settle + self.args.seconds + 60

    def load_program(self, *words, file) -> subprocess.Popen:
        return start([str(self.args.loads), *words, "--seconds", str(self.budget())], self.mid_tier, stdout=(self.folder / file).open("w"),
                     stderr=(self.folder / f"{file}.err").open("w"))

    def wait_for_lines(self, file: str, seconds: float) -> bool:
        path = self.folder / file
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            if path.exists() and path.stat().st_size:
                return True
            time.sleep(0.05)
        return False

    def start_gpu(self):
        a = self.args
        process = self.load_program("gpu", "--width", str(a.gpu_width), "--height", str(a.gpu_height),
                                    "--steps", str(a.split_steps if self.load == "split" else a.gpu_steps), "--vram-mb", str(a.gpu_vram),
                                    "--slices", str(a.gpu_slices if self.load == "split" else 1), file="gpu.log")
        self.processes.append(process)
        if not self.wait_for_lines("gpu.log", 30):
            self.result["error"] = "the GPU load never drew a frame"
        return process

    def start_gpu_cpu(self):
        if "gpu" in self.loads and self.load != "burst":
            self.start_gpu()
        if "cpu" in self.loads:
            self.processes.append(self.load_program("cpu", "--threads", str(self.args.cpu_threads), "--duty", str(self.args.cpu_duty), file="cpu.log"))

    def profile(self, entry) -> dict:
        args = self.args
        if args.real_phone:
            config = json.loads((args.profile / "telescope_config.json").read_text())
            phone_id = config["plugin_configs"]["connection"].get("selected_phone")
        else:
            connection = {"phones": [entry], "route": "auto", "udp": False, "selected_phone": entry["id"]}
            config = {"version": 3, "plugin_configs": {"connection": connection}, "devices": {}}
            phone_id = entry["id"]
        plugins = config["plugin_configs"]
        plugins.setdefault("onboarding", {})["streamed"] = True
        plugins.setdefault("setup", {}).update({"language": "en", "hw_decode": args.hw_decode})
        plugins.update(SCENARIOS[self.scenario])
        # An effect's settings can be per phone or global, so they go in both
        config["selected_device"] = phone_id
        # Optional built-ins start out off
        config.setdefault("plugins", {}).setdefault("on", []).extend(SCENARIOS[self.scenario])
        section = config.setdefault("devices", {}).setdefault(phone_id, {}).setdefault("plugin_configs", {})
        section.update(SCENARIOS[self.scenario])
        return config

    def start_app(self) -> bool:
        args = self.args
        entry = None
        if not args.real_phone:
            self.phone = start([sys.executable, "-I", str(RUST / "tools" / "fake_phone.py"), str(args.clip), "--fps", "30"], self.spare,
                               env=dict(os.environ, TMPDIR=str(self.folder)), stdout=subprocess.PIPE)
            entry = json.loads(self.phone.stdout.readline())
        profile = self.folder / "profile"
        profile.mkdir()
        (profile / "telescope_config.json").write_text(json.dumps(self.profile(entry), indent=2))
        env = dict(os.environ, WAYLAND_DISPLAY=self.session.socket, DBUS_SESSION_BUS_ADDRESS=self.session.bus,
                   TELESCOPE_DEV_PROFILE=str(profile), XDG_CONFIG_HOME=str(profile), TELESCOPE_STRESS_LOG=str(self.folder / "delivery.log"))
        if args.trace:
            env["TELESCOPE_STRESS_TRACE"] = str(self.folder / "trace.log")
        env.pop("DISPLAY", None)
        if args.vision_pack:
            # A dev profile keeps its vision pack inside it
            (profile / "vision").mkdir()
            (profile / "vision" / args.vision_pack.name).symlink_to(args.vision_pack.resolve())
        if not args.real_phone:
            # No adb means a phone on the cable stays out of it
            stub = self.folder / "bin"
            stub.mkdir()
            (stub / "adb").write_text('#!/bin/sh\necho "List of devices attached"\n')
            (stub / "adb").chmod(0o755)
            env["PATH"] = f"{stub}{os.pathsep}{env['PATH']}"
        log = (self.folder / "app.log").open("w")
        self.app = start([str(args.app), "--autostart", "--device", args.device, "--width", "1920", "--height", "1080", "--fps", "30"],
                         self.mid_tier, env=env, stdout=log, stderr=log)
        return self.wait_for_lines("delivery.log", args.boot_timeout)

    def start_obs(self):
        a = self.args
        if self.scenario == "none":
            # With nothing feeding the camera, the same clip stands in for it, and that adds a software H.264 decode
            source = ["-stream_loop", "-1", "-f", "h264", "-framerate", "30", "-i", str(a.clip)]
        else:
            # v4l2loopback serves one capture at a time, so the camera reader passes its frames on through this
            os.mkfifo(self.fifo)
            source = ["-use_wallclock_as_timestamps", "1", "-f", "rawvideo", "-pix_fmt", "yuv420p", "-video_size", "1920x1080", "-i", str(self.fifo)]
        encoder = ["-c:v", "libx264", "-preset", "veryfast", "-b:v", "6000k"] if a.obs_encoder == "x264" else \
            ["-vaapi_device", "/dev/dri/renderD128", "-c:v", "h264_vaapi", "-b:v", "6000k"]
        # The noise makes each of the 60 frames new, the way a real 60 fps canvas costs the encoder, where repeats would cost it nothing
        filters = ("realtime," if self.scenario == "none" else "") + "fps=60,noise=alls=6:allf=t,format=nv12" + (",hwupload" if a.obs_encoder == "vaapi" else "")
        cmd = ["ffmpeg", "-hide_banner", "-nostats", "-loglevel", "error", "-progress", "pipe:1", *source, "-vf", filters, *encoder, "-f", "null", "-"]
        process = start(cmd, self.mid_tier, stdout=subprocess.PIPE, stderr=(self.folder / "obs.err").open("w"))
        self.processes.append(process)

        def follow():
            for line in process.stdout:
                if line.startswith(b"frame="):
                    self.obs_lines.append((time.monotonic_ns(), int(line[6:])))

        threading.Thread(target=follow, daemon=True).start()

    def sample(self, process_lists) -> dict:
        return {"t": time.monotonic_ns(), "ticks": {k: process_ticks(v) for k, v in process_lists.items()}, "threads": thread_ticks(process_lists["app"]),
                "gpu": {k: gpu_stats(v) for k, v in process_lists.items()}, "cpus": cpu_times()}

    def run(self) -> dict:
        a = self.args
        try:
            self.start_gpu_cpu()
            if self.scenario != "none" and not self.start_app():
                self.result["error"] = "no frame reached the camera in time"
                return self.result
            if "obs" in self.loads:
                self.start_obs()
            if self.scenario != "none":
                tee = ["--tee", str(self.fifo)] if "obs" in self.loads else []
                self.processes.append(start([str(a.loads), "read", "--device", a.device, "--seconds", str(self.budget()), *tee], self.spare,
                                            stdout=(self.folder / "reader.log").open("w"), stderr=(self.folder / "reader.err").open("w")))
            time.sleep(a.settle)
            lists = {"app": tree(self.app.pid) if self.app else [], "kwin": self.session.pids(),
                     "loads": sum((tree(p.pid) for p in self.processes), []) + ([self.phone.pid] if self.phone else [])}
            before = self.sample(lists)
            busy, rss, vram = [], [], []
            card = graphics_card()
            begun = time.monotonic()
            until = begun + a.seconds
            burst = None
            sensors, energy = [], package_joules()
            energy_at = time.monotonic()
            while time.monotonic() < until:
                time.sleep(0.5)
                sensors.append(card_sensors())
                if self.load == "burst":
                    elapsed = (time.monotonic() - begun) / a.seconds
                    if burst is None and elapsed >= 0.2:
                        burst = self.start_gpu()
                    elif burst is not None and elapsed >= 0.5 and burst.poll() is None:
                        stop(burst)
                if card:
                    busy.append(int(card.read_text()))
                rss.append(memory_kb(lists["app"]))
            after = self.sample(lists)
            self.result.update(self.measure(before, after, busy, rss))
            self.result["power"] = self.power(sensors, energy, energy_at)
        except OSError as e:
            self.result["error"] = str(e)
        finally:
            self.stop_all()
        return self.result

    def power(self, sensors, energy, energy_at) -> dict:
        """Mean sensor values over the window. The first few of the card's are dropped, since its figure is a rolling average."""
        out = {}
        settled = sensors[len(sensors) // 5:]
        for key in ("gpu_w", "core_mhz", "memory_mhz"):
            values = [row[key] for row in settled if not math.isnan(row.get(key, math.nan))]
            if values:
                out[key] = rounded(statistics.mean(values), 1)
                out[f"{key}_spread"] = rounded(statistics.pstdev(values), 1)
        finish = package_joules()
        if energy and finish:
            used = finish[0] - energy[0] + (energy[1] if finish[0] < energy[0] else 0)
            out["cpu_w"] = rounded(used / (time.monotonic() - energy_at), 1)
        return out

    def stop_all(self):
        for process in [self.app, *reversed(self.processes), self.phone]:
            stop(process)
        if self.app and self.result.get("exit") is None and self.app.returncode not in (0, -15, 143):
            self.result["app_exit"] = self.app.returncode

    def measure(self, before, after, busy, rss) -> dict:
        t0, t1 = before["t"], after["t"]
        seconds = (t1 - t0) / 1e9
        out = {"seconds": rounded(seconds, 2)}
        if self.app:
            out.update(self.camera_numbers(t0, t1, seconds))
            ticks = {k: v - before["threads"].get(k, 0) for k, v in after["threads"].items()}
            total = after["ticks"]["app"] - before["ticks"]["app"]
            out["cpu_pct"] = rounded(total * 100 / TICK / seconds)
            named = {}
            for (_, _, name), value in ticks.items():
                named[name] = named.get(name, 0) + value
            top = sorted(named.items(), key=lambda kv: -kv[1])[:SHOWN_THREADS]
            out["threads"] = {name: rounded(value * 100 / TICK / seconds) for name, value in top if value}
            # Threads that came and went inside the window: the frame steps' workers
            out["threads"]["(short-lived)"] = rounded(max(0, total - sum(ticks.values())) * 100 / TICK / seconds)
            gpu0, gpu1 = before["gpu"]["app"], after["gpu"]["app"]
            out["engines_pct"] = {e: rounded((v - gpu0["busy"].get(e, 0)) / (t1 - t0) * 100) for e, v in gpu1["busy"].items() if v > gpu0["busy"].get(e, 0)}
            out["vram_mb"] = rounded(gpu1["vram"] / 1e6, 0)
            out["rss_mb"] = rounded(max(rss) / 1024, 0)
            out["kwin_cpu_pct"] = rounded((after["ticks"]["kwin"] - before["ticks"]["kwin"]) * 100 / TICK / seconds)
            out["steps_log"] = self.steps_line()
            out["swaps"] = self.swaps()
        out.update(self.load_numbers(t0, t1, seconds))
        out["gpu_busy_pct"] = rounded(statistics.mean(busy)) if busy else None
        out["warnings"] = self.noise(before, after, seconds, out)
        return out

    def steps_line(self):
        line = None
        for path in (self.folder / "profile").rglob("telescope.log"):
            for text in path.read_text(errors="replace").splitlines():
                if "Frame steps cost" in text:
                    line = text.split("Frame steps cost")[1].strip()
        return line

    def swaps(self) -> list:
        """Where frame steps moved between the card and the CPU over the whole run, newest last."""
        lines = []
        for path in (self.folder / "profile").rglob("telescope.log"):
            lines += [text.split("Frame steps moved to the ")[1].strip() for text in path.read_text(errors="replace").splitlines() if "Frame steps moved to the " in text]
        return lines

    def camera_numbers(self, t0, t1, seconds) -> dict:
        frames, previous = [], None
        duplicates = 0
        for line in (self.folder / "reader.log").read_text().splitlines():
            parts = line.split()
            if len(parts) != 2:
                continue
            stamp = int(parts[0])
            if t0 <= stamp <= t1:
                if parts[1] == previous:
                    duplicates += 1
                else:
                    frames.append(stamp)
            previous = parts[1]
        gaps = [(b - a) / 1e6 for a, b in zip(frames, frames[1:])]
        out = {"fps": rounded(len(frames) / seconds), "duplicates": duplicates}
        if len(gaps) < 2:
            return out
        median = statistics.median(gaps)
        over = sum(g > 1.5 * median for g in gaps)
        out.update(p50_ms=rounded(median), p95_ms=rounded(percentile(gaps, 0.95)), p99_ms=rounded(percentile(gaps, 0.99)), worst_ms=rounded(max(gaps)),
                   jitter_ms=rounded(statistics.pstdev(gaps), 2), stutter=over, stutter_pct=rounded(100 * over / len(gaps), 2))
        path = self.folder / "delivery.log"
        rows = []
        if path.exists():
            for line in path.read_text().splitlines():
                parts = line.split()
                if len(parts) == 4 and t0 <= int(parts[3]) <= t1:
                    rows.append((int(parts[1]), int(parts[2]), int(parts[3])))
        latency = [(sent - arrived) / 1e6 for arrived, _, sent in rows if arrived]
        ready = [(sent - done) / 1e6 for _, done, sent in rows]
        arrivals = sorted({row[0] for row in rows})
        between = [(b - a) / 1e6 for a, b in zip(arrivals, arrivals[1:])]
        if latency:
            out.update(latency_p50_ms=rounded(percentile(latency, 0.5)), latency_p95_ms=rounded(percentile(latency, 0.95)),
                       latency_p99_ms=rounded(percentile(latency, 0.99)), latency_worst_ms=rounded(max(latency)),
                       camera_step_p99_ms=rounded(percentile(ready, 0.99)), arrival_p99_ms=rounded(percentile(between, 0.99)))
        if self.args.trace:
            out["stages_ms"] = self.stages(t0, t1)
        return out

    def stages(self, t0, t1) -> dict:
        """What each traced stage of a frame took, from TELESCOPE_STRESS_TRACE."""
        spans = {}
        path = self.folder / "trace.log"
        if path.exists():
            for line in path.read_text().splitlines():
                parts = line.split()
                if len(parts) == 3 and t0 <= int(parts[2]) <= t1:
                    spans.setdefault(parts[0], []).append((int(parts[2]) - int(parts[1])) / 1e6)
        return {stage: {"count": len(took), "p50": rounded(percentile(took, 0.5), 2), "p99": rounded(percentile(took, 0.99), 2),
                        "worst": rounded(max(took), 2), "total": rounded(sum(took), 1)} for stage, took in spans.items()}

    def load_numbers(self, t0, t1, seconds) -> dict:
        out = {}
        if "gpu" in self.loads:
            stamps = [int(line) for line in (self.folder / "gpu.log").read_text().split() if t0 <= int(line) <= t1]
            gaps = [(b - a) / 1e6 for a, b in zip(stamps, stamps[1:])]
            out["gpu_load"] = {"fps": rounded(len(stamps) / seconds), "p99_ms": rounded(percentile(gaps, 0.99)), "worst_ms": rounded(max(gaps, default=None))}
        if "cpu" in self.loads:
            rows = [line.split() for line in (self.folder / "cpu.log").read_text().splitlines()]
            rows = [(int(r[1]), int(r[2])) for r in rows if len(r) == 3 and t0 <= int(r[0]) <= t1]
            out["cpu_load"] = {"slices_per_s": rounded(sum(r[0] for r in rows) / seconds), "worst_slice_ms": rounded(max((r[1] for r in rows), default=0) / 1000)}
        if "obs" in self.loads:
            inside = [(t, n) for t, n in self.obs_lines if t0 <= t <= t1]
            if len(inside) >= 2:
                span = (inside[-1][0] - inside[0][0]) / 1e9
                out["obs_load"] = {"fps": rounded((inside[-1][1] - inside[0][1]) / span)}
        return out

    def noise(self, before, after, seconds, out) -> list:
        warnings = []
        ours = sum(after["ticks"][k] - before["ticks"][k] for k in after["ticks"])
        total = after["cpus"][None] - before["cpus"][None]
        other = max(0, total - ours) * 100 / TICK / seconds
        out["other_cpu_pct"] = rounded(other)
        if other > NOISE_CPU:
            warnings.append(f"something else used {other:.0f}% of a core")
        confined = sum(after["cpus"][c] - before["cpus"][c] for c in self.mid_tier)
        out["budget_busy_pct"] = rounded(confined * 100 / TICK / seconds / len(self.mid_tier))
        if "gpu" not in self.loads and out.get("gpu_busy_pct") is not None:
            mine = sum(v for k, v in out.get("engines_pct", {}).items() if k.startswith(("gfx", "compute")))
            if out["gpu_busy_pct"] - mine > NOISE_GPU:
                warnings.append(f"the GPU was {out['gpu_busy_pct']:.0f}% busy, {mine:.0f} of it ours")
        if self.app and self.app.poll() is not None:
            warnings.append(f"the app quit early with {self.app.returncode}")
        return warnings


def matrix(args) -> list:
    if args.cells:
        return args.cells.split(",")
    if args.quick:
        return list(QUICK)
    scenes = [s for s in SCENARIOS if s != "colour3d"]
    cells = [f"none/{load}" for load in LOADS if load != "idle"]
    cells += [f"{s}/{load}" for s in scenes for load in LOADS]
    # The one shader step's cell beside a game cut into many draws
    cells.insert(cells.index("colour/cpu"), "colour/split")
    cells.insert(0, "none/split")
    # The extra idle runs go in the middle and at the end, so a drift across the run shows up in the spread too
    cells.insert(len(cells) // 2, "plain/idle#2")
    cells.append("plain/idle#3")
    return cells


def cell(value, width, digits=1):
    text = "-" if value is None else f"{value:.{digits}f}" if isinstance(value, float) else str(value)
    return text.rjust(width)


COLUMNS = (("cell", 14), ("fps", 5), ("p50", 5), ("p95", 5), ("p99", 6), ("worst", 6), ("stut", 4), ("lat50", 6), ("lat99", 6), ("cpu%", 5), ("3d%", 4), ("dec%", 4),
           ("mem", 4), ("gpuL", 5), ("cpuL", 5), ("obsL", 5), ("gpuW", 5), ("cpuW", 5), ("", 1))


def row(r, alone) -> str:
    engines = r.get("engines_pct") or {}

    def share(key, field, name):
        have, base = (r.get(key) or {}).get(field), (alone.get(key) or {}).get(field)
        if have is None:
            return None
        return f"{have:.0f}" if not base or r["scenario"] == "none" else f"{100 * have / base:.0f}%"

    values = [r["cell"], r.get("fps"), r.get("p50_ms"), r.get("p95_ms"), r.get("p99_ms"), r.get("worst_ms"), r.get("stutter"), r.get("latency_p50_ms"), r.get("latency_p99_ms"),
              r.get("cpu_pct"), sum(v for k, v in engines.items() if k.startswith(("gfx", "compute"))) if engines else None,
              sum(v for k, v in engines.items() if k.startswith(("dec", "vcn", "enc", "jpeg"))) if engines else None,
              r.get("rss_mb"), share("gpu_load", "fps", "gpu"), share("cpu_load", "slices_per_s", "cpu"), share("obs_load", "fps", "obs"),
              (r.get("power") or {}).get("gpu_w"), (r.get("power") or {}).get("cpu_w"),
              "!" if r.get("warnings") or r.get("error") else ""]
    return " ".join(cell(v, w[1]) if i else str(v).ljust(w[1]) for i, (v, w) in enumerate(zip(values, COLUMNS)))


def header() -> str:
    return " ".join(name.ljust(width) if i == 0 else name.rjust(width) for i, (name, width) in enumerate(COLUMNS))


def alone_numbers(results) -> dict:
    out = {}
    for r in results:
        if r["scenario"] == "none":
            for key in ("gpu_load", "cpu_load", "obs_load"):
                if r.get(key) and (r["load"] != "all" or key not in out):
                    out[key] = r[key]
    return out


def alone_for(r, results) -> dict:
    """The load's numbers without Telescope, from the cell with the same loads running."""
    for other in results:
        if other["scenario"] == "none" and other["load"] == r["load"]:
            return other
    return {}


def spread(results) -> dict:
    repeats = [r for r in results if r["cell"].split("#")[0] == "plain/idle" and "fps" in r]
    if len(repeats) < 2:
        return {}
    out = {}
    for key in ("fps", "p50_ms", "p95_ms", "p99_ms", "worst_ms", "jitter_ms", "stutter", "latency_p50_ms", "latency_p99_ms", "cpu_pct", "rss_mb"):
        values = [r[key] for r in repeats if r.get(key) is not None]
        if values:
            out[key] = {"values": values, "spread": rounded((max(values) - min(values)) / statistics.mean(values), 3) if statistics.mean(values) else 0}
    out["noisy"] = [k for k, limit in SPREAD_LIMITS.items() if k in out and out[k]["spread"] > limit]
    return out


def dead_effects(results) -> list:
    """Scenarios that cost no more than plain when idle, which means the effect never switched on."""
    idle = {r["scenario"]: r for r in results if r["load"] == "idle" and "cpu_pct" in r}
    plain = idle.get("plain")

    def cost(r):
        return r["cpu_pct"] + sum((r.get("engines_pct") or {}).values())

    if not plain:
        return []
    return [f"{name} cost no more than plain, so the effect looks switched off" for name, r in idle.items() if name != "plain" and cost(r) < cost(plain) + 1]


def compare(results, path: Path):
    old = {r["cell"]: r for r in json.loads(path.read_text())["cells"]}
    print(f"\nAgainst {path}, old > new:")
    for r in results:
        was = old.get(r["cell"])
        if not was:
            continue
        parts = []
        for key, label in (("fps", "fps"), ("p99_ms", "p99"), ("stutter", "stutter"), ("latency_p99_ms", "lat99"), ("cpu_pct", "cpu")):
            if r.get(key) is not None and was.get(key) is not None and r[key] != was[key]:
                parts.append(f"{label} {was[key]} > {r[key]}")
        for key, field in (("gpu_load", "fps"), ("cpu_load", "slices_per_s"), ("obs_load", "fps")):
            a, b = (was.get(key) or {}).get(field), (r.get(key) or {}).get(field)
            if a and b:
                parts.append(f"{key.split('_')[0]} load {a} > {b}")
        if r.get("swaps"):
            parts.append(f"{len(r['swaps'])} swaps")
        print(f"  {r['cell']:<14} {', '.join(parts) or 'same'}")


def git_commit() -> str:
    done = subprocess.run(["git", "-C", str(REPO), "rev-parse", "--short", "HEAD"], capture_output=True, text=True)
    return done.stdout.strip()


def build_if_missing(args):
    if not args.app.exists():
        subprocess.run(["cargo", "build", "--release", "-p", "telescope-app"], cwd=RUST, check=True)
    if not args.loads.exists():
        subprocess.run(["cargo", "build", "--release"], cwd=HERE / "loads", check=True)


def parse() -> argparse.Namespace:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--clip", type=Path, help="recorded H.264 (record_clip.py)")
    p.add_argument("--quick", action="store_true", help="a few cells, for use after every change")
    p.add_argument("--cells", help="comma-separated scenario/load names; scenarios: " + ", ".join(["none", *SCENARIOS]) + "; loads: " + ", ".join(LOADS) + ", split")
    p.add_argument("--seconds", type=int, default=25, help="measured seconds per cell")
    p.add_argument("--settle", type=int, default=5, help="seconds between the first frame and measuring")
    p.add_argument("--boot-timeout", type=int, default=40)
    p.add_argument("--cpus", type=int, default=6, help="threads the app and loads are confined to, 0 for all")
    p.add_argument("--device", default="/dev/video11", help="virtual camera to use")
    p.add_argument("--hw-decode", action="store_true", help="turn on the graphics card decoder, which is off by default")
    p.add_argument("--app", type=Path, default=Path(TARGET_DIR or RUST / "target") / "release" / "telescope-rs")
    p.add_argument("--loads", type=Path, default=Path(TARGET_DIR or HERE / "loads" / "target") / "release" / "stress-loads")
    p.add_argument("--vision-pack", type=Path, default=os.environ.get("TELESCOPE_VISION_PACK"), help="needed for a real person mask in blur and desk")
    p.add_argument("--real-phone", action="store_true", help="stream from the phone paired in --profile instead of the clip")
    p.add_argument("--profile", type=Path, help="with --real-phone: a folder with a telescope_config.json that has the phone paired")
    p.add_argument("--gpu-width", type=int, default=2560)
    p.add_argument("--gpu-height", type=int, default=1440)
    p.add_argument("--gpu-steps", type=int, default=20, help="texture reads per pixel, which sets how long one frame holds the card")
    p.add_argument("--split-steps", type=int, default=25, help="the same for split, whose bands cost less, so it matches gpu's frame time")
    p.add_argument("--gpu-vram", type=int, default=3000, help="MB of textures the load keeps in use")
    p.add_argument("--gpu-slices", type=int, default=16, help="submissions a frame of the split load comes in")
    p.add_argument("--cpu-threads", type=int, default=0, help="0 for one per confined thread")
    p.add_argument("--cpu-duty", type=int, default=100)
    p.add_argument("--obs-encoder", choices=("x264", "vaapi"), default="x264")
    p.add_argument("--out", type=Path, default=Path("stress-result.json"))
    p.add_argument("--compare", type=Path)
    p.add_argument("--keep-logs", action="store_true")
    p.add_argument("--trace", action="store_true", help="time each stage of a frame and print the stages under each row")
    args = p.parse_args()
    if not args.real_phone and not args.clip:
        p.error("--clip is needed, or --real-phone")
    if args.real_phone and not args.profile:
        p.error("--real-phone needs --profile")
    return args


def main():
    args = parse()
    build_if_missing(args)
    mid_tier, spare = cpu_set(args.cpus) if args.cpus else (sorted(os.sched_getaffinity(0)),) * 2
    args.cpu_threads = args.cpu_threads or len(mid_tier)
    cells = matrix(args)
    meta = {"commit": git_commit(), "desktop": "rust", "date": time.strftime("%Y-%m-%d %H:%M"), "clip": str(args.clip), "real_phone": args.real_phone, "device": args.device,
            "budget_cpus": mid_tier, "seconds": args.seconds, "settle": args.settle, "hw_decode": args.hw_decode,
            "levels": {"gpu": f"{args.gpu_width}x{args.gpu_height} Rgba16Float, {args.gpu_steps} reads per pixel, {args.gpu_vram} MB of textures, two frames in flight",
                       "cpu": f"{args.cpu_threads} threads of hashing, pointer chasing, float maths and sorting at {args.cpu_duty}% duty",
                       "obs": f"ffmpeg reading the camera, 1080p60 through {args.obs_encoder} at 6 Mbps with temporal noise so no frame is a repeat"}}
    print(f"{len(cells)} cells, {args.seconds} s each, on CPUs {mid_tier}, camera {args.device}", flush=True)
    results = []
    folder = Path(tempfile.mkdtemp(prefix="telescope-stress-", dir=args.out.resolve().parent))
    session = Session(folder, mid_tier)
    signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))
    try:
        print(header(), flush=True)
        for i, name in enumerate(cells):
            r = Cell(name, args, session, folder / f"cell{i}", mid_tier, spare).run()
            results.append(r)
            print(row(r, alone_for(r, results)), flush=True)
            for stage, took in r.get("stages_ms", {}).items():
                print(f"    {stage:<12} n {took['count']:<5} p50 {took['p50']:7.2f}  p99 {took['p99']:7.2f}  worst {took['worst']:7.2f}", flush=True)
            for text in r.get("warnings", []) + ([r["error"]] if r.get("error") else []):
                print(f"    ! {text}", flush=True)
            args.out.write_text(json.dumps({"meta": meta, "cells": results, "spread": spread(results)}, indent=1))
            time.sleep(1)
    finally:
        session.close()
        if args.keep_logs:
            print(f"Logs kept in {folder}")
        else:
            shutil.rmtree(folder, ignore_errors=True)
    for line in dead_effects(results):
        print(f"! {line}")
    report = spread(results)
    if report:
        print("\nSpread of the idle repeats (max - min over mean):")
        for key, value in report.items():
            if key != "noisy":
                print(f"  {key:<16} {value['spread'] * 100:5.1f}%  {value['values']}")
        if report["noisy"]:
            print(f"  ! Noisy run: {', '.join(report['noisy'])} moved more than the limit, so small differences here mean nothing")
    if args.compare:
        compare(results, args.compare)
    print(f"\nWrote {args.out}. fps to worst are at the camera in ms, stut is intervals over 1.5 x median, lat from arrival to delivery,\n"
          "cpu% of one core, gpuL, cpuL and obsL are each load's own rate, as a share of its rate without Telescope.")


if __name__ == "__main__":
    main()
