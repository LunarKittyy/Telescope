"""Frames from other streams (HostFrames): the newest picture of every running stream, for plugins that combine them.

The window tells it which streams run (on its own thread) and plugins ask from any thread, often from inside another
stream's frame steps. Each worker swaps in its newest frame as one tuple and never changes a frame after that, so
handing one out is just handing out a read-only view of it: no lock and no copy.
"""

from typing import Iterable, Optional

from telescope import vision
from telescope.plugin import StreamFrame


class Frames:
    """Which streams run, and their newest frames."""

    def __init__(self):
        self._streams: tuple = ()  # ((source id, name, worker), ...), replaced whole so readers never see it half done

    def set_streams(self, streams: Iterable[tuple]):
        """GUI thread: the streams running now as (source id, name, worker), in the order they started; a stream with
        the camera off has no worker."""
        self._streams = tuple((sid or "", name, worker) for sid, name, worker in streams if worker is not None)

    def frames(self, copy: bool = False) -> list:
        out = []
        for entry in self._streams:
            frame = _frame(entry, copy)
            if frame is not None:
                out.append(frame)
        return out

    def frame(self, source_id: Optional[str], copy: bool = False) -> Optional[StreamFrame]:
        entry = next((e for e in self._streams if e[0] == (source_id or "")), None)
        return _frame(entry, copy) if entry is not None else None

    def replace_frame(self, source_id: Optional[str], frame, number: int) -> bool:
        entry = next((e for e in self._streams if e[0] == (source_id or "")), None)
        return entry is not None and entry[2].replace_frame(frame, number)

    def source_of_current(self) -> Optional[str]:
        """The source id of the stream whose frame steps are running on this thread now, or None outside them."""
        here = vision.current_frame.get()
        if here is None:
            return None
        return next((sid for sid, _name, worker in self._streams if worker is here[0]), None)


def _frame(entry: tuple, copy: bool) -> Optional[StreamFrame]:
    sid, name, worker = entry
    published = worker.published()
    if published is None:
        return None
    frame, number, made = published
    if copy:
        frame = frame.copy()
    else:
        frame = frame.view()
        frame.flags.writeable = False
    return StreamFrame(sid, name, frame, number, made)
