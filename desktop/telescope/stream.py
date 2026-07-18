import logging
import platform
import threading
import time
from typing import Optional

import cv2
import numpy as np
import pyvirtualcam
from PyQt6.QtCore import QThread, pyqtSignal

from telescope.mjpeg_reader import MjpegReader

logger = logging.getLogger(__name__)

IS_LINUX = platform.system() == "Linux"

VCAM_BACKEND   = "v4l2loopback" if IS_LINUX else "unitycapture"
V4L2_PHONE_DEV = "/dev/video11"
RECONNECT_DELAY = 3

# Sentinel for "leave this parameter unchanged" (distinct from None which
# means "pass-through / no resize").
_UNCHANGED = object()


def _fit_frame(frame, target_w, target_h):
    """Resize *frame* to exactly target_w × target_h, preserving aspect ratio.

    Black bars (letterbox / pillarbox) are added when the aspect ratios
    differ.  When the frame already matches the target, it is returned
    as-is (zero-copy).
    """
    fh, fw = frame.shape[:2]
    if fw == target_w and fh == target_h:
        return frame

    scale = min(target_w / fw, target_h / fh)
    new_w = int(fw * scale)
    new_h = int(fh * scale)

    # Same aspect ratio — plain resize, no bars needed.
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
        self._latest_rgb   = None

    def _process(self, frame):
        for fn in self._pipeline:
            frame = fn(frame)
        return frame

    def update_output(self, width=_UNCHANGED, height=_UNCHANGED, fps=_UNCHANGED):
        """Update stream processing parameters live.

        Pass *None* for width/height to enable pass-through (no resize).
        Omit a parameter (or don't pass it) to leave it as-is.

        Resolution and rotation changes take effect immediately — the reader
        thread picks up the new values and ``_fit_frame`` adapts the output.
        Only *fps* changes trigger a virtual-camera restart (the
        ``pyvirtualcam.Camera`` object is bound to a fixed fps).
        """
        if width  is not _UNCHANGED: self._width  = width
        if height is not _UNCHANGED: self._height = height
        if fps is not _UNCHANGED:
            self._fps = fps
            self._restart_vcam.set()

    def request_stop(self):
        self._stop_flag = True
        self._restart_vcam.set()

    def _open_cap(self):
        # cv2.VideoCapture's FFmpeg backend has no way to attach the bearer
        # header the phone's /v1/video now requires, so the stream is read
        # and multipart-parsed directly instead of handing the URL to OpenCV.
        reader = MjpegReader(self.url, self.token)
        reader.open()
        return reader

    def _reconnect_cap(self, stop_event: threading.Event) -> Optional[object]:
        self.status.emit("warn", "Stream dropped - reconnecting...")
        while not stop_event.is_set() and not self._stop_flag:
            for _ in range(RECONNECT_DELAY * 10):
                if stop_event.is_set() or self._stop_flag:
                    return None
                time.sleep(0.1)
            cap = self._open_cap()
            if cap.isOpened():
                ret, _ = cap.read()
                if ret:
                    return cap
            cap.release()
        return None

    def _stream_reader(self, cap, stop_event: threading.Event):
        """Read frames from the capture device, resize, process, and store.

        Reads ``self._width`` / ``self._height`` on every iteration so
        mid-stream resolution changes take effect immediately without
        restarting the reader or the stream connection.
        """
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
                self.status.emit("warn", f"Cannot open stream - retry in {RECONNECT_DELAY}s...")
                self._restart_vcam.wait(timeout=RECONNECT_DELAY)
                self._restart_vcam.clear()
                continue

            ret, frame = cap.read()
            if not ret or frame is None:
                cap.release()
                self.status.emit("warn", "Empty first frame - retrying...")
                self._restart_vcam.wait(timeout=RECONNECT_DELAY)
                self._restart_vcam.clear()
                continue

            if self._width or self._height:
                rw = self._width  or frame.shape[1]
                rh = self._height or frame.shape[0]
                frame = cv2.resize(frame, (rw, rh))
            frame_rgb = cv2.cvtColor(frame, cv2.COLOR_BGR2RGB)
            # Run the first frame through the pipeline so the vcam dimensions
            # account for any active transforms (e.g. 90° rotation swaps W↔H).
            frame_rgb = self._process(frame_rgb)
            self._latest_rgb = frame_rgb
            cam_w = self._canvas_w or frame_rgb.shape[1]
            cam_h = self._canvas_h or frame_rgb.shape[0]
            self._restart_vcam.clear()
            self.status.emit("ok", f"Stream {cam_w}x{cam_h} @ {self._fps} fps -> {VCAM_BACKEND}")

            reader_stop = threading.Event()
            reader = threading.Thread(
                target=self._stream_reader,
                args=(cap, reader_stop),
                daemon=True,
            )
            reader.start()

            try:
                with pyvirtualcam.Camera(width=cam_w, height=cam_h, fps=self._fps,
                                         backend=VCAM_BACKEND,
                                         device=V4L2_PHONE_DEV if IS_LINUX else None) as cam:
                    self.status.emit("ok", f"Virtual camera: {cam.device}")
                    fc, t0 = 0, time.time()
                    while not self._stop_flag and not self._restart_vcam.is_set():
                        rgb = self._latest_rgb
                        if rgb is not None:
                            # Adapt the (potentially resized / rotated /
                            # differently-shaped) frame to the fixed vcam
                            # dimensions, preserving aspect ratio.
                            rgb = _fit_frame(rgb, cam_w, cam_h)
                            cam.send(rgb)
                        cam.sleep_until_next_frame()
                        fc += 1
                        if (elapsed := time.time() - t0) >= 2.0:
                            self.status.emit("fps", f"{fc/elapsed:.1f} fps  {cam_w}x{cam_h}")
                            fc, t0 = 0, time.time()
            except Exception as exc:
                self.status.emit("warn", f"Virtual cam error: {exc}")

            reader_stop.set()
            reader.join(timeout=3)

            if self._stop_flag:
                break
            if not self._restart_vcam.is_set():
                self._restart_vcam.wait(timeout=RECONNECT_DELAY)
            self._restart_vcam.clear()

        self.status.emit("idle", "Stopped.")
