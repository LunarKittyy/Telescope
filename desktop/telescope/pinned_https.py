"""HTTPS to a paired phone, trusting only the certificate fingerprint it gave at pairing. Checked before any request byte is sent."""

import functools
import hashlib
import hmac
import http.client
import ssl
import urllib.error
import urllib.request
from dataclasses import dataclass, field
from typing import Optional


class PinMismatch(ssl.SSLError):
    """Whatever answered isn't the phone that paired: its certificate has another fingerprint."""


class HandshakeFailed(OSError):
    """The TCP connection opened but TLS didn't: something is listening that doesn't speak it, like an app from before TLS."""


def fingerprint(der: bytes) -> str:
    return hashlib.sha256(der).hexdigest()


def is_fingerprint(value) -> bool:
    return isinstance(value, str) and len(value) == 64 and all(c in "0123456789abcdef" for c in value)


def _client_context() -> ssl.SSLContext:
    # No CA or hostname checks: the phone's certificate is self-signed, and the pin replaces both.
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    ctx.minimum_version = ssl.TLSVersion.TLSv1_2
    return ctx


class _PinnedConnection(http.client.HTTPSConnection):
    def __init__(self, *args, pin: str, **kwargs):
        super().__init__(*args, **kwargs)
        self._pin = pin

    def connect(self):
        http.client.HTTPConnection.connect(self)  # plain TCP first, so a failed handshake can be told from nobody there
        try:
            self.sock = self._context.wrap_socket(self.sock, server_hostname=self.host)
        except OSError as exc:
            self.sock.close()
            self.sock = None
            raise HandshakeFailed(f"TLS handshake failed: {exc}") from exc
        der = self.sock.getpeercert(binary_form=True) or b""
        if not hmac.compare_digest(fingerprint(der), self._pin):
            self.sock.close()
            self.sock = None
            raise PinMismatch("the phone's certificate doesn't match the one it paired with")


class _PinnedHandler(urllib.request.HTTPSHandler):
    def __init__(self, pin: str):
        super().__init__(context=_client_context())
        self._pin = pin

    def https_open(self, req):
        return self.do_open(functools.partial(_PinnedConnection, pin=self._pin), req, context=self._context)


class _NoRedirects(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


@dataclass
class PhoneAuth:
    """A paired phone's bearer token plus its certificate pin; opens authenticated requests to it."""

    token: str
    pin: str = ""
    _opener: Optional[urllib.request.OpenerDirector] = field(default=None, init=False, compare=False, repr=False)

    def headers(self) -> dict:
        return {"Authorization": f"Bearer {self.token}"}

    def open(self, req, timeout: float):
        """Like urlopen. https needs the pin; plain http only when there's no pin (tests' local servers)."""
        url = req.full_url if isinstance(req, urllib.request.Request) else req
        if url.startswith("https://"):
            if not is_fingerprint(self.pin):
                raise urllib.error.URLError(PinMismatch("no certificate pin for this phone"))
            return self._pinned_opener().open(req, timeout=timeout)
        if url.startswith("http://") and not self.pin:
            return urllib.request.urlopen(req, timeout=timeout)
        raise urllib.error.URLError("refusing to send a pinned phone's token without TLS")

    def _pinned_opener(self) -> urllib.request.OpenerDirector:
        if self._opener is None:
            # No proxies: a phone is on the LAN or behind adb, and a proxy would only see a TLS stream anyway.
            self._opener = urllib.request.build_opener(
                urllib.request.ProxyHandler({}), _NoRedirects(), _PinnedHandler(self.pin))
        return self._opener


def handshake_failed(exc: BaseException) -> bool:
    """Whether a failed open reached a listener that doesn't speak TLS."""
    return isinstance(exc, HandshakeFailed) or isinstance(getattr(exc, "reason", None), HandshakeFailed)


def pin_rejected(exc: BaseException) -> bool:
    """Whether a failed open was the pin check: something answered, but not the paired phone."""
    return isinstance(exc, PinMismatch) or isinstance(getattr(exc, "reason", None), PinMismatch)
