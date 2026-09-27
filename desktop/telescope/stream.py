import itertools
import logging
import os
import threading
import time
from typing import Optional

import cv2
import numpy as np
import pyvirtualcam
from PyQt6.QtCore import QThread, pyqtSignal

from telescope import vcam
from telescope.h264_reader import H264Reader
from telescope.mjpeg_reader import MjpegReader
from telescope.pinned_https import PhoneAuth

logger = logging.getLogger(__name__)

RECONNECT_DELAY = 3
# JPEG decoders running side by side: at 4K one decode takes longer than a frame lasts.
MJPEG_DECODERS = max(1, min(2, (os.cpu_count() or 1) - 1))

# Sentinel: "leave unchanged" (distinct from None = pass-through).
_UNCHANGED = object()


def _fit_frame(frame, target_w, target_h):
    """Resize frame preserving aspect (black bars as needed; zero-copy if already matches)."""
    fh, fw = frame.shape[:2]
    if fw == target_w and fh == target_h:
        return frame

    scale = min(target_w / fw, target_h / fh)
    new_w = int(fw * scale)
    new_h = int(fh * scale)

    if new_w == target_w and new_h == target_h:
        return cv2.resize(frame, (target_w, target_h),
                          interpolation=cv2.INTER_LINEAR)

    resized = cv2.resize(frame, (new_w, new_h),
                         interpolation=cv2.INTER_LINEAR)
    canvas = np.zeros((target_h, target_w, 3), dtype=frame.dtype)
    x_off = (target_w - new_w) // 2
    y_off = (target_h - new_h) // 2
    canvas[y_off:y_off + new_h, x_off:x_off + new_w] = resized
    return canvas


class _Newest:
    """Hands the newest packet from the reader to the decoders; one nobody took in time is dropped, so a slow decoder
    skips frames instead of letting them queue up (in here or in the TCP buffers) as lag."""

    def __init__(self):
        self._cond = threading.Condition()
        self._item = None

    def put(self, item):
        with self._cond:
            self._item = item
            self._cond.notify()

    def take(self, done: threading.Event):
        """The newest item, waiting for one; None once done is set and nothing is left."""
        with self._cond:
            while self._item is None:
                if done.is_set():
                    return None
                self._cond.wait(0.1)
            item, self._item = self._item, None
            return item


class StreamWorker(QThread):
    status      = pyqtSignal(str, str)   # (kind, msg): info/ok/warn/fps/idle
    reconnected = pyqtSignal()           # mid-stream reconnect succeeded (not the initial connect)
    vcam_opened = pyqtSignal(int, int)   # the virtual camera opened at this width, height

    def __init__(self, url: str, width: Optional[int], height: Optional[int],
                 fps: int, frame_pipeline: list = None,
                 canvas_width: Optional[int] = None,
                 canvas_height: Optional[int] = None,
                 auth: Optional[PhoneAuth] = None):
        super().__init__()
        self.url       = url
        self.auth      = auth
        self._width    = width
        self._height   = height
        self._fps      = fps
        self._pipeline = frame_pipeline or []
        self._canvas_w = canvas_width
        self._canvas_h = canvas_height
        self._stop_flag    = False
        self._restart_vcam = threading.Event()
        self._retry_now    = threading.Event()  # retarget(): skip the rest of the reconnect wait
        # The newest processed frame (BGR, as the decoders give it and the virtual camera takes it).
        self._latest       = None
        self._frame_ready  = threading.Event()  # set when _latest changes, so the vcam sends it at once
        self._seq          = itertools.count(1)
        self._shown_seq    = 0  # the newest packet that made it to _latest; an older one finishing later is dropped
        self._publish_lock = threading.Lock()
        # Cumulative wire bytes; vcam loop reads deltas, not resets on mid-stream reconnect.
        self._bytes_total  = 0
        # Frames decoded and shown; separate from the vcam send rate (which resends the latest frame regardless).
        self._frames_received = 0
        # Consecutive 2s windows with sustained low decode rate (distinguishes congestion from blips).
        self._weak_streak  = 0

    def _process(self, frame):
        for fn in self._pipeline:
            frame = fn(frame)
        return frame

    def update_output(self, width=_UNCHANGED, height=_UNCHANGED, fps=_UNCHANGED):
        """Update stream parameters live (None = pass-through; omit to leave unchanged; fps changes restart vcam)."""
        if width  is not _UNCHANGED: self._width  = width
        if height is not _UNCHANGED: self._height = height
        if fps is not _UNCHANGED:
            self._fps = fps
            self._restart_vcam.set()
            self._frame_ready.set()

    def request_stop(self):
        self._stop_flag = True
        self._restart_vcam.set()
        self._frame_ready.set()

    def retarget(self, url: str):
        """Reconnect to url from the next attempt on (the phone moved to another route), and try now."""
        self.url = url
        self._retry_now.set()

    def _open_cap(self):
        # Our own readers, since cv2's FFmpeg backend can't attach the bearer header. The route says which.
        reader_cls = H264Reader if self.url.endswith(".h264") else MjpegReader
        reader = reader_cls(self.url, self.auth)
        reader.open()
        return reader

    def _reconnect_cap(self, stop_event: threading.Event) -> Optional[object]:
        self.status.emit("reconnecting", "Stream dropped - reconnecting")
        while not stop_event.is_set() and not self._stop_flag:
            for _ in range(RECONNECT_DELAY * 10):
                if stop_event.is_set() or self._stop_flag:
                    return None
                if self._retry_now.is_set():
                    break
                time.sleep(0.1)
            self._retry_now.clear()
            cap = self._open_cap()
            if cap.isOpened():
                ret, _ = cap.read()
                if ret:
                    return cap
            cap.release()
        return None

    def _stream_reader(self, cap, stop_event: threading.Event):
        """Pull packets off the wire as fast as they come and hand the newest to the decoder threads, reconnecting
        when the stream drops. Returns once the decoders have finished what they were given."""
        newest, done = _Newest(), threading.Event()
        count = MJPEG_DECODERS if getattr(cap, "parallel_decode", False) else 1
        decoders = [threading.Thread(target=self._decode_loop, args=(newest, done), daemon=True,
                                     name=f"stream-decode-{i}") for i in range(count)]
        for d in decoders:
            d.start()
        try:
            while not stop_event.is_set() and not self._stop_flag:
                ret, packet = cap.read_packet()
                if not ret or packet is None:
                    cap.release()
                    cap = self._reconnect_cap(stop_event)
                    if cap is None:
                        return
                    self.status.emit("ok", "Stream reconnected")
                    self.reconnected.emit()
                    continue
                self._bytes_total += cap.last_frame_bytes
                newest.put((next(self._seq), cap.decode, packet))
            if cap is not None:
                cap.release()
        finally:
            done.set()
            for d in decoders:
                d.join(timeout=3)

    def _decode_loop(self, newest: _Newest, done: threading.Event):
        while (item := newest.take(done)) is not None:
            seq, decode, packet = item
            try:
                raw = decode(packet)
            except Exception:
                logger.exception("Frame decode failed; dropping this frame")
                continue
            if raw is not None:
                self._publish(seq, raw)

    def _publish(self, seq: int, raw):
        """Resize and run the plugin pipeline on raw, then make it the frame the vcam sends. One frame at a time (the
        plugins aren't thread-safe), and never one older than what's already out."""
        with self._publish_lock:
            if seq <= self._shown_seq:
                return
            try:
                self._latest = self._prepare(raw)
            except Exception:
                logger.exception("Frame processing failed; dropping this frame")
                return
            self._shown_seq = seq
            self._frames_received += 1
        self._frame_ready.set()

    def _prepare(self, raw):
        rw, rh = self._width, self._height
        if rw or rh:
            raw = cv2.resize(raw, (rw or raw.shape[1], rh or raw.shape[0]))
        return self._process(raw)

    def run(self):
        self.status.emit("info", f"Connecting to {self.url}...")
        while not self._stop_flag:
            cap = self._open_cap()
            if not cap.isOpened():
                cap.release()
                self.status.emit("warn", f"Can't reach the phone's stream. Trying again in {RECONNECT_DELAY} s…")
                self._restart_vcam.wait(timeout=RECONNECT_DELAY)
                self._restart_vcam.clear()
                continue

            ret, frame = cap.read()
            if not ret or frame is None:
                cap.release()
                self.status.emit("warn", "Waiting for the first frame…")
                self._restart_vcam.wait(timeout=RECONNECT_DELAY)
                self._restart_vcam.clear()
                continue
            self._bytes_total += cap.last_frame_bytes
            # Run first frame through pipeline so vcam dimensions account for transforms (e.g. 90° rotation swaps W↔H).
            self._latest = None
            self._publish(next(self._seq), frame)
            if self._latest is None:
                cap.release()
                self._restart_vcam.wait(timeout=RECONNECT_DELAY)
                self._restart_vcam.clear()
                continue

            # The reader (and its phone connection) outlives vcam restarts: an FPS change only rebuilds the vcam.
            reader_stop = threading.Event()
            reader = threading.Thread(
                target=self._stream_reader,
                args=(cap, reader_stop),
                daemon=True,
            )
            reader.start()

            while not self._stop_flag and reader.is_alive():
                self._restart_vcam.clear()
                self._run_vcam()
                if self._stop_flag:
                    break
                if not self._restart_vcam.is_set():
                    # vcam failed rather than being asked to restart; back off before reopening it.
                    self._restart_vcam.wait(timeout=RECONNECT_DELAY)

            reader_stop.set()
            reader.join(timeout=3)

        self.status.emit("idle", "Not streaming")

    def _open_vcam(self, cam_w: int, cam_h: int):
        return vcam.open_camera(cam_w, cam_h, self._fps, pyvirtualcam.PixelFormat.BGR)

    def _run_vcam(self):
        """Open the virtual camera at the current size/fps and feed it until stop or a restart request."""
        src0 = self._latest
        cam_w = self._canvas_w or src0.shape[1]
        cam_h = self._canvas_h or src0.shape[0]
        # An app already reading the camera (the wait screen's) keeps it at that size; frames are fitted to it.
        cam_w, cam_h = vcam.locked_size() or (cam_w, cam_h)
        self.status.emit("ok", f"Streaming {cam_w}x{cam_h} at {self._fps} fps")
        try:
            with self._open_vcam(cam_w, cam_h) as cam:
                # Name the camera the way other apps list it (the v4l2loopback card label on Linux).
                shown_as = vcam.V4L2_PHONE_LABEL if vcam.IS_LINUX else cam.device
                self.vcam_opened.emit(cam_w, cam_h)
                self.status.emit("ok", f"Streaming {cam_w}x{cam_h} at {self._fps} fps to {shown_as}")
                fc, t0, bytes0, recv0 = 0, time.time(), self._bytes_total, self._frames_received
                period = 1 / self._fps
                last_src = fitted = None
                last_sent = 0.0
                while not self._stop_flag and not self._restart_vcam.is_set():
                    # A new frame goes out as soon as it's ready. The last one is resent only after two periods
                    # without one, so a frame that's a little late isn't held back behind a resend.
                    self._frame_ready.wait(max(0.0, last_sent + 2 * period - time.monotonic()))
                    self._frame_ready.clear()
                    # A burst from the phone is paced to at most twice the camera's rate.
                    if (wait := last_sent + period / 2 - time.monotonic()) > 0:
                        self._restart_vcam.wait(wait)
                    src = self._latest
                    if src is not None:
                        # Adapt frame to fixed vcam dimensions (src shape read fresh for live resolution switches);
                        # a frame re-sent because the phone hasn't delivered a new one reuses its fitted copy.
                        if src is not last_src:
                            last_src, fitted = src, _fit_frame(src, cam_w, cam_h)
                        cam.send(fitted)
                        last_sent = time.monotonic()
                    fc += 1
                    if (elapsed := time.time() - t0) >= 2.0:
                        src_now = self._latest
                        if src_now is not None:
                            src_h, src_w = src_now.shape[:2]
                        else:
                            src_w, src_h = cam_w, cam_h
                        self.status.emit("fps", f"{fc/elapsed:.1f} fps  {src_w}x{src_h}")

                        bytes_now = self._bytes_total
                        mbps = (bytes_now - bytes0) * 8 / elapsed / 1_000_000

                        # Warn only if sustained decode rate trails target significantly (not just high quality).
                        recv_now = self._frames_received
                        decode_fps = (recv_now - recv0) / elapsed
                        struggling = decode_fps < self._fps * 0.85
                        self._weak_streak = self._weak_streak + 1 if struggling else 0
                        net_kind = "net_warn" if self._weak_streak >= 2 else "net"
                        self.status.emit(net_kind, f"{mbps:.1f} Mbps")

                        fc, t0, bytes0, recv0 = 0, time.time(), bytes_now, recv_now
        except Exception as exc:
            self.status.emit("warn", f"Virtual camera error: {exc}")
