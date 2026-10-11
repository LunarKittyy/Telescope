"""HTTPS for Telescope's own downloads (updates, adb, VB-Cable), checked by the system's certificate store.

Python's own check only trusts root certificates already in Windows' store, and a fresh Windows only gets most of
them the first time Windows itself needs one, so a new PC failed with "unable to get local issuer certificate".
truststore asks Windows (or macOS, or the Linux CA bundle) to check the certificate, which fetches a missing root
like Edge would. Connections to a paired phone don't come through here (pinned_https.py).
"""

import ssl
import urllib.request

try:
    import truststore
except ImportError:  # a source checkout without it still works where the roots are already there
    truststore = None


def _context() -> ssl.SSLContext:
    if truststore is not None:
        return truststore.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    return ssl.create_default_context()


def urlopen(url, timeout: float):
    """urllib.request.urlopen with the system's certificate check; url can be a Request."""
    return urllib.request.urlopen(url, timeout=timeout, context=_context())


def certificate_failed(exc: BaseException) -> bool:
    """Whether a download failed because the server's certificate couldn't be checked, not because it's offline."""
    return isinstance(getattr(exc, "reason", exc), ssl.SSLCertVerificationError)
