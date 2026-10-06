"""Browser camera: any device with a browser streams its camera and mic here, no app needed. Qt-free.

The device opens https://<this computer>:<port>/#<token>, allows the camera, and sends H.264 (when its browser
has a hardware encoder and PyAV can decode here) or JPEG frames, and 48 kHz mono s16le audio, over one WebSocket. Browsers only allow the camera on HTTPS (or localhost), so the
server has a self-signed certificate the browser warns about once. The token is new every time the server starts.

Several browsers can connect at once. Each page keeps an id of its own (in the browser's storage), so a reload or a
second tab on the same device takes over that browser's feed, while another device gets a feed of its own.

BrowserFeed is where the server puts what one browser sends, and BrowserHub keeps a feed per browser; BrowserReader
hands a feed's frames to StreamWorker the way MjpegReader does, and BrowserFeed.open_audio() stands in for the phone's
/v1/audio for AudioWorker.
"""

import base64
import datetime
import hashlib
import hmac
import io
import ipaddress
import json
import logging
import os
import re
import secrets
import socket
import ssl
import struct
import sys
import threading
import time
import urllib.error
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Callable, Optional

import cv2
import numpy as np

from telescope import h264_reader

logger = logging.getLogger(__name__)

BROWSER_PORT = 8767

FRAME_JPEG = 1  # first byte of a binary message: what follows
FRAME_PCM = 2
FRAME_H264 = 3  # then 1 for a keyframe or 0, then Annex-B

_WS_GUID = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11"
_MAX_MESSAGE_BYTES = 16 * 1024 * 1024  # far above a 1080p JPEG; anything bigger ends the connection
_MAX_TEXT_BYTES = 16 * 1024
_REQUEST_TIMEOUT_S = 10  # handshake and HTTP request
_IDLE_TIMEOUT_S = 10  # no message at all for this long (a phone that went to sleep) ends the connection
_TICK_S = 0.5  # how often the connection loop looks for something to send while nothing arrives
_MAX_CONNECTIONS = 16
_MAX_PER_ADDRESS = 6
# H.264 waiting for the decoder past this is dropped up to the next keyframe, which the page is asked for
_MAX_H264_BYTES = 8 * 1024 * 1024
_BROWSER_ID = re.compile(r"[A-Za-z0-9_-]{8,40}")
DEFAULT_ID = "default"  # a page that sends no id of its own (an older one)
CLOSE_REPLACED = 4000  # opened again on the same browser
CLOSE_FULL = 4001  # every camera is taken

# Browsers refuse certificates valid for more than 398 days, so it's renewed a month before it runs out.
_CERT_DAYS = 397
_CERT_RENEW_DAYS = 30
_CERT_FILE = "browser_camera_cert.pem"
_KEY_FILE = "browser_camera_key.pem"

_WEB_DIR = Path(__file__).resolve().parent / "web"
_PAGES = {
    "/": ("index.html", "text/html; charset=utf-8"),
    "/app.js": ("app.js", "text/javascript; charset=utf-8"),
    "/mic-worklet.js": ("mic-worklet.js", "text/javascript; charset=utf-8"),
}


def certificate_available() -> bool:
    try:
        import cryptography  # noqa: F401
    except ImportError:
        return False
    return True


def ensure_certificate(folder: Path, hosts: list, now: Optional[datetime.datetime] = None) -> tuple:
    """(cert path, key path) in folder, made or renewed when missing, unreadable or about to run out.

    The same certificate is kept as long as it's valid, so a browser that was told to trust it once keeps doing so.
    """
    cert_path, key_path = folder / _CERT_FILE, folder / _KEY_FILE
    now = now or datetime.datetime.now(datetime.timezone.utc)
    if _certificate_fresh(cert_path, key_path, now):
        return cert_path, key_path
    from cryptography import x509
    from cryptography.hazmat.primitives import hashes, serialization
    from cryptography.hazmat.primitives.asymmetric import ec
    from cryptography.x509.oid import NameOID

    key = ec.generate_private_key(ec.SECP256R1())
    name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, "Telescope")])
    alt = [x509.DNSName("localhost")]
    for host in hosts:
        try:
            alt.append(x509.IPAddress(ipaddress.ip_address(host)))
        except ValueError:
            pass
    cert = (x509.CertificateBuilder()
            .subject_name(name).issuer_name(name)
            .public_key(key.public_key())
            .serial_number(x509.random_serial_number())
            .not_valid_before(now - datetime.timedelta(days=1))
            .not_valid_after(now + datetime.timedelta(days=_CERT_DAYS))
            .add_extension(x509.SubjectAlternativeName(alt), critical=False)
            .add_extension(x509.BasicConstraints(ca=False, path_length=None), critical=True)
            .add_extension(x509.ExtendedKeyUsage([x509.oid.ExtendedKeyUsageOID.SERVER_AUTH]), critical=False)
            .sign(key, hashes.SHA256()))
    folder.mkdir(parents=True, exist_ok=True)
    _write_private(key_path, key.private_bytes(serialization.Encoding.PEM, serialization.PrivateFormat.PKCS8,
                                               serialization.NoEncryption()))
    _write_private(cert_path, cert.public_bytes(serialization.Encoding.PEM))
    return cert_path, key_path


def _certificate_fresh(cert_path: Path, key_path: Path, now: datetime.datetime) -> bool:
    if not cert_path.is_file() or not key_path.is_file():
        return False
    try:
        from cryptography import x509
        cert = x509.load_pem_x509_certificate(cert_path.read_bytes())
        ssl.create_default_context(ssl.Purpose.CLIENT_AUTH).load_cert_chain(cert_path, key_path)
    except Exception:
        return False
    return cert.not_valid_after_utc - now > datetime.timedelta(days=_CERT_RENEW_DAYS)


def _write_private(path: Path, data: bytes):
    tmp = path.with_suffix(path.suffix + ".tmp")
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "wb") as f:
        f.write(data)
    os.replace(tmp, path)


# ── What arrives ─────────────────────────────────────────────────────────────

class BrowserFeed:
    """Frames and audio from whichever browser is connected; thread-safe. A connection is a generation: a newer one
    takes over, and the frames of an older one are never handed out after it."""

    def __init__(self, h264: Optional[bool] = None):
        self._cond = threading.Condition()
        self._gen = 0
        self._connected = False
        self._frame: Optional[bytes] = None
        self._frame_seq = 0
        self.h264 = h264_reader.available() if h264 is None else h264  # whether the page may send H.264
        self._h264 = bytearray()  # H.264 not yet read, in order: each frame needs the ones before it
        self._h264_need_key = True
        self._key_seq = 0  # bumped to ask the page for a keyframe
        self.device = ""  # what the browser says it runs on, for the card
        self.mic_error = ""  # the browser's reason it has no mic, if it said one
        self.codec = ""  # what the page sends: "h264" or "jpeg"
        self.codec_note = ""  # why it isn't H.264, if it said
        self._audio_streams: list = []
        self._settings = {"width": 1280, "height": 720, "fps": 30}
        self._settings_seq = 0  # bumped on every change, so each connection sends the newest
        self._camera = ""  # the virtual camera it streams to, for the page to show

    # The server's side

    def connect(self) -> int:
        with self._cond:
            self._gen += 1
            self._connected = True
            self._frame = None
            self._h264.clear()
            self._h264_need_key = True
            self.device = ""
            self.mic_error = ""
            self.codec = self.codec_note = ""
            self._cond.notify_all()
            return self._gen

    def disconnect(self, gen: int):
        with self._cond:
            if gen != self._gen:
                return
            self._connected = False
            self._frame = None
            self._h264.clear()
            self._cond.notify_all()

    def current(self, gen: int) -> bool:
        return gen == self._gen

    def put_frame(self, gen: int, jpeg: bytes):
        with self._cond:
            if gen != self._gen:
                return
            self._frame = jpeg
            self._h264.clear()
            self._frame_seq += 1
            self._cond.notify_all()

    def put_h264(self, gen: int, key: bool, data: bytes):
        with self._cond:
            if gen != self._gen:
                return
            self._frame = None
            if key:
                self._h264_need_key = False
            elif self._h264_need_key:
                return  # nothing to decode it against
            if len(self._h264) + len(data) > _MAX_H264_BYTES:
                self._h264.clear()
                if not key:
                    self._h264_need_key = True
                    self._key_seq += 1
                    return
            self._h264 += data
            self._frame_seq += 1
            self._cond.notify_all()

    def request_keyframe(self):
        with self._cond:
            self._h264.clear()
            self._h264_need_key = True
            self._key_seq += 1

    def put_audio(self, gen: int, pcm: bytes):
        if gen != self._gen:
            return
        for stream in list(self._audio_streams):
            stream.push(pcm)

    def note_hello(self, gen: int, device: str, mic_error: str, codec: str = "", codec_note: str = ""):
        if gen == self._gen:
            self.device, self.mic_error = device, mic_error
            self.codec, self.codec_note = codec, codec_note

    def page_config(self) -> tuple:
        """(change counter, what the page should capture), sent to the page when the counter moves."""
        with self._cond:
            cfg = dict(self._settings, audio=bool(self._audio_streams), h264=self.h264)
            if self._camera:
                cfg["camera"] = self._camera
            return self._settings_seq, cfg

    def keyframe_requests(self) -> int:
        """A counter that moves each time the decoder needs a keyframe; the connection asks the page when it does."""
        return self._key_seq

    # The desktop's side

    @property
    def connected(self) -> bool:
        return self._connected

    @property
    def generation(self) -> int:
        """Moves on each time a browser connects to it."""
        return self._gen

    def set_capture(self, width: int, height: int, fps: int):
        with self._cond:
            self._settings = {"width": int(width), "height": int(height), "fps": int(fps)}
            self._settings_seq += 1

    def set_camera(self, name: str):
        """The virtual camera this browser streams to ("" for none); its page shows it."""
        with self._cond:
            if name != self._camera:
                self._camera = name
                self._settings_seq += 1

    def wait_connected(self, timeout: float) -> Optional[int]:
        """The connected browser's generation, waiting up to timeout for one; None if nothing connected."""
        deadline = time.monotonic() + timeout
        with self._cond:
            while not self._connected:
                left = deadline - time.monotonic()
                if left <= 0:
                    return None
                self._cond.wait(left)
            return self._gen

    def next_frame(self, gen: int, after: int, timeout: float) -> Optional[tuple]:
        """(seq, codec, data) newer than after from connection gen, waiting up to timeout; None once it's gone or
        quiet. JPEG is the newest frame alone; H.264 is everything since the last call, which the reader decodes."""
        deadline = time.monotonic() + timeout
        with self._cond:
            while True:
                if gen != self._gen or not self._connected:
                    return None
                if self._frame_seq > after:
                    if self._frame is not None:
                        return self._frame_seq, "jpeg", self._frame
                    if self._h264:
                        data = bytes(self._h264)
                        self._h264.clear()
                        return self._frame_seq, "h264", data
                left = deadline - time.monotonic()
                if left <= 0:
                    return None
                self._cond.wait(left)

    def open_audio(self, req=None, timeout: float = 5.0) -> "BrowserAudioStream":
        """AudioWorker's opener: the browser's mic as a never-ending response. It outlives the browser reconnecting
        (silence meanwhile); only the worker closing it ends it. Refuses, with the browser's reason, when the
        connected browser has no mic."""
        if self._connected and self.mic_error:
            body = json.dumps({"error": self.mic_error}).encode()
            raise urllib.error.HTTPError("browser:/audio", 409, "No microphone", {}, io.BytesIO(body))
        stream = BrowserAudioStream(self._drop_audio)
        with self._cond:
            self._audio_streams.append(stream)
            self._settings_seq += 1  # the page starts sending audio
        return stream

    def _drop_audio(self, stream):
        with self._cond:
            if stream in self._audio_streams:
                self._audio_streams.remove(stream)
                self._settings_seq += 1


class BrowserHub:
    """A BrowserFeed per browser, by the id its page keeps; thread-safe. can_join(id) decides whether a browser
    that isn't known yet gets one (False: every camera is taken). Given a feed, every browser shares it."""

    def __init__(self, can_join: Optional[Callable[[str], bool]] = None, h264: Optional[bool] = None,
                 feed: Optional[BrowserFeed] = None):
        self._can_join = can_join or (lambda _bid: True)
        self._h264 = h264
        self._single = feed
        self._feeds: dict = {}
        self._capture = (1280, 720, 30)
        self._lock = threading.Lock()

    def join(self, bid: str) -> Optional[BrowserFeed]:
        if self._single is not None:
            return self._single
        with self._lock:
            feed = self._feeds.get(bid)
            if feed is not None:
                return feed
        if not self._can_join(bid):
            return None
        with self._lock:
            feed = self._feeds.get(bid)
            if feed is None:
                feed = self._feeds[bid] = BrowserFeed(self._h264)
                feed.set_capture(*self._capture)
            return feed

    def feed(self, bid: str) -> Optional[BrowserFeed]:
        if self._single is not None:
            return self._single
        with self._lock:
            return self._feeds.get(bid)

    def feeds(self) -> dict:
        """{id: feed} of every browser that connected since the server started (or was dropped)."""
        if self._single is not None:
            return {DEFAULT_ID: self._single}
        with self._lock:
            return dict(self._feeds)

    def drop(self, bid: str):
        """Forget a browser that left, so it needs room to join again."""
        with self._lock:
            feed = self._feeds.get(bid)
            if feed is not None and not feed.connected:
                del self._feeds[bid]

    def set_capture(self, width: int, height: int, fps: int):
        self._capture = (width, height, fps)
        for feed in self.feeds().values():
            feed.set_capture(width, height, fps)


class BrowserAudioStream:
    """read1()/close() like an HTTP response, fed by BrowserFeed. Keeps at most a second, so a stalled reader
    can't build up delay (the jitter buffer drops it back to its target anyway)."""

    _MAX_BYTES = 48_000 * 2

    def __init__(self, on_close: Callable):
        self._cond = threading.Condition()
        self._buf = bytearray()
        self._closed = False
        self._on_close = on_close

    def push(self, data: bytes):
        with self._cond:
            self._buf += data
            if len(self._buf) > self._MAX_BYTES:
                drop = len(self._buf) - self._MAX_BYTES
                del self._buf[:drop - drop % 2]
            self._cond.notify_all()

    def read1(self, n: int = 4096) -> bytes:
        with self._cond:
            while not self._buf and not self._closed:
                self._cond.wait(0.5)
            if self._closed:
                return b""
            out = bytes(self._buf[:n])
            del self._buf[:n]
            return out

    def close(self):
        with self._cond:
            self._closed = True
            self._cond.notify_all()
        self._on_close(self)


class BrowserAuth:
    """What AudioWorker expects of a phone's auth: no headers, and open() reaches the browser's mic."""

    def __init__(self, feed: BrowserFeed):
        self._feed = feed

    def headers(self) -> dict:
        return {}

    def open(self, req, timeout: float = 5.0):
        return self._feed.open_audio(req, timeout)


class BrowserControl:
    """Stands in for PhoneControlClient while the browser streams: there's no phone to control or ask for state.
    The microphone plugin reads base and auth to reach the audio."""

    base = "browser:"

    def __init__(self, feed: BrowserFeed):
        self.auth = BrowserAuth(feed)

    def get_state(self):
        return None

    def send(self, **params):
        pass

    def close(self):
        pass


class BrowserReader:
    """StreamWorker's reader for the browser: the same open/read_packet/decode/release as MjpegReader. JPEG frames
    decode in parallel in decode(); H.264 decodes in order in read_packet(), and decode() passes the frame on."""

    parallel_decode = True
    waiting_text = "Waiting for the browser. Scan the code on the Browser camera card and tap Start."
    # How long open() waits for a browser, and how long read_packet() waits for a frame before the worker reconnects.
    OPEN_WAIT_S = 1.0
    FRAME_WAIT_S = 3.0

    def __init__(self, feed: BrowserFeed):
        self._feed = feed
        self._gen: Optional[int] = None
        self._seq = 0
        self._codec = None
        self.last_frame_bytes = 0
        self.last_frame_count = 1
        self._pending_bytes = self._pending_frames = 0

    def open(self) -> bool:
        self._gen = self._feed.wait_connected(self.OPEN_WAIT_S)
        self._seq = 0
        self._codec = None
        if self._gen is not None:
            self._feed.request_keyframe()  # a new decoder starts at one
        return self._gen is not None

    def isOpened(self) -> bool:
        return self._gen is not None

    def read_packet(self):
        while self._gen is not None:
            got = self._feed.next_frame(self._gen, self._seq, self.FRAME_WAIT_S)
            if got is None:
                return False, None
            self._seq, kind, data = got
            if kind == "jpeg":
                self.last_frame_bytes, self.last_frame_count = len(data), 1
                return True, data
            frame = self._decode_h264(data)
            if frame is not None:
                return True, frame
        return False, None

    def _decode_h264(self, data: bytes):
        if self._codec is None:
            self._codec = h264_reader.new_decoder()
        try:
            frame, count = h264_reader.decode_counted(self._codec, data)
        except Exception:
            logger.exception("Browser camera: H.264 decode failed; waiting for a keyframe")
            self._codec = None
            self._feed.request_keyframe()
            return None
        self._pending_bytes += len(data)
        self._pending_frames += count
        if frame is None:
            return None
        self.last_frame_bytes, self._pending_bytes = self._pending_bytes, 0
        self.last_frame_count, self._pending_frames = self._pending_frames, 0
        return frame

    @staticmethod
    def decode(packet):
        if isinstance(packet, np.ndarray):
            return packet  # H.264, already decoded in order
        return cv2.imdecode(np.frombuffer(packet, dtype=np.uint8), cv2.IMREAD_COLOR)

    def read(self):
        ok, packet = self.read_packet()
        if not ok:
            return False, None
        frame = self.decode(packet)
        return (True, frame) if frame is not None else (False, None)

    def release(self):
        self._gen = None
        self._codec = None


# ── WebSocket framing (RFC 6455, just what a browser sends) ──────────────────

class _Closed(Exception):
    """The connection ended, cleanly or not."""


def ws_accept_key(key: str) -> str:
    return base64.b64encode(hashlib.sha1(key.encode("ascii") + _WS_GUID).digest()).decode("ascii")


def ws_frame(opcode: int, payload: bytes = b"") -> bytes:
    """A server-to-client frame: final, unmasked."""
    n = len(payload)
    if n < 126:
        head = struct.pack("!BB", 0x80 | opcode, n)
    elif n < 1 << 16:
        head = struct.pack("!BBH", 0x80 | opcode, 126, n)
    else:
        head = struct.pack("!BBQ", 0x80 | opcode, 127, n)
    return head + payload


def ws_close_frame(code: int, reason: str = "") -> bytes:
    return ws_frame(0x8, struct.pack("!H", code) + reason.encode("utf-8")[:120])


def unmask(payload: bytes, mask: bytes) -> bytes:
    if not payload:
        return b""
    data = np.frombuffer(payload, np.uint8)
    key = np.frombuffer(mask * (len(payload) // 4 + 1), np.uint8)[:len(payload)]
    return (data ^ key).tobytes()


class _WsConnection:
    """Reads a browser's frames off the socket with its own buffer, so a read timeout (used to send pending
    messages and notice a quiet peer) never loses half a frame. Everything is sent from this thread too:
    an SSL socket mustn't be written while another thread reads it."""

    def __init__(self, sock, on_tick: Callable):
        self._sock = sock
        self._buf = bytearray()
        self._on_tick = on_tick
        self._last_heard = time.monotonic()

    def send(self, data: bytes):
        self._sock.sendall(data)

    def _fill(self, n: int):
        while len(self._buf) < n:
            try:
                chunk = self._sock.recv(65536)
            except (socket.timeout, TimeoutError):
                if time.monotonic() - self._last_heard > _IDLE_TIMEOUT_S:
                    raise _Closed("idle")
                self._on_tick()
                continue
            except (OSError, ssl.SSLError) as exc:
                raise _Closed(str(exc))
            if not chunk:
                raise _Closed("eof")
            self._last_heard = time.monotonic()
            self._buf += chunk

    def _take(self, n: int) -> bytes:
        self._fill(n)
        out = bytes(self._buf[:n])
        del self._buf[:n]
        return out

    def read_frame(self) -> tuple:
        b0, b1 = self._take(2)
        fin, opcode = bool(b0 & 0x80), b0 & 0x0F
        if b0 & 0x70:
            raise _Closed("reserved bits")
        if not b1 & 0x80:
            raise _Closed("unmasked frame")  # browsers always mask
        n = b1 & 0x7F
        if n == 126:
            n = struct.unpack("!H", self._take(2))[0]
        elif n == 127:
            n = struct.unpack("!Q", self._take(8))[0]
        if n > _MAX_MESSAGE_BYTES:
            raise _Closed("frame too big")
        mask = self._take(4)
        return fin, opcode, unmask(self._take(n), mask)

    def read_message(self) -> tuple:
        """(opcode, payload) of the next text or binary message, answering pings and assembling fragments."""
        opcode, parts, size = None, [], 0
        while True:
            fin, op, payload = self.read_frame()
            if op == 0x8:
                self.send(ws_close_frame(1000))
                raise _Closed("closed by browser")
            if op == 0x9:
                self.send(ws_frame(0xA, payload[:125]))
                continue
            if op == 0xA:
                continue
            if op in (0x1, 0x2):
                if opcode is not None:
                    raise _Closed("new message inside a fragmented one")
                opcode = op
            elif op != 0x0 or opcode is None:
                raise _Closed(f"unexpected opcode {op}")
            parts.append(payload)
            size += len(payload)
            if size > _MAX_MESSAGE_BYTES:
                raise _Closed("message too big")
            if fin:
                return opcode, b"".join(parts)


# ── The server ───────────────────────────────────────────────────────────────

class ServerStats:
    """How far browsers got, for telling a firewall from a certificate warning from an old link. Counts only: no
    addresses or tokens. Each step is logged the first time it happens."""

    STEPS = {
        "connections": "a device reached this computer",
        "tls_failed": "a browser hung up during the TLS handshake (it hasn't accepted the certificate yet)",
        "pages": "a browser loaded the page",
        "refused": "a browser was refused with an old or wrong link",
        "browsers": "a browser connected",
    }

    def __init__(self, port: int, wanted_port: int, fell_back: bool = False):
        self.port, self.wanted_port = port, wanted_port
        self.fell_back = fell_back  # wanted_port was taken, so port is whatever was free
        self.started = time.monotonic()
        self._counts = dict.fromkeys(self.STEPS, 0)
        self._lock = threading.Lock()

    def note(self, step: str):
        with self._lock:
            self._counts[step] += 1
            first = self._counts[step] == 1
        if first:
            logger.info("Browser camera: %s", self.STEPS[step])

    def __getitem__(self, step: str) -> int:
        return self._counts[step]

    def nothing_arrived(self) -> bool:
        return self._counts["connections"] == 0


class _Server(ThreadingHTTPServer):
    """TLS per connection, on that connection's own thread, so a slow handshake holds up nobody else. Open
    connections are capped in total and per address."""

    allow_reuse_address = sys.platform != "win32"  # on Windows it would let two servers share the port
    daemon_threads = True

    def __init__(self, address, handler, ctx: ssl.SSLContext, owner: "BrowserServer"):
        self.ctx, self.owner = ctx, owner
        self._open: dict = {}
        self._open_lock = threading.Lock()
        super().__init__(address, handler)

    def process_request(self, request, client_address):
        host = client_address[0] if client_address else ""
        with self._open_lock:
            if sum(self._open.values()) >= _MAX_CONNECTIONS or self._open.get(host, 0) >= _MAX_PER_ADDRESS:
                self.shutdown_request(request)
                return
            self._open[host] = self._open.get(host, 0) + 1
        self.owner.stats.note("connections")
        super().process_request(request, client_address)

    def process_request_thread(self, request, client_address):
        try:
            super().process_request_thread(request, client_address)
        finally:
            host = client_address[0] if client_address else ""
            with self._open_lock:
                left = self._open.get(host, 0) - 1
                if left <= 0:
                    self._open.pop(host, None)
                else:
                    self._open[host] = left

    def finish_request(self, request, client_address):
        request.settimeout(_REQUEST_TIMEOUT_S)
        try:
            tls = self.ctx.wrap_socket(request, server_side=True)
        except (OSError, ssl.SSLError):
            self.owner.stats.note("tls_failed")
            return  # a browser that hasn't accepted the certificate yet hangs up here; that's expected
        try:
            self.RequestHandlerClass(tls, client_address, self)
        finally:
            try:
                tls.close()
            except OSError:
                pass

    def handle_error(self, request, client_address):
        if isinstance(sys.exc_info()[1], (OSError, ssl.SSLError)):
            return  # a dropped connection
        super().handle_error(request, client_address)


class _Handler(BaseHTTPRequestHandler):
    server_version = "Telescope"
    sys_version = ""
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        pass  # no addresses or tokens in the log

    def _reply(self, code: int, body: bytes = b"", content_type: str = "text/plain; charset=utf-8"):
        self.send_response(code)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("X-Content-Type-Options", "nosniff")
        self.send_header("Referrer-Policy", "no-referrer")
        self.send_header("Content-Security-Policy",
                         "default-src 'self'; connect-src 'self' wss:; img-src 'self' data:; "
                         "style-src 'self' 'unsafe-inline'; media-src 'self' blob: mediastream:; frame-ancestors 'none'")
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(body)

    def do_HEAD(self):
        self.do_GET()

    def do_GET(self):
        url = urllib.parse.urlsplit(self.path)
        if url.path == "/ws":
            query = urllib.parse.parse_qs(url.query)
            bid = query.get("id", [""])[0]
            self._websocket(query.get("token", [""])[0], bid if _BROWSER_ID.fullmatch(bid) else DEFAULT_ID)
            return
        page = _PAGES.get(url.path)
        if page is None:
            self._reply(404, b"Not found")
            return
        if url.path == "/":
            self.server.owner.stats.note("pages")
        self._reply(200, (_WEB_DIR / page[0]).read_bytes(), page[1])

    def _websocket(self, token: str, bid: str):
        owner: BrowserServer = self.server.owner
        key = self.headers.get("Sec-WebSocket-Key", "")
        if (self.headers.get("Upgrade", "").lower() != "websocket" or not key
                or self.headers.get("Sec-WebSocket-Version") != "13"):
            self._reply(400, b"WebSocket only")
            return
        if not owner.token_ok(token):
            owner.stats.note("refused")
            self._reply(403, b"This link has expired. Scan the code on the computer again.")
            return
        self.send_response(101, "Switching Protocols")
        self.send_header("Upgrade", "websocket")
        self.send_header("Connection", "Upgrade")
        self.send_header("Sec-WebSocket-Accept", ws_accept_key(key))
        self.end_headers()
        self.wfile.flush()
        self.close_connection = True
        owner.stats.note("browsers")
        owner.serve_browser(self.connection, bid)


class BrowserServer:
    """Serves the page and takes each browser's camera into its feed in hub (or, given a feed, one browser at a
    time into it). start() binds, stop() ends everything."""

    def __init__(self, hub, cert_path: Path, key_path: Path,
                 on_change: Optional[Callable[[], None]] = None, port: int = BROWSER_PORT, host: str = ""):
        self.hub = hub if isinstance(hub, BrowserHub) else BrowserHub(feed=hub)
        self.token = secrets.token_urlsafe(18)
        self._cert, self._key = cert_path, key_path
        self._on_change = on_change or (lambda: None)
        self._want_port, self._host = port, host
        self._server: Optional[_Server] = None
        self._thread: Optional[threading.Thread] = None
        self._socks: set = set()
        self._kicked: set = set()  # connections to end; each one's own thread notices within _TICK_S
        self._socks_lock = threading.Lock()
        self.stats = ServerStats(0, port)

    @property
    def feed(self) -> Optional[BrowserFeed]:
        """The feed of a browser that sent no id of its own (and, given one feed, every browser's)."""
        return self.hub.feed(DEFAULT_ID)

    @property
    def port(self) -> int:
        return self._server.server_address[1] if self._server else 0

    def url_for(self, host: str) -> str:
        return f"https://{host}:{self.port}/#{self.token}"

    def token_ok(self, token: str) -> bool:
        return bool(token) and hmac.compare_digest(token.encode(), self.token.encode())

    def new_token(self):
        """Make the old link stop working (and drop whoever used it)."""
        self.token = secrets.token_urlsafe(18)
        self._close_all()

    def start(self):
        ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        ctx.minimum_version = ssl.TLSVersion.TLSv1_2
        ctx.load_cert_chain(self._cert, self._key)
        fell_back = False
        try:
            self._server = _Server((self._host, self._want_port), _Handler, ctx, self)
        except OSError:
            self._server = _Server((self._host, 0), _Handler, ctx, self)  # taken: any free port
            fell_back = True
        self.stats = ServerStats(self.port, self._want_port, fell_back)
        if fell_back:
            logger.warning("Browser camera: port %d was taken, listening on %d instead", self._want_port, self.port)
        else:
            logger.info("Browser camera: listening on port %d", self.port)
        self._thread = threading.Thread(target=self._server.serve_forever, name="browser-camera", daemon=True)
        self._thread.start()

    def stop(self):
        server, self._server = self._server, None
        if server is not None:
            server.shutdown()
            server.server_close()
        self._close_all()

    def _close_all(self):
        # Not shut down from here: that doesn't wake a blocked read on Windows, and an SSL socket mustn't be touched
        # from another thread while its own reads it
        with self._socks_lock:
            self._kicked |= self._socks

    def serve_browser(self, sock, bid: str = DEFAULT_ID):
        """One browser's connection, on its handler thread, until it ends or a newer one takes over."""
        feed = self.hub.join(bid)
        if feed is None:
            _send_quietly(_WsConnection(sock, lambda: None), ws_close_frame(CLOSE_FULL, "Every camera is in use"))
            return
        gen = feed.connect()
        with self._socks_lock:
            self._socks.add(sock)
        sent = [-1]
        keys_asked = [feed.keyframe_requests()]

        def tick():
            if not feed.current(gen):
                raise _Closed("replaced")
            if sock in self._kicked:
                raise _Closed("ended here")
            seq, cfg = feed.page_config()
            if seq != sent[0]:
                sent[0] = seq
                ws.send(ws_frame(0x1, json.dumps(dict(cfg, type="config")).encode()))
            if (keys := feed.keyframe_requests()) != keys_asked[0]:
                keys_asked[0] = keys
                ws.send(ws_frame(0x1, b'{"type":"keyframe"}'))

        sock.settimeout(_TICK_S)
        ws = _WsConnection(sock, tick)
        self._on_change()
        try:
            tick()
            while True:
                opcode, payload = ws.read_message()
                tick()
                if opcode == 0x2 and payload:
                    if payload[0] == FRAME_JPEG:
                        feed.put_frame(gen, payload[1:])
                    elif payload[0] == FRAME_PCM:
                        feed.put_audio(gen, payload[1:])
                    elif payload[0] == FRAME_H264 and len(payload) > 2 and feed.h264:
                        feed.put_h264(gen, payload[1] == 1, payload[2:])
                elif opcode == 0x1 and len(payload) <= _MAX_TEXT_BYTES:
                    self._on_text(feed, gen, payload)
        except _Closed as why:
            if str(why) == "replaced":
                _send_quietly(ws, ws_close_frame(CLOSE_REPLACED, "Opened somewhere else"))
            elif str(why) == "ended here":
                _send_quietly(ws, ws_close_frame(1001))
        except (OSError, ssl.SSLError):
            pass
        finally:
            with self._socks_lock:
                self._socks.discard(sock)
                self._kicked.discard(sock)
            feed.disconnect(gen)
            self._on_change()

    def _on_text(self, feed: BrowserFeed, gen: int, payload: bytes):
        try:
            msg = json.loads(payload.decode("utf-8"))
        except (UnicodeDecodeError, ValueError):
            return
        if not isinstance(msg, dict) or msg.get("type") != "hello":
            return
        device = _clean(msg.get("device"), 48)
        mic_error = _clean(msg.get("mic_error"), 160)
        codec = msg.get("codec") if msg.get("codec") in ("h264", "jpeg") else ""
        feed.note_hello(gen, device, mic_error, codec, _clean(msg.get("codec_note"), 160))
        self._on_change()


def _send_quietly(ws: _WsConnection, data: bytes):
    try:
        ws.send(data)
    except (OSError, ssl.SSLError):
        pass


def _clean(value, limit: int) -> str:
    """A string from the browser as plain display text."""
    if not isinstance(value, str):
        return ""
    return "".join(c for c in value if c.isprintable() and c not in "<>")[:limit]
