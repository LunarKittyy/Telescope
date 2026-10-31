#!/usr/bin/env python3
"""QR codes with odd segments and what cv2's detectAndDecode gives for them, as Rust constants for qr_reader_tests.rs."""
import cv2
import numpy as np
import qrcode
import qrcode.util as util
from qrcode.constants import ERROR_CORRECT_H, ERROR_CORRECT_L, ERROR_CORRECT_M

_lengths = util.length_in_bits


class Eci:
    """An ECI segment: its 8 bit designator sits where a length would, and it writes nothing else."""

    mode = 7

    def __init__(self, number):
        self.number = number

    def __len__(self):
        return self.number

    def write(self, buffer):
        pass


class Raw(util.QRData):
    """A byte segment of exactly these bytes."""

    def __init__(self, data):
        self.mode, self.data = util.MODE_8BIT_BYTE, data

    def __len__(self):
        return len(self.data)

    def write(self, buffer):
        for b in self.data:
            buffer.put(b, 8)


class Kanji:
    """A Kanji segment of Shift-JIS pairs."""

    mode = 8

    def __init__(self, data):
        self.data = data

    def __len__(self):
        return len(self.data) // 2

    def write(self, buffer):
        for i in range(0, len(self.data), 2):
            code = self.data[i] << 8 | self.data[i + 1]
            code -= 0x8140 if code < 0xe040 else 0xc140
            buffer.put((code >> 8) * 0xc0 + (code & 0xff), 13)


def lengths(mode, version):
    if mode == 7:
        return 8
    if mode == 8:
        return 8 if version < 10 else 10 if version < 27 else 12
    return _lengths(mode, version)


util.length_in_bits = lengths
qrcode.main.util = util


def modules(segments, version=1, level=ERROR_CORRECT_L) -> np.ndarray:
    qr = qrcode.QRCode(version=version, error_correction=level, border=0)
    qr.data_list = list(segments)
    qr.make(fit=False)
    return np.array(qr.get_matrix(), bool)


def picture(rows: np.ndarray) -> np.ndarray:
    side = (len(rows) + 8) * 6
    image = np.full((side, side), 255, np.uint8)
    for r, row in enumerate(rows):
        for c, dark in enumerate(row):
            if dark:
                image[(r + 4) * 6:(r + 5) * 6, (c + 4) * 6:(c + 5) * 6] = 0
    return image


CASES = (
    ("ECI_UTF8", "ECI 26 and valid UTF-8", [Eci(26), Raw("café".encode())], 1, ERROR_CORRECT_L),
    ("ECI_UTF8_BROKEN", "ECI 26 and bytes that aren't UTF-8", [Eci(26), Raw("café".encode("latin-1"))], 1, ERROR_CORRECT_L),
    ("ECI_UTF16", "ECI 25, whose raw bytes open with a nul", [Eci(25), Raw("hi".encode("utf-16-be"))], 1, ERROR_CORRECT_L),
    ("ECI_LATIN1", "ECI 3", [Eci(3), Raw("café".encode("latin-1"))], 1, ERROR_CORRECT_L),
    ("ECI_GREEK", "ECI 9 (ISO 8859-7), kept as raw bytes like any ECI", [Eci(9), Raw(b"caf\xe1")], 1, ERROR_CORRECT_L),
    ("ECI_UTF16_DIGITS", "ECI 25 on a numeric segment, where it isn't looked at", [Eci(25), util.QRData("1234", util.MODE_NUMBER)], 1, ERROR_CORRECT_L),
    ("BYTE_THEN_KANJI", "a byte segment before a Kanji one: the first segment decides", [Raw(b"abc"), Kanji("日本".encode("shift-jis"))], 1, ERROR_CORRECT_L),
    ("BYTES_WITH_A_NUL", "a nul byte: Python's text stops there", [Raw(b"ab\0cd")], 1, ERROR_CORRECT_L),
    ("ALPHA_THEN_BYTES", "an alphanumeric segment first, so bytes past ASCII fail the decode", [util.QRData("AB", util.MODE_ALPHA_NUM), Raw(b"\xe9")], 1, ERROR_CORRECT_L),
    ("ECI_UTF8_BLOCKS", "ECI 26 broken, in a code of four blocks", [Eci(26), Raw(b"\xe9" * 12)], 6, ERROR_CORRECT_H),
    ("ECI_UTF8_LONG_BLOCKS", "ECI 26 broken, in a code with long and short blocks", [Eci(26), Raw(b"\xe9" * 12)], 7, ERROR_CORRECT_M),
)


def main():
    detector = cv2.QRCodeDetector()
    for name, about, segments, version, level in CASES:
        rows = modules(segments, version, level)
        size = len(rows)
        try:
            text, points, _straight = detector.detectAndDecode(picture(rows))
            said = repr(text) if points is not None else "no code"
        except UnicodeDecodeError:
            said = "UnicodeDecodeError"
        words = ", ".join(hex(int("".join("1" if v else "0" for v in row), 2)) for row in rows)
        print(f"// Python: {said}, for {about}")
        print(f"const {name}: Recorded = ({size}, &[{words}]);")


main()
