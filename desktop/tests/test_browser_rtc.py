"""The Browser camera's WebRTC side: the SDP helpers, and a real connection with aiortc standing in for the browser."""

import asyncio
import fractions
import threading
import time

import numpy as np
import pytest

from telescope import browser_rtc
from telescope import browser_server as bs

ANSWER = "\r\n".join([
    "v=0", "o=- 1 1 IN IP4 0.0.0.0", "s=-", "t=0 0",
    "m=audio 9 UDP/TLS/RTP/SAVPF 111", "a=rtpmap:111 opus/48000/2", "a=fmtp:111 minptime=10;useinbandfec=1",
    "m=video 9 UDP/TLS/RTP/SAVPF 102 103 96",
    "a=rtpmap:102 H264/90000", "a=fmtp:102 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f",
    "a=rtpmap:103 rtx/90000", "a=fmtp:103 apt=102",
    "a=rtpmap:96 VP8/90000", "",
])


def test_answer_codec_is_the_first_video_codec():
    assert browser_rtc.answer_codec(ANSWER) == "H264"
    assert browser_rtc.answer_codec(ANSWER.replace("H264/", "AV1/")) == "VP8"
    assert browser_rtc.answer_codec(ANSWER.replace("m=video 9 ", "m=video 0 ")) == ""  # rejected
    assert browser_rtc.answer_codec("m=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=rtpmap:111 opus/48000/2") == ""


def test_tune_answer_sets_the_bitrates_on_video_codecs_only():
    out = browser_rtc.tune_answer(ANSWER, 6000).split("\r\n")
    assert ("a=fmtp:102 level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f;"
            "x-google-start-bitrate=4000;x-google-max-bitrate=6000") in out
    assert out[out.index("a=rtpmap:96 VP8/90000") + 1] == "a=fmtp:96 x-google-start-bitrate=4000;x-google-max-bitrate=6000"
    assert "a=fmtp:111 minptime=10;useinbandfec=1" in out and "a=fmtp:103 apt=102" in out
    assert out[-1] == ""  # still ends in a line break


def test_max_kbps_follows_size_and_rate_within_bounds():
    assert browser_rtc.max_kbps_for(1920, 1080, 30) == 9331
    assert browser_rtc.max_kbps_for(854, 480, 15) == 1500
    assert browser_rtc.max_kbps_for(3840, 2160, 60) == 12_000


# ── A real connection ────────────────────────────────────────────────────────

def _fake_browser_offer():
    """An aiortc peer sending a moving grey picture and a tone, the way the page sends its camera and mic."""
    aiortc = pytest.importorskip("aiortc")
    av = pytest.importorskip("av")
    from aiortc.mediastreams import AudioStreamTrack, VideoStreamTrack

    class Picture(VideoStreamTrack):
        def __init__(self):
            super().__init__()
            self.n = 0

        async def recv(self):
            pts, time_base = await self.next_timestamp()
            self.n += 1
            frame = av.VideoFrame.from_ndarray(np.full((240, 320, 3), 100 + self.n % 50, np.uint8), format="bgr24")
            frame.pts, frame.time_base = pts, time_base
            return frame

    class Tone(AudioStreamTrack):
        async def recv(self):
            frame = await super().recv()
            samples = (np.sin(np.arange(frame.samples) / 5) * 8000).astype("<i2")
            out = av.AudioFrame.from_ndarray(samples.reshape(1, -1), format="s16", layout="mono")
            out.sample_rate, out.pts, out.time_base = frame.sample_rate, frame.pts, fractions.Fraction(1, frame.sample_rate)
            return out

    loop = asyncio.new_event_loop()
    threading.Thread(target=loop.run_forever, daemon=True).start()

    async def make():
        pc = aiortc.RTCPeerConnection(aiortc.RTCConfiguration(iceServers=[]))
        pc.addTransceiver(Picture(), direction="sendonly")
        pc.addTransceiver(Tone(), direction="sendonly")
        await pc.setLocalDescription(await pc.createOffer())
        return pc

    pc = asyncio.run_coroutine_threadsafe(make(), loop).result(10)
    return loop, pc


def test_a_browser_over_webrtc_reaches_the_reader_and_the_mic():
    loop, peer = _fake_browser_offer()
    from aiortc import RTCSessionDescription

    feed = bs.BrowserFeed()
    gen = feed.connect()
    mic = feed.open_audio()
    session = browser_rtc.RtcSession(lambda frame, size: feed.put_video(gen, frame, size),
                                     lambda pcm: feed.put_audio(gen, pcm))
    try:
        answer = session.answer(peer.localDescription.sdp, 4000)
        assert "x-google-start-bitrate" in answer and session.codec in ("H264", "VP8")
        asyncio.run_coroutine_threadsafe(peer.setRemoteDescription(RTCSessionDescription(answer, "answer")),
                                         loop).result(10)

        reader = bs.BrowserReader(feed)
        assert reader.open()
        ok, frame = reader.read()
        assert ok and frame.shape == (240, 320, 3) and 90 <= int(frame.mean()) <= 160

        heard = bytearray()
        deadline = time.monotonic() + 10
        while len(heard) < 9600 and time.monotonic() < deadline:  # 100 ms of 48 kHz mono s16le
            heard += mic.read1(4096)
        samples = np.frombuffer(bytes(heard), "<i2")
        assert len(samples) >= 4800 and np.abs(samples).max() > 1000
    finally:
        mic.close()
        session.close()
        asyncio.run_coroutine_threadsafe(peer.close(), loop).result(10)
        loop.call_soon_threadsafe(loop.stop)


def test_an_offer_without_a_codec_this_computer_decodes_is_refused():
    pytest.importorskip("aiortc")
    session = browser_rtc.RtcSession(lambda *a: None, lambda *a: None)
    with pytest.raises(browser_rtc.RtcError):
        session.answer("v=0\r\nnot an offer\r\n", 4000)
