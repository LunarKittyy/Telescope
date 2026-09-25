import logging
import urllib.request
from typing import Optional

import cv2
import numpy as np

logger = logging.getLogger(__name__)

_CHUNK = 4096
_MAX_PART_HEADER_BYTES = 4096


class MjpegReader:
    """Authenticated multipart/x-mixed-replace MJPEG reader (mirrors cv2.VideoCapture interface; handles bearer token)."""

    def __init__(self, url: str, token: str, timeout: float = 3.0):
        self.url = url
        self.token = token
        self.timeout = timeout
        self._response = None
        self._boundary: Optional[bytes] = None
        self._buf = bytearray()
        # JPEG wire size from most recent read (read cross-thread by vcam loop for throughput).
        self.last_frame_bytes = 0

    def isOpened(self) -> bool:
        return self._response is not None

    def open(self) -> bool:
        try:
            req = urllib.request.Request(
                self.url, headers={"Authorization": f"Bearer {self.token}"}
            )
            resp = urllib.request.urlopen(req, timeout=self.timeout)
        except Exception:
            return False
        content_type = resp.headers.get("Content-Type", "")
        if "multipart/x-mixed-replace" not in content_type or "boundary=" not in content_type:
            try:
                resp.close()
            except Exception:
                pass
            return False
        # Server boundary parameter includes leading "--" (use as-is, not re-prefixed).
        boundary = content_type.split("boundary=", 1)[1].strip().strip('"')
        self._boundary = boundary.encode("utf-8")
        self._response = resp
        self._buf = bytearray()
        return True

    def read(self):
        if self._response is None:
            return False, None
        try:
            jpeg = self._read_part()
        except Exception:
            return False, None
        if jpeg is None:
            return False, None
        frame = cv2.imdecode(np.frombuffer(jpeg, dtype=np.uint8), cv2.IMREAD_COLOR)
        if frame is None:
            return False, None
        self.last_frame_bytes = len(jpeg)
        return True, frame

    def release(self):
        if self._response is not None:
            try:
                self._response.close()
            except Exception:
                pass
        self._response = None
        self._buf = bytearray()
        self._boundary = None

    # ── Multipart parsing ────────────────────────────────────────────────

    def _recv(self, n: int) -> bool:
        # read1() hands back whatever has arrived; read(n) would block until n bytes, i.e. until the next frame starts.
        chunk = self._response.read1(n)
        if not chunk:
            return False
        self._buf += chunk
        return True

    def _fill(self, n: int) -> bool:
        while len(self._buf) < n:
            if not self._recv(max(n - len(self._buf), _CHUNK)):
                return False
        return True

    def _read_line(self) -> Optional[bytes]:
        """Read until line break, return None if header exceeded limit."""
        while (idx := self._buf.find(b"\r\n")) < 0:
            if len(self._buf) > _MAX_PART_HEADER_BYTES:
                return None
            if not self._recv(_CHUNK):
                return None
        line = bytes(self._buf[:idx])
        del self._buf[:idx + 2]
        return line

    def _read_part(self) -> Optional[bytes]:
        while True:
            line = self._read_line()
            if line is None:
                return None
            stripped = line.strip()
            if stripped == self._boundary or stripped == self._boundary + b"--":
                break
        content_length = None
        while True:
            line = self._read_line()
            if line is None:
                return None
            if line == b"":
                break
            if line.lower().startswith(b"content-length:"):
                try:
                    content_length = int(line.split(b":", 1)[1].strip())
                except ValueError:
                    content_length = None
        if content_length is None:
            return None
        if not self._fill(content_length):
            return None
        jpeg = bytes(self._buf[:content_length])
        del self._buf[:content_length]
        # The part's trailing CRLF is left for the next boundary scan, which skips it as an empty line.
        return jpeg
