import logging
import threading
import time
from typing import Optional

import cv2
import numpy as np
from PyQt6.QtCore import QThread, pyqtSignal

from telescope import vcam
from telescope.h264_reader import H264Reader
from telescope.mjpeg_reader import MjpegReader

logger = logging.getLogger(__name__)

RECONNECT_DELAY = 3

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


class StreamWorker(QThread):
    status      = pyqtSignal(str, str)   # (kind, msg): info/ok/warn/fps/idle
    reconnected = pyqtSignal()           # mid-stream reconnect succeeded (not the initial connect)
    vcam_opened = pyqtSignal(int, int)   # the virtual camera opened at this width, height

    def __init__(self, url: str, width: Optional[int], height: Optional[int],
                 fps: int, frame_pipeline: list = None,
                 canvas_width: Optional[int] = None,
                 canvas_height: Optional[int] = None,
                 token: Optional[str] = None):
        super().__init__()
        self.url       = url
        self.token     = token
        self._width    = width
        self._height   = height
        self._fps      = fps
        self._pipeline = frame_pipeline or []
        self._canvas_w = canvas_width
        self._canvas_h = canvas_height
        self._stop_flag    = False
        self._restart_vcam = threading.Event()
        self._retry_now    = threading.Event()  # retarget(): skip the rest of the reconnect wait
        self._latest_rgb   = None
        # Cumulative wire bytes; vcam loop reads deltas, not resets on mid-stream reconnect.
        self._bytes_total  = 0
        # Actual decodes off wire; separate from vcam send-loop fps (pyvirtualcam resends latest frame regardless).
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

    def request_stop(self):
        self._stop_flag = True
        self._restart_vcam.set()

    def retarget(self, url: str):
        """Reconnect to url from the next attempt on (the phone moved to another route), and try now."""
        self.url = url
        self._retry_now.set()

    def _open_cap(self):
        # Our own readers, since cv2's FFmpeg backend can't attach the bearer header. The route says which.
        reader_cls = H264Reader if self.url.endswith(".h264") else MjpegReader
        reader = reader_cls(self.url, self.token)
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
        """Read frames from device; check width/height each iteration for live resolution changes."""
        while not stop_event.is_set() and not self._stop_flag:
            ret, raw = cap.read()
            if not ret or raw is None:
                cap.release()
                cap = self._reconnect_cap(stop_event)
                if cap is None:
                    return
                self.status.emit("ok", "Stream reconnected")
                self.reconnected.emit()
                continue
            self._bytes_total += cap.last_frame_bytes
            self._frames_received += 1
            try:
                rw = self._width
                rh = self._height
                if rw or rh:
                    rw = rw or raw.shape[1]
                    rh = rh or raw.shape[0]
                    raw = cv2.resize(raw, (rw, rh))
                raw_rgb = cv2.cvtColor(raw, cv2.COLOR_BGR2RGB)
                self._latest_rgb = self._process(raw_rgb)
            except Exception:
                logger.exception("Frame processing failed; dropping this frame")
        if cap is not None:
            cap.release()

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
            self._frames_received += 1

            if self._width or self._height:
                rw = self._width  or frame.shape[1]
                rh = self._height or frame.shape[0]
                frame = cv2.resize(frame, (rw, rh))
            frame_rgb = cv2.cvtColor(frame, cv2.COLOR_BGR2RGB)
            # Run first frame through pipeline so vcam dimensions account for transforms (e.g. 90° rotation swaps W↔H).
            self._latest_rgb = self._process(frame_rgb)

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
        return vcam.open_camera(cam_w, cam_h, self._fps)

    def _run_vcam(self):
        """Open the virtual camera at the current size/fps and feed it until stop or a restart request."""
        src0 = self._latest_rgb
        cam_w = self._canvas_w or src0.shape[1]
        cam_h = self._canvas_h or src0.shape[0]
        self.status.emit("ok", f"Streaming {cam_w}x{cam_h} at {self._fps} fps")
        try:
            with self._open_vcam(cam_w, cam_h) as cam:
                # Name the camera the way other apps list it (the v4l2loopback card label on Linux).
                shown_as = vcam.V4L2_PHONE_LABEL if vcam.IS_LINUX else cam.device
                self.vcam_opened.emit(cam_w, cam_h)
                self.status.emit("ok", f"Streaming {cam_w}x{cam_h} at {self._fps} fps to {shown_as}")
                fc, t0, bytes0, recv0 = 0, time.time(), self._bytes_total, self._frames_received
                last_src = fitted = None
                while not self._stop_flag and not self._restart_vcam.is_set():
                    src = self._latest_rgb
                    if src is not None:
                        # Adapt frame to fixed vcam dimensions (src shape read fresh for live resolution switches);
                        # a frame re-sent because the phone hasn't delivered a new one reuses its fitted copy.
                        if src is not last_src:
                            last_src, fitted = src, _fit_frame(src, cam_w, cam_h)
                        cam.send(fitted)
                    cam.sleep_until_next_frame()
                    fc += 1
                    if (elapsed := time.time() - t0) >= 2.0:
                        src_now = self._latest_rgb
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
