#!/usr/bin/env python3
"""Check a release on a real phone in a few minutes: start time, a steady stream, a Wi-Fi drop and moving to USB.

Runs this checkout's app offscreen with a copy of the dev profile, so pair the dev phone app with
`python scripts/dev_desktop.py --keep` first. It prints numbers only, no names, addresses or serials.

Usage: python scripts/phone_check.py [--starts N] [--wifi-drop SECONDS] [--handover] [--thin-link MBPS ...]
  --wifi-drop  streams over Wi-Fi and turns the phone's Wi-Fi off for that long over adb (needs the USB cable)
  --handover   starts on Automatic with USB hidden from discovery, then reveals it and checks seamless handover
  --thin-link  streams over Wi-Fi through a thin link simulated on this computer, once per --modes, and prints how
               much got through, the phone's bitrate, the share of UDP datagrams lost (each asked for again) and the
               lag from an exposure change sent to it showing up in the picture. --loss PCT, --buffer-ms and --delay-ms shape it further (TCP
               only gets the speed limit), --seconds is how long each mode runs and --emulator measures the lag by
               rotating an emulator instead, whose camera ignores exposure
"""

import argparse
import bisect
import getpass
import os
import random
import select as select_module
import shutil
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path
from unittest.mock import patch

DESKTOP = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(DESKTOP))


def kept_profile() -> Path:
    return Path(tempfile.gettempdir()) / f"telescope-dev-profile-{getpass.getuser()}"


def main(argv) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--starts", type=int, default=3)
    parser.add_argument("--wifi-drop", type=float, default=0)
    parser.add_argument("--handover", action="store_true")
    parser.add_argument("--thin-link", type=float, default=0, metavar="MBPS")
    parser.add_argument("--loss", type=float, default=0, metavar="PCT")
    parser.add_argument("--buffer-ms", type=float, default=100)
    parser.add_argument("--delay-ms", type=float, default=10)
    parser.add_argument("--seconds", type=float, default=45)
    parser.add_argument("--emulator", action="store_true",
                        help="measure lag by rotating an emulator, whose camera ignores exposure changes")
    parser.add_argument("--modes", default="dynamic-tcp,auto-udp,dynamic-udp",
                        help="comma-separated: auto-tcp, dynamic-tcp, auto-udp, dynamic-udp")
    args = parser.parse_args(argv)

    config = kept_profile() / "telescope_config.json"
    if not config.is_file():
        raise SystemExit("No dev profile yet. Pair the dev phone app with `python scripts/dev_desktop.py --keep` first.")
    profile = Path(tempfile.mkdtemp(prefix="telescope-phone-check-"))
    shutil.copy(config, profile / "telescope_config.json")
    os.environ["TELESCOPE_DEV_PROFILE"] = str(profile)
    os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")
    try:
        return Check().run(args)
    finally:
        shutil.rmtree(profile, ignore_errors=True)


class Check:
    def __init__(self):
        from PyQt6.QtWidgets import QApplication
        from telescope import stream, theme
        from telescope.app import TelescopeWindow
        from telescope.plugins import builtin_plugins

        self.frames, self.banners = [], []
        check = self
        publish, show_issue = stream.StreamWorker._publish, TelescopeWindow.show_issue

        def counted_publish(worker, seq, raw, *a, **k):
            now = time.monotonic()
            check.frames.append(now)
            check.worker = worker
            if check.thumbs is not None:
                check.thumbs.append((now, thumb(raw)))
            return publish(worker, seq, raw, *a, **k)

        def noted_issue(win, key, issue):
            check.banners.append(issue.title)
            return show_issue(win, key, issue)

        stream.StreamWorker._publish = counted_publish
        TelescopeWindow.show_issue = noted_issue
        self.worker = None
        self.thumbs = None  # (when, small grey picture) per frame while --thin-link measures lag
        self.app = QApplication.instance() or QApplication([])
        theme.apply_theme(self.app)
        self.win = TelescopeWindow()
        self.win.load_plugins(builtin_plugins())
        self.win.apply_saved_config()
        self.win.show()
        self.connection = self.win._plugin("connection")

    def pump(self, seconds: float, until=None) -> bool:
        end = time.monotonic() + seconds
        while time.monotonic() < end:
            self.app.processEvents()
            time.sleep(0.005)
            if until and until():
                return True
        return False

    def start(self) -> float:
        """Seconds from Start to the first frame, or 0 if none came."""
        self.frames.clear()
        t0 = time.monotonic()
        self.win.start_stream(interactive=False)
        return self.frames[0] - t0 if self.pump(20, until=lambda: self.frames) else 0.0

    def stop(self):
        self.win.stop_stream()
        self.pump(3)

    def route(self) -> str:
        url = getattr(self.worker, "url", "") or ""
        return "USB" if "127.0.0.1" in url else "Wi-Fi"

    def longest_gap(self, since: float) -> float:
        times = sorted(self.frames)
        first = bisect.bisect_left(times, since)
        times = times[max(0, first - 1):]
        if not times:
            times = [since]
        times.append(time.monotonic())  # count a freeze that has not ended yet too
        return max((b - a for a, b in zip(times, times[1:])), default=0.0)

    def use_route(self, preference: str):
        self.connection.set_route_preference(preference)
        self.pump(4)

    def run(self, args) -> int:
        from telescope.phones import ROUTE_AUTO, ROUTE_USB, ROUTE_WIFI

        self.pump(3)
        failed = False
        starts = []
        for _ in range(args.starts):
            starts.append(self.start())
            self.stop()
        if not all(starts):
            failed = True
        if starts:
            print("Start to first frame: " + ", ".join(f"{s:.2f} s" if s else "no frame" for s in starts)
                  + (f" (median {statistics.median(starts):.2f} s)" if all(starts) else ""))

        if self.start():
            self.pump(2)
            since, bytes0 = time.monotonic(), self.worker._bytes_total
            self.pump(10)
            took = time.monotonic() - since
            count = len([t for t in self.frames if t >= since])
            mbps = (self.worker._bytes_total - bytes0) * 8 / took / 1e6
            print(f"Steady over {self.route()}: {count / took:.1f} fps, {mbps:.1f} Mbps, "
                  f"longest gap {self.longest_gap(since):.2f} s")
        else:
            failed = True
            print("Steady: no frames")
        self.stop()

        if args.wifi_drop:
            failed |= not self.wifi_drop(args.wifi_drop, ROUTE_WIFI)
        if args.handover:
            failed |= not self.handover()
        if args.thin_link:
            failed |= not self.thin_link(args)
        if args.wifi_drop or args.handover or args.thin_link:
            self.use_route(ROUTE_AUTO)
        self.win.close()
        return 1 if failed else 0

    def wifi_drop(self, seconds: float, wifi_route: str) -> bool:
        self.use_route(wifi_route)
        if not self.start():
            print("Wi-Fi drop: couldn't start over Wi-Fi")
            return False
        self.pump(3)
        if self.route() != "Wi-Fi":
            print(f"Wi-Fi drop: the stream went over {self.route()}, not Wi-Fi")
            self.stop()
            return False
        self.banners.clear()
        dropped = time.monotonic()
        adb_wifi(False)
        try:
            self.pump(seconds)
        finally:
            adb_wifi(True)
        back_on = time.monotonic()
        came_back = self.pump(30, until=lambda: self.frames[-1] > back_on)
        wrong = [b for b in self.banners if b == "Can't keep up"]
        if came_back:
            print(f"Wi-Fi drop of {seconds:.0f} s: longest gap {self.longest_gap(dropped):.1f} s, video back "
                  f"{self.frames[-1] - back_on:.1f} s after Wi-Fi returned"
                  + (", but it said Can't keep up meanwhile" if wrong else ""))
        else:
            print(f"Wi-Fi drop of {seconds:.0f} s: no video 30 s after Wi-Fi returned")
        self.stop()
        return came_back and not wrong

    def handover(self) -> bool:
        from telescope.phones import ROUTE_AUTO

        # Only discovery hides the cable, so this runs the real plug-in path without dropping adb or video
        with patch.object(self.connection._resolver, "_adb_device_states", return_value=[]):
            self.use_route(ROUTE_AUTO)
            if not self.start():
                print("Moving to USB: couldn't start over Wi-Fi")
                return False
            self.pump(3)
            if self.route() != "Wi-Fi":
                self.stop()
                print("Moving to USB: didn't begin on Wi-Fi")
                return False
        worker, moves = self.worker, []
        worker.moved.connect(moves.append)
        since = time.monotonic()
        self.connection._watch_usb()
        moved = self.pump(15, until=lambda: bool(moves))
        moved = moved and self.worker is worker and self.route() == "USB"
        took = time.monotonic() - since
        self.pump(3)
        if moved:
            print(f"Moving to USB: took {took:.1f} s, longest gap {self.longest_gap(since):.2f} s")
        else:
            print("Moving to USB: the stream didn't move within 15 s")
        self.stop()
        return moved


    def thin_link(self, args) -> bool:
        from telescope.phones import ROUTE_WIFI
        from telescope.udp_reader import UdpH264Reader

        self.use_route(ROUTE_WIFI)
        print(f"Thin link: {args.thin_link:g} Mbps, {args.loss:g}% loss, {args.buffer_ms:g} ms buffer, "
              f"{args.delay_ms:g} ms each way, {args.seconds:g} s per mode")
        shape = {"on": True}
        open_udp, open_tcp = UdpH264Reader._open_udp, h264_reader_class().open

        def shaped_udp(reader):
            ok = open_udp(reader)
            if ok and shape["on"]:
                rnd = random.Random(1)
                reader._sock = ShapedSocket(reader._sock,
                                            ThinLink(args.thin_link, args.loss / 100, args.buffer_ms, args.delay_ms, rnd.random),
                                            ThinLink(1000, args.loss / 100, 1000, args.delay_ms, rnd.random))
                self.reader = reader
            return ok

        def shaped_tcp(reader):
            ok = open_tcp(reader)
            if ok and shape["on"]:
                reader._response = Throttled(reader._response, args.thin_link)
                self.reader = reader
            return ok

        connection = self.connection
        hook = connection.on_stream_start

        def started(url, ctrl):
            self.ctrl = ctrl
            return hook(url, ctrl)

        UdpH264Reader._open_udp, h264_reader_class().open = shaped_udp, shaped_tcp
        connection.on_stream_start = started
        states = []
        ok = True
        try:
            for mode in args.modes.split(","):
                ok &= self.thin_run(mode.strip(), args, states)
        finally:
            UdpH264Reader._open_udp, h264_reader_class().open = open_udp, open_tcp
            del connection.on_stream_start
            self.connection._on_udp(False)
        return ok

    def thin_run(self, mode: str, args, states: list) -> bool:
        output = self.win._plugin("stream_output")
        cfg = output.get_config()
        output.set_config({**cfg, "format": "h264", "bitrate_mbps": -1 if mode.startswith("dynamic") else 0})
        self.connection._on_udp(mode.endswith("udp"))
        self.reader, self.ctrl = None, None
        import telescope.udp_reader as udp_reader
        udp_reader._UDP_FAILED.clear()  # a mode that collapsed on UDP shouldn't send the next one to TCP
        if not self.start():
            print(f"  {mode}: no frames")
            return False
        settle = min(15.0, args.seconds / 3)
        self.pump(settle)
        reader = self.reader
        udp = hasattr(reader, "_seal_in")
        if mode.endswith("udp") and not udp:
            print(f"  {mode}: the stream didn't go over UDP")
            self.stop()
            return False
        since, bytes0 = time.monotonic(), self.worker._bytes_total
        counts0 = (reader._received, reader._seal_in.highest) if udp else None
        self.thumbs = []
        probes = []
        brighter = False
        turns = 0
        polling = threading.Event()

        def poll():  # the app only reads the phone's state as a stream starts
            while not polling.wait(1.0):
                state = self.ctrl.get_state() if self.ctrl is not None else None
                if state:
                    states.append((time.monotonic(), state))
        threading.Thread(target=poll, daemon=True).start()
        while time.monotonic() - since < args.seconds - settle:
            self.pump(3)
            brighter = not brighter
            probes.append(time.monotonic())
            if args.emulator:
                subprocess.run(["adb", "emu", "rotate"], capture_output=True)
                turns += 1
            elif self.ctrl is not None:  # two stops brighter or back: a change the picture shows right away
                self.ctrl.send(action="ae_comp", value=4 if brighter else 0)
        self.pump(2)
        polling.set()
        took = time.monotonic() - since
        thumbs, self.thumbs = self.thumbs, None
        if self.ctrl is not None and brighter and not args.emulator:
            self.ctrl.send(action="ae_comp", value=0)
        for _ in range(-turns % 4):  # back the way it was
            subprocess.run(["adb", "emu", "rotate"], capture_output=True)
        mbps = (self.worker._bytes_total - bytes0) * 8 / took / 1e6
        rates = [s["bitrate"] for t, s in states if t >= since and s.get("bitrate")]
        lost = ""
        if udp:
            got, highest = reader._received - counts0[0], reader._seal_in.highest - counts0[1]
            lost = f", lost and asked again {100 * (1 - got / highest) if highest > 0 else 0:.1f}%"
        lags = change_lags(thumbs, probes)
        seen = [lag for lag in lags if lag is not None]
        lag = (f"lag {statistics.median(seen) * 1000:.0f} ms median, {max(seen) * 1000:.0f} ms worst"
               + (f" ({len(lags) - len(seen)} changes never showed)" if len(seen) < len(lags) else "")) if seen else "lag not measured"
        count = len([t for t in self.frames if t >= since])
        bitrate = f"phone at {statistics.median(rates) / 1e6:.1f} Mbps (last {rates[-1] / 1e6:.1f})" if rates else "phone bitrate unknown"
        print(f"  {mode}: {mbps:.1f} Mbps through, {bitrate}{lost}, {lag}, "
              f"{count / took:.1f} fps, longest gap {self.longest_gap(since):.2f} s")
        self.stop()
        return True


def h264_reader_class():
    from telescope.h264_reader import H264Reader
    return H264Reader


def thumb(frame):
    import cv2
    import numpy as np
    small = cv2.resize(frame, (32, 18), interpolation=cv2.INTER_AREA)
    return np.asarray(small, dtype=np.float32).mean(axis=2)


def change_lags(thumbs: list, probes: list) -> list:
    """Seconds from each change sent to the first frame that clearly changed, or None where none did within 3 s."""
    lags = []
    times = [t for t, _ in thumbs]
    for at in probes:
        i = bisect.bisect_left(times, at)
        if i == 0 or i >= len(thumbs):
            lags.append(None)
            continue
        before = thumbs[max(0, i - 15):i]
        noise = [abs(b[1] - a[1]).mean() for a, b in zip(before, before[1:])]
        limit = max(4.0, 4 * statistics.median(noise)) if noise else 4.0
        ref = thumbs[i - 1][1]
        lags.append(next((t - at for t, picture in thumbs[i:] if t - at <= 3 and abs(picture - ref).mean() > limit), None))
    return lags


class ThinLink:
    """A router that forwards mbps, holds buffer_ms of it, drops the rest, loses a share at random and adds a delay."""

    def __init__(self, mbps: float, loss: float = 0.0, buffer_ms: float = 100, delay_ms: float = 10, rnd=random.random):
        self.rate = mbps * 1e6 / 8
        self.loss, self.buffer, self.delay = loss, buffer_ms / 1000, delay_ms / 1000
        self._rnd = rnd
        self._free_at = 0.0  # when the router's line is free again
        self.queue = []  # (when it arrives, item), in order
        self.dropped = self.passed = 0

    def push(self, item, size: int, now: float):
        start = max(now, self._free_at)
        if self._rnd() < self.loss or start - now > self.buffer:
            self.dropped += 1
            return
        self._free_at = start + size / self.rate
        self.queue.append((self._free_at + self.delay, item))
        self.passed += 1

    def due(self, now: float) -> list:
        n = bisect.bisect_right(self.queue, now, key=lambda q: q[0])
        out, self.queue = [item for _t, item in self.queue[:n]], self.queue[n:]
        return out


class ShapedSocket:
    """The UDP reader's socket through a ThinLink each way, fed by a thread so the link sees when datagrams really came."""

    def __init__(self, sock, down: ThinLink, up: ThinLink):
        import socket
        self.sock, self.down, self.up = sock, down, up
        self._wake_out, self._wake_in = socket.socketpair()
        self._wake_in.setblocking(False)
        self._ready = []
        self._lock = threading.Lock()
        self._closed = False
        threading.Thread(target=self._run, daemon=True).start()

    def _run(self):
        while not self._closed:
            try:
                select_module.select([self.sock], [], [], 0.002)
            except (OSError, ValueError):
                return
            now = time.monotonic()
            with self._lock:
                while True:
                    try:
                        datagram, addr = self.sock.recvfrom(2048)
                    except (BlockingIOError, InterruptedError):
                        break
                    except OSError:
                        return
                    self.down.push((datagram, addr), len(datagram) + 28, now)
                due = self.down.due(now)
                self._ready += due
                for datagram, addr in self.up.due(now):
                    try:
                        self.sock.sendto(datagram, addr)
                    except OSError:
                        pass
            if due:
                try:
                    self._wake_out.send(b"x")
                except OSError:
                    pass

    def fileno(self):
        return self._wake_in.fileno()  # what udp_reader's select waits on

    def recvfrom(self, _size):
        try:
            while self._wake_in.recv(4096):
                pass
        except (BlockingIOError, InterruptedError):
            pass
        with self._lock:
            if not self._ready:
                raise BlockingIOError
            return self._ready.pop(0)

    def sendto(self, datagram, addr):
        with self._lock:
            self.up.push((datagram, addr), len(datagram) + 28, time.monotonic())

    def close(self):
        self._closed = True
        for s in (self.sock, self._wake_out, self._wake_in):
            s.close()

    def __getattr__(self, name):
        return getattr(self.sock, name)


class Throttled:
    """A TCP response read no faster than mbps, so the phone's own queue fills up as it would on a thin link."""

    def __init__(self, response, mbps: float):
        self._response, self._rate = response, mbps * 1e6 / 8
        self._start, self._taken = time.monotonic(), 0
        try:  # a small receive buffer, or the kernel would hold seconds of video between the phone and the limit
            import socket
            response.fp.raw._sock.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 32 * 1024)
        except Exception:
            pass

    def read1(self, n: int = -1):
        chunk = self._response.read1(min(n, 16 * 1024) if n and n > 0 else 16 * 1024)
        self._taken += len(chunk)
        wait = self._start + self._taken / self._rate - time.monotonic()
        if wait < -0.05:  # the phone sent less meanwhile: an idle link doesn't bank speed for later
            self._start, self._taken = time.monotonic() - 0.05, int(0.05 * self._rate)
        if wait > 0:
            time.sleep(wait)
        return chunk

    def __getattr__(self, name):
        return getattr(self._response, name)


def adb_wifi(on: bool):
    subprocess.run(["adb", "shell", "cmd", "wifi", "set-wifi-enabled", "enabled" if on else "disabled"],
                   check=True, capture_output=True)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
