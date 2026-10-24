"""The Browser camera's WebRTC side, Qt-free: answers a page's offer and hands its decoded video and mic on.

aiortc does the WebRTC, on one asyncio loop in a thread of its own that every browser shares; everything else in
Telescope talks to it through RtcSession, from ordinary threads. The page's video arrives as av.VideoFrame (H.264 or
VP8, decoded by aiortc through PyAV) and its mic as 48 kHz mono s16le, the same PCM the phone's /v1/audio sends.

Nothing here reaches outside the local network: no STUN or TURN server is set (aiortc would use a public one), so
only the two devices' own addresses are tried.
"""

import asyncio
import logging
import re
import threading
import time
from typing import Callable, Optional

from telescope import h264_reader

logger = logging.getLogger(__name__)

JITTER_PACKETS = 1024  # aiortc's 128 (~150 KB) never completes a 1080p keyframe, and it asks for one again forever
ANSWER_TIMEOUT_S = 10
STATS_EVERY_S = 1.0  # how often the bytes behind each frame are worked out, for the throughput readout
VIDEO_CODECS = ("H264", "VP8")  # what aiortc decodes, in the order the page is asked to prefer

_loop: Optional[asyncio.AbstractEventLoop] = None
_loop_lock = threading.Lock()
_warned_jitter = False


class RtcError(Exception):
    """The offer couldn't be answered; the message is for the log and the page."""


def available() -> bool:
    h264_reader.load_av()  # aiortc imports PyAV, which mustn't race the start-up preload
    try:
        import aiortc  # noqa: F401
    except ImportError:
        return False
    return True


def _get_loop() -> asyncio.AbstractEventLoop:
    global _loop
    with _loop_lock:
        if _loop is None:
            loop = asyncio.new_event_loop()
            threading.Thread(target=loop.run_forever, name="browser-rtc", daemon=True).start()
            _loop = loop
        return _loop


def _run(coro, timeout: float):
    return asyncio.run_coroutine_threadsafe(coro, _get_loop()).result(timeout)


# ── SDP ──────────────────────────────────────────────────────────────────────

def answer_codec(sdp: str) -> str:
    """The video codec an answer settled on ("H264", "VP8"), or "" when it has none."""
    in_video = False
    for line in sdp.splitlines():
        if line.startswith("m="):
            in_video = line.startswith("m=video") and not line.startswith("m=video 0 ")
        elif in_video and (m := re.match(r"a=rtpmap:\d+ ([A-Za-z0-9]+)/", line)) and m.group(1).upper() in VIDEO_CODECS:
            return m.group(1).upper()
    return ""


def tune_answer(sdp: str, max_kbps: int) -> str:
    """Tells Chrome to start near max_kbps instead of 300 kbps (it would take half a minute to get there) and not to go
    over it. Browsers that don't know these parameters ignore them."""
    start = max(300, max_kbps * 2 // 3)
    extra = f"x-google-start-bitrate={start};x-google-max-bitrate={max_kbps}"
    lines = sdp.split("\r\n")
    pts = {m.group(1) for line in lines
           if (m := re.match(r"a=rtpmap:(\d+) ([A-Za-z0-9]+)/", line)) and m.group(2).upper() in VIDEO_CODECS}
    out, has_fmtp = [], {pt: any(line.startswith(f"a=fmtp:{pt} ") for line in lines) for pt in pts}
    for line in lines:
        m = re.match(r"a=(rtpmap|fmtp):(\d+) ", line)
        if m and m.group(2) in pts and m.group(1) == "fmtp":
            line = f"{line};{extra}"
        out.append(line)
        if m and m.group(2) in pts and m.group(1) == "rtpmap" and not has_fmtp[m.group(2)]:
            out.append(f"a=fmtp:{m.group(2)} {extra}")
    return "\r\n".join(out)


def max_kbps_for(width: int, height: int, fps: int) -> int:
    """The bitrate a browser's camera may use on the local network at this size and rate."""
    return max(1500, min(12_000, int(width * height * fps * 0.15 / 1000)))


# ── A browser's connection ───────────────────────────────────────────────────

class RtcSession:
    """One page's peer connection. on_video(frame, bytes_per_frame) gets each decoded av.VideoFrame, on_audio(pcm) each
    20 ms of 48 kHz mono s16le; both are called on the WebRTC thread, so they must only hand the data on."""

    def __init__(self, on_video: Callable, on_audio: Callable):
        self._on_video, self._on_audio = on_video, on_audio
        self._pc = None
        self._tasks: list = []
        self._closed = False
        self.codec = ""

    def answer(self, offer_sdp: str, max_kbps: int) -> str:
        """The answer to a complete (non-trickle) offer; blocks up to ANSWER_TIMEOUT_S. Raises RtcError."""
        try:
            return _run(self._answer(offer_sdp, max_kbps), ANSWER_TIMEOUT_S)
        except RtcError:
            raise
        except Exception as exc:
            self.close()
            raise RtcError(f"{type(exc).__name__}: {exc}") from exc

    def close(self):
        """Ends the connection; returns at once."""
        if self._closed:
            return
        self._closed = True
        if _loop is not None:
            asyncio.run_coroutine_threadsafe(self._close(), _loop)

    async def _answer(self, offer_sdp: str, max_kbps: int) -> str:
        from aiortc import RTCConfiguration, RTCPeerConnection, RTCSessionDescription

        if self._closed:
            raise RtcError("closed")
        pc = self._pc = RTCPeerConnection(RTCConfiguration(iceServers=[]))
        pc.on("track", self._on_track)
        await pc.setRemoteDescription(RTCSessionDescription(offer_sdp, "offer"))
        _widen_jitter_buffers(pc)
        await pc.setLocalDescription(await pc.createAnswer())
        if self._closed:
            raise RtcError("closed")  # _close ends pc
        sdp = pc.localDescription.sdp
        self.codec = answer_codec(sdp)
        if not self.codec:
            raise RtcError("the browser offered no video codec this computer decodes")
        logger.info("Browser camera: WebRTC answered with %s", self.codec)
        return tune_answer(sdp, max_kbps)

    def _on_track(self, track):
        target = self._video if track.kind == "video" else self._audio
        self._tasks.append(asyncio.ensure_future(target(track)))

    async def _video(self, track):
        from aiortc.mediastreams import MediaStreamError

        frames, last_bytes, since, per_frame = 0, 0, time.monotonic(), 0
        while True:
            try:
                frame = await track.recv()
            except MediaStreamError:
                return
            frames += 1
            if time.monotonic() - since >= STATS_EVERY_S:
                received = await self._bytes_received()
                per_frame = (received - last_bytes) // frames if received > last_bytes else per_frame
                frames, last_bytes, since = 0, received, time.monotonic()
            self._on_video(frame, per_frame)

    async def _audio(self, track):
        import av
        from aiortc.mediastreams import MediaStreamError

        resampler = av.AudioResampler(format="s16", layout="mono", rate=48000)
        while True:
            try:
                frame = await track.recv()
            except MediaStreamError:
                return
            for out in resampler.resample(frame):
                self._on_audio(out.to_ndarray().tobytes())

    async def _bytes_received(self) -> int:
        try:
            report = await self._pc.getStats()
        except Exception:
            return 0
        return sum(getattr(s, "bytesReceived", 0) or 0 for s in report.values() if s.type == "transport")

    async def _close(self):
        for task in self._tasks:
            task.cancel()
        if self._pc is not None:
            await self._pc.close()


def _widen_jitter_buffers(pc):
    """Gives each video receiver a jitter buffer big enough for a sharp keyframe (see JITTER_PACKETS)."""
    global _warned_jitter
    from aiortc.jitterbuffer import JitterBuffer

    for transceiver in pc.getTransceivers():
        receiver = transceiver.receiver
        if transceiver.kind != "video" or receiver is None:
            continue
        if not hasattr(receiver, "_RTCRtpReceiver__jitter_buffer"):
            if not _warned_jitter:
                _warned_jitter = True
                logger.warning("Browser camera: this aiortc has no jitter buffer to widen; big frames may not arrive")
            continue
        receiver._RTCRtpReceiver__jitter_buffer = JitterBuffer(capacity=JITTER_PACKETS, is_video=True)
