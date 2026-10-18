#!/usr/bin/env python3
"""Check a release on a real phone in a few minutes: start time, a steady stream, a Wi-Fi drop and moving to USB.

Runs this checkout's app offscreen with a copy of the dev profile, so pair the dev phone app with
`python scripts/dev_desktop.py --keep` first. It prints numbers only, no names, addresses or serials.

Usage: python scripts/phone_check.py [--starts N] [--wifi-drop SECONDS] [--handover]
  --wifi-drop  streams over Wi-Fi and turns the phone's Wi-Fi off for that long over adb (needs the USB cable)
  --handover   streams over Wi-Fi, then switches Connect via to USB and measures the gap (needs the USB cable)
"""

import argparse
import getpass
import os
import shutil
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

DESKTOP = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(DESKTOP))


def kept_profile() -> Path:
    return Path(tempfile.gettempdir()) / f"telescope-dev-profile-{getpass.getuser()}"


def main(argv) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--starts", type=int, default=3)
    parser.add_argument("--wifi-drop", type=float, default=0)
    parser.add_argument("--handover", action="store_true")
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

        def counted_publish(worker, *a, **k):
            check.frames.append(time.monotonic())
            check.worker = worker
            return publish(worker, *a, **k)

        def noted_issue(win, key, issue):
            check.banners.append(issue.title)
            return show_issue(win, key, issue)

        stream.StreamWorker._publish = counted_publish
        TelescopeWindow.show_issue = noted_issue
        self.worker = None
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
        times = [t for t in self.frames if t >= since]
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
            failed |= not self.handover(ROUTE_WIFI, ROUTE_USB)
        if args.wifi_drop or args.handover:
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

    def handover(self, wifi_route: str, usb_route: str) -> bool:
        self.use_route(wifi_route)
        if not self.start():
            print("Moving to USB: couldn't start over Wi-Fi")
            return False
        self.pump(3)
        since = time.monotonic()
        self.connection.set_route_preference(usb_route)
        moved = self.pump(15, until=lambda: self.route() == "USB")
        took = time.monotonic() - since
        self.pump(3)
        if moved:
            print(f"Moving to USB: took {took:.1f} s, longest gap {self.longest_gap(since):.2f} s")
        else:
            print("Moving to USB: the stream didn't move within 15 s")
        self.stop()
        return moved


def adb_wifi(on: bool):
    subprocess.run(["adb", "shell", "cmd", "wifi", "set-wifi-enabled", "enabled" if on else "disabled"],
                   check=True, capture_output=True)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
