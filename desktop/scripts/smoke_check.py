#!/usr/bin/env python3
"""Packaging smoke checks, run against a built/installed bundle (or the
source tree directly) before publishing a release.

Not a substitute for the pytest suite - this exercises the things pytest
can't easily cover: that the app actually constructs end-to-end under
whatever Qt platform plugin is really available, that the ADB/virtual-
camera detection code paths run without crashing on this machine, and that
a real authenticated MJPEG round-trip (auth header, multipart framing,
JPEG decode) works against a local server. A failure or "not available" is
expected on a CI runner with no phone/ADB/v4l2 present - what a red exit
code here is guarding against is a crash, not a missing runtime dependency.

Usage: python scripts/smoke_check.py
"""

import http.server
import os
import sys
import threading
from pathlib import Path

os.environ.setdefault("QT_QPA_PLATFORM", "offscreen")

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

_FAILURES: list[str] = []


def _check(name: str, fn):
    try:
        detail = fn()
        print(f"[ok]   {name}" + (f" - {detail}" if detail else ""))
    except Exception as exc:  # noqa: BLE001 - a smoke check must never propagate a raw traceback silently
        _FAILURES.append(name)
        print(f"[FAIL] {name} - {type(exc).__name__}: {exc}")


def check_app_construction():
    from PyQt6.QtWidgets import QApplication

    from telescope.app import TelescopeWindow
    from telescope.plugins.camera_control import CameraControlPlugin
    from telescope.plugins.connection import ConnectionPlugin
    from telescope.plugins.monitoring import MonitoringPlugin
    from telescope.plugins.preview import PreviewPlugin
    from telescope.plugins.setup import SetupPlugin
    from telescope.plugins.stream_output import StreamOutputPlugin
    from telescope.plugins.transforms import TransformsPlugin

    app = QApplication.instance() or QApplication([])
    win = TelescopeWindow()
    for plugin_cls in (
        SetupPlugin, ConnectionPlugin, CameraControlPlugin, StreamOutputPlugin,
        TransformsPlugin, PreviewPlugin, MonitoringPlugin,
    ):
        win.register_plugin(plugin_cls())
    win.apply_saved_config()
    return f"{len(win._plugins)} plugins registered"


def check_adb_discovery():
    from telescope.platform import adb_available, adb_devices

    available = adb_available()
    devices = adb_devices() if available else []
    return f"available={available}, devices={len(devices)}"


def check_virtual_camera_availability():
    from telescope.platform import IS_LINUX, IS_WINDOWS

    if IS_LINUX:
        from telescope.platform.linux import v4l2_devices_ready, v4l2_module_loaded
        return f"module_loaded={v4l2_module_loaded()}, devices_ready={v4l2_devices_ready()}"
    if IS_WINDOWS:
        from telescope.platform.windows import uc_is_registered
        return f"unitycapture_registered={uc_is_registered()}"
    return "unsupported platform - skipped"


def check_authenticated_stream_round_trip():
    import cv2
    import numpy as np

    from telescope.mjpeg_reader import MjpegReader

    frame = np.zeros((2, 2, 3), dtype=np.uint8)
    ok, buf = cv2.imencode(".jpg", frame)
    assert ok, "failed to encode the smoke-test JPEG"
    jpeg = buf.tobytes()
    token = "smoke-test-token"

    class _Handler(http.server.BaseHTTPRequestHandler):
        def do_GET(self):
            if self.headers.get("Authorization") != f"Bearer {token}":
                self.send_response(401)
                self.end_headers()
                return
            self.send_response(200)
            self.send_header("Content-Type", "multipart/x-mixed-replace; boundary=--mjpegframe")
            self.end_headers()
            self.wfile.write(b"--mjpegframe\r\n")
            self.wfile.write(b"Content-Type: image/jpeg\r\n")
            self.wfile.write(f"Content-Length: {len(jpeg)}\r\n\r\n".encode())
            self.wfile.write(jpeg)
            self.wfile.write(b"\r\n")

        def log_message(self, *args):
            pass

    server = http.server.HTTPServer(("127.0.0.1", 0), _Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        port = server.server_address[1]
        reader = MjpegReader(f"http://127.0.0.1:{port}/v1/video", token)
        assert reader.open(), "authenticated open() failed"
        ok, decoded = reader.read()
        assert ok and decoded is not None, "failed to read/decode the streamed frame"
        reader.release()

        # An unauthenticated request must be rejected, not silently accepted.
        unauth = MjpegReader(f"http://127.0.0.1:{port}/v1/video", "wrong-token")
        assert not unauth.open(), "unauthenticated request was accepted"
    finally:
        server.shutdown()
        thread.join(timeout=2)
    return "authenticated frame round-tripped, unauthenticated request rejected"


def main() -> int:
    _check("Application constructs and registers all plugins", check_app_construction)
    _check("ADB discovery runs without crashing", check_adb_discovery)
    _check("Virtual-camera availability check runs without crashing", check_virtual_camera_availability)
    _check("Authenticated MJPEG stream round-trip", check_authenticated_stream_round_trip)

    print()
    if _FAILURES:
        print(f"{len(_FAILURES)} smoke check(s) failed: {', '.join(_FAILURES)}")
        return 1
    print("All smoke checks passed.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
