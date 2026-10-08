"""Decoding a JPEG from a peer without trusting the size it declares: a few hundred bytes can claim a 3 GB frame."""

import struct
from typing import Optional

import cv2
import numpy as np

MAX_SIDE = 8192
MAX_PIXELS = 64_000_000

# Start-of-frame markers: baseline, extended, progressive and lossless, Huffman or arithmetic coded (not C4, C8, CC)
_SOF = frozenset({0xC0, 0xC1, 0xC2, 0xC3, 0xC5, 0xC6, 0xC7, 0xC9, 0xCA, 0xCB, 0xCD, 0xCE, 0xCF})
_NO_LENGTH = frozenset({0x01, 0xD8, 0xD9, *range(0xD0, 0xD8)})  # TEM, SOI, EOI, RSTn


def jpeg_size(data: bytes) -> Optional[tuple]:
    """(width, height) from the first start-of-frame header, or None if there isn't a readable one before the scan."""
    n = len(data)
    if n < 4 or data[0] != 0xFF or data[1] != 0xD8:
        return None
    i = 2
    while i + 1 < n:
        if data[i] != 0xFF:
            return None
        while i + 1 < n and data[i + 1] == 0xFF:  # fill bytes before a marker
            i += 1
        if i + 1 >= n:
            return None
        marker = data[i + 1]
        i += 2
        if marker in _NO_LENGTH:
            continue
        if marker == 0xDA or marker == 0x00 or i + 2 > n:
            return None  # the scan starts without a frame header before it, or the stream is garbage
        (length,) = struct.unpack_from(">H", data, i)
        if length < 2:
            return None
        if marker in _SOF:
            if length < 8 or i + 7 > n:
                return None
            height, width = struct.unpack_from(">HH", data, i + 3)
            return width, height
        i += length
    return None


def within_limits(data: bytes) -> bool:
    """Whether the JPEG declares a size worth decoding; one with no readable header isn't."""
    size = jpeg_size(data)
    if size is None:
        return False
    width, height = size
    return 0 < width <= MAX_SIDE and 0 < height <= MAX_SIDE and width * height <= MAX_PIXELS


def decode_jpeg(data: bytes):
    """The BGR frame in data, or None if it doesn't decode or declares more pixels than we'll allocate."""
    if not within_limits(data):
        return None
    return cv2.imdecode(np.frombuffer(data, dtype=np.uint8), cv2.IMREAD_COLOR)
