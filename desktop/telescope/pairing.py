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
from telescope.ip_utils import PairingAddress

PAIRING_PORT = 8765

# Bumped when the QR payload's shape changes. The phone rejects anything it
# doesn't recognise outright rather than guessing at a partial parse, so
# desktop and app are expected to ship together.
PAIRING_PROTOCOL_VERSION = 2


@dataclass(frozen=True)
class PairingOffer:
    """What to render as a QR code, plus the values needed to validate the
    phone's pairing POST against this specific session."""

    payload: str
    port: int
    nonce: str
    token: str
    candidates: List[PairingAddress] = field(default_factory=list)


@dataclass(frozen=True)
class PairingResult:
    name: str
    ips: List[str] = field(default_factory=list)
    token: str = ""
    # The address the pairing POST actually arrived from. By construction
    # this is an address of the phone that can reach this desktop right now,
    # over whichever path the phone found - so it's the one to stream to,
    # rather than guessing from the reported list. Empty, or absent from
    # [ips], when there's nothing useful to learn from it: USB pairing sees
    # the loopback end of the adb tunnel, not a phone address.
    source_ip: str = ""


class PairingServer:
    """Binds the pairing HTTP server for one dialog session and validates a
    single phone's pairing POST against it."""

    _MAX_BODY_BYTES = 16 * 1024
    # Cap on how much of an oversized body we'll drain before rejecting it.
    # Closing a socket while the kernel still has unread bytes queued sends
    # an abortive RST instead of a graceful close - Windows then discards
    # the client's receive buffer on that RST, so the client never gets to
    # read our 413 (ConnectionAbortedError / WinError 10053) even though we
    # already wrote it. Draining avoids that for any reasonably-sized
    # oversized body; a wildly large Content-Length just eats the RST, which
    # is fine since that's already an abuse case, not a real client.
    _DRAIN_LIMIT = 1024 * 1024

    def __init__(self, on_paired: Callable[[PairingResult], None]):
        self._on_paired = on_paired
        self._server: Optional[HTTPServer] = None
        self._server_thread: Optional[threading.Thread] = None
        self.offer: Optional[PairingOffer] = None

    def start(self, advertise: Optional[List[PairingAddress]] = None) -> Optional[PairingOffer]:
        """Binds the server and returns the offer to display as a QR code, or
        None if there's no network interface to pair over. Calling this
        again while already started is a no-op that returns the existing
        offer.

        [advertise], if given, is used verbatim instead of enumerating the
        machine's own interfaces - the USB-pairing path passes the loopback
        address here, since the phone reaches it through an adb reverse
        tunnel rather than the LAN (which may not exist, or may be shadowed
        by a VPN route, for a USB-only phone)."""
        if self._server is not None:
            return self.offer

        candidates = advertise if advertise is not None else ip_utils.get_pairing_addresses()
        if not candidates:
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
        drain_limit = self._DRAIN_LIMIT
        pair_path = f"/pair/{nonce}"
        on_paired = self._on_paired

        class _Handler(BaseHTTPRequestHandler):
            def do_GET(self):
                self.send_response(200)
                self.end_headers()
                self.wfile.write(b"Telescope pairing server")

            def _drain(self, length):
                # See _DRAIN_LIMIT above: any response path that returns
                # without reading a body the client already sent must drain
                # it first, or the close races an unread-data RST on Windows
                # that can wipe out the response we just wrote.
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
                    ips = list(dict.fromkeys(str(x).strip() for x in data.get("ips", [])))
                    echoed_token = str(data.get("token", ""))
                    # ips may legitimately be empty for a USB-only phone with
                    # no Wi-Fi at all - only reject if something reported is
                    # actually malformed.
                    if not name or not all(ip_utils.valid_ipv4(ip) for ip in ips):
                        raise ValueError("invalid pairing payload")
                    if not hmac.compare_digest(echoed_token, token):
                        raise ValueError("token mismatch")
                    source_ip = self.client_address[0] if self.client_address else ""
                    on_paired(PairingResult(
                        name=name, ips=ips, token=token, source_ip=source_ip,
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

        payload = json.dumps({
            "version": PAIRING_PROTOCOL_VERSION,
            "port": port,
            # Each candidate carries the interface it came from and what kind
            # of network that is, so the phone can route a LAN attempt over
            # its own Wi-Fi interface instead of whatever holds the default
            # route (a VPN, typically), and so the dialog can show the user
            # exactly which addresses it's waiting on.
            "candidates": [
                {"ip": c.ip, "interface": c.interface, "kind": c.kind} for c in candidates
            ],
            "nonce": nonce,
            "token": token,
        })
        self.offer = PairingOffer(
            payload=payload, port=port, nonce=nonce, token=token, candidates=list(candidates),
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
