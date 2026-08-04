"""Client for the phone's session port (8766).

Separate from :mod:`telescope.phone_client`, which talks to the streaming
server on 8080 and therefore only exists while a stream is already running.
This one reaches the phone's ``SessionServer``, which stays bound while the
phone app is on screen *or* its camera service is live - the two states from
which the desktop needs to ask "are we still paired?" and "start the camera".

Qt-free, like :mod:`telescope.pairing` and :mod:`telescope.ip_utils`, so it can
be exercised without a ``QApplication``.
"""

import json
import logging
import urllib.error
import urllib.request
from dataclasses import dataclass
from typing import Optional

logger = logging.getLogger(__name__)

PING_PORT = 8766

#: Ping/probe budget. Matches the timeout the pair-status probe has always
#: used; long enough for a phone on a slow Wi-Fi link, short enough that a
#: 3s poll can't pile up.
REQUEST_TIMEOUT = 3

#: How long to wait for the phone's camera to actually come up after a start
#: is accepted. Opening a camera and configuring a capture session takes a
#: couple of seconds on most phones, and rather more on a cold app.
START_TIMEOUT = 12

#: Gap between "is it streaming yet?" polls while waiting out START_TIMEOUT.
START_POLL_INTERVAL = 0.5


@dataclass(frozen=True)
class PingResult:
    """Outcome of ``GET /v1/ping``.

    ``status`` keeps the exact vocabulary the connection panel has always
    displayed: ``paired`` / ``not_paired`` / ``unreachable``. The rest is the
    phone's reported state, absent (``None``) when talking to an app old
    enough to answer ping with a bare body.
    """

    status: str
    streaming: Optional[bool] = None
    busy: Optional[bool] = None
    local_only: Optional[bool] = None

    @property
    def paired(self) -> bool:
        return self.status == "paired"

    @property
    def knows_session(self) -> bool:
        """True when the phone reported a state, i.e. it is new enough to
        support remote start."""
        return self.streaming is not None


@dataclass(frozen=True)
class SessionResult:
    """Outcome of ``POST /v1/session``.

    ``unsupported`` is its own outcome rather than an error: an app predating
    this endpoint 404s, and the desktop's answer to that is to fall back to
    connecting to a stream the user started by hand, not to complain.
    """

    ok: bool
    error: Optional[str] = None
    unsupported: bool = False


class PhoneSessionClient:
    """Speaks the two session-port routes against one already-resolved base URL.

    The caller resolves the base (a device IP over Wi-Fi, ``localhost`` behind
    an ``adb forward`` over USB) and owns the lifetime of any tunnel it sits
    on; see ``ConnectionPlugin.session_channel``.
    """

    def __init__(self, base_url: str, token: str):
        self.base = base_url.rstrip("/")
        self.token = token

    def _headers(self, json_body: bool = False) -> dict:
        headers = {"Authorization": f"Bearer {self.token}"}
        if json_body:
            headers["Content-Type"] = "application/json"
        return headers

    def ping(self) -> PingResult:
        """Ask whether this token is still the paired one, and what the phone
        is doing. Status-code mapping is unchanged from the probe this
        replaces: 200 is paired, 401 means the phone has since paired with
        someone else (or been reset), anything else is a reachability
        problem."""
        req = urllib.request.Request(f"{self.base}/v1/ping", headers=self._headers())
        try:
            with urllib.request.urlopen(req, timeout=REQUEST_TIMEOUT) as r:
                if r.status != 200:
                    return PingResult("unreachable")
                return self._parse_ping_body(r.read())
        except urllib.error.HTTPError as exc:
            return PingResult("not_paired" if exc.code == 401 else "unreachable")
        except Exception:
            return PingResult("unreachable")

    @staticmethod
    def _parse_ping_body(raw: bytes) -> PingResult:
        # An older app answers 200 with the bare string "OK". That is still a
        # valid "yes, paired" - it just can't tell us anything more, and the
        # caller falls back to connect-only behaviour.
        try:
            body = json.loads(raw.decode())
            if not isinstance(body, dict):
                raise ValueError("not an object")
        except Exception:
            return PingResult("paired")
        return PingResult(
            status="paired",
            streaming=bool(body.get("streaming", False)),
            busy=bool(body.get("busy", False)),
            local_only=bool(body.get("localOnly", False)),
        )

    def start(self) -> SessionResult:
        return self._session("start")

    def stop(self) -> SessionResult:
        return self._session("stop")

    def _session(self, action: str) -> SessionResult:
        payload = json.dumps({"action": action}).encode()
        req = urllib.request.Request(
            f"{self.base}/v1/session",
            data=payload,
            headers=self._headers(json_body=True),
            method="POST",
        )
        try:
            with urllib.request.urlopen(req, timeout=REQUEST_TIMEOUT) as r:
                body = json.loads(r.read().decode())
            if body.get("ok"):
                return SessionResult(ok=True)
            return SessionResult(ok=False, error=body.get("error") or "refused")
        except urllib.error.HTTPError as exc:
            if exc.code == 404:
                return SessionResult(ok=False, unsupported=True)
            if exc.code == 401:
                return SessionResult(ok=False, error="not_paired")
            return SessionResult(ok=False, error=f"http_{exc.code}")
        except Exception:
            logger.debug("session %s failed", action, exc_info=True)
            return SessionResult(ok=False, error="unreachable")
