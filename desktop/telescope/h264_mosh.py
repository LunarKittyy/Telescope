"""Datamosh on H.264 access units: keyframes dropped, frames repeated, and the headers fixed up so the decoder takes it.

Dropping a keyframe alone doesn't mosh: the decoder throws away the frames after it, since their frame numbers and
picture order counts don't follow on from the last frame it showed. So once a frame has been dropped or repeated,
every slice header gets frame_num and pic_order_cnt_lsb that continue from the last frame decoded, and the new motion
lands on the old picture. Only the first bytes of each slice are touched; the rest is copied as it came.
"""

import re
from dataclasses import dataclass
from typing import Callable, Optional

_START = re.compile(b"\x00\x00\x01")
_SLICE, _IDR, _SPS = 1, 5, 7
# Enough escaped bytes for a slice header up to pic_order_cnt_lsb
_HEAD = 24
_HIGH_PROFILES = (100, 110, 122, 244, 44, 83, 86, 118, 128, 138, 139, 134, 135)


@dataclass(frozen=True)
class Mosh:
    """What a Mosher does, swapped whole from the GUI thread."""

    drop_keyframes: bool = True
    repeats: int = 0  # extra copies of every frame, which smears the motion further


def _nals(data: bytes) -> list:
    """(start, end) of each NAL unit's bytes after its start code."""
    starts = [m.end() for m in _START.finditer(data)]
    out = []
    for i, s in enumerate(starts):
        e = starts[i + 1] - 3 if i + 1 < len(starts) else len(data)
        while e > s and data[e - 1] == 0:
            e -= 1
        out.append((s, e))
    return out


def _unescape(b: bytes) -> bytes:
    out, zeros = bytearray(), 0
    for x in b:
        if zeros >= 2 and x == 3:
            zeros = 0
            continue
        out.append(x)
        zeros = zeros + 1 if x == 0 else 0
    return bytes(out)


def _escape(b: bytes) -> bytes:
    out, zeros = bytearray(), 0
    for x in b:
        if zeros >= 2 and x <= 3:
            out.append(3)
            zeros = 0
        out.append(x)
        zeros = zeros + 1 if x == 0 else 0
    return bytes(out)


class _Bits:
    def __init__(self, data: bytes, pos: int = 8):
        self.data, self.pos = data, pos

    def u(self, n: int) -> int:
        v = 0
        for _ in range(n):
            v = (v << 1) | ((self.data[self.pos >> 3] >> (7 - (self.pos & 7))) & 1)
            self.pos += 1
        return v

    def ue(self) -> int:
        zeros = 0
        while self.u(1) == 0:
            zeros += 1
        return (1 << zeros) - 1 + self.u(zeros)

    def se(self) -> int:
        k = self.ue()
        return (k + 1) // 2 if k & 1 else -(k // 2)


def _put(buf: bytearray, pos: int, n: int, v: int):
    for i in range(n):
        p = pos + i
        if (v >> (n - 1 - i)) & 1:
            buf[p >> 3] |= 1 << (7 - (p & 7))
        else:
            buf[p >> 3] &= ~(1 << (7 - (p & 7))) & 0xFF


@dataclass(frozen=True)
class _Sps:
    frame_num_bits: int
    poc_type: int
    poc_bits: int
    frame_mbs_only: bool


def _parse_sps(nal: bytes) -> Optional[_Sps]:
    """The fields a slice header needs, or None for POC type 1, which this doesn't rewrite."""
    b = _Bits(_unescape(nal))
    profile = b.u(8)
    b.u(16)
    b.ue()
    if profile in _HIGH_PROFILES:
        chroma = b.ue()
        if chroma == 3:
            b.u(1)
        b.ue()
        b.ue()
        b.u(1)
        if b.u(1):
            for i in range(8 if chroma != 3 else 12):
                if b.u(1):
                    last = nxt = 8
                    for _ in range(16 if i < 6 else 64):
                        if nxt:
                            nxt = (last + b.se() + 256) % 256
                        last = nxt or last
    frame_num_bits = b.ue() + 4
    poc_type = b.ue()
    poc_bits = 0
    if poc_type == 0:
        poc_bits = b.ue() + 4
    elif poc_type == 1:
        return None
    b.ue()
    b.u(1)
    b.ue()
    b.ue()
    return _Sps(frame_num_bits, poc_type, poc_bits, bool(b.u(1)))


def _cut(nal: bytes) -> int:
    """Where to split a slice so its head holds the header and no escape sequence straddles the split."""
    cut = min(_HEAD, len(nal))
    while cut < len(nal) and (nal[cut - 1] == 0 or nal[cut - 2] == 0):
        cut += 1
    return cut


class Mosher:
    """A packet step (HostPackets) that moshes one stream. settings() is read for each access unit. clean_up: let the
    next keyframe through first, so the picture comes back."""

    def __init__(self, settings: Callable[[], Mosh], clean_up: bool = False):
        self._settings = settings
        self._sps: Optional[_Sps] = None
        self._sps_bytes = b""
        self._rewriting = False  # once a frame is dropped or repeated, every header after it gets new numbers
        self._decoded = False  # nothing to smear onto before the first frame
        self._clean_up = clean_up
        self._frame_num = 0  # the last reference frame's, as decoded
        self._poc = 0  # the last frame's, as decoded
        self._poc_step = 2
        self._sent_poc: Optional[int] = None  # the last frame's as the phone sent it

    def __call__(self, unit: bytes) -> list:
        mosh = self._settings()
        nals = _nals(unit)
        types = [unit[s] & 31 for s, _e in nals]
        if _SPS in types:
            s, e = nals[types.index(_SPS)]
            if unit[s:e] != self._sps_bytes:
                self._sps_bytes, self._sps = unit[s:e], _parse_sps(unit[s:e])
                return self._pass(unit, nals, types)  # a new size or format: this keyframe has to go through
        if self._sps is None or not any(t in (_SLICE, _IDR) for t in types):
            return [unit]
        if _IDR in types:
            if not mosh.drop_keyframes or not self._decoded or self._clean_up:
                self._clean_up = False
                return self._pass(unit, nals, types)
            kept = b"".join(b"\x00\x00\x00\x01" + unit[s:e] for (s, e), t in zip(nals, types) if t not in (_SLICE, _IDR))
            self._rewriting = True
            return [kept] if kept else []
        if mosh.repeats > 0:
            self._rewriting = True
        copies = 1 + max(0, mosh.repeats)
        return [self._renumber(unit, nals, types, i) for i in range(copies)]

    def _pass(self, unit: bytes, nals: list, types: list) -> list:
        """A keyframe let through: the numbers start again from it."""
        self._rewriting = False
        self._decoded = True
        if _IDR in types:
            self._note(unit, nals, types)
        return [unit]

    def _note(self, unit: bytes, nals: list, types: list):
        """Remember a frame's numbers as decoded, unchanged."""
        for (s, e), t in zip(nals, types):
            if t in (_SLICE, _IDR):
                head = _unescape(unit[s:s + _cut(unit[s:e])])
                _fn_pos, fn, _poc_pos, poc = self._fields(head, t == _IDR)
                if unit[s] & 0x60:
                    self._frame_num = fn
                if self._sent_poc is not None and t != _IDR:
                    step = (poc - self._sent_poc) % (1 << max(self._sps.poc_bits, 1))
                    self._poc_step = step if 0 < step <= 16 else 2
                self._sent_poc, self._poc = poc, poc
                return

    def _fields(self, head: bytes, idr: bool) -> tuple:
        """Bit positions and values of frame_num and pic_order_cnt_lsb in an unescaped slice head."""
        sps = self._sps
        b = _Bits(head)
        b.ue()
        b.ue()
        b.ue()
        fn_pos = b.pos
        fn = b.u(sps.frame_num_bits)
        if not sps.frame_mbs_only and b.u(1):
            b.u(1)
        if idr:
            b.ue()
        if sps.poc_type != 0:
            return fn_pos, fn, None, 0
        poc_pos = b.pos
        return fn_pos, fn, poc_pos, b.u(sps.poc_bits)

    def _renumber(self, unit: bytes, nals: list, types: list, copy: int) -> bytes:
        """unit with frame_num and pic_order_cnt_lsb continuing from the last frame decoded."""
        self._decoded = True
        if not self._rewriting:
            self._note(unit, nals, types)
            return unit
        sps = self._sps
        out, frame_num, poc = [], None, None
        for (s, e), t in zip(nals, types):
            nal = unit[s:e]
            if t == _SLICE:
                cut = _cut(nal)
                head = bytearray(_unescape(nal[:cut]))
                fn_pos, _fn, poc_pos, sent_poc = self._fields(bytes(head), False)
                if frame_num is None:
                    if copy == 0 and self._sent_poc is not None:
                        step = (sent_poc - self._sent_poc) % (1 << max(sps.poc_bits, 1))
                        self._poc_step = step if 0 < step <= 16 else self._poc_step
                        self._sent_poc = sent_poc
                    frame_num = (self._frame_num + 1) % (1 << sps.frame_num_bits)
                    poc = (self._poc + self._poc_step) % (1 << max(sps.poc_bits, 1))
                    if nal[0] & 0x60:
                        self._frame_num = frame_num
                    self._poc = poc
                _put(head, fn_pos, sps.frame_num_bits, frame_num)
                if poc_pos is not None:
                    _put(head, poc_pos, sps.poc_bits, poc)
                nal = _escape(bytes(head)) + nal[cut:]
            out.append(b"\x00\x00\x00\x01" + nal)
        return b"".join(out)
