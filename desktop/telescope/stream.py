import itertools
import logging
import os
import threading
import time
from typing import Callable, Optional

import cv2
import numpy as np
import pyvirtualcam
from PyQt6.QtCore import QThread, pyqtSignal

from telescope import h264_reader, vcam, vision
from telescope.h264_reader import H264Reader
from telescope.udp_reader import UdpH264Reader
from telescope.mjpeg_reader import MjpegReader
from telescope.pinned_https import PhoneAuth
from telescope.i18n import tr

logger = logging.getLogger(__name__)

RECONNECT_DELAY = 3
DROP_RETRY_DELAY = 1  # Between tries after a stream drops mid-way, since a phone back on Wi-Fi answers within seconds.
# hand_over(): the new route has to keep frames coming this long, with no gap past HANDOVER_GAP_S, before the stream moves
HANDOVER_HEALTHY_S = 2.0
HANDOVER_GAP_S = 0.5
HANDOVER_TIMEOUT_S = 10.0  # ...or it gives up on it
HANDOVER_TAKE_S = 5.0  # how long a ready one waits for the reader to take it (a read on the old route can block)
# JPEG decoders running side by side: at 4K one decode takes longer than a frame lasts.
MJPEG_DECODERS = max(1, min(2, (os.cpu_count() or 1) - 1))

# A plugin step that fails this many frames in a row is skipped until the next stream.
STEP_FAILS_BEFORE_SKIP = 30

# After a Start, a lens switch or a new size or fps, the phone takes a while to make frames at full rate again (an older
# phone can take seconds to switch lenses). The link is judged only once frames have flowed this long without a gap...
SETTLE_AFTER_FRAMES_S = 2.5
SETTLE_GAP_S = 1.0  # ...a gap this long (the old lens's last frames, then nothing) starts that over...
SETTLE_MAX_S = 10.0  # ...or once this much has passed, frames or not, so a phone that never recovers still says so.


def guarded_step(name: str, process):
    """A plugin's frame step that can't take the stream down: a frame it fails on or mangles goes through unchanged."""
    fails = 0

    def step(frame):
        nonlocal fails
        if fails >= STEP_FAILS_BEFORE_SKIP:
            return frame
        try:
            out = process(frame)
            if not (isinstance(out, np.ndarray) and out.ndim == 3 and out.shape[2] == 3 and out.dtype == np.uint8
                    and out.size):
                raise ValueError(f"returned {getattr(out, 'shape', type(out).__name__)} instead of a BGR frame")
        except Exception:
            fails += 1
            if fails == 1:
                logger.exception("Plugin %s failed on a frame; passing it through unchanged", name)
            elif fails == STEP_FAILS_BEFORE_SKIP:
                logger.error("Plugin %s failed on %d frames in a row; skipping it until the stream restarts", name, fails)
            return frame
        fails = 0
        return out
    return step


def _vcam_format(width: int, height: int):
    """(pixel format, BGR converter or None) for the virtual camera; OpenCV makes v4l2loopback's I420 ~8x cheaper than pyvirtualcam."""
    if vcam.IS_LINUX and width % 2 == 0 and height % 2 == 0:
        return pyvirtualcam.PixelFormat.I420, lambda frame: cv2.cvtColor(frame, cv2.COLOR_BGR2YUV_I420)
    return pyvirtualcam.PixelFormat.BGR, None


# Sentinel: "leave unchanged" (distinct from None = pass-through).
_UNCHANGED = object()


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
    moved       = pyqtSignal(str)        # hand_over() moved the stream to this URL without a gap
    move_failed = pyqtSignal(str)        # hand_over() gave up on this URL; the stream carries on where it was
    vcam_opened = pyqtSignal(int, int)   # the virtual camera opened at this width, height

    def __init__(self, url: str, width: Optional[int], height: Optional[int],
                 fps: int, frame_pipeline: list = None,
                 canvas_width: Optional[int] = None,
                 canvas_height: Optional[int] = None,
                 auth: Optional[PhoneAuth] = None,
                 open_reader: Optional[Callable] = None,
                 slot: int = 0):
        super().__init__()
        self.url       = url
        self.auth      = auth
        self._open_reader = open_reader  # a StreamSource's reader instead of the phone's
        self.slot      = slot  # which of the virtual cameras it feeds (vcam.MAX_SLOTS)
        self._width    = width
        self._height   = height
        self._fps      = fps
        self._pipeline = frame_pipeline or []
        self._canvas_w = canvas_width
        self._canvas_h = canvas_height
        self._stop_flag    = False
        self._restart_vcam = threading.Event()
        self._retry_now    = threading.Event()  # retarget(): skip the rest of the reconnect wait
        self._handover_lock = threading.Lock()
        self._handover_busy = False  # a hand_over() is trying its URL
        self._handover_ready = None  # (reader, url, taken event): its reader, healthy and waiting to take over
        self.parked        = False  # the virtual camera is let go (the wait screen takes it) until set_parked(False)
        self._vcam_free    = threading.Event()  # set while this worker doesn't hold the virtual camera
        self._vcam_free.set()
        # The newest processed frame (BGR, as the decoders give it and the plugins work on it).
        self._latest       = None
        self._to_cam: Optional[Callable] = None  # the camera's own format from BGR, set by _open_vcam (None: BGR)
        self._frame_ready  = threading.Event()  # set when _latest changes, so the vcam sends it at once
        self._seq          = itertools.count(1)
        self._shown_seq    = 0  # the newest packet that made it to _latest; an older one finishing later is dropped
        self._publish_lock = threading.Lock()
        # Cumulative wire bytes; vcam loop reads deltas, not resets on mid-stream reconnect.
        self._bytes_total  = 0
        # Frames decoded and shown; separate from the vcam send rate (which resends the latest frame regardless).
        self._frames_received = 0
        # Frames that arrived from the phone, the ones skipped to stay live included: whether the link keeps up.
        self._frames_arrived = 0
        # Consecutive 2s windows with sustained low decode rate (distinguishes congestion from blips).
        self._weak_streak  = 0
        # Frames per second that arrived over the last window, read by the app when that's under the target.
        self.last_arrival_fps = 0.0
        # Settling (see SETTLE_AFTER_FRAMES_S): since when, and since when frames have flowed; since is None once over.
        self._settle_lock  = threading.Lock()
        self._settle_since: Optional[float] = time.monotonic()
        self._settle_flowing: Optional[float] = None
        self._settle_last_frame = 0.0
        self._photo_until = 0.0  # until then frames stopping is the phone taking a photo, not a dropped stream

    def _process(self, frame):
        for fn in self._pipeline:
            frame = fn(frame)
        return frame

    def latest_frame(self):
        """The newest frame sent out (BGR), or None before the first; for a thumbnail."""
        return self._latest

    def set_pipeline(self, frame_pipeline: list):
        """Swap the plugin steps from the next frame on (the panels moved to another stream, or back)."""
        with self._publish_lock:
            self._pipeline = list(frame_pipeline)

    def update_output(self, width=_UNCHANGED, height=_UNCHANGED, fps=_UNCHANGED):
        """Update stream parameters live (None = pass-through; omit to leave unchanged; fps changes restart vcam)."""
        if width  is not _UNCHANGED: self._width  = width
        if height is not _UNCHANGED: self._height = height
        if fps is not _UNCHANGED and fps != self._fps:  # the same fps again (a box losing focus) would still flicker the camera
            self._fps = fps
            self.settle()
            self._restart_vcam.set()
            self._frame_ready.set()

    def settle(self):
        """The phone is restarting its camera (a lens switch, a new size): hold off judging the link until it's back."""
        with self._settle_lock:
            self._settle_since = time.monotonic()
            self._settle_flowing = None

    def hold_for_photo(self, seconds: float = 3.0):
        """The phone is taking a photo, which can pause its video: for seconds from now a stream that stops is
        reconnected quietly instead of being reported as dropped. Called again to extend it."""
        self._photo_until = time.monotonic() + seconds
        self.settle()

    def _count_arrived(self, frames: int):
        self._frames_arrived += frames
        if self._settle_since is None:
            return
        now = time.monotonic()
        with self._settle_lock:
            flowing, last = self._settle_flowing, self._settle_last_frame
            # Once frames have kept coming long enough the phone is back, and a gap after that is the link's.
            if flowing is None or (last - flowing < SETTLE_AFTER_FRAMES_S and now - last > SETTLE_GAP_S):
                self._settle_flowing = now
            self._settle_last_frame = now

    def _judges_window(self, start: float) -> bool:
        """Whether the window from start on says anything about the link: it has to start after settling ended."""
        with self._settle_lock:
            if self._settle_since is None:
                return True
            end = self._settle_since + SETTLE_MAX_S
            if self._settle_flowing is not None:
                end = min(end, self._settle_flowing + SETTLE_AFTER_FRAMES_S)
            if start < end:
                return False
            self._settle_since = None
            return True

    def request_stop(self):
        self._stop_flag = True
        self._restart_vcam.set()
        self._frame_ready.set()

    def set_parked(self, parked: bool):
        """Let go of the virtual camera while the stream is gone (so the wait screen can show), or take it back."""
        self.parked = parked
        self._restart_vcam.set()
        self._frame_ready.set()
        if parked:
            self._vcam_free.wait(1.0)  # the wait screen opens it next; it retries if this ran late

    def retarget(self, url: str):
        """Reconnect to url from the next attempt on (the phone moved to another route), and try now."""
        self.url = url
        self._retry_now.set()

    def hand_over(self, url: str) -> bool:
        """Open url next to the running stream and move over once it has kept frames coming for HANDOVER_HEALTHY_S;
        only then does the old connection close. moved or move_failed says how it went. False if it can't try."""
        if self._open_reader is not None or self._stop_flag:
            return False
        with self._handover_lock:
            if self._handover_busy:
                return False
            self._handover_busy = True
        threading.Thread(target=self._try_handover, args=(url,), daemon=True, name="stream-handover").start()
        return True

    def _try_handover(self, url: str):
        reader = self._new_reader(url)
        try:
            reader.open()
            if reader.isOpened() and self._healthy(reader):
                taken = threading.Event()
                with self._handover_lock:
                    self._handover_ready = (reader, url, taken)
                if taken.wait(HANDOVER_TAKE_S):
                    return
                with self._handover_lock:
                    if self._handover_ready is not None and self._handover_ready[2] is taken:
                        self._handover_ready = None
                    elif taken.is_set():
                        return  # taken just now
        except Exception:
            logger.exception("Trying the new route failed")
        finally:
            with self._handover_lock:
                self._handover_busy = False
        reader.release()
        if not self._stop_flag:
            self.move_failed.emit(url)

    def _healthy(self, reader) -> bool:
        """Read (and, for H.264, decode, so it's ready to carry on) until frames have come without a gap for a while."""
        start = since = last = time.monotonic()
        while not self._stop_flag and last - start < HANDOVER_TIMEOUT_S:
            ret, _packet = reader.read_packet()
            now = time.monotonic()
            if not ret:
                return False
            if now - last > HANDOVER_GAP_S:
                since = now  # a gap: the time without one starts over
            last = now
            if now - since >= HANDOVER_HEALTHY_S:
                return True
        return False

    def _take_handover(self):
        """The new route's reader, if one is ready to take over; the stream moves to it."""
        with self._handover_lock:
            ready, self._handover_ready = self._handover_ready, None
        if ready is None:
            return None
        reader, url, taken = ready
        taken.set()
        self.url = url
        self.settle()
        logger.info("Stream moved to %s", url)
        self.moved.emit(url)
        return reader

    def _new_reader(self, url: str):
        # Our own readers, since cv2's FFmpeg backend can't attach the bearer header. The route says which.
        reader_cls = H264Reader if url.endswith(".h264") else UdpH264Reader if url.endswith(".udp") else MjpegReader
        return reader_cls(url, self.auth)

    def _open_cap(self):
        if self._open_reader is not None:
            reader = self._open_reader()
            reader.open()
            return reader
        reader = self._new_reader(self.url)
        reader.open()
        return reader

    def _reconnect_cap(self, stop_event: threading.Event) -> Optional[object]:
        if time.monotonic() < self._photo_until:
            self.status.emit("info", tr("The video is paused while the phone takes a photo"))
        else:
            self.status.emit("reconnecting", tr("Stream dropped - reconnecting"))
        while not stop_event.is_set() and not self._stop_flag:
            for _ in range(DROP_RETRY_DELAY * 10):
                if stop_event.is_set() or self._stop_flag:
                    return None
                if self._retry_now.is_set():
                    break
                time.sleep(0.1)
            self._retry_now.clear()
            moved = self._take_handover()
            if moved is not None:
                return moved
            cap = self._open_cap()
            if cap.isOpened():
                ret, _ = cap.read()
                if ret:
                    return cap
            cap.release()
        return None

    def _stream_reader(self, cap, stop_event: threading.Event, finished: threading.Event):
        """Pull packets off the wire as fast as they come and hand the newest to the decoder threads, reconnecting
        when the stream drops. Returns once the decoders have finished what they were given."""
        try:
            self._read_packets(cap, stop_event)
        except Exception:
            logger.exception("Stream reader crashed; reconnecting")
        finally:
            # Wake the vcam loop so a reader that died doesn't leave the last frame frozen on the camera
            finished.set()
            self._restart_vcam.set()
            self._frame_ready.set()

    def _read_packets(self, cap, stop_event: threading.Event):
        newest, done = _Newest(), threading.Event()
        count = MJPEG_DECODERS if getattr(cap, "parallel_decode", False) else 1
        decoders = [threading.Thread(target=self._decode_loop, args=(newest, done), daemon=True,
                                     name=f"stream-decode-{i}") for i in range(count)]
        for d in decoders:
            d.start()
        try:
            while not stop_event.is_set() and not self._stop_flag:
                moved = self._take_handover()
                if moved is not None:
                    cap.release()
                    cap = moved
                    continue
                ret, packet = cap.read_packet()
                if not ret or packet is None:
                    cap.release()
                    cap = self._reconnect_cap(stop_event)
                    if cap is None:
                        return
                    self.settle()
                    self.status.emit("ok", tr("Stream reconnected"))
                    self.reconnected.emit()
                    continue
                self._bytes_total += cap.last_frame_bytes
                self._count_arrived(getattr(cap, "last_frame_count", 1))
                newest.put((next(self._seq), cap.decode, packet))
        finally:
            if cap is not None:
                cap.release()
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
            here = vision.current_frame.set((self, seq))  # the plugins' person_mask() knows which stream this is
            try:
                self._latest = self._prepare(raw)
            except Exception:
                logger.exception("Frame processing failed; dropping this frame")
                return
            finally:
                vision.current_frame.reset(here)
            self._shown_seq = seq
            self._frames_received += 1
        self._frame_ready.set()

    def _prepare(self, raw):
        rw, rh = self._width, self._height
        if rw or rh:
            raw = cv2.resize(raw, (rw or raw.shape[1], rh or raw.shape[0]))
        return self._process(raw)

    def run(self):
        try:
            self._run()
        except Exception as exc:
            # A bug here must end the stream visibly, not leave a thread the app thinks is still streaming
            logger.exception("Stream worker crashed")
            self.status.emit("error", tr("Stream error: {error}", error=exc))
        finally:
            if vcam.IS_LINUX:
                from telescope.platform.linux import release_freed_memory
                release_freed_memory()
            self.status.emit("idle", tr("Not streaming"))

    def _run(self):
        if self._open_reader is None and self.url.endswith((".h264", ".udp")) and not h264_reader.loaded():
            # The start-up preload is still importing PyAV (a first start on Windows checks each of its files)
            self.status.emit("info", tr("Getting ready… The first start after installing can take a minute."))
            h264_reader.load_av()
        self.status.emit("info", tr("Connecting...") if self._open_reader else tr("Connecting to {url}...", url=self.url))
        failed = False  # the phone may be reachable another way by now, so the host looks for it (see "waiting")
        while not self._stop_flag:
            cap = self._open_cap()
            if not cap.isOpened():
                cap.release()
                failed = True
                waiting = getattr(cap, "waiting_text", None)
                self.status.emit("waiting", tr(waiting) if waiting
                                 else tr("Can't reach the phone's stream. Trying again in {seconds} s…",
                                       seconds=RECONNECT_DELAY))
                self._restart_vcam.wait(timeout=RECONNECT_DELAY)
                self._restart_vcam.clear()
                continue

            ret, frame = cap.read()
            if not ret or frame is None:
                cap.release()
                failed = True
                self.status.emit("waiting", tr("Waiting for the first frame…"))
                self._restart_vcam.wait(timeout=RECONNECT_DELAY)
                self._restart_vcam.clear()
                continue
            self._bytes_total += cap.last_frame_bytes
            self._count_arrived(getattr(cap, "last_frame_count", 1))
            # Run first frame through pipeline so vcam dimensions account for transforms (e.g. 90° rotation swaps W↔H).
            self._latest = None
            self._publish(next(self._seq), frame)
            if self._latest is None:
                cap.release()
                self._restart_vcam.wait(timeout=RECONNECT_DELAY)
                self._restart_vcam.clear()
                continue
            if failed:
                failed = False
                self.reconnected.emit()

            # The reader (and its phone connection) outlives vcam restarts: an FPS change only rebuilds the vcam.
            reader_stop, reader_done = threading.Event(), threading.Event()
            reader = threading.Thread(
                target=self._stream_reader,
                args=(cap, reader_stop, reader_done),
                daemon=True,
            )
            reader.start()

            while not self._stop_flag and not reader_done.is_set():
                self._restart_vcam.clear()
                if self.parked:
                    self._restart_vcam.wait(timeout=RECONNECT_DELAY)
                    continue
                self._run_vcam()
                if self._stop_flag:
                    break
                if not self._restart_vcam.is_set():
                    # vcam failed rather than being asked to restart; back off before reopening it.
                    self._restart_vcam.wait(timeout=RECONNECT_DELAY)

            reader_stop.set()
            reader.join(timeout=3)

    def _open_vcam(self, cam_w: int, cam_h: int):
        fmt, self._to_cam = _vcam_format(cam_w, cam_h)
        return vcam.open_camera(cam_w, cam_h, self._fps, fmt, slot=self.slot)

    def _run_vcam(self):
        """Open the virtual camera at the current size/fps and feed it until stop or a restart request."""
        src0 = self._latest
        cam_w = self._canvas_w or src0.shape[1]
        cam_h = self._canvas_h or src0.shape[0]
        # An app already reading the camera (the wait screen's) keeps it at that size; frames are fitted to it.
        cam_w, cam_h = vcam.locked_size(self.slot) or (cam_w, cam_h)
        self._vcam_free.clear()
        self._to_cam = None
        try:
            with self._open_vcam(cam_w, cam_h) as cam:
                # Name the camera the way other apps list it (the v4l2loopback card label on Linux).
                shown_as = vcam.slot_label(self.slot) if vcam.IS_LINUX else cam.device
                self.vcam_opened.emit(cam_w, cam_h)
                self.status.emit("ok", tr("Streaming {size} at {fps} fps to {camera}", size=f"{cam_w} × {cam_h}",
                                         fps=self._fps, camera=shown_as))
                t0, bytes0, recv0 = time.monotonic(), self._bytes_total, self._frames_arrived
                period = 1 / self._fps
                last_src = fitted = None
                last_sent = 0.0
                while not self._stop_flag and not self._restart_vcam.is_set() and not self.parked:
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
                            last_src, fitted = src, vcam.fit_frame(src, cam_w, cam_h)
                            if self._to_cam is not None:
                                fitted = self._to_cam(fitted)
                        cam.send(fitted)
                        last_sent = time.monotonic()
                    if (elapsed := time.monotonic() - t0) >= 2.0:
                        if vcam.camera_changed(cam, self.slot):
                            self._restart_vcam.set()  # the camera was installed or updated while streaming
                        src_now = self._latest
                        if src_now is not None:
                            src_h, src_w = src_now.shape[:2]
                        else:
                            src_w, src_h = cam_w, cam_h
                        # Frames that came from the phone, not what the virtual camera was sent: resends of the last
                        # frame padded that out, so a camera stuck at 30 read as 42 at 60 fps.
                        recv_now = self._frames_arrived
                        arrival_fps = (recv_now - recv0) / elapsed
                        self.last_arrival_fps = arrival_fps
                        self.status.emit("fps", f"{arrival_fps:.1f} fps  {src_w}x{src_h}")

                        bytes_now = self._bytes_total
                        mbps = (bytes_now - bytes0) * 8 / elapsed / 1_000_000

                        # Warn only if frames sustainedly arrive well under the target rate. Arrive, not shown: frames
                        # that come in a burst are all counted, though only the newest is shown to stay live.
                        # A window that started while the phone was still settling says nothing about the link.
                        # Nothing at all is a dropped link the reader is about to reconnect, not a slow one.
                        struggling = self._judges_window(t0) and 0 < arrival_fps < self._fps * 0.85
                        self._weak_streak = self._weak_streak + 1 if struggling else 0
                        net_kind = "net_warn" if self._weak_streak >= 2 else "net"
                        self.status.emit(net_kind, f"{mbps:.1f} Mbps")

                        t0, bytes0, recv0 = time.monotonic(), bytes_now, recv_now
        except Exception as exc:
            self.status.emit("warn", tr("Virtual camera error: {error}", error=exc))
        finally:
            self._vcam_free.set()
