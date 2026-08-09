"""Client for phone's session port (8766), always reachable unlike streaming server."""

import json
import logging
import urllib.error
import urllib.request
from dataclasses import dataclass
from typing import Optional

logger = logging.getLogger(__name__)

PING_PORT = 8766

REQUEST_TIMEOUT = 3  # Ping timeout; long enough for slow Wi-Fi, short enough for polling.
START_TIMEOUT = 12   # Wait for camera to come up after accepting start.
START_POLL_INTERVAL = 0.5  # Poll interval while waiting for camera startup.


@dataclass(frozen=True)
class PingResult:
    """Outcome of GET /v1/ping; status: paired/not_paired/unreachable."""

    status: str
    streaming: Optional[bool] = None
    busy: Optional[bool] = None
    local_only: Optional[bool] = None

    @property
    def paired(self) -> bool:
        return self.status == "paired"

    @property
    def knows_session(self) -> bool:
        """True when phone reported state (new enough to support remote start)."""
        return self.streaming is not None


@dataclass(frozen=True)
class SessionResult:
    """Outcome of POST /v1/session; unsupported (404) triggers fallback to manual start."""

    ok: bool
    error: Optional[str] = None
    unsupported: bool = False


class PhoneSessionClient:
    """Talks to resolved base URL (device IP or localhost via adb forward)."""

    def __init__(self, base_url: str, token: str):
        self.base = base_url.rstrip("/")
        self.token = token

    def _headers(self, json_body: bool = False) -> dict:
        headers = {"Authorization": f"Bearer {self.token}"}
        if json_body:
            headers["Content-Type"] = "application/json"
        return headers

    def ping(self) -> PingResult:
        """Check if token is still paired and phone status (200=paired, 401=unpaired, other=unreachable)."""
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
        # Older apps respond with bare "OK"; still means paired but no state details.
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
