import logging
import os
import sys
import threading

import pytest

from telescope import diagnostics
from telescope.diagnostics import EventLog, describe_exception, link_log, sanitize


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
    events = EventLog()
    for _ in range(40):
        events.note("Status: Reconnecting")
    assert events.lines()[-1] == "    (repeated 39 more times)"
    events.note("Status: Streaming")
    lines = events.lines()
    assert len(lines) == 3
    assert lines[0].endswith("NOTE Status: Reconnecting")
    assert lines[1] == "    (repeated 39 more times)"
    assert lines[2].endswith("NOTE Status: Streaming")


def test_writes_to_the_file_and_reads_it_back_after_a_restart(tmp_path):
    path = tmp_path / "logs" / "telescope.log"
    first = EventLog(path)
    first.note("Status: Streaming")
    first.add("CRASH", "RuntimeError: boom")
    first.close()

    second = EventLog(path)
    second.add("START", "Telescope 0.5.0")
    lines = second.lines()
    assert [line.split(" ", 2)[2] for line in lines] == [
        "NOTE Status: Streaming", "CRASH RuntimeError: boom", "START Telescope 0.5.0"]
    assert "boom" in path.read_text()
    second.close()


def test_rotates_and_keeps_one_previous_file(tmp_path):
    path = tmp_path / "telescope.log"
    events = EventLog(path, max_bytes=2000)
    for i in range(200):
        events.note(f"event {i}")
    events.close()
    assert path.stat().st_size < 2200
    assert (tmp_path / "telescope.log.1").exists()
    assert not (tmp_path / "telescope.log.2").exists()
    reread = EventLog(path)
    assert reread.lines()[-1].endswith("event 199")
    reread.close()


def test_report_is_bounded(tmp_path):
    events = EventLog(tmp_path / "telescope.log")
    for i in range(1000):
        events.note(f"event {i} " + "x" * 1000)
    lines = events.lines()
    assert len(lines) == diagnostics.REPORT_LINES
    assert sum(len(line) + 1 for line in lines) < 65536  # fits a GitHub issue field
    events.close()


def test_without_a_file_it_keeps_the_last_lines_in_memory():
    events = EventLog()
    for i in range(500):
        events.note(f"event {i}")
    lines = events.lines()
    assert len(lines) == diagnostics.REPORT_LINES
    assert lines[-1].endswith("event 499")


def test_file_lines_are_sanitized(tmp_path):
    path = tmp_path / "telescope.log"
    events = EventLog(path)
    events.note("Can't reach http://192.168.1.4:8766/v1/ping")
    events.close()
    assert "192.168" not in path.read_text()


def test_link_log(tmp_path):
    target = tmp_path / "tmp" / "telescope.log"
    target.parent.mkdir()
    target.write_text("")
    link = tmp_path / "app" / "telescope.log"
    link.parent.mkdir()
    link_log(link, target)
    if not link.is_symlink():
        pytest.skip("no symlink rights here")
    assert link.resolve() == target.resolve()
    link_log(link, tmp_path / "elsewhere.log")  # the temp folder moved: repoint
    assert link.is_symlink() and os.readlink(link) == str(tmp_path / "elsewhere.log")


def test_link_log_leaves_a_real_file_alone(tmp_path):
    link = tmp_path / "telescope.log"
    link.write_text("mine")
    link_log(link, tmp_path / "other.log")
    assert link.read_text() == "mine"


def test_keeps_telescope_info_but_only_warnings_from_libraries():
    events = EventLog()
    ours = logging.getLogger("telescope.test_diag")
    theirs = logging.getLogger("zeroconf.test_diag")
    for log in (ours, theirs):
        log.addHandler(events)
        log.setLevel(logging.DEBUG)
        log.propagate = False
    try:
        ours.debug("too detailed")
        ours.info("migrated config")
        theirs.info("library chatter")
        theirs.warning("library trouble")
        try:
            raise ValueError("bad frame")
        except ValueError:
            ours.exception("decode failed")
    finally:
        for log in (ours, theirs):
            log.removeHandler(events)
            log.setLevel(logging.NOTSET)
            log.propagate = True
    lines = events.lines()
    assert len(lines) == 3
    assert "INFO telescope.test_diag: migrated config" in lines[0]
    assert "WARNING zeroconf.test_diag: library trouble" in lines[1]
    assert "ValueError: bad frame at test_diagnostics.py:" in lines[2]


def test_describe_exception_has_no_full_path():
    try:
        raise OSError("disk full")
    except OSError as exc:
        text = describe_exception(exc)
    assert text.startswith("OSError: disk full at test_diagnostics.py:")
    assert "/" not in text and "\\" not in text


def test_install_records_thread_crashes_and_keeps_old_hooks(monkeypatch, tmp_path):
    monkeypatch.setattr(diagnostics, "events", EventLog())
    monkeypatch.setattr(diagnostics, "log_dir", lambda: tmp_path / "tmp")
    seen = []
    monkeypatch.setattr(threading, "excepthook", lambda args: seen.append(args.exc_type))
    monkeypatch.setattr(sys, "excepthook", sys.excepthook)
    root = logging.getLogger()
    before, level = list(root.handlers), root.level
    try:
        diagnostics.install(tmp_path / "app")

        def boom():
            raise RuntimeError("worker died")
        t = threading.Thread(target=boom)
        t.start()
        t.join()
    finally:
        root.handlers[:] = before
        root.setLevel(level)
        diagnostics.events.close()
    assert seen == [RuntimeError]
    log = (tmp_path / "tmp" / "telescope.log").read_text()
    assert "START Telescope" in log
    assert "CRASH RuntimeError: worker died" in log
    link = tmp_path / "app" / "telescope.log"
    assert link.is_symlink() or not link.exists()


def test_report(monkeypatch):
    monkeypatch.setattr(diagnostics, "events", EventLog())
    diagnostics.events.note("Status: Can't reach the phone at 192.168.1.4")
    text = diagnostics.report({"Connection": "usb", "Phone app": "0.5.0"}, qt_platform="wayland")
    lines = text.splitlines()
    assert lines[0].startswith("Telescope ")
    assert "Connection: usb" in lines
    assert "Phone app: 0.5.0" in lines
    assert any("Qt wayland" in line for line in lines)
    assert f"Log (last {diagnostics.REPORT_LINES} lines):" in lines
    assert "192.168" not in text


def test_report_without_problems(monkeypatch):
    monkeypatch.setattr(diagnostics, "events", EventLog())
    assert diagnostics.report({}).splitlines()[-1] == "Log: empty"
