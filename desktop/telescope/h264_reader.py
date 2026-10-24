"""Authenticated H.264 reader for the phone's /v1/video.h264 route (Annex-B over one HTTP response).

Same interface as MjpegReader, so StreamWorker doesn't care which one it has. Decoding is PyAV's
bundled FFmpeg; without PyAV installed, available() is False and the desktop only offers MJPEG.
"""

import importlib.util
import logging
import re
import sys
import threading
import urllib.request
import weakref
from typing import Optional

from telescope.pinned_https import PhoneAuth

logger = logging.getLogger(__name__)

# PyAV is optional (MJPEG works without it) and slow to load, so it's imported by the first thing that decodes
_av_module = None
_av_failed = False
_av_lock = threading.Lock()  # two threads importing PyAV at once can deadlock on its submodules

_CHUNK = 64 * 1024
# Data with no finished frame after this much isn't a stream the phone sent; stop instead of buffering on.
_MAX_BYTES_WITHOUT_FRAME = 32 * 1024 * 1024


# Hardware decoders to try, best first: D3D11 works on any Windows GPU. On Linux, PyAV's own wheel only has CUDA (NVIDIA); one
# built against the system FFmpeg also has VA-API (AMD and Intel)
_HW_TYPES = ("d3d11va", "cuda") if sys.platform == "win32" else ("vaapi", "cuda")
_hw_wanted = False
_hw_found: Optional[str] = None  # the device type that opened, once probed
_hw_probed = False
_hw_failed = False  # a hardware decoder errored this run: software from then on
_hw_lock = threading.Lock()
_decoders = weakref.WeakSet()
_ANNEX_B_START = re.compile(b"\x00\x00(?:\x00)?\x01")


def _av():
    """The av module, imported on first use; None if PyAV isn't installed."""
    global _av_module, _av_failed
    if _av_module is None and not _av_failed:
        with _av_lock:
            if _av_module is None and not _av_failed:
                try:
                    import av
                    _av_module = av
                except ImportError:
                    _av_failed = True
    return _av_module


def loaded() -> bool:
    """Whether PyAV has been imported (by the start-up preload or a decoder) and decoding can start right away."""
    return _av_module is not None


def load_av():
    """Imports PyAV behind the same lock as the decoders, for code that imports something built on it (aiortc)."""
    return _av()


def available() -> bool:
    """Whether PyAV is installed. Looks without importing it, so asking at startup stays cheap."""
    if _av_failed:
        return False
    return _av_module is not None or importlib.util.find_spec("av") is not None


def set_hardware(on: bool):
    """Whether new decoders try the graphics card (Advanced > Hardware decoding). Takes effect on the next connect."""
    global _hw_wanted
    _hw_wanted = bool(on)


def hardware_device() -> Optional[str]:
    """The graphics card decoder this computer has ("d3d11va", "cuda"), or None. Probed once, by opening one."""
    global _hw_probed, _hw_found
    with _hw_lock:
        if not _hw_probed:
            _hw_probed = True
            _hw_found = next((t for t in _hw_types() if _open_hw(t) is not None), None)
        return _hw_found


def hardware_in_use() -> bool:
    with _hw_lock:
        return any(d.hardware is not None for d in list(_decoders))


def decoding_backends() -> tuple:
    """Backends of live decoder instances, independent of the preference for the next connection."""
    with _hw_lock:
        return tuple(sorted({d.hardware or "processor" for d in list(_decoders)}))


def hardware_failed() -> bool:
    return _hw_failed


def _hardware_requested() -> bool:
    return _hw_wanted and not _hw_failed and hardware_device() is not None


def _hw_types() -> list:
    if _av() is None:
        return []
    try:
        from av.codec.hwaccel import hwdevices_available
        offered = set(hwdevices_available())
    except Exception:  # an older PyAV without hardware decoding
        return []
    return [t for t in _HW_TYPES if t in offered]


def _open_hw(device_type: str):
    try:
        from av.codec.hwaccel import HWAccel
        codec = _av().CodecContext.create("h264", "r", hwaccel=HWAccel(device_type=device_type, allow_software_fallback=False))
        return codec if codec.is_hwaccel else None
    except Exception:
        return None


class ToBgr:
    """Converts decoded frames to BGR with one reused converter. frame.to_ndarray() builds a new one, threads and all, per frame."""

    _THREADS = 4

    def __init__(self):
        _av()
        from av.video.reformatter import VideoReformatter
        self._reformatter = VideoReformatter()
        self._threads = True

    def __call__(self, frame):
        if self._threads:
            try:
                return self._reformatter.reformat(frame, format="bgr24", threads=self._THREADS).to_ndarray()
            except TypeError:  # PyAV before 14 has no threads argument
                self._threads = False
        return self._reformatter.reformat(frame, format="bgr24").to_ndarray()


def _software_codec():
    codec = _av().CodecContext.create("h264", "r")
    # Show each frame as soon as it's decoded, instead of holding some back for reordering.
    codec.options = {"flags": "low_delay"}
    codec.thread_count = 2
    return codec


class H264Reader:
    def __init__(self, url: str, auth: PhoneAuth, timeout: float = 3.0):
        self.url = url
        self.auth = auth
        self.timeout = timeout
        self._response = None
        self._codec = None
        self._pending_bytes = 0
        # Wire bytes behind the most recent frame read() returned (StreamWorker's throughput).
        self.last_frame_bytes = 0
        # Frames that arrived for it, the ones skipped to keep up included (StreamWorker's "keeping up").
        self.last_frame_count = 0
        self._pending_frames = 0

    def isOpened(self) -> bool:
        return self._response is not None

    def open(self) -> bool:
        if not available():
            return False
        try:
            req = urllib.request.Request(self.url, headers=self.auth.headers())
            resp = self.auth.open(req, timeout=self.timeout)
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
        self._pending_frames = 0
        return True

    # Each frame refers to the ones before it, so read_packet() decodes in order and decode() has nothing left to do.
    parallel_decode = False

    def read_packet(self):
        return self.read()

    @staticmethod
    def decode(frame):
        return frame

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
                if self._pending_bytes > _MAX_BYTES_WITHOUT_FRAME:
                    return False, None
                frame, count = decode_counted(self._codec, chunk)
                self._pending_frames += count
                if frame is not None:
                    self.last_frame_bytes, self._pending_bytes = self._pending_bytes, 0
                    self.last_frame_count, self._pending_frames = self._pending_frames, 0
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


class Decoder:
    """An H.264 decoder on the graphics card when that's on and works, else in software. A hardware one that errors is
    swapped for software on the spot (the picture resumes at the next keyframe) and not tried again this run."""

    def __init__(self):
        self.hardware: Optional[str] = None
        self._codec = None
        self._headers = {}
        self._header_tail = b""
        self._hw_proven = False  # decoded a frame: from then on a bad packet is the stream's fault, not the card's
        self._to_bgr = ToBgr() if available() else None
        if _hardware_requested():
            device = hardware_device()
            self._codec = _open_hw(device)
            if self._codec is not None:
                self._codec.options = {"flags": "low_delay"}
                self.hardware = device
        if self._codec is None:
            self._codec = _software_codec()
        with _hw_lock:
            _decoders.add(self)

    def feed(self, data: bytes):
        """(newest BGR frame the data completed or None, how many frames it completed)."""
        self._remember_headers(data)
        try:
            frame, count = _decode_counted(self._codec, data, fail_on_invalid=self.hardware is not None and not self._hw_proven,
                                           to_bgr=self._to_bgr)
            self._hw_proven = self._hw_proven or count > 0
            return frame, count
        except Exception:
            if self.hardware is None:
                raise
            global _hw_failed
            _hw_failed = True
            logger.warning("Hardware decoding (%s) failed; decoding in software from now on", self.hardware, exc_info=True)
            self.hardware = None
            self._codec = _software_codec()
            # TCP sends SPS and PPS only when joining, and a fresh decoder can't start from a keyframe alone
            setup = b"".join(self._headers[k] for k in (7, 8) if k in self._headers)
            return _decode_counted(self._codec, setup + data, to_bgr=self._to_bgr)

    def _remember_headers(self, data: bytes):
        data = self._header_tail + data
        starts = list(_ANNEX_B_START.finditer(data))
        for first, last in zip(starts, starts[1:]):
            if first.end() < last.start() and (kind := data[first.end()] & 31) in (7, 8):
                self._headers[kind] = data[first.start():last.start()]
        # Keep split start codes and incomplete SPS/PPS, without retaining ordinary video frames.
        last = starts[-1] if starts else None
        if last is not None and last.end() < len(data) and data[last.end()] & 31 in (7, 8):
            self._header_tail = data[last.start():][-65536:]
        else:
            self._header_tail = data[-4:]


def new_decoder() -> Decoder:
    return Decoder()


def decode_newest(codec, data: bytes):
    """Feed Annex-B bytes to the decoder; the newest frame they completed (BGR ndarray), or None.
    Older frames from the same data are dropped, which keeps latency down after a stall."""
    return decode_counted(codec, data)[0]


def decode_counted(codec, data: bytes):
    """decode_newest(), and how many frames the data completed (the dropped ones too)."""
    if isinstance(codec, Decoder):
        return codec.feed(data)
    return _decode_counted(codec, data)


def _decode_counted(codec, data: bytes, fail_on_invalid: bool = False, to_bgr=None):
    newest, count = None, 0
    for packet in codec.parse(data):
        try:
            frames = codec.decode(packet)
        except _av().error.InvalidDataError:
            if fail_on_invalid:
                raise  # hardware refusal must reach Decoder's software fallback too
            continue  # e.g. joined mid-GOP: skip until a keyframe
        if frames:
            newest = frames[-1]
            count += len(frames)
    if newest is None:
        return None, count
    return (to_bgr(newest) if to_bgr is not None else newest.to_ndarray(format="bgr24")), count
