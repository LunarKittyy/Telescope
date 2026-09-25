import logging
import sys
import threading

import pytest

from telescope import diagnostics
from telescope.diagnostics import RecentEvents, describe_exception, sanitize


@pytest.mark.parametrize("text", [
    "GET http://192.168.1.20:8765/v1/state failed",
    "phone at 10.0.0.5 didn't answer",
    "reached fe80::1c2:3aff:fe44:5566 over wifi",
    "Authorization: Bearer abc123def456",
    "token=Zx9_kq2-LmN8pQrS7tUvWxYz0aBcDeF",
    "pairing payload had nonce: 55aa55aa",
])
def test_sanitize_removes_addresses_and_secrets(text):
    out = sanitize(text)
    for secret in ("192.168", "10.0.0.5", "fe80", "abc123def456", "Zx9_kq2", "55aa55aa", "http://"):
        assert secret not in out


def test_sanitize_keeps_ordinary_text():
    text = "12:30:45 stream dropped after 3 frames at 1920x1080, version 0.5.0"
    assert sanitize(text) == text


def test_sanitize_hides_home(monkeypatch, tmp_path):
    home = tmp_path / "luna"
    monkeypatch.setattr(diagnostics.Path, "home", lambda: home)
    assert sanitize(f"can't read {home}/.config/telescope/config.json") == \
        "can't read ~/.config/telescope/config.json"


def test_repeats_collapse_into_a_count():
    events = RecentEvents()
    for _ in range(40):
        events.note("Status: Reconnecting")
    events.note("Status: Streaming")
    lines = events.lines()
    assert len(lines) == 2
    assert lines[0].endswith("Status: Reconnecting (x40)")


def test_bounded():
    events = RecentEvents(maxlen=5)
    for i in range(20):
        events.note(f"event {i}")
    lines = events.lines()
    assert len(lines) == 5
    assert lines[-1].endswith("event 19")


def test_long_messages_are_cut():
    events = RecentEvents()
    events.note("x " * 1000)
    assert len(events.lines()[0]) < 350


def test_only_warnings_and_up_are_kept():
    events = RecentEvents()
    log = logging.getLogger("telescope.test_diag")
    log.addHandler(events)
    log.propagate = False
    try:
        log.info("routine")
        log.warning("phone went quiet")
        try:
            raise ValueError("bad frame")
        except ValueError:
            log.exception("decode failed")
    finally:
        log.removeHandler(events)
        log.propagate = True
    lines = events.lines()
    assert len(lines) == 2
    assert "WARNING telescope.test_diag: phone went quiet" in lines[0]
    assert "ValueError: bad frame at test_diagnostics.py:" in lines[1]


def test_describe_exception_has_no_full_path():
    try:
        raise OSError("disk full")
    except OSError as exc:
        text = describe_exception(exc)
    assert text.startswith("OSError: disk full at test_diagnostics.py:")
    assert "/" not in text and "\\" not in text


def test_install_records_thread_crashes_and_keeps_old_hooks(monkeypatch):
    monkeypatch.setattr(diagnostics, "events", RecentEvents())
    seen = []
    monkeypatch.setattr(threading, "excepthook", lambda args: seen.append(args.exc_type))
    monkeypatch.setattr(sys, "excepthook", sys.excepthook)
    root = logging.getLogger()
    before = list(root.handlers)
    try:
        diagnostics.install()

        def boom():
            raise RuntimeError("worker died")
        t = threading.Thread(target=boom)
        t.start()
        t.join()
    finally:
        root.handlers[:] = before
    assert seen == [RuntimeError]
    assert "CRASH RuntimeError: worker died" in diagnostics.events.lines()[0]


def test_report(monkeypatch):
    monkeypatch.setattr(diagnostics, "events", RecentEvents())
    diagnostics.events.note("Status: Can't reach the phone at 192.168.1.4")
    text = diagnostics.report({"Connection": "usb", "Phone app": "0.5.0"}, qt_platform="wayland")
    lines = text.splitlines()
    assert lines[0].startswith("Telescope ")
    assert "Connection: usb" in lines
    assert "Phone app: 0.5.0" in lines
    assert any("Qt wayland" in line for line in lines)
    assert "Recent events:" in lines
    assert "192.168" not in text


def test_report_without_problems(monkeypatch):
    monkeypatch.setattr(diagnostics, "events", RecentEvents())
    assert diagnostics.report({}).splitlines()[-1] == "Recent events: none"
