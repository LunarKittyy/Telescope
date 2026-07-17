import logging
import urllib.request
from typing import Optional

import cv2
import numpy as np

logger = logging.getLogger(__name__)

_CHUNK = 4096
_MAX_PART_HEADER_BYTES = 4096


class MjpegReader:
    """Reads an authenticated multipart/x-mixed-replace MJPEG stream and
    decodes each part to a BGR numpy frame.

    Mirrors the small subset of cv2.VideoCapture's interface (isOpened,
    read, release) that StreamWorker already drives, so it drops in without
    changing the reconnect/reader control flow. Needed because the phone's
    MJPEG endpoint now requires a bearer token, which cv2.VideoCapture's
    opaque FFmpeg-backed HTTP client has no way to send.
    """

    def __init__(self, url: str, token: str, timeout: float = 3.0):
        self.url = url
        self.token = token
        self.timeout = timeout
        self._response = None
        self._boundary: Optional[bytes] = None
        self._buf = b""

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
        # The server's boundary parameter already includes its own leading
        # "--" (matches exactly what appears on each part's delimiter line -
        # see MjpegServer.kt's MjpegClient.stream()), so it's used as-is
        # rather than re-prefixed per the general multipart RFC convention.
        boundary = content_type.split("boundary=", 1)[1].strip().strip('"')
        self._boundary = boundary.encode("utf-8")
        self._response = resp
        self._buf = b""
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
        return True, frame

    def release(self):
        if self._response is not None:
            try:
                self._response.close()
            except Exception:
                pass
        self._response = None
        self._buf = b""
        self._boundary = None

    # ── Multipart parsing ────────────────────────────────────────────────

    def _fill(self, n: int) -> bool:
        """Reads more from the connection until at least n bytes are buffered."""
        while len(self._buf) < n:
            chunk = self._response.read(_CHUNK)
            if not chunk:
                return False
            self._buf += chunk
        return True

    def _read_line(self) -> Optional[bytes]:
        while b"\r\n" not in self._buf:
            if len(self._buf) > _MAX_PART_HEADER_BYTES:
                return None
            chunk = self._response.read(_CHUNK)
            if not chunk:
                return None
            self._buf += chunk
        line, self._buf = self._buf.split(b"\r\n", 1)
        return line

    def _read_part(self) -> Optional[bytes]:
        # Skip to and past the next boundary line.
        while True:
            line = self._read_line()
            if line is None:
                return None
            stripped = line.strip()
            if stripped == self._boundary or stripped == self._boundary + b"--":
                break
        # Part headers, terminated by a blank line.
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
        jpeg = self._buf[:content_length]
        self._buf = self._buf[content_length:]
        # Consume the trailing CRLF after the JPEG bytes.
        if not self._fill(2):
            return None
        self._buf = self._buf[2:]
        return jpeg
