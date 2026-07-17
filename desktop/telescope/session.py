from dataclasses import dataclass


@dataclass(frozen=True)
class StreamSession:
    """Identity for one connect-to-disconnect stream lifecycle.

    Threaded through async completions (phone-state fetches, monitoring
    polls) so a result that arrives after the session has moved on (device
    switch, stop) can be recognized as stale and discarded instead of
    silently reaching plugins for the wrong device.
    """

    id: int
    url: str
