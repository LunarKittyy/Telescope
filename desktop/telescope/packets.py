"""Plugins changing a stream's H.264 before it's decoded (HostPackets): each stream's tap, and what's safe to hand back.

A decoder only looks at Tap.step, which stays None while nobody changes that stream's packets, so it costs nothing then.
"""

import logging
from typing import Callable, Optional

logger = logging.getLogger(__name__)

# A step that fails this many access units in a row is left out until it's put on again
FAILS_BEFORE_SKIP = 30


class Tap:
    """The packet steps of one stream (its StreamWorker's). Set from the GUI thread, read on the decoder's."""

    def __init__(self):
        self.step: Optional[Callable[[bytes], list]] = None
        self._steps: dict = {}  # owner: its step on this stream

    def set(self, makers: dict) -> bool:
        """The steps from {owner: make_step}: an owner new here gets a fresh one from make_step(), one already on keeps
        its own, and {} takes them all off. True if any came off, so the picture needs a keyframe to recover."""
        removed = any(owner not in makers for owner in self._steps)
        self._steps = {owner: self._steps.get(owner) or guarded(owner, make()) for owner, make in makers.items()}
        self.step = chain(list(self._steps.values())) if self._steps else None
        return removed


def guarded(name: str, step: Callable[[bytes], list]) -> Callable[[bytes], list]:
    """A plugin's packet step that can't take the stream down: an access unit it fails on, or hands back something
    that isn't a list of bytes for, goes through unchanged."""
    fails = 0

    def run(unit: bytes) -> list:
        nonlocal fails
        if fails >= FAILS_BEFORE_SKIP:
            return [unit]
        try:
            out = step(unit)
            if not isinstance(out, (list, tuple)) or not all(isinstance(u, (bytes, bytearray)) for u in out):
                raise ValueError(f"returned {type(out).__name__} instead of a list of bytes")
        except Exception:
            fails += 1
            if fails == 1:
                logger.exception("Plugin %s failed on a packet; passing it through unchanged", name)
            elif fails == FAILS_BEFORE_SKIP:
                logger.error("Plugin %s failed on %d packets in a row; leaving it out", name, fails)
            return [unit]
        fails = 0
        return [bytes(u) for u in out]
    return run


def chain(steps: list) -> Callable[[bytes], list]:
    """The steps one after the other, each on every unit the one before handed back."""
    if len(steps) == 1:
        return steps[0]

    def run(unit: bytes) -> list:
        units = [unit]
        for step in steps:
            units = [out for u in units for out in step(u)]
        return units
    return run
