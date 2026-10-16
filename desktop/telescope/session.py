"""One stream's session, and the bookkeeping of every stream the window runs.

Sessions holds which sources stream, which one the panels show, which one the plugins that stay put (the mic) are on,
and what's starting or waiting to. It knows nothing about Qt, widgets or plugins' hooks, so the rules about who hears
what can be tested on their own; the window drives it and does the starting and stopping.
"""

from dataclasses import dataclass
from typing import TYPE_CHECKING, Callable, Optional

if TYPE_CHECKING:
    from telescope.phone_client import PhoneControlClient
    from telescope.sources import Source
    from telescope.stream import StreamWorker


@dataclass(frozen=True)
class StreamSession:
    """Owns worker/client for one stream lifecycle; id discards stale async results."""

    id: int
    url: str
    client: "PhoneControlClient"
    worker: "StreamWorker"
    source: "Source"  # where it streams from: the phone, or a plugin's StreamSource


class Sessions:
    """Every stream the window runs, and where starting and stopping them stands."""

    def __init__(self):
        self.streams: list = []  # the sources streaming now, in the order they started
        self.focus: Optional["Source"] = None  # the stream the panels show (and plugins that follow them hear about)
        self.main: Optional["Source"] = None  # the first stream: plugins that don't follow the panels (the mic) hear it
        self.pending: Optional["Source"] = None  # added next to the streams, waiting for something to stream (a browser)
        self.waking = False
        self.wake_source: Optional["Source"] = None  # the source a start is waking, which a stop meanwhile has to reach
        self.preparing = False  # a start is finding the phone; timers still fire meanwhile, so it must not start again
        self.restarting = False  # stopping only to start again (reconnect, virtual camera resize)
        self.stopping_all = False
        self.restart_later: set = set()  # streams to restart once the start under way is through
        self.held_states: dict = {}  # id: (source, session id, state) that needs the panels once that start is through
        self.start_later: list = []  # starts that waited for the extra cameras, one at a time once they're there
        self.canvas_restart = False  # a canvas resize stopped the stream and starts it once done; a Stop cancels it

    # ── Which sources stream ──────────────────────────────────────────────

    def add(self, source: "Source", session: StreamSession):
        """source streams with session now (a new stream, or the same one with a new worker or URL)."""
        source.session = session
        if source not in self.streams:
            self.streams.append(source)

    def forget(self, source: "Source"):
        source.session = None
        if source in self.streams:
            self.streams.remove(source)

    @property
    def shown(self) -> Optional[StreamSession]:
        """The session of the stream the panels show."""
        return self.focus.session if self.focus is not None else None

    def find(self, source_id: Optional[str]) -> Optional["Source"]:
        """The streaming source with that id."""
        return next((s for s in self.streams if s.id == source_id), None)

    def background(self) -> list:
        """The streaming sources the panels don't show."""
        return [s for s in self.streams if s is not self.focus]

    def by_worker(self, worker) -> Optional["Source"]:
        """The streaming source whose worker that is (None: one that's been let go)."""
        return next((s for s in self.streams if s.session.worker is worker), None)

    # ── Starting ──────────────────────────────────────────────────────────

    def is_starting(self) -> bool:
        return self.waking or self.preparing

    def is_waking(self, source_id: Optional[str]) -> bool:
        """Whether the start under way is waking source_id."""
        return self.waking and self.wake_source is not None and self.wake_source.id == source_id

    def is_streaming_from(self, source_id: Optional[str]) -> bool:
        """Whether source_id streams, or a start is waking it, whether or not the panels show it."""
        return self.is_waking(source_id) or self.find(source_id) is not None

    def active(self, selected: Callable[[], "Source"]) -> "Source":
        """The panels' stream's source, the one a start is waking, or while idle the one Start would stream from
        (selected(), only asked for then)."""
        if self.focus is not None and (self.focus.session is not None or not self.waking):
            return self.focus
        if self.waking and self.wake_source is not None:
            return self.wake_source  # the picker may have moved on already: a switch stops the wake after that
        return selected()

    def drop_queued(self):
        """Forget the restarts, held phone states and starts waiting for the start under way (a Stop)."""
        self.restart_later.clear()
        self.held_states.clear()
        self.start_later.clear()

    # ── Who hears about which stream ──────────────────────────────────────

    def hearing(self, plugins: list, source: "Source") -> list:
        """The plugins that hear about source: those following the panels if they show it, and those staying with
        the main stream (the mic) if it's that one."""
        return [p for p in plugins if source is (self.focus if p.follows_focus else self.main)]

    @staticmethod
    def followers(plugins: list) -> list:
        """The plugins that follow the panels from stream to stream."""
        return [p for p in plugins if p.follows_focus]

    @staticmethod
    def stays(plugins: list) -> list:
        """The plugins that stay with one stream (the mic) whichever the panels show."""
        return [p for p in plugins if not p.follows_focus]
