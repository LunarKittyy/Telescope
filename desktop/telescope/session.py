from dataclasses import dataclass
from typing import TYPE_CHECKING, Optional

if TYPE_CHECKING:
    from telescope.phone_client import PhoneControlClient
    from telescope.stream import StreamWorker


@dataclass(frozen=True)
class StreamSession:
    """Owns worker/client for one stream lifecycle; id discards stale async results."""

    id: int
    url: str
    client: "PhoneControlClient"
    worker: "StreamWorker"
    source: Optional[object] = None  # the StreamSource it streams from, or None for a phone
