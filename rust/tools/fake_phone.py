#!/usr/bin/env python3
"""A stand-in phone for measuring desktops: the session and stream ports of docs/protocol.md over TLS, serving a
pre-encoded Annex-B clip at its frame rate as /v1/video.h264. It's already paired: it accepts one token and prints
the phone entry (id, token, certificate pin) to put in a desktop's config. With --record it writes every request a
desktop sends, one JSON line each.

For scripted sessions it can also pair from a desktop's QR offer (pair()), switch between its two lenses, drop off
the network for a while (drop()) and be plugged in over USB (plug()), which fake_adb.py answers for.

  python fake_phone.py [CLIP.h264] [--host 127.0.0.2] [--fps 30] [--token TOKEN] [--record FILE]
                       [--video-file ANY_VIDEO] [--res 1080|4k] [--clip-seconds 10] [--heavy [--jpegs DIR]]
                       [--audio speech|FILE] [--phones N] [--name NAME]

Without a clip it makes a short test pattern with ffmpeg.
For measuring, --video-file serves any video scaled and cropped to --res (cached under ~/.cache/telescope-scratch/stock/derived),
--heavy sends it as MJPEG on /v1/video instead of H.264, --audio adds /v1/audio (a generated speech-like signal, or any
audio file as 48 kHz mono s16le) and --phones N starts N phones in one process on 127.0.0.2, .3, ... (one entry line each,
so a desktop can pair them all). Nothing is downloaded.
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
import sys
import tempfile
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import parse_qs, urlsplit

PHONE_ID = "fake-phone-0000-0000-0000-000000000001"
PHONE_NAME = "Fake phone"
SESSION_PROTOCOL = 3
SIZES = {"1080": (1920, 1080), "4k": (3840, 2160)}
SCRATCH = Path.home() / ".cache" / "telescope-scratch"
CACHE = SCRATCH / "stock" / "derived"
AUDIO_RATE = 48000
AUDIO_CHUNK_MS = 20


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


def cached(kind: str, source, res: str, fps: float, seconds: float, suffix: str) -> Path:
    """A path in the cache for this source, size, rate and length; its mtime and size are part of the name."""
    if source is None:
        key = "testpattern"
    else:
        st = Path(source).stat()
        key = f"{Path(source).resolve()}|{st.st_mtime_ns}|{st.st_size}"
    key += f"|{kind}|{res}|{fps:g}|{seconds:g}"
    CACHE.mkdir(parents=True, exist_ok=True)
    return CACHE / f"{kind}-{res}-{hashlib.sha256(key.encode()).hexdigest()[:12]}{suffix}"


def finish(tmp: Path, out: Path, source):
    """Moves a finished cache file into place and adds a line for it to DOWNLOADS.md, so it is cleaned up with the rest."""
    tmp.rename(out)
    with (SCRATCH / "DOWNLOADS.md").open("a", encoding="utf-8") as f:
        f.write(f"- stock/derived/{out.name}: made by rust/tools/fake_phone.py from {source or 'a generated signal'}\n")


def source_args(source, res: str, fps: float, seconds: float) -> list:
    """ffmpeg input and filter arguments: the video (looped to fill seconds) or a test pattern, filled and cropped to res."""
    w, h = SIZES[res]
    fit = f"fps={fps:g},scale={w}:{h}:force_original_aspect_ratio=increase,crop={w}:{h},setsar=1"
    if source is None:
        return ["-f", "lavfi", "-i", f"testsrc2=size={w}x{h}:rate={fps:g},noise=alls=12:allf=t", "-t", f"{seconds:g}", "-vf", "setsar=1"]
    return ["-stream_loop", "-1", "-i", str(source), "-t", f"{seconds:g}", "-vf", fit, "-an"]


def prepare_clip(source, res: str, fps: float, seconds: float) -> Path:
    """The source as an Annex-B H.264 clip at res, in the way the phone sends it (cached)."""
    out = cached("h264", source, res, fps, seconds, ".h264")
    if not out.exists():
        tmp = out.with_suffix(".part")
        bitrate = "25M" if res == "4k" else "8M"
        subprocess.run(["ffmpeg", "-v", "error", "-y", *source_args(source, res, fps, seconds), "-c:v", "libx264", "-preset", "veryfast",
                        "-profile:v", "high", "-bf", "0", "-g", str(int(fps)), "-b:v", bitrate, "-x264-params", "aud=1:repeat-headers=1",
                        "-f", "h264", str(tmp)], check=True)
        finish(tmp, out, source)
    return out


def prepare_jpegs(source, res: str, fps: float, seconds: float) -> list:
    """The source as JPEG frames at res for the Heavy route (cached as one file of concatenated JPEGs)."""
    out = cached("mjpeg", source, res, fps, seconds, ".mjpeg")
    if not out.exists():
        tmp = out.with_suffix(".part")
        subprocess.run(["ffmpeg", "-v", "error", "-y", *source_args(source, res, fps, seconds), "-c:v", "mjpeg", "-q:v", "3",
                        "-f", "image2pipe", "-vcodec", "mjpeg", str(tmp)], check=True)
        finish(tmp, out, source)
    data = out.read_bytes()
    marks = []
    i = data.find(b"\xff\xd8\xff")
    while i != -1:
        marks.append(i)
        i = data.find(b"\xff\xd8\xff", i + 3)
    marks.append(len(data))
    return [data[a:b] for a, b in zip(marks, marks[1:])]


def speech_pcm(seconds: float = 12.0) -> bytes:
    """A speech-like 48 kHz mono s16le signal: a voiced pulse train with a moving pitch through three vowel formants,
    in syllables with pauses between, over a low hiss and a mains hum, so noise suppression has both to work on."""
    import array
    import math
    import random
    rng = random.Random(7)
    rate, n = AUDIO_RATE, int(seconds * AUDIO_RATE)
    vowels = [(730, 1090, 2440), (270, 2290, 3010), (300, 870, 2240), (530, 1840, 2480), (570, 840, 2410)]

    def resonator(freq, width):
        r = math.exp(-math.pi * width / rate)
        a1, a2 = 2 * r * math.cos(2 * math.pi * freq / rate), -r * r
        return a1, a2, 1 - r

    out, state = [], [[0.0, 0.0] for _ in range(3)]
    phase, t, syllable_end, pause_end, vowel, f0, hum = 0.0, 0, 0, 0, vowels[0], 120.0, 0.0
    coeffs = [resonator(f, 90 + f / 10) for f in vowel]
    for i in range(n):
        if i >= syllable_end and i >= pause_end:
            # A new syllable of 120-280 ms, a new vowel and pitch, then a gap of 30-250 ms
            length = rng.randint(int(0.12 * rate), int(0.28 * rate))
            syllable_end, pause_end = i + length, i + length + rng.randint(int(0.03 * rate), int(0.25 * rate))
            vowel, f0 = rng.choice(vowels), rng.uniform(95, 190)
            coeffs = [resonator(f, 90 + f / 10) for f in vowel]
        voiced = i < syllable_end
        env = 0.0
        if voiced:
            pos = (i - (syllable_end - length)) / length
            env = math.sin(math.pi * pos) ** 0.6
        phase += (f0 * (1 + 0.05 * math.sin(i / rate * 2 * math.pi * 5.5))) / rate
        if phase >= 1:
            phase -= 1
        # A glottal-ish pulse: a falling ramp, with a little breath noise
        excitation = ((1 - phase) ** 3 - 0.25 + rng.uniform(-0.08, 0.08)) * env
        v = 0.0
        for k, (a1, a2, gain) in enumerate(coeffs):
            y = gain * excitation + a1 * state[k][0] + a2 * state[k][1]
            state[k][1], state[k][0] = state[k][0], y
            v += y * (1.0, 0.7, 0.4)[k]
        hum += 2 * math.pi * 50 / rate
        sample = 0.9 * v + rng.uniform(-1, 1) * 0.012 + math.sin(hum) * 0.004
        out.append(sample)
    peak = max(abs(x) for x in out) or 1.0
    scale = 0.5 * 32767 / peak
    pcm = array.array("h", (max(-32768, min(32767, int(x * scale))) for x in out))
    if sys.byteorder == "big":
        pcm.byteswap()
    return pcm.tobytes()


def prepare_audio(source: str) -> bytes:
    """48 kHz mono s16le for /v1/audio: "speech" is the generated signal, anything else an audio or video file (cached)."""
    if source == "speech":
        out = CACHE / "audio-speech-v1.pcm"
        CACHE.mkdir(parents=True, exist_ok=True)
        if not out.exists():
            tmp = out.with_suffix(".part")
            tmp.write_bytes(speech_pcm())
            finish(tmp, out, source)
        return out.read_bytes()
    out = cached("audio", source, "-", 0, 0, ".pcm")
    if not out.exists():
        tmp = out.with_suffix(".part")
        subprocess.run(["ffmpeg", "-v", "error", "-y", "-i", str(source), "-vn", "-ac", "1", "-ar", str(AUDIO_RATE), "-f", "s16le", str(tmp)], check=True)
        finish(tmp, out, source)
    return out.read_bytes()


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
    """What the phone's pairingProof() sends (rust/crates/link/src/pairing.rs checks it)."""
    message = f"telescope-pair-v4\n{nonce}\n{phone_id}\n{cert_sha256}".encode()
    return hmac.new(token.encode(), message, hashlib.sha256).hexdigest()


class Phone:
    def __init__(self, units: list, fps: float, token: str, version: str, record=None, phone_id=PHONE_ID, name=PHONE_NAME,
                 audio=None, jpegs=None):
        self.units, self.fps, self.token, self.version = units, fps, token, version
        self.id, self.name = phone_id, name
        self.audio = audio  # 48 kHz mono s16le served on /v1/audio; None leaves it out
        self.jpegs = jpegs  # JPEG frames served on /v1/video (Heavy); None leaves it out
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
        if jpegs:
            self.state.update({"codec": "mjpeg"})

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
        state = {"devices": [["FAKE0001", "device", self.name]] if self.plugged else [],
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
        body = {"name": self.name, "phone_id": self.id, "cert_sha256": self.pin,
                "proof": pairing_proof(token, nonce, self.id, self.pin), "ips": [host]}
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
                return self._json({"protocol": SESSION_PROTOCOL, "phoneId": phone.id, "phoneName": phone.name,
                                   "appVersion": phone.version, "build": 1})
            if not self._authorized():
                return
            if path == "/v1/ping":
                return self._json({"protocol": SESSION_PROTOCOL, "streaming": phone.streaming, "busy": False,
                                   "localOnly": False, "phoneId": phone.id, "phoneName": phone.name})
            if path == "/v1/state" and phone.streaming:
                return self._json(phone.state)
            if path == "/v1/video.h264" and phone.streaming:
                return self._video()
            if path == "/v1/video" and phone.streaming and phone.jpegs:
                return self._mjpeg()
            if path == "/v1/audio":
                if phone.audio is None:
                    return self._json({"error": "No microphone", "code": "mic_none"}, 404)
                if not phone.streaming:
                    return self._json({"error": "Not streaming", "code": "mic_not_streaming"}, 409)
                return self._audio()
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

        def _mjpeg(self):
            self.send_response(200)
            self.send_header("Content-Type", "multipart/x-mixed-replace; boundary=--frame")
            self.send_header("Cache-Control", "no-cache")
            self.send_header("Connection", "close")
            self.end_headers()
            self.close_connection = True
            period, start, n = 1.0 / phone.fps, time.monotonic(), 0
            try:
                while phone.streaming and not phone.gone(via):
                    jpeg = phone.jpegs[n % len(phone.jpegs)]
                    self.wfile.write(b"--frame\r\nContent-Type: image/jpeg\r\nContent-Length: " + str(len(jpeg)).encode() + b"\r\n\r\n" + jpeg + b"\r\n")
                    self.wfile.flush()
                    n += 1
                    wait = start + n * period - time.monotonic()
                    if wait > 0:
                        time.sleep(wait)
            except OSError:
                pass

        def _audio(self):
            self.send_response(200)
            self.send_header("Content-Type", "audio/L16")
            self.send_header("Cache-Control", "no-cache")
            self.send_header("Connection", "close")
            self.end_headers()
            self.close_connection = True
            # Real time, in chunks of 20 ms, looping the signal
            chunk = AUDIO_RATE * 2 * AUDIO_CHUNK_MS // 1000
            data, start, n = phone.audio, time.monotonic(), 0
            try:
                while phone.streaming and not phone.gone(via):
                    at = (n * chunk) % len(data)
                    piece = data[at:at + chunk]
                    if len(piece) < chunk:
                        piece += data[:chunk - len(piece)]
                    self.wfile.write(piece)
                    self.wfile.flush()
                    n += 1
                    wait = start + n * AUDIO_CHUNK_MS / 1000 - time.monotonic()
                    if wait > 0:
                        time.sleep(wait)
            except OSError:
                pass

    return Handler


def serve(clip, host="127.0.0.2", fps=30.0, token="", record=None, adb_state=None, phone_id=PHONE_ID, name=PHONE_NAME,
          audio=None, jpegs=None, size=None) -> tuple:
    """Starts the phone's two ports on threads; returns the phone, its entry for a desktop's config, and a stop().
    With adb_state (a file for fake_adb.py) the two ports also listen on 127.0.0.1 for USB forwards. phone_id and name tell
    phones apart (give each its own host too); audio (PCM bytes) adds /v1/audio, jpegs (frames) adds the Heavy /v1/video and
    size is (width, height) of the stream when it isn't 1080p."""
    folder = Path(tempfile.mkdtemp(prefix="fake-phone-"))
    if clip is None:
        clip = folder / "clip.h264"
        make_clip(clip, fps)
    units = access_units(Path(clip).read_bytes())
    version = (Path(__file__).resolve().parents[2] / "VERSION").read_text().strip()
    phone = Phone(units, fps, token or secrets.token_urlsafe(32), version, record, phone_id, name, audio, jpegs)
    if size:
        phone.state.update({"stream_width": size[0], "stream_height": size[1]})
    cert, key, pin = make_cert(folder)
    phone.pin = pin
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    ctx.minimum_version = ssl.TLSVersion.TLSv1_2
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
    entry = {"id": phone_id, "name": name, "token": phone.token, "ips": [host], "active_ip": host,
             "cert_sha256": pin}
    return phone, entry, stop


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("clip", nargs="?")
    parser.add_argument("--host", default="127.0.0.2", help="the first phone's address; more phones take the next ones")
    parser.add_argument("--fps", type=float, default=30)
    parser.add_argument("--token", default="")
    parser.add_argument("--record", type=Path, help="write every request here, one JSON line each")
    parser.add_argument("--video-file", type=Path, help="any video, scaled and cropped to --res, looped (instead of CLIP)")
    parser.add_argument("--res", choices=sorted(SIZES), help="1080 or 4k for --video-file, --heavy or a test pattern")
    parser.add_argument("--clip-seconds", type=float, default=10, help="how much of --video-file to loop")
    parser.add_argument("--heavy", action="store_true", help="send MJPEG on /v1/video, as Heavy does")
    parser.add_argument("--jpegs", type=Path, help="with --heavy: a folder of .jpg frames to loop instead of encoding a video")
    parser.add_argument("--audio", metavar="speech|FILE", help="serve /v1/audio: a generated speech-like signal, or this file")
    parser.add_argument("--phones", type=int, default=1, help="how many phones to start (127.0.0.2, 127.0.0.3, ...)")
    parser.add_argument("--name", default=PHONE_NAME)
    args = parser.parse_args()
    res = args.res or ("1080" if args.video_file or (args.heavy and not args.jpegs) else None)
    source = args.video_file
    clip = args.clip
    if (source or res) and not (args.heavy and args.jpegs):
        clip = prepare_clip(source, res, args.fps, args.clip_seconds)
    jpegs = None
    if args.heavy and args.jpegs:
        jpegs = [f.read_bytes() for f in sorted(args.jpegs.glob("*.jpg"))]
    elif args.heavy:
        jpegs = prepare_jpegs(source, res, args.fps, args.clip_seconds)
    audio = prepare_audio(args.audio) if args.audio else None
    recorder = Recorder(args.record) if args.record else None
    base, last = args.host.rsplit(".", 1)
    hosts = []
    phone = None
    for n in range(args.phones):
        host = args.host if n == 0 else f"{base}.{int(last) + n}"
        phone_id = PHONE_ID[:-3] + f"{n + 1:03d}"
        name = args.name if n == 0 else f"{args.name} {n + 1}"
        phone, entry, _stop = serve(clip, host, args.fps, args.token if n == 0 else "", recorder, None, phone_id, name, audio, jpegs,
                                    SIZES[res] if res else None)
        print(json.dumps(entry), flush=True)
        hosts.append(host)
    extras = (" heavy" if jpegs else "") + (" audio" if audio else "")
    print(f"{len(phone.units)} frames at {args.fps:g} fps{extras} on {', '.join(h + ':8766/:8080' for h in hosts)}", flush=True)
    threading.Event().wait()


if __name__ == "__main__":
    main()
