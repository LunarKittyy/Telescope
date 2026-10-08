"""Decoding peer JPEGs: the declared size is checked before anything is allocated."""

import struct

import cv2
import numpy as np
import pytest

from telescope import jpeg_guard
from telescope.browser_server import BrowserReader
from telescope.jpeg_guard import decode_jpeg, jpeg_size, within_limits
from telescope.mjpeg_reader import MjpegReader


def _jpeg(w=64, h=48, params=()):
    frame = np.random.default_rng(1).integers(0, 255, (h, w, 3), dtype=np.uint8)
    ok, buf = cv2.imencode(".jpg", frame, list(params))
    assert ok
    return bytearray(buf.tobytes())


def _patch_size(jpg, w, h, marker=b"\xff\xc0"):
    struct.pack_into(">HH", jpg, jpg.find(marker) + 5, h, w)
    return bytes(jpg)


def test_reads_the_size_of_real_encoder_output():
    assert jpeg_size(bytes(_jpeg(64, 48))) == (64, 48)
    assert jpeg_size(bytes(_jpeg(33, 7))) == (33, 7)


def test_reads_a_progressive_jpeg():
    jpg = bytes(_jpeg(80, 60, params=(cv2.IMWRITE_JPEG_PROGRESSIVE, 1)))
    assert b"\xff\xc2" in jpg
    assert jpeg_size(jpg) == (80, 60)


@pytest.mark.parametrize("marker", [0xC0, 0xC1, 0xC2, 0xC3, 0xC5, 0xC6, 0xC7, 0xC9, 0xCA, 0xCB, 0xCD, 0xCE, 0xCF])
def test_every_start_of_frame_variant_is_read(marker):
    jpg = bytearray(_jpeg(16, 16))
    jpg[jpg.find(b"\xff\xc0") + 1] = marker
    assert jpeg_size(_patch_size(jpg, 1234, 567, bytes([0xFF, marker]))) == (1234, 567)


def test_the_dht_marker_is_not_a_frame_header():
    jpg = bytes(_jpeg())
    assert b"\xff\xc4" in jpg and jpeg_size(jpg) == (64, 48)  # skipped as a segment, not mistaken for a frame


def test_segments_are_skipped_by_their_length_not_by_searching():
    jpg = bytes(_jpeg(20, 10))
    # A comment segment whose payload looks like a start-of-frame for 9999x9999 must not be believed
    fake = b"\xff\xc0\x00\x11\x08\x27\x0f\x27\x0f\x03" + bytes(8)
    comment = b"\xff\xfe" + struct.pack(">H", 2 + len(fake)) + fake
    assert jpeg_size(jpg[:2] + comment + jpg[2:]) == (20, 10)


def test_fill_bytes_and_standalone_markers_are_tolerated():
    jpg = bytes(_jpeg(20, 10))
    assert jpeg_size(jpg[:2] + b"\xff\xff\xff" + jpg[2:]) == (20, 10)
    assert jpeg_size(jpg[:2] + b"\xff\x01" + jpg[2:]) == (20, 10)


def test_exif_in_front_is_skipped():
    jpg = bytes(_jpeg(20, 10))
    exif = b"\xff\xe1" + struct.pack(">H", 2 + 300) + bytes(300)
    assert jpeg_size(jpg[:2] + exif + jpg[2:]) == (20, 10)


@pytest.mark.parametrize("data", [b"", b"\xff", b"\xff\xd8", b"not a jpeg at all", b"\xff\xd8\xff\xda\x00\x02",
                                  b"\xff\xd8\xff\xc0\x00", b"\xff\xd8\xff\xc0\x00\x0b\x08\x00", b"\xff\xd8\x00\x00",
                                  b"\xff\xd8\xff\xe0\x00\x01", b"\xff\xd8\xff\xe0\xff\xff\x00"])
def test_garbage_and_truncation_give_none_without_raising(data):
    assert jpeg_size(data) is None
    assert decode_jpeg(data) is None


def test_a_truncated_real_jpeg_is_still_sized_when_its_header_is_whole():
    jpg = bytes(_jpeg(64, 48))
    assert jpeg_size(jpg[:jpg.find(b"\xff\xda") + 2]) == (64, 48)
    assert jpeg_size(jpg[:jpg.find(b"\xff\xc0") + 4]) is None


@pytest.mark.parametrize("w, h, ok", [
    (8192, 4096, True), (8000, 8000, True), (8193, 10, False), (10, 8193, False),
    (32767, 32767, False), (20000, 20000, False), (8192, 8192, False), (0, 100, False),
])
def test_limits(w, h, ok):
    assert within_limits(_patch_size(_jpeg(), w, h)) is ok


def test_a_normal_frame_decodes_and_a_patched_huge_one_is_dropped_before_allocating(monkeypatch):
    good = bytes(_jpeg(64, 48))
    assert decode_jpeg(good).shape == (48, 64, 3)
    calls = []
    monkeypatch.setattr(jpeg_guard.cv2, "imdecode", lambda *a: calls.append(a))
    assert decode_jpeg(_patch_size(_jpeg(), 32767, 32767)) is None
    assert calls == []  # never reached the decoder


def test_both_readers_drop_an_oversized_frame_and_decode_a_normal_one():
    good, huge = bytes(_jpeg(32, 24)), _patch_size(_jpeg(), 32767, 32767)
    for decode in (MjpegReader.decode, BrowserReader.decode):
        assert decode(good).shape == (24, 32, 3)
        assert decode(huge) is None
