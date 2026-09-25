"""Qt-free QR pairing HTTP server: binds a port, mints a nonce and bearer token, waits for the phone's POST at /pair/{nonce} echoing the token back, and hands the caller a PairingResult. No PyQt import - the dialog layer (plugins/connection.py) owns rendering and bridging the result onto a Qt signal."""

import hmac
import json
import secrets
import socket
import threading
from dataclasses import dataclass, field
from http.server import BaseHTTPRequestHandler, HTTPServer
from typing import Callable, List, Optional

from telescope import ip_utils
from telescope.ip_utils import PairingAddress

PAIRING_PORT = 8765

# Protocol version; desktop and app must ship together.
# 3: the offer names this computer (computer_id/computer_name), the phone answers with its phone_id.
PAIRING_PROTOCOL_VERSION = 3


@dataclass(frozen=True)
class PairingOffer:
    """What to render as a QR code, plus the values needed to validate the phone's pairing POST against this specific session."""

    payload: str
    port: int
    nonce: str
    token: str
    candidates: List[PairingAddress] = field(default_factory=list)
    # Same session, advertising only 127.0.0.1: what the adb broadcast carries (reached via adb reverse).
    usb_payload: str = ""


@dataclass(frozen=True)
class PairingResult:
    name: str
    ips: List[str] = field(default_factory=list)
    token: str = ""
    # Source of successful pairing POST (preferred over reported IPs).
    source_ip: str = ""
    phone_id: str = ""


class PairingServer:
    """Binds the pairing HTTP server for one dialog session and validates a single phone's pairing POST against it."""

    _MAX_BODY_BYTES = 16 * 1024
    # Drain limit: avoid RST on Windows when closing with unread bytes.
    _DRAIN_LIMIT = 1024 * 1024

    def __init__(self, on_paired: Callable[[PairingResult], None],
                 computer_id: str = "", computer_name: str = ""):
        self._on_paired = on_paired
        self._computer_id = computer_id
        self._computer_name = computer_name
        self._server: Optional[HTTPServer] = None
        self._server_thread: Optional[threading.Thread] = None
        self.offer: Optional[PairingOffer] = None

    def start(self, advertise: Optional[List[PairingAddress]] = None) -> PairingOffer:
        """Binds the server and returns the offer (idempotent). candidates is empty when this computer has
        no usable network; usb_payload still works then, over adb reverse. [advertise] replaces interface
        enumeration (tests)."""
        if self._server is not None:
            return self.offer

        candidates = advertise if advertise is not None else ip_utils.get_pairing_addresses()

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
        drain_limit = self._DRAIN_LIMIT
        pair_path = f"/pair/{nonce}"
        on_paired = self._on_paired

        class _Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                self.send_response(200)
                self.end_headers()
                self.wfile.write(b"Telescope pairing server")

            # Must read the body before closing an error response - Windows can RST a socket that still has unread received bytes, dropping the response the client was reading.
            def _drain(self, length):
                if length is not None and 0 <= length <= drain_limit:
                    try:
                        self.rfile.read(length)
                    except Exception:
                        pass

            def do_POST(self):
                length_hdr = self.headers.get("Content-Length")
                try:
                    length = int(length_hdr)
                except (TypeError, ValueError):
                    length = None

                if self.path != pair_path:
                    self._drain(length)
                    self.send_response(404); self.end_headers(); return
                if length is None:
                    self.send_response(411); self.end_headers(); return
                if length < 0 or length > max_body:
                    self._drain(length)
                    self.send_response(413); self.end_headers(); return
                body = self.rfile.read(length)
                try:
                    data = json.loads(body)
                    name = str(data.get("name", "Phone")).strip()
                    phone_id = str(data.get("phone_id", "")).strip()
                    ips = list(dict.fromkeys(str(x).strip() for x in data.get("ips", [])))
                    echoed_token = str(data.get("token", ""))
                    if not name or not phone_id or not all(ip_utils.valid_ipv4(ip) for ip in ips):
                        raise ValueError("invalid pairing payload")
                    if not hmac.compare_digest(echoed_token, token):
                        raise ValueError("token mismatch")
                    source_ip = self.client_address[0] if self.client_address else ""
                    on_paired(PairingResult(
                        name=name, ips=ips, token=token, source_ip=source_ip, phone_id=phone_id,
                    ))
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

        def payload_for(addrs):
            return json.dumps({
                "version": PAIRING_PROTOCOL_VERSION,
                "port": port,
                "candidates": [{"ip": c.ip, "interface": c.interface, "kind": c.kind} for c in addrs],
                "nonce": nonce,
                "token": token,
                "computer_id": self._computer_id,
                "computer_name": self._computer_name,
            })

        self.offer = PairingOffer(
            payload=payload_for(candidates), port=port, nonce=nonce, token=token,
            candidates=list(candidates),
            usb_payload=payload_for([PairingAddress(ip="127.0.0.1", interface="USB (adb)", kind="other")]),
        )
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
