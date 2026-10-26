"""On Air: whether you're on a call, from what Telescope knows, and the two ways of telling the people around you. Qt-free.

Debounce turns the raw answer into one that doesn't flap while an app reopens the camera. webhook_request builds what
goes to a smart home hub and WebhookSender sends it off the GUI thread. OnAirServer serves the sign page to another
device over HTTPS, with a link of its own that keeps working across restarts, on the Browser camera's certificate and
TlsServer (so the same connection caps and headers apply). It answers nothing but the page, its script and the state.
"""

import hmac
import json
import logging
import secrets
import ssl
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Callable, Optional

from telescope.browser_server import PageHandler, ServerStats, TlsServer
from telescope.version import VERSION

logger = logging.getLogger(__name__)

ON_AIR_PORT = 8768
ON_AFTER_S = 1.0  # the raw answer has to hold this long before the sign goes on
OFF_AFTER_S = 6.0  # and this long before it goes off (apps close and reopen the camera when settings change)
WEBHOOK_TIMEOUT_S = 5.0
_POLL_WAIT_S = 20.0  # how long a page's request for the state waits for a change before answering anyway

TRIGGERS = ("camera", "camera_mic", "mic", "streaming")
DEFAULT_TRIGGER = "camera"
METHODS = ("POST", "GET")

_WEB_DIR = Path(__file__).resolve().parent / "web"
_PAGES = {
    "/": ("on-air.html", "text/html; charset=utf-8"),
    "/on-air.js": ("on-air.js", "text/javascript; charset=utf-8"),
}


def new_token() -> str:
    return secrets.token_urlsafe(18)


def is_on_air(trigger: str, watched: bool, streaming: bool, mic_enabled: bool, mic_muted: bool) -> bool:
    """The raw answer: watched is an app reading the virtual camera, the mic counts while it streams unmuted."""
    mic = streaming and mic_enabled and not mic_muted
    if trigger == "mic":
        return mic
    if trigger == "streaming":
        return streaming
    if trigger == "camera_mic":
        return watched or mic
    return watched


class Debounce:
    """A raw on or off only counts once it has held for a moment: on_after going on, off_after going off."""

    def __init__(self, on_after: float = ON_AFTER_S, off_after: float = OFF_AFTER_S):
        self.on_after, self.off_after = on_after, off_after
        self.state = False
        self._raw = False
        self._since = 0.0

    def feed(self, raw: bool, now: float):
        if raw != self._raw:
            self._raw, self._since = raw, now

    def due_in(self, now: float) -> Optional[float]:
        """Seconds until the raw answer counts, or None when it already does."""
        if self._raw == self.state:
            return None
        wait = self.on_after if self._raw else self.off_after
        return max(0.0, self._since + wait - now)

    def settle(self, now: float) -> bool:
        """Takes the raw answer once it has held long enough; True when the state changed."""
        left = self.due_in(now)
        if left is None or left > 0:
            return False
        self.state = self._raw
        return True


# ── Webhook ──────────────────────────────────────────────────────────────────

def default_payload(method: str, on: bool) -> str:
    if method == "GET":
        return f"on_air={'true' if on else 'false'}"
    return json.dumps({"on_air": on})


def webhook_problem(url: str) -> str:
    """Why url can't be used as a webhook (English, for the log and tests), or "" when it can."""
    if not url:
        return "no address"
    parts = urllib.parse.urlsplit(url)
    if parts.scheme not in ("http", "https"):
        return "it has to start with http:// or https://"
    if not parts.hostname:
        return "there's no host in it"
    return ""


def webhook_request(url: str, method: str, payload: str, on: bool) -> urllib.request.Request:
    """What's sent for on or off: POST sends payload as the body (JSON when it parses as JSON), GET adds it to the
    query. An empty payload means default_payload."""
    method = method if method in METHODS else METHODS[0]
    payload = payload.strip() or default_payload(method, on)
    headers = {"User-Agent": f"Telescope/{VERSION}"}
    if method == "GET":
        parts = urllib.parse.urlsplit(url)
        query = "&".join(q for q in (parts.query, payload.lstrip("?&")) if q)
        return urllib.request.Request(urllib.parse.urlunsplit(parts._replace(query=query, fragment="")), headers=headers,
                                      method="GET")
    try:
        json.loads(payload)
        headers["Content-Type"] = "application/json"
    except ValueError:
        headers["Content-Type"] = "text/plain; charset=utf-8"
    return urllib.request.Request(url, data=payload.encode("utf-8"), headers=headers, method="POST")


def send_webhook(request: urllib.request.Request, timeout: float = WEBHOOK_TIMEOUT_S,
                 opener: Callable = urllib.request.urlopen) -> str:
    """Sends it; "" when the other end took it, else a short reason (English, no address in it)."""
    try:
        with opener(request, timeout=timeout) as response:
            response.read(4096)
        return ""
    except urllib.error.HTTPError as exc:
        return f"HTTP {exc.code}"
    except urllib.error.URLError as exc:
        reason = exc.reason
        if isinstance(reason, (TimeoutError, OSError)) and getattr(reason, "strerror", None):
            return reason.strerror
        return str(reason) or "couldn't connect"
    except (TimeoutError, OSError, ValueError) as exc:
        return str(exc) or type(exc).__name__


class WebhookSender:
    """Sends requests one at a time on a thread of its own; a newer request replaces one still waiting, so a quick
    on-off-on only sends what's true at the end. on_result(tag, error) runs on that thread ("" error: it worked)."""

    def __init__(self, on_result: Callable[[str, str], None], send: Callable = send_webhook):
        self._on_result, self._send = on_result, send
        self._lock = threading.Lock()
        self._pending: Optional[tuple] = None
        self._busy = False

    def send(self, request: urllib.request.Request, tag: str = ""):
        with self._lock:
            self._pending = (request, tag)
            if self._busy:
                return
            self._busy = True
        threading.Thread(target=self._run, name="on-air-webhook", daemon=True).start()

    def _run(self):
        while True:
            with self._lock:
                pending, self._pending = self._pending, None
                if pending is None:
                    self._busy = False
                    return
            request, tag = pending
            try:
                error = self._send(request)
            except Exception as exc:
                logger.exception("On Air webhook failed")
                error = str(exc) or type(exc).__name__
            try:
                self._on_result(tag, error)
            except Exception:
                logger.exception("On Air webhook result callback failed")


# ── The sign page ────────────────────────────────────────────────────────────

class _Handler(PageHandler):
    def do_GET(self):
        url = urllib.parse.urlsplit(self.path)
        owner: OnAirServer = self.server.owner
        if url.path == "/state":
            auth = self.headers.get("Authorization", "")
            if not owner.token_ok(auth[len("Bearer "):] if auth.startswith("Bearer ") else ""):
                owner.stats.note("refused")
                self._reply(403, b"This link doesn't work any more.")
                return
            try:
                after = int(urllib.parse.parse_qs(url.query).get("after", ["-1"])[0])
            except ValueError:
                after = -1
            state = owner.wait_state(after, 0 if self.command == "HEAD" else _POLL_WAIT_S)
            self._reply(200, json.dumps(state).encode(), "application/json")
            return
        page = _PAGES.get(url.path)
        if page is None:
            self._reply(404, b"Not found")
            return
        if url.path == "/":
            owner.stats.note("pages")
        self._reply(200, (_WEB_DIR / page[0]).read_bytes(), page[1])


class OnAirServer:
    """Serves the sign. publish(state) hands it what pages show (a dict, with "seq" added); a page's request for the
    state waits until it changes, so the sign updates as soon as it does."""

    def __init__(self, token: str, cert_path: Path, key_path: Path, port: int = ON_AIR_PORT, host: str = ""):
        self.token = token
        self._cert, self._key = cert_path, key_path
        self._want_port, self._host = port, host
        self._server: Optional[TlsServer] = None
        self._thread: Optional[threading.Thread] = None
        self._cond = threading.Condition()
        self._state: dict = {"seq": 0}
        self._stopping = False
        self.stats = ServerStats(0, port, label="On Air")

    @property
    def port(self) -> int:
        return self._server.server_address[1] if self._server else 0

    def url_for(self, host: str) -> str:
        return f"https://{host}:{self.port}/#{self.token}"

    def token_ok(self, token: str) -> bool:
        return bool(token) and hmac.compare_digest(token.encode(), self.token.encode())

    def set_token(self, token: str):
        """A new link: pages on the old one get refused from their next request on."""
        with self._cond:
            self.token = token
            self._cond.notify_all()

    def publish(self, state: dict):
        with self._cond:
            self._state = dict(state, seq=self._state["seq"] + 1)
            self._cond.notify_all()

    def wait_state(self, after: int, timeout: float) -> dict:
        """The state once its seq isn't after any more (right away for a page that hasn't seen it), or as it is after
        timeout. A new link or stop() also ends the wait."""
        deadline = time.monotonic() + timeout
        with self._cond:
            token = self.token
            while self._state["seq"] == after and not self._stopping and self.token == token:
                left = deadline - time.monotonic()
                if left <= 0:
                    break
                self._cond.wait(left)
            return dict(self._state)

    def start(self):
        ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        ctx.minimum_version = ssl.TLSVersion.TLSv1_2
        ctx.load_cert_chain(self._cert, self._key)
        fell_back = False
        try:
            self._server = TlsServer((self._host, self._want_port), _Handler, ctx, self)
        except OSError:
            self._server = TlsServer((self._host, 0), _Handler, ctx, self)  # taken: any free port
            fell_back = True
        self.stats = ServerStats(self.port, self._want_port, fell_back, label="On Air")
        if fell_back:
            logger.warning("On Air: port %d was taken, listening on %d instead", self._want_port, self.port)
        else:
            logger.info("On Air: listening on port %d", self.port)
        self._thread = threading.Thread(target=self._server.serve_forever, name="on-air", daemon=True)
        self._thread.start()

    def stop(self):
        with self._cond:
            self._stopping = True
            self._cond.notify_all()
        server, self._server = self._server, None
        if server is not None:
            server.shutdown()
            server.server_close()
