"""Client for phone's session port (8766), always reachable unlike streaming server."""

import http.client
import json
import logging
import urllib.error
import urllib.request
from dataclasses import dataclass
from typing import Optional

from telescope.pinned_https import PhoneAuth, handshake_failed

logger = logging.getLogger(__name__)

PING_PORT = 8766

# Shape of the phone's session API (the phone's SessionServer.PROTOCOL_VERSION). Both apps must match.
# 3: TLS on both of the phone's ports, pinned to the certificate fingerprint it gave at pairing.
SESSION_PROTOCOL = 3

REQUEST_TIMEOUT = 3  # Ping timeout; long enough for slow Wi-Fi, short enough for polling.
START_TIMEOUT = 12   # Wait for camera to come up after accepting start.
START_POLL_INTERVAL = 0.5  # Poll interval while waiting for camera startup.
# Largest JSON reply taken from anything answering on a phone's ports; whoever answers may not be the phone.
MAX_REPLY_BYTES = 256 * 1024


def read_capped(response, limit: int = MAX_REPLY_BYTES) -> bytes:
    """The response body, or ValueError when it's over limit."""
    raw = response.read(limit + 1)
    if len(raw) > limit:
        raise ValueError(f"reply over {limit} bytes")
    return raw


MAX_NAME_CHARS = 64


def clean_name(raw) -> str:
    """A phone's name as safe display text: no control characters, no < or > (Qt would render it as HTML), capped."""
    text = "".join(" " if c.isspace() else c for c in str(raw) if (c.isprintable() or c.isspace()) and c not in "<>")
    return " ".join(text.split())[:MAX_NAME_CHARS]


# The phone is on this network or behind an adb forward; a configured proxy would answer for it instead.
_NO_PROXY = urllib.request.build_opener(urllib.request.ProxyHandler({}))


@dataclass(frozen=True)
class PingResult:
    """Outcome of GET /v1/ping; status: paired/not_paired/unreachable."""

    status: str
    streaming: Optional[bool] = None
    busy: Optional[bool] = None
    local_only: Optional[bool] = None
    # Which phone answered; lets a USB probe tell our phone from any other one plugged in.
    phone_id: Optional[str] = None
    phone_name: Optional[str] = None

    @property
    def paired(self) -> bool:
        return self.status == "paired"


HELLO_OK = "ok"            # a Telescope app answered with its identity
HELLO_MISSING = "missing"  # something answered on the session port but has no /v1/hello: an app too old
HELLO_NONE = "none"        # nothing answered
HELLO_USB_ONLY = "usb_only"  # the phone has Local only on and answers only ping off USB (403)


@dataclass(frozen=True)
class Hello:
    """Outcome of GET /v1/hello."""

    status: str
    phone_id: str = ""
    phone_name: str = ""
    protocol: int = 0
    app_version: str = ""
    build: int = 0

    @property
    def ok(self) -> bool:
        return self.status == HELLO_OK


@dataclass(frozen=True)
class SessionResult:
    """Outcome of POST /v1/session."""

    ok: bool
    error: Optional[str] = None


class PhoneSessionClient:
    """Talks to resolved base URL (device IP or localhost via adb forward)."""

    def __init__(self, base_url: str, auth: PhoneAuth):
        self.base = base_url.rstrip("/")
        self.auth = auth

    def _headers(self, json_body: bool = False) -> dict:
        headers = self.auth.headers()
        if json_body:
            headers["Content-Type"] = "application/json"
        return headers

    def hello(self, timeout: float = REQUEST_TIMEOUT) -> Hello:
        """Who answers on the session port (/v1/hello, no token sent)."""
        try:
            with self.auth.open(f"{self.base}/v1/hello", timeout=timeout) as r:
                body = json.loads(read_capped(r).decode())
        except urllib.error.HTTPError as exc:
            # The session server answers 404 for routes it doesn't know: an app from before /v1/hello.
            if exc.code == 404:
                return Hello(HELLO_MISSING)
            return Hello(HELLO_USB_ONLY if exc.code == 403 else HELLO_NONE)
        except Exception as exc:
            # Only a listener that took the connection but not TLS is worth a plain look; nobody there, or the wrong certificate, is just not our phone.
            # LEGACY MIGRATION PATH: a phone app from before TLS.
            if handshake_failed(exc) and self._answers_plain_http(timeout):
                return Hello(HELLO_MISSING)
            return Hello(HELLO_NONE)
        if not isinstance(body, dict):
            return Hello(HELLO_NONE)
        phone_id = body.get("phoneId")
        if not isinstance(phone_id, str) or not phone_id:
            return Hello(HELLO_NONE)

        def field(key, kind, default):
            value = body.get(key)
            return value if isinstance(value, kind) and not isinstance(value, bool) else default
        return Hello(HELLO_OK, phone_id, clean_name(field("phoneName", str, "")), field("protocol", int, 0),
                     field("appVersion", str, ""), field("build", int, 0))

    # LEGACY MIGRATION PATH: remove with its caller in hello() once no phone runs an app from before TLS.
    def _answers_plain_http(self, timeout: float) -> bool:
        """Whether an app from before TLS answers here: plain HTTP, no token, nothing sent that matters."""
        if not self.base.startswith("https://"):
            return False
        try:
            with _NO_PROXY.open("http://" + self.base[len("https://"):] + "/v1/hello", timeout=timeout) as r:
                read_capped(r)
            return True
        except urllib.error.HTTPError:
            return True
        except (OSError, ValueError, http.client.HTTPException):  # HTTPException: something there that isn't HTTP
            return False

    def ping(self) -> PingResult:
        """Check if token is still paired and phone status (200=paired, 401=unpaired, other=unreachable)."""
        req = urllib.request.Request(f"{self.base}/v1/ping", headers=self._headers())
        try:
            with self.auth.open(req, timeout=REQUEST_TIMEOUT) as r:
                if r.status != 200:
                    return PingResult("unreachable")
                return self._parse_ping_body(read_capped(r))
        except urllib.error.HTTPError as exc:
            return PingResult("not_paired" if exc.code == 401 else "unreachable")
        except Exception:
            return PingResult("unreachable")

    @staticmethod
    def _parse_ping_body(raw: bytes) -> PingResult:
        # Anything but a JSON object isn't Telescope (or is an app too old to pair with this computer).
        try:
            body = json.loads(raw.decode())
            if not isinstance(body, dict):
                raise ValueError("not an object")
        except Exception:
            return PingResult("unreachable")
        return PingResult(
            status="paired",
            streaming=bool(body.get("streaming", False)),
            busy=bool(body.get("busy", False)),
            local_only=bool(body.get("localOnly", False)),
            phone_id=body.get("phoneId") if isinstance(body.get("phoneId"), str) else None,
            phone_name=clean_name(body["phoneName"]) if isinstance(body.get("phoneName"), str) else None,
        )

    def start(self, opening: Optional[dict] = None) -> SessionResult:
        """opening: the size and rate to open at ({"width", "height", "fps"}, any of them); else the phone's last."""
        return self._session("start", **(opening or {}))

    def stop(self) -> SessionResult:
        return self._session("stop")

    def unpair(self) -> bool:
        """Ask the phone to revoke this computer's token (best effort; True if it confirmed)."""
        req = urllib.request.Request(
            f"{self.base}/v1/unpair", data=b"{}", headers=self._headers(json_body=True), method="POST",
        )
        try:
            with self.auth.open(req, timeout=REQUEST_TIMEOUT) as r:
                return r.status == 200
        except Exception:
            logger.debug("unpair failed", exc_info=True)
            return False

    def _session(self, action: str, **params) -> SessionResult:
        payload = json.dumps({"action": action, **params}).encode()
        req = urllib.request.Request(
            f"{self.base}/v1/session",
            data=payload,
            headers=self._headers(json_body=True),
            method="POST",
        )
        try:
            with self.auth.open(req, timeout=REQUEST_TIMEOUT) as r:
                body = json.loads(read_capped(r).decode())
            if body.get("ok"):
                return SessionResult(ok=True)
            return SessionResult(ok=False, error=body.get("error") or "refused")
        except urllib.error.HTTPError as exc:
            if exc.code == 401:
                return SessionResult(ok=False, error="not_paired")
            return SessionResult(ok=False, error=f"http_{exc.code}")
        except Exception:
            logger.debug("session %s failed", action, exc_info=True)
            return SessionResult(ok=False, error="unreachable")
