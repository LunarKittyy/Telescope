"""Experimental H.264 over UDP for Wi-Fi (/v1/video.udp, docs/protocol.md), falling back to H264Reader's TCP stream."""

import base64
import importlib.util
import json
import logging
import select
import socket
import struct
import time
import urllib.parse
import urllib.request
from typing import Optional

from telescope import h264_reader
from telescope.h264_reader import H264Reader
from telescope.pinned_https import PhoneAuth

logger = logging.getLogger(__name__)


def _aesgcm():
    """cryptography's AESGCM, imported when a stream seals its first datagram. None without it: then only TCP."""
    try:
        from cryptography.hazmat.primitives.ciphers.aead import AESGCM
    except ImportError:  # it's a dependency
        return None
    return AESGCM


def _can_seal() -> bool:
    """Whether AESGCM is installed. Looks without importing it, so a start-up check stays cheap."""
    return importlib.util.find_spec("cryptography") is not None

HEADER = struct.Struct(">IHHB")  # frame, index, count, flags
FLAG_KEY = 1
TO_DESKTOP, TO_PHONE = 1, 2
NACK, KEYFRAME, HELLO = 1, 2, 3
WINDOW = 4096  # replay window, in datagrams
HELLO_S = 0.5  # keeps the desktop's firewall open for the phone's datagrams
FIRST_FRAME_S = 3.0  # nothing decodable by then: the datagrams don't get here, use TCP
NACK_AFTER_S = 0.015  # a frame behind newer ones this long is missing pieces...
NACK_AGAIN_S = 0.05  # ...asked for again this often...
GIVE_UP_S = 0.25  # ...and given up on after this: freeze until a keyframe
KEY_ASK_S = 0.3
MAX_FRAMES = 300  # in reassembly at once
_UDP_FAILED: set = set()  # phones UDP didn't get through from, for the rest of this run


class Seal:
    """AES-GCM for one direction: each datagram is its counter (8 bytes) then the sealed plaintext."""

    def __init__(self, key: bytes, direction: int):
        self._aead = _aesgcm()(key)
        self._direction = direction
        self._counter = 0
        self._highest = -1
        self._seen: set = set()

    def _nonce(self, n: int) -> bytes:
        return bytes([self._direction, 0, 0, 0]) + struct.pack(">q", n)

    def seal(self, plain: bytes) -> bytes:
        n, self._counter = self._counter, self._counter + 1
        return struct.pack(">q", n) + self._aead.encrypt(self._nonce(n), plain, None)

    def open(self, datagram: bytes) -> Optional[bytes]:
        """The plaintext, or None for anything forged, damaged, replayed or far too old."""
        if len(datagram) < 8 + 16:
            return None
        n = struct.unpack(">q", datagram[:8])[0]
        if n < 0 or n <= self._highest - WINDOW or n in self._seen:
            return None
        try:
            plain = self._aead.decrypt(self._nonce(n), datagram[8:], None)
        except Exception:
            return None
        self._seen.add(n)
        if n > self._highest:
            self._highest = n
            if len(self._seen) > 2 * WINDOW:
                self._seen = {s for s in self._seen if s > n - WINDOW}
        return plain


class _Frame:
    __slots__ = ("count", "parts", "key", "noticed", "asked", "asks", "progress")

    def __init__(self):
        self.count = 0  # 0 until a piece of it arrived
        self.parts: dict = {}
        self.key = False
        self.noticed = None  # when it was first seen holding up newer frames
        self.asked = float("-inf")
        self.asks = 0
        self.progress = 0

    def complete(self) -> bool:
        return self.count > 0 and len(self.parts) == self.count

    def missing(self) -> list:
        return [i for i in range(self.count) if i not in self.parts]

    def notice(self, now: float):
        # A large keyframe gets time to finish while new fragments are still arriving.
        if self.noticed is None or (self.key and self.progress != len(self.parts)):
            self.noticed = now
            self.progress = len(self.parts)


class Reassembler:
    """Puts datagrams back into whole frames, in order. Pure, so tests drive it with their own clock."""

    def __init__(self):
        self.frames: dict = {}
        self.next: Optional[int] = None  # the next frame to hand on; None while waiting for a keyframe
        self.newest = -1
        self.dropped = 0  # frames given up on

    def add(self, plain: bytes):
        if len(plain) < HEADER.size:
            return
        seq, idx, count, flags = HEADER.unpack_from(plain)
        if count == 0 or idx >= count or (self.next is not None and seq < self.next):
            return
        f = self.frames.get(seq)
        if f is None:
            if len(self.frames) >= MAX_FRAMES:
                if not flags & FLAG_KEY:
                    return
                # A fresh keyframe is the way out of a backlog, so make room for it.
                self._forget_before(seq)
                if len(self.frames) >= MAX_FRAMES:
                    return
                self.next = None
            f = self.frames[seq] = _Frame()
        f.count, f.key = count, bool(flags & FLAG_KEY)
        f.parts[idx] = plain[HEADER.size:]
        self.newest = max(self.newest, seq)

    def take(self) -> Optional[tuple]:
        """(frame bytes, how many frames it stands for) for the next whole frame in order, else None."""
        if self.next is None:
            keys = [s for s, f in self.frames.items() if f.key and f.complete()]
            if not keys:
                return None
            start = max(keys)
            skipped = sum(1 for s in self.frames if s < start)
            self._forget_before(start)
            self.next = start
        else:
            skipped = 0
        f = self.frames.get(self.next)
        if f is None or not f.complete():
            return None
        del self.frames[self.next]
        self.next += 1
        return b"".join(f.parts[i] for i in range(f.count)), 1 + skipped

    def chase(self, now: float) -> tuple:
        """([(frame, missing indexes, [] for all of it)] to ask the phone for, whether to ask for a keyframe)."""
        if self.next is None:
            keys = [s for s, f in self.frames.items() if f.key]
            if not keys:
                return [], self.newest >= 0
            # Repair the newest partial keyframe instead of repeatedly replacing it with another large burst.
            seq = max(keys)
            f = self.frames[seq]
            if f.complete():
                return [], False
            f.notice(now)
            if now - f.noticed >= GIVE_UP_S:
                self._forget_before(seq + 1)
                return [], True
            if now - f.noticed >= NACK_AFTER_S and now - f.asked >= NACK_AGAIN_S:
                f.asked = now
                return [(seq, f.missing())], False
            return [], False
        nacks = []
        for seq in range(self.next, self.newest + 1):
            f = self.frames.get(seq)
            if f is None:
                if len(self.frames) >= MAX_FRAMES:
                    break
                f = self.frames[seq] = _Frame()  # none of it came, but newer ones did
            if f.complete():
                continue
            f.notice(now)
            waited = now - f.noticed
            if waited >= GIVE_UP_S:
                self.dropped += 1
                self.next = None  # freeze until a keyframe rather than decode on a broken reference
                self._forget_before(seq + 1)
                return [], True
            patience = NACK_AFTER_S if seq < self.newest else NACK_AFTER_S * 4  # the newest may still be arriving
            if waited >= patience and now - f.asked >= NACK_AGAIN_S:
                f.asked, f.asks = now, f.asks + 1
                nacks.append((seq, f.missing()))
        return nacks, False

    def _forget_before(self, seq: int):
        for s in [s for s in self.frames if s < seq]:
            del self.frames[s]


class UdpH264Reader:
    def __init__(self, url: str, auth: PhoneAuth, timeout: float = 3.0):
        self.url = url
        self.auth = auth
        self.timeout = timeout
        self.last_frame_bytes = 0
        self.last_frame_count = 0
        self._tcp: Optional[H264Reader] = None  # the fallback, once UDP didn't work out
        self._response = None
        self._sock: Optional[socket.socket] = None
        self._phone = None
        self._seal_in = self._seal_out = None
        self._codec = None
        self._frames = Reassembler()
        self._opened_at = 0.0
        self._decoded_any = False
        self._last_hello = 0.0
        self._last_key_ask = 0.0
        self.fell_back = False

    parallel_decode = False

    @staticmethod
    def decode(frame):
        return frame

    def isOpened(self) -> bool:
        return self._tcp.isOpened() if self._tcp is not None else self._response is not None

    def tcp_url(self) -> str:
        return self.url[: -len(".udp")] + ".h264"

    def open(self) -> bool:
        if not h264_reader.available():
            return False
        host = urllib.parse.urlsplit(self.url).hostname
        if self._tcp is None and _can_seal() and host not in _UDP_FAILED and self._open_udp():
            return True
        return self._fall_back()

    def _open_udp(self) -> bool:
        self._close_udp()
        sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        try:
            sock.setsockopt(socket.SOL_SOCKET, socket.SO_RCVBUF, 4 << 20)
            sock.bind(("0.0.0.0", 0))
            sock.setblocking(False)
            req = urllib.request.Request(f"{self.url}?port={sock.getsockname()[1]}", headers=self.auth.headers())
            resp = self.auth.open(req, timeout=self.timeout)
            if "application/x-telescope-udp" not in resp.headers.get("Content-Type", ""):
                resp.close()
                raise OSError("not a UDP session")
            hello = json.loads(resp.readline(4096))
            key = base64.b64decode(hello["key"])
            phone_port = int(hello["port"])
        except Exception as e:
            logger.info("UDP video didn't start (%s); using TCP", e)
            sock.close()
            return False
        host = urllib.parse.urlsplit(self.url).hostname
        self._sock, self._response, self._phone = sock, resp, (host, phone_port)
        self._seal_in, self._seal_out = Seal(key, TO_DESKTOP), Seal(key, TO_PHONE)
        self._codec = h264_reader.new_decoder()
        self._frames = Reassembler()
        self._opened_at = time.monotonic()
        self._decoded_any = False
        self._hello(force=True)
        return True

    def _fall_back(self) -> bool:
        self._close_udp()
        if not self.fell_back:
            self.fell_back = True
            logger.info("UDP video fell back to TCP")
        self._tcp = H264Reader(self.tcp_url(), self.auth, self.timeout)
        return self._tcp.open()

    def read_packet(self):
        return self.read()

    def read(self):
        """(True, BGR frame) for the next frame decoded, else (False, None) when the stream's over."""
        if self._tcp is not None:
            ok, frame = self._tcp.read()
            self.last_frame_bytes, self.last_frame_count = self._tcp.last_frame_bytes, self._tcp.last_frame_count
            return ok, frame
        if self._sock is None:
            return False, None
        count = 0
        size = 0
        deadline = time.monotonic() + self.timeout
        try:
            while True:
                now = time.monotonic()
                if not self._decoded_any and now - self._opened_at > FIRST_FRAME_S:
                    _UDP_FAILED.add(self._phone[0])  # likely a firewall: don't wait for it again
                    if self._fall_back():
                        return self.read()
                    return False, None
                if now > deadline:  # the phone stopped sending: the session's over
                    return False, None
                self._hello()
                ready, _, _ = select.select([self._sock], [], [], 0.005)
                if ready:
                    self._receive()
                taken = self._frames.take()
                while taken is not None:
                    data, n = taken
                    count += n
                    size += len(data)
                    frame, _ = h264_reader.decode_counted(self._codec, data)
                    if frame is not None:
                        self._decoded_any = True
                        self.last_frame_bytes, self.last_frame_count = size, count
                        return True, frame
                    taken = self._frames.take()
                self._chase(now)
        except Exception:
            logger.exception("UDP video failed")
            return False, None

    def _receive(self):
        for _ in range(256):
            try:
                datagram, addr = self._sock.recvfrom(2048)
            except BlockingIOError:
                return
            if addr[0] != self._phone[0]:
                continue
            plain = self._seal_in.open(datagram)
            if plain is not None:
                self._frames.add(plain)

    def _chase(self, now: float):
        nacks, want_key = self._frames.chase(now)
        for seq, missing in nacks:
            body = struct.pack(">BI", NACK, seq) + b"".join(struct.pack(">H", i) for i in missing[:400])
            self._send(body)
        if want_key and now - self._last_key_ask >= KEY_ASK_S:
            self._last_key_ask = now
            self._send(bytes([KEYFRAME]))

    def _hello(self, force: bool = False):
        now = time.monotonic()
        if force or now - self._last_hello >= HELLO_S:
            self._last_hello = now
            self._send(bytes([HELLO]))

    def _send(self, plain: bytes):
        try:
            self._sock.sendto(self._seal_out.seal(plain), self._phone)
        except OSError:
            pass

    def _close_udp(self):
        for thing in (self._response, self._sock):
            if thing is not None:
                try:
                    thing.close()
                except Exception:
                    pass
        self._response = self._sock = None

    def release(self):
        self._close_udp()
        if self._tcp is not None:
            self._tcp.release()
        self._codec = None
