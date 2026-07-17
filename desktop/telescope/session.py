from dataclasses import dataclass
from typing import TYPE_CHECKING

if TYPE_CHECKING:
    from telescope.phone_client import PhoneControlClient
    from telescope.stream import StreamWorker


@dataclass(frozen=True)
class StreamSession:
    """Owns the worker/client for one connect-to-disconnect stream lifecycle.

    Its `id` is threaded through async completions (phone-state fetches) so
    a result that arrives after the session has moved on (device switch,
    stop) can be recognized as stale and discarded instead of silently
    reaching plugins for the wrong device. Frozen: a device switch or stop
    always creates a fresh session (or clears it to None) rather than
    mutating this one in place.
    """

    id: int
    url: str
    client: "PhoneControlClient"
    worker: "StreamWorker"
