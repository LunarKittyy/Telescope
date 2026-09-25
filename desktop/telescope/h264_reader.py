"""Authenticated H.264 reader for the phone's /v1/video.h264 route (Annex-B over one HTTP response).

Same interface as MjpegReader, so StreamWorker doesn't care which one it has. Decoding is PyAV's
bundled FFmpeg; without PyAV installed, available() is False and the desktop only offers MJPEG.
"""

import logging
import urllib.request

logger = logging.getLogger(__name__)

try:
    import av
except ImportError:  # optional: MJPEG works without it
    av = None

_CHUNK = 64 * 1024


def available() -> bool:
    return av is not None


class H264Reader:
    def __init__(self, url: str, token: str, timeout: float = 3.0):
        self.url = url
        self.token = token
        self.timeout = timeout
        self._response = None
        self._codec = None
        self._pending_bytes = 0
        # Wire bytes behind the most recent frame read() returned (StreamWorker's throughput).
        self.last_frame_bytes = 0

    def isOpened(self) -> bool:
        return self._response is not None

    def open(self) -> bool:
        if av is None:
            return False
        try:
            req = urllib.request.Request(self.url, headers={"Authorization": f"Bearer {self.token}"})
            resp = urllib.request.urlopen(req, timeout=self.timeout)
        except Exception:
            return False
        if "video/h264" not in resp.headers.get("Content-Type", ""):
            try:
                resp.close()
            except Exception:
                pass
            return False
        self._codec = new_decoder()
        self._response = resp
        self._pending_bytes = 0
        return True

    def read(self):
        """(True, BGR frame) for the newest frame decoded from the next data that has any, else (False, None)."""
        if self._response is None:
            return False, None
        try:
            while True:
                chunk = self._response.read1(_CHUNK)
                if not chunk:
                    return False, None
                self._pending_bytes += len(chunk)
                frame = decode_newest(self._codec, chunk)
                if frame is not None:
                    self.last_frame_bytes, self._pending_bytes = self._pending_bytes, 0
                    return True, frame
        except Exception:
            return False, None

    def release(self):
        if self._response is not None:
            try:
                self._response.close()
            except Exception:
                pass
        self._response = None
        self._codec = None


def new_decoder():
    codec = av.CodecContext.create("h264", "r")
    # Show each frame as soon as it's decoded, instead of holding some back for reordering.
    codec.options = {"flags": "low_delay"}
    codec.thread_count = 2
    return codec


def decode_newest(codec, data: bytes):
    """Feed Annex-B bytes to the decoder; the newest frame they completed (BGR ndarray), or None.
    Older frames from the same data are dropped, which keeps latency down after a stall."""
    newest = None
    for packet in codec.parse(data):
        try:
            frames = codec.decode(packet)
        except av.error.InvalidDataError:
            continue  # e.g. joined mid-GOP: skip until a keyframe
        if frames:
            newest = frames[-1]
    return newest.to_ndarray(format="bgr24") if newest is not None else None
