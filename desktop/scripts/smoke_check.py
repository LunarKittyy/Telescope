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
import ssl
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
    from PyQt6 import sip
    from PyQt6.QtWidgets import QApplication

    from telescope.app import TelescopeWindow
    from telescope.plugins.background import BackgroundPlugin
    from telescope.plugins.browser_camera import BrowserCameraPlugin
    from telescope.plugins.camera_control import CameraControlPlugin
    from telescope.plugins.cinema import CinemaPlugin
    from telescope.plugins.connection import ConnectionPlugin
    from telescope.plugins.framing import FramingPlugin
    from telescope.plugins.microphone import MicrophonePlugin
    from telescope.plugins.monitoring import MonitoringPlugin
    from telescope.plugins.presets import PresetsPlugin
    from telescope.plugins.preview import PreviewPlugin
    from telescope.plugins.setup import SetupPlugin
    from telescope.plugins.shortcuts import ShortcutsPlugin
    from telescope.plugins.stream_output import StreamOutputPlugin
    from telescope.plugins.transforms import TransformsPlugin
    from telescope.plugins.startup import StartupPlugin
    from telescope.plugins.updates import UpdatesPlugin

    app = QApplication.instance() or QApplication([])
    win = TelescopeWindow()
    try:
        for plugin_cls in (
            SetupPlugin, ConnectionPlugin, CameraControlPlugin, BrowserCameraPlugin, CinemaPlugin, BackgroundPlugin,
            StreamOutputPlugin,
            TransformsPlugin, FramingPlugin, MicrophonePlugin, PresetsPlugin, PreviewPlugin, MonitoringPlugin, UpdatesPlugin,
            ShortcutsPlugin, StartupPlugin,
        ):
            win.register_plugin(plugin_cls())
        win.apply_saved_config()
        app.processEvents()  # the shortcuts plugin starts (and asks for the system's global shortcuts) from the loop
        shortcuts = win._plugin("shortcuts")
        detail = f"{len(shortcuts.actions())} shortcut actions, global shortcuts via " \
                 f"{type(shortcuts.backend).__name__ if shortcuts.backend else 'nothing'}"
        return f"{len(win._plugins)} plugins registered, {detail}"
    finally:
        try:
            win._shut_down()
        finally:
            app.processEvents()
            sip.delete(win)  # Qt must outlive the window and its signal receivers.


def check_background_speed():
    import time

    import cv2
    import numpy as np

    from telescope.plugins.background import blurred, compose, soften

    cv2.setNumThreads(2)  # as the app runs it (main.py)
    frame = np.random.default_rng(0).integers(0, 255, (1080, 1920, 3), np.uint8)
    mask = np.zeros((1080, 1920), np.uint8)
    mask[:, :900] = 255
    times = []
    for _ in range(30):
        t0 = time.perf_counter()
        compose(frame, soften(mask, 3), blurred(frame, 6, mask))
        times.append((time.perf_counter() - t0) * 1000)
    median = sorted(times)[len(times) // 2]
    if median > 40:  # far past a 33 ms frame at 30 fps: something made it a lot slower
        raise RuntimeError(f"a 1080p background blur took {median:.1f} ms")
    return f"1080p blur without the model: {median:.1f} ms"


def check_colour_speed():
    import time

    import cv2
    import numpy as np

    from telescope.colour import ColourStage, bake
    from telescope.grade import Grade

    cv2.setNumThreads(2)  # as the app runs it (main.py)
    frame = np.random.default_rng(0).integers(0, 255, (1080, 1920, 3), np.uint8)
    lut = bake([ColourStage(Grade(temperature=20, saturation=1.2, shadows=10))])
    times = []
    for _ in range(30):
        t0 = time.perf_counter()
        lut.apply(frame)
        times.append((time.perf_counter() - t0) * 1000)
    median = sorted(times)[len(times) // 2]
    if median > 90:  # busy CI runners take 37 to 54 ms; numpy instead of OpenCV would be around 300
        raise RuntimeError(f"a 1080p 3D LUT took {median:.1f} ms on {os.cpu_count()} processors")
    return f"1080p 3D LUT: {median:.1f} ms on {os.cpu_count()} processors"


def check_cinema_speed():
    import time

    import cv2
    import numpy as np

    from telescope import scopes
    from telescope.plugins.cinema import CinemaLayer, Look

    cv2.setNumThreads(2)
    frame = cv2.GaussianBlur(np.random.default_rng(0).integers(0, 255, (540, 960, 3), np.uint8), (0, 0), 2)
    layer = CinemaLayer()
    layer.look = Look(peaking=True, zebras=True, false_colour=True)
    tints, scope_times = [], []
    for _ in range(30):
        t0 = time.perf_counter()
        layer.tint(frame.copy())
        tints.append((time.perf_counter() - t0) * 1000)
    for name in scopes.SCOPES:
        t0 = time.perf_counter()
        scopes.scope_image(name, frame)
        scope_times.append((time.perf_counter() - t0) * 1000)
    tint = sorted(tints)[len(tints) // 2]
    if tint > 15 or max(scope_times) > 40:  # the tint runs per preview frame; scopes 12 times a second off the stream
        raise RuntimeError(f"peaking, zebras and false colour took {tint:.1f} ms, the slowest scope {max(scope_times):.1f} ms")
    return f"all three on the preview: {tint:.1f} ms, slowest scope {max(scope_times):.1f} ms"


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
        from telescope.platform.windows import uc_is_registered, use_mf
        return f"windows11_camera={use_mf(0)}, unitycapture_registered={uc_is_registered()}"
    return "unsupported platform - skipped"


def check_authenticated_stream_round_trip():
    import cv2
    import numpy as np

    from telescope.mjpeg_reader import MjpegReader
    from telescope.pinned_https import PhoneAuth, fingerprint

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

    fixtures = Path(__file__).resolve().parent.parent / "tests" / "fixtures"
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    ctx.minimum_version = ssl.TLSVersion.TLSv1_2
    ctx.load_cert_chain(fixtures / "test_phone.crt", fixtures / "test_phone.key")
    pin = fingerprint(ssl.PEM_cert_to_DER_cert((fixtures / "test_phone.crt").read_text()))
    wrong_pin = fingerprint(ssl.PEM_cert_to_DER_cert((fixtures / "test_impostor.crt").read_text()))

    server = http.server.HTTPServer(("127.0.0.1", 0), _Handler)
    server.handle_error = lambda *_args: None  # the wrong-certificate client hangs up mid-handshake on purpose
    server.socket = ctx.wrap_socket(server.socket, server_side=True)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        url = f"https://127.0.0.1:{server.server_address[1]}/v1/video"
        reader = MjpegReader(url, PhoneAuth(token, pin))
        assert reader.open(), "authenticated open() failed"
        ok, decoded = reader.read()
        assert ok and decoded is not None, "failed to read/decode the streamed frame"
        reader.release()

        # A wrong token, or the right token to a certificate that isn't the paired one, must fail.
        assert not MjpegReader(url, PhoneAuth("wrong-token", pin)).open(), "unauthenticated request was accepted"
        assert not MjpegReader(url, PhoneAuth(token, wrong_pin)).open(), "an unpinned certificate was accepted"
    finally:
        server.shutdown()
        thread.join(timeout=2)
    return "pinned TLS frame round-tripped; wrong token and wrong certificate rejected"


def check_browser_camera_round_trip():
    """The page and its certificate work in this bundle (web files, cryptography and aiortc packaged), and video sent
    over WebRTC the way the page sends it, set up over the WebSocket, comes out of the reader."""
    import asyncio
    import base64
    import http.client
    import json
    import socket
    import struct
    import tempfile

    import av
    import numpy as np
    from aiortc import RTCConfiguration, RTCPeerConnection, RTCSessionDescription
    from aiortc.mediastreams import VideoStreamTrack

    from telescope import browser_server as bs

    class Grey(VideoStreamTrack):
        async def recv(self):
            pts, time_base = await self.next_timestamp()
            frame = av.VideoFrame.from_ndarray(np.full((48, 64, 3), 128, np.uint8), format="bgr24")
            frame.pts, frame.time_base = pts, time_base
            return frame

    loop = asyncio.new_event_loop()
    threading.Thread(target=loop.run_forever, daemon=True).start()

    def run(coro):
        return asyncio.run_coroutine_threadsafe(coro, loop).result(10)

    async def offer():
        pc = RTCPeerConnection(RTCConfiguration(iceServers=[]))
        pc.addTransceiver(Grey(), direction="sendonly")
        await pc.setLocalDescription(await pc.createOffer())
        return pc

    def send_text(sock, text: str):
        payload, mask = text.encode(), b"\x01\x02\x03\x04"
        head = struct.pack("!BB", 0x81, 0x80 | len(payload)) if len(payload) < 126 else \
            struct.pack("!BBH", 0x81, 0x80 | 126, len(payload))
        sock.sendall(head + mask + bs.unmask(payload, mask))

    def take(sock, n: int) -> bytes:
        data = b""
        while len(data) < n:
            data += sock.recv(n - len(data))
        return data

    def recv_text(sock) -> dict:
        while True:
            b0, b1 = take(sock, 2)
            n = b1 & 0x7F
            if n == 126:
                n = struct.unpack("!H", take(sock, 2))[0]
            data = take(sock, n)
            if b0 & 0x0F == 0x1:
                return json.loads(data)

    peer = run(offer())
    with tempfile.TemporaryDirectory() as folder:
        cert, key = bs.ensure_certificate(Path(folder), ["127.0.0.1"])
        feed = bs.BrowserFeed()
        server = bs.BrowserServer(feed, cert, key, port=0, host="127.0.0.1")
        server.start()
        try:
            ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
            ctx.minimum_version = ssl.TLSVersion.TLSv1_2
            ctx.check_hostname = False
            ctx.verify_mode = ssl.CERT_NONE
            conn = http.client.HTTPSConnection("127.0.0.1", server.port, context=ctx, timeout=5)
            for path in ("/", "/app.js"):
                conn.request("GET", path)
                resp = conn.getresponse()
                assert resp.status == 200 and resp.read(), f"{path} isn't served"
            conn.close()

            sock = ctx.wrap_socket(socket.create_connection(("127.0.0.1", server.port), timeout=10))
            ws_key = base64.b64encode(b"smoke-check-key!").decode()
            sock.sendall((f"GET /ws?token={server.token} HTTP/1.1\r\nHost: x\r\nUpgrade: websocket\r\n"
                          f"Connection: Upgrade\r\nSec-WebSocket-Key: {ws_key}\r\nSec-WebSocket-Version: 13\r\n\r\n").encode())
            head = b""
            while b"\r\n\r\n" not in head:
                head += sock.recv(1)
            assert head.startswith(b"HTTP/1.1 101") and bs.ws_accept_key(ws_key).encode() in head, "no WebSocket"
            assert recv_text(sock)["type"] == "config", "no settings for the page"
            send_text(sock, json.dumps({"type": "offer", "sdp": peer.localDescription.sdp}))
            answer = recv_text(sock)
            assert answer["type"] == "answer", f"the offer wasn't answered: {answer}"
            run(peer.setRemoteDescription(RTCSessionDescription(answer["sdp"], "answer")))
            reader = bs.BrowserReader(feed)
            assert reader.open(), "the browser didn't connect"
            ok, frame = reader.read()
            assert ok and frame.shape == (48, 64, 3), "the video didn't come through"
            sock.close()
        finally:
            server.stop()
            run(peer.close())
            loop.call_soon_threadsafe(loop.stop)
    return f"page served over TLS; an offer was answered and {feed.codec} video reached the reader"


def main() -> int:
    _check("Application constructs and registers all plugins", check_app_construction)
    _check("ADB discovery runs without crashing", check_adb_discovery)
    _check("Virtual-camera availability check runs without crashing", check_virtual_camera_availability)
    _check("Authenticated MJPEG stream round-trip", check_authenticated_stream_round_trip)
    _check("Browser camera round-trip", check_browser_camera_round_trip)
    _check("Background blur keeps up at 1080p", check_background_speed)
    _check("Colour LUT keeps up at 1080p", check_colour_speed)
    _check("Cinema tools keep up on the preview", check_cinema_speed)

    print()
    if _FAILURES:
        print(f"{len(_FAILURES)} smoke check(s) failed: {', '.join(_FAILURES)}")
        return 1
    print("All smoke checks passed.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
