import ssl
import urllib.error

import pytest

from telescope import web


def test_a_certificate_failure_is_told_apart_from_being_offline():
    cert = ssl.SSLCertVerificationError(1, "unable to get local issuer certificate")
    assert web.certificate_failed(urllib.error.URLError(cert))
    assert web.certificate_failed(cert)
    assert not web.certificate_failed(urllib.error.URLError(TimeoutError("timed out")))
    assert not web.certificate_failed(ConnectionResetError())


def test_the_context_checks_certificates_and_hostnames():
    ctx = web._context()
    assert ctx.verify_mode == ssl.CERT_REQUIRED and ctx.check_hostname


def test_it_uses_the_system_store_when_truststore_is_there():
    truststore = pytest.importorskip("truststore")
    assert isinstance(web._context(), truststore.SSLContext)
