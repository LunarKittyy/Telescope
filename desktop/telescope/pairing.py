"""Qt-free QR pairing HTTP server.

Runs the one-shot, nonce-gated pairing handshake used by the "Pair via QR
code" dialog: bind a port, mint a nonce and bearer token, wait for the phone's
POST at /pair/{nonce} echoing the token back, and hand the caller a
PairingResult. No PyQt import here - the dialog layer (plugins/connection.py)
owns rendering the QR code and bridging the result onto a Qt signal.
"""

import hmac
import json
import secrets
import socket
import threading
from dataclasses import dataclass, field
from http.server import BaseHTTPRequestHandler, HTTPServer
from typing import Callable, List, Optional

from telescope import ip_utils

PAIRING_PORT = 8765


@dataclass(frozen=True)
class PairingOffer:
    """What to render as a QR code, plus the values needed to validate the
    phone's pairing POST against this specific session."""

    payload: str
    port: int
    nonce: str
    token: str


@dataclass(frozen=True)
class PairingResult:
    name: str
    ips: List[str] = field(default_factory=list)
    token: str = ""


class PairingServer:
    """Binds the pairing HTTP server for one dialog session and validates a
    single phone's pairing POST against it."""

    _MAX_BODY_BYTES = 16 * 1024

    def __init__(self, on_paired: Callable[[PairingResult], None]):
        self._on_paired = on_paired
        self._server: Optional[HTTPServer] = None
        self._server_thread: Optional[threading.Thread] = None
        self.offer: Optional[PairingOffer] = None

    def start(self) -> Optional[PairingOffer]:
        """Binds the server and returns the offer to display as a QR code, or
        None if there's no network interface to pair over. Calling this
        again while already started is a no-op that returns the existing
        offer."""
        if self._server is not None:
            return self.offer

        local_ips = ip_utils.get_local_ips()
        if not local_ips:
            return None

        # Try to bind the fixed pairing port; fall back to random if in use.
        port = PAIRING_PORT
        try:
            test = socket.socket()
            test.bind(("", port))
            test.close()
        except OSError:
            with socket.socket() as s:
                s.bind(("", 0))
                port = s.getsockname()[1]

        # A fresh nonce per pairing session - the POST path must include it,
        # so a LAN peer that doesn't already know it (i.e. hasn't scanned the
        # current QR code) can't add itself as a paired device.
        nonce = secrets.token_urlsafe(16)
        # The bearer token the phone will require on every /v1/* request once
        # paired. Embedded in the QR code and echoed back in the pairing POST
        # body as a second, defense-in-depth confirmation (on top of the
        # nonce) that this POST came from a phone that actually read the
        # current QR code.
        token = secrets.token_urlsafe(32)
        max_body = self._MAX_BODY_BYTES
        pair_path = f"/pair/{nonce}"
        on_paired = self._on_paired

        class _Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                self.send_response(200)
                self.end_headers()
                self.wfile.write(b"Telescope pairing server")

            def do_POST(self):
                if self.path != pair_path:
                    self.send_response(404); self.end_headers(); return
                length_hdr = self.headers.get("Content-Length")
                try:
                    length = int(length_hdr)
                except (TypeError, ValueError):
                    self.send_response(411); self.end_headers(); return
                if length < 0 or length > max_body:
                    self.send_response(413); self.end_headers(); return
                body = self.rfile.read(length)
                try:
                    data = json.loads(body)
                    name = str(data.get("name", "Phone")).strip()
                    ips = list(dict.fromkeys(str(x).strip() for x in data.get("ips", [])))
                    echoed_token = str(data.get("token", ""))
                    if not name or not ips or not all(ip_utils.valid_ipv4(ip) for ip in ips):
                        raise ValueError("invalid pairing payload")
                    if not hmac.compare_digest(echoed_token, token):
                        raise ValueError("token mismatch")
                    on_paired(PairingResult(name=name, ips=ips, token=token))
                    self.send_response(200)
                    self.end_headers()
                    self.wfile.write(b"OK")
                except Exception:
                    self.send_response(400); self.end_headers()

            def log_message(self, *args):
                pass

        self._server = HTTPServer(("", port), _Handler)
        self._server_thread = threading.Thread(target=self._server.serve_forever, daemon=True)
        self._server_thread.start()

        payload = json.dumps({"version": 1, "port": port, "ips": local_ips, "nonce": nonce, "token": token})
        self.offer = PairingOffer(payload=payload, port=port, nonce=nonce, token=token)
        return self.offer

    def stop(self):
        if self._server is None:
            return
        server, thread = self._server, self._server_thread
        self._server = None
        self._server_thread = None
        self.offer = None

        def _shutdown():
            server.shutdown()
            if thread:
                thread.join(timeout=5)
            server.server_close()

        threading.Thread(target=_shutdown, daemon=True).start()
