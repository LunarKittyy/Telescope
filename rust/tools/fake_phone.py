#!/usr/bin/env python3
"""A stand-in phone for measuring desktops: the session and stream ports of docs/protocol.md over TLS, serving a
pre-encoded Annex-B clip at its frame rate as /v1/video.h264. It's already paired: it accepts one token and prints
the phone entry (id, token, certificate pin) to put in a desktop's config. With --record it writes every request a
desktop sends, one JSON line each (rust/parity/phone_session.py drives a desktop against it).

For scripted sessions it can also pair from a desktop's QR offer (pair()), switch between its two lenses, drop off
the network for a while (drop()) and be plugged in over USB (plug()), which fake_adb.py answers for.

  python fake_phone.py [CLIP.h264] [--host 127.0.0.2] [--fps 30] [--token TOKEN] [--record FILE]

Without a clip it makes a short test pattern with ffmpeg.
Encode the clip with an access unit delimiter per frame and SPS/PPS on every keyframe, like the phone sends, e.g.
  ffmpeg -f lavfi -i "testsrc2=size=1920x1080:rate=30,noise=alls=12:allf=t" -t 60 -c:v libx264 -profile:v high
         -bf 0 -g 30 -b:v 8M -x264-params aud=1:repeat-headers=1 -f h264 clip.h264
"""

import argparse
import contextlib
import hashlib
import hmac
import http.client
import json
import os
import secrets
import socket
import ssl
import subprocess
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlsplit

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


def make_clip(path: Path, fps: float = 30):
    """Two seconds of 640x360 test pattern, encoded the way the phone sends H.264."""
    subprocess.run(["ffmpeg", "-v", "error", "-y", "-f", "lavfi", "-i", f"testsrc2=size=640x360:rate={fps:g}", "-t", "2",
                    "-c:v", "libx264", "-profile:v", "high", "-bf", "0", "-g", str(int(fps)), "-b:v", "1M",
                    "-x264-params", "aud=1:repeat-headers=1", "-f", "h264", str(path)], check=True)


class Recorder:
    """Every request a desktop sends, one JSON line each: seconds since the start, port, method, path, query, body
    and how it came (via "wifi" or "usb"). Opened for appending, so fake_adb.py can add its own lines in between."""

    def __init__(self, path: Path):
        path.write_text("", encoding="utf-8")
        self.path = path
        self.file = path.open("a", encoding="utf-8")
        self.start = time.monotonic()
        self.lock = threading.Lock()

    def __call__(self, port: int, method: str, target: str, body, via: str = "wifi"):
        parts = urlsplit(target)
        line = {"t": round(time.monotonic() - self.start, 3), "port": port, "method": method, "path": parts.path,
                "query": {k: v[0] if len(v) == 1 else v for k, v in parse_qs(parts.query).items()}, "body": body,
                "via": via}
        self.write(line)

    def mark(self, name: str):
        self.write({"t": round(time.monotonic() - self.start, 3), "mark": name})

    def write(self, line: dict):
        with self.lock:
            self.file.write(json.dumps(line, ensure_ascii=False) + "\n")
            self.file.flush()

    def close(self):
        with self.lock:
            self.file.close()


def make_cert(folder: Path) -> tuple:
    key, cert = folder / "key.pem", folder / "cert.pem"
    subprocess.run(["openssl", "req", "-x509", "-newkey", "ec", "-pkeyopt", "ec_paramgen_curve:P-256", "-nodes",
                    "-keyout", str(key), "-out", str(cert), "-days", "30", "-subj", "/CN=Telescope"],
                   check=True, capture_output=True)
    der = ssl.PEM_cert_to_DER_cert(cert.read_text())
    return str(cert), str(key), hashlib.sha256(der).hexdigest()


# A second lens, so a desktop has something to switch to: the first one's entry with these changes
ULTRA_WIDE = {"id": "3", "label": "Back ~13mm [phys]", "focalMm": 13, "current": False, "hasOis": False,
              "isoMax": 1600, "shutterMaxNs": 500000000, "supportsManualFocus": False, "minFocusDistance": 0.0,
              "supportsFlash": False, "supportsFocusPoint": False, "zoomRatioMax": 4.0, "lensZooms": [], "maxFps": 30}


def pairing_proof(token: str, nonce: str, phone_id: str, cert_sha256: str) -> str:
    """What the phone's pairingProof() sends (desktop/telescope/pairing.py checks it)."""
    message = f"telescope-pair-v4\n{nonce}\n{phone_id}\n{cert_sha256}".encode()
    return hmac.new(token.encode(), message, hashlib.sha256).hexdigest()


class Phone:
    def __init__(self, units: list, fps: float, token: str, version: str, record=None):
        self.units, self.fps, self.token, self.version = units, fps, token, version
        self.record = record or (lambda *_args, **_kw: None)
        self.streaming = False
        self.down_until = 0.0  # off the network until then: every connection is cut without an answer
        self.plugged = False
        self.pin = ""
        self.adb_state = None  # where fake_adb.py reads what's plugged in and which ports forward where
        self.usb_ports = {}
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
        self.state["cameras"] = self.state["cameras"] + [{**self.state["cameras"][0], **ULTRA_WIDE}]

    @property
    def down(self) -> bool:
        return time.monotonic() < self.down_until

    def drop(self, seconds: float = None):
        """Off the network for seconds (until back() without), as when Wi-Fi drops: open streams end and nothing
        answers meanwhile."""
        self.down_until = float("inf") if seconds is None else time.monotonic() + seconds

    def back(self):
        self.down_until = 0.0

    def switch_lens(self, lens_id: str):
        for cam in self.state["cameras"]:
            cam["current"] = cam["id"] == lens_id

    def plug(self, plugged: bool = True):
        """Plugged in over USB (fake_adb.py lists it and forwards to it) or pulled out (USB streams end)."""
        self.plugged = plugged
        self.write_adb_state()

    def write_adb_state(self):
        if self.adb_state is None:
            return
        record = getattr(self.record, "path", None)
        state = {"devices": [["FAKE0001", "device", PHONE_NAME]] if self.plugged else [],
                 "ports": {str(k): v for k, v in self.usb_ports.items()},
                 "record": str(record) if record else None, "start": getattr(self.record, "start", time.monotonic())}
        tmp = self.adb_state.with_suffix(".tmp")
        tmp.write_text(json.dumps(state), encoding="utf-8")
        os.replace(tmp, self.adb_state)

    def gone(self, via: str) -> bool:
        return self.down or (via == "usb" and not self.plugged)

    def pair(self, offer: dict, host: str) -> int:
        """Answers a desktop's QR offer the way the phone does after scanning it: POST /pair/{nonce} from host with
        a proof of the token. Takes the token on success and returns the desktop's HTTP status."""
        nonce, token = offer["nonce"], offer["token"]
        body = {"name": PHONE_NAME, "phone_id": PHONE_ID, "cert_sha256": self.pin,
                "proof": pairing_proof(token, nonce, PHONE_ID, self.pin), "ips": [host]}
        raw = json.dumps(body).encode()
        conn = http.client.HTTPConnection("127.0.0.1", int(offer["port"]), timeout=10, source_address=(host, 0))
        try:
            conn.request("POST", f"/pair/{nonce}", raw, {"Content-Type": "application/json"})
            status = conn.getresponse().status
        finally:
            conn.close()
        if status == 200:
            self.token = token
        # What's compared: the offer's fields and version, and the desktop's answer (the nonce and token change)
        self.record(8765, "PAIR", "/pair", {"status": status, "version": offer.get("version"),
                                            "offer": sorted(offer)}, via="wifi")
        return status


def handler_for(phone: Phone, port: int = None, via: str = "wifi"):
    """Serves the phone's port (the server's own unless given, as for a USB forward) and records it as come via."""
    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *_args):
            pass

        def _port(self) -> int:
            return port or self.server.server_address[1]

        def _gone(self) -> bool:
            """Off the network or unplugged: cut the connection without an answer."""
            if not phone.gone(via):
                return False
            self.close_connection = True
            with contextlib.suppress(OSError):
                self.connection.shutdown(socket.SHUT_RDWR)
            return True

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
            raw = self.rfile.read(length)
            try:
                body = json.loads(raw or b"{}")
            except ValueError:
                body = raw.decode("utf-8", "replace")
            phone.record(self._port(), "POST", self.path, body, via=via)
            return body if isinstance(body, dict) else {}

        def do_GET(self):
            path = self.path.split("?")[0]
            phone.record(self._port(), "GET", self.path, None, via=via)
            if self._gone():
                return
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
            body = self._body()
            if self._gone() or not self._authorized():
                return
            if path == "/v1/session":
                phone.streaming = body.get("action") == "start"
                return self._json({"ok": True})
            if path == "/v1/control":
                action, value = body.get("action"), body.get("value")
                keys = {"ois": "ois", "torch": "torch", "black_level_lock": "black_level_lock", "nr_mode": "nr_mode",
                        "edge_mode": "edge_mode", "ae_comp": "ae_comp", "focus_mode": "focus_mode"}
                if action in keys:
                    phone.state[keys[action]] = (str(value) == "1") if action in ("ois", "torch", "black_level_lock") else value
                elif action == "camera":
                    phone.switch_lens(str(body.get("id")))
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
                while phone.streaming and not phone.gone(via):
                    self.wfile.write(phone.units[n % len(phone.units)])
                    self.wfile.flush()
                    n += 1
                    wait = start + n * period - time.monotonic()
                    if wait > 0:
                        time.sleep(wait)
            except OSError:
                pass

    return Handler


def serve(clip, host="127.0.0.2", fps=30.0, token="", record=None, adb_state=None) -> tuple:
    """Starts the phone's two ports on threads; returns the phone, its entry for a desktop's config, and a stop().
    With adb_state (a file for fake_adb.py) the two ports also listen on 127.0.0.1 for USB forwards."""
    folder = Path(tempfile.mkdtemp(prefix="fake-phone-"))
    if clip is None:
        clip = folder / "clip.h264"
        make_clip(clip, fps)
    units = access_units(Path(clip).read_bytes())
    version = (Path(__file__).resolve().parents[2] / "VERSION").read_text().strip()
    phone = Phone(units, fps, token or secrets.token_urlsafe(32), version, record)
    cert, key, pin = make_cert(folder)
    phone.pin = pin
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    ctx.load_cert_chain(cert, key)
    servers = []
    listens = [(host, port, port, "wifi") for port in (8766, 8080)]
    if adb_state is not None:
        listens += [("127.0.0.1", 0, port, "usb") for port in (8766, 8080)]
    for address, bind, port, via in listens:
        server = ThreadingHTTPServer((address, bind), handler_for(phone, port, via))
        server.daemon_threads = True
        server.socket = ctx.wrap_socket(server.socket, server_side=True)
        threading.Thread(target=server.serve_forever, daemon=True).start()
        servers.append(server)
        if via == "usb":
            phone.usb_ports[port] = server.server_address[1]
    if adb_state is not None:
        phone.adb_state = Path(adb_state)
        phone.write_adb_state()

    def stop():
        phone.streaming = False
        for server in servers:
            server.shutdown()
            server.server_close()
    entry = {"id": PHONE_ID, "name": PHONE_NAME, "token": phone.token, "ips": [host], "active_ip": host,
             "cert_sha256": pin}
    return phone, entry, stop


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("clip", nargs="?")
    parser.add_argument("--host", default="127.0.0.2")
    parser.add_argument("--fps", type=float, default=30)
    parser.add_argument("--token", default="")
    parser.add_argument("--record", type=Path, help="write every request here, one JSON line each")
    args = parser.parse_args()
    recorder = Recorder(args.record) if args.record else None
    phone, entry, _stop = serve(args.clip, args.host, args.fps, args.token, recorder)
    print(json.dumps(entry), flush=True)
    print(f"{len(phone.units)} frames at {args.fps:g} fps on {args.host}:8766 and :8080", flush=True)
    threading.Event().wait()


if __name__ == "__main__":
    main()
