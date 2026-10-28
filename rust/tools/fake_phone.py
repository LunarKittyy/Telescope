#!/usr/bin/env python3
"""A stand-in phone for measuring desktops: the session and stream ports of docs/protocol.md over TLS, serving a
pre-encoded Annex-B clip at its frame rate as /v1/video.h264. It's already paired: it accepts one token and prints
the phone entry (id, token, certificate pin) to put in a desktop's config.

  python fake_phone.py CLIP.h264 [--host 127.0.0.2] [--fps 30] [--token TOKEN]

Encode the clip with an access unit delimiter per frame and SPS/PPS on every keyframe, like the phone sends, e.g.
  ffmpeg -f lavfi -i "testsrc2=size=1920x1080:rate=30,noise=alls=12:allf=t" -t 60 -c:v libx264 -profile:v high
         -bf 0 -g 30 -b:v 8M -x264-params aud=1:repeat-headers=1 -f h264 clip.h264
"""

import argparse
import hashlib
import json
import secrets
import ssl
import subprocess
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

PHONE_ID = "fake-phone-0000-0000-0000-000000000001"
PHONE_NAME = "Fake phone"
SESSION_PROTOCOL = 3


def access_units(data: bytes) -> list:
    """The clip split at each access unit delimiter (00 00 00 01 09)."""
    marks = []
    i = data.find(b"\x00\x00\x00\x01\x09")
    while i != -1:
        marks.append(i)
        i = data.find(b"\x00\x00\x00\x01\x09", i + 5)
    marks.append(len(data))
    return [data[a:b] for a, b in zip(marks, marks[1:])]


def make_cert(folder: Path) -> tuple:
    key, cert = folder / "key.pem", folder / "cert.pem"
    subprocess.run(["openssl", "req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256", "-nodes",
                    "-keyout", str(key), "-out", str(cert), "-days", "30", "-subj", "/CN=Telescope"],
                   check=True, capture_output=True)
    der = ssl.PEM_cert_to_DER_cert(cert.read_text())
    return str(cert), str(key), hashlib.sha256(der).hexdigest()


class Phone:
    def __init__(self, units: list, fps: float, token: str, version: str):
        self.units, self.fps, self.token, self.version = units, fps, token, version
        self.streaming = False
        # The protocol's own full example (docs/state.json), streaming H.264 at 1080p30
        self.state = json.loads((Path(__file__).resolve().parents[2] / "docs" / "state.json").read_text())
        # Fields that report trouble (a failed codec, heat, a taken camera, notes) stay out, as on a phone with none
        for key in [k for k in self.state if k.startswith(("codec_error", "camera_error", "heat_", "notes", "dynamic_fps"))]:
            del self.state[key]
        for key in ("codec_unsupported", "camera_taken", "camera_off", "low_light"):
            self.state[key] = False
        self.state.update({"auto": True, "wb_manual": False, "iso": None, "shutter_ns": None, "focus_mode": "continuous",
                           "jpeg_quality": 85, "battery_temp_c": 31.0, "charging": True})
        self.state.update({"codec": "h264", "codecs": ["mjpeg", "h264"], "stream_width": 1920, "stream_height": 1080,
                           "phone_fps": int(fps), "camera_fps": float(fps), "bitrate": 8000000})


def handler_for(phone: Phone):
    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *_args):
            pass

        def _json(self, body, status=200):
            raw = json.dumps(body).encode()
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(raw)))
            self.send_header("Connection", "close")
            self.end_headers()
            self.wfile.write(raw)
            self.close_connection = True

        def _authorized(self) -> bool:
            if self.headers.get("Authorization") == f"Bearer {phone.token}":
                return True
            self._json({"error": "Unauthorized"}, 401)
            return False

        def _body(self) -> dict:
            length = int(self.headers.get("Content-Length") or 0)
            try:
                return json.loads(self.rfile.read(length) or b"{}")
            except ValueError:
                return {}

        def do_GET(self):
            path = self.path.split("?")[0]
            if path == "/v1/hello":
                return self._json({"protocol": SESSION_PROTOCOL, "phoneId": PHONE_ID, "phoneName": PHONE_NAME,
                                   "appVersion": phone.version, "build": 1})
            if not self._authorized():
                return
            if path == "/v1/ping":
                return self._json({"protocol": SESSION_PROTOCOL, "streaming": phone.streaming, "busy": False,
                                   "localOnly": False, "phoneId": PHONE_ID, "phoneName": PHONE_NAME})
            if path == "/v1/state" and phone.streaming:
                return self._json(phone.state)
            if path == "/v1/video.h264" and phone.streaming:
                return self._video()
            self._json({"error": "Not Found"}, 404)

        def do_POST(self):
            path = self.path.split("?")[0]
            if not self._authorized():
                return
            body = self._body()
            if path == "/v1/session":
                phone.streaming = body.get("action") == "start"
                return self._json({"ok": True})
            if path == "/v1/control":
                action, value = body.get("action"), body.get("value")
                keys = {"ois": "ois", "torch": "torch", "black_level_lock": "black_level_lock", "nr_mode": "nr_mode",
                        "edge_mode": "edge_mode", "ae_comp": "ae_comp", "focus_mode": "focus_mode"}
                if action in keys:
                    phone.state[keys[action]] = (str(value) == "1") if action in ("ois", "torch", "black_level_lock") else value
                elif action == "auto":
                    phone.state["auto"] = True
                elif action in ("iso", "shutter"):
                    phone.state["auto"] = False
                    phone.state["iso" if action == "iso" else "shutter_ns"] = value
                return self._json({"ok": True})
            if path == "/v1/unpair":
                return self._json({"ok": True})
            self._json({"error": "Not Found"}, 404)

        def _video(self):
            self.send_response(200)
            self.send_header("Content-Type", "video/h264")
            self.send_header("Cache-Control", "no-cache")
            self.send_header("Connection", "close")
            self.end_headers()
            self.close_connection = True
            # Start at a keyframe (the first unit carries SPS/PPS), then keep the clip's rate, looping it
            period, start, n = 1.0 / phone.fps, time.monotonic(), 0
            try:
                while phone.streaming:
                    self.wfile.write(phone.units[n % len(phone.units)])
                    self.wfile.flush()
                    n += 1
                    wait = start + n * period - time.monotonic()
                    if wait > 0:
                        time.sleep(wait)
            except OSError:
                pass

    return Handler


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("clip")
    parser.add_argument("--host", default="127.0.0.2")
    parser.add_argument("--fps", type=float, default=30)
    parser.add_argument("--token", default="")
    args = parser.parse_args()
    units = access_units(Path(args.clip).read_bytes())
    version = (Path(__file__).resolve().parents[2] / "VERSION").read_text().strip()
    phone = Phone(units, args.fps, args.token or secrets.token_urlsafe(32), version)
    folder = Path(tempfile.mkdtemp(prefix="fake-phone-"))
    cert, key, pin = make_cert(folder)
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    ctx.load_cert_chain(cert, key)
    for port in (8766, 8080):
        server = ThreadingHTTPServer((args.host, port), handler_for(phone))
        server.daemon_threads = True
        server.socket = ctx.wrap_socket(server.socket, server_side=True)
        threading.Thread(target=server.serve_forever, daemon=True).start()
    print(json.dumps({"id": PHONE_ID, "name": PHONE_NAME, "token": phone.token, "ips": [args.host],
                      "active_ip": args.host, "cert_sha256": pin}), flush=True)
    print(f"{len(units)} frames at {args.fps:g} fps on {args.host}:8766 and :8080", flush=True)
    threading.Event().wait()


if __name__ == "__main__":
    main()
