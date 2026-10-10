"""The mic card's VB-Cable install: VB-Audio's pack fetched, checked and its own setup opened, with a fake
download and a fake ShellExecute (the real pack is checked by hand, see virtual_mic's pinned hashes)."""

import hashlib
import io
import os
import time
import urllib.error
import zipfile

import pytest

from telescope.platform import virtual_mic
from telescope.plugins import microphone
from telescope.plugins.microphone import WindowsMic

from test_microphone import _Ctrl, _plugin


def _pack(files: dict) -> bytes:
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w") as z:
        for name, data in files.items():
            z.writestr(name, data)
    return buf.getvalue()


SETUP = b"VB-Audio's setup"
GOOD = _pack({virtual_mic.VB_CABLE_SETUP: SETUP, "vbMmeCable64_win10.inf": b"inf", "readme.txt": b"hi"})


@pytest.fixture
def pinned(monkeypatch):
    """Pin the fake pack the way the real one is pinned."""
    monkeypatch.setattr(virtual_mic, "VB_CABLE_PACK_SHA256", hashlib.sha256(GOOD).hexdigest())
    monkeypatch.setattr(virtual_mic, "VB_CABLE_SETUP_SHA256", hashlib.sha256(SETUP).hexdigest())


def _serving(data: bytes, seen: list = None):
    def urlopen(url, timeout):
        if seen is not None:
            seen.append(url)
        return io.BytesIO(data)
    return urlopen


def test_the_pack_comes_from_vb_audio_and_unpacks_next_to_its_setup(tmp_path, pinned):
    seen = []
    setup, err = virtual_mic.download_vb_cable(tmp_path, _serving(GOOD, seen))
    assert err == ""
    assert seen == ["https://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack45.zip"]
    assert setup == tmp_path / "VBCABLE_Setup_x64.exe" and setup.read_bytes() == SETUP
    assert (tmp_path / "vbMmeCable64_win10.inf").exists()  # the setup needs the driver files beside it


def test_a_changed_pack_isnt_unpacked(tmp_path, pinned):
    setup, err = virtual_mic.download_vb_cable(tmp_path, _serving(GOOD + b"x"))
    assert setup is None and "changed" in err
    assert not (tmp_path / "VBCABLE_Setup_x64.exe").exists()


def test_paths_in_the_pack_stay_in_its_folder(tmp_path, monkeypatch):
    evil = _pack({virtual_mic.VB_CABLE_SETUP: SETUP, "../escape.exe": b"x", "sub/dir.dll": b"x"})
    monkeypatch.setattr(virtual_mic, "VB_CABLE_PACK_SHA256", hashlib.sha256(evil).hexdigest())
    folder = tmp_path / "pack"
    folder.mkdir()
    setup, err = virtual_mic.download_vb_cable(folder, _serving(evil))
    assert err == ""
    assert not (tmp_path / "escape.exe").exists() and not (folder / "sub").exists()


def test_a_failed_download_says_so_plainly(tmp_path):
    def offline(url, timeout):
        raise urllib.error.URLError(OSError("no route to host"))
    setup, err = virtual_mic.download_vb_cable(tmp_path, offline)
    assert setup is None and err == "Couldn't reach vb-audio.com. Check the internet connection and try again."


def test_a_download_far_bigger_than_the_pack_stops(tmp_path, pinned, monkeypatch):
    monkeypatch.setattr(virtual_mic, "_MAX_PACK", len(GOOD) - 1)
    setup, err = virtual_mic.download_vb_cable(tmp_path, _serving(GOOD))
    assert setup is None and "Couldn't download VB-Cable" in err


def _unpacked(tmp_path):
    setup, err = virtual_mic.download_vb_cable(tmp_path, _serving(GOOD))
    assert err == ""
    return setup


def test_the_setup_opens_as_admin_only_when_its_hash_still_matches(tmp_path, pinned):
    setup = _unpacked(tmp_path)
    calls = []
    assert virtual_mic.run_vb_cable_setup(setup, lambda path, verb: calls.append((path, verb))) == ""
    assert calls == [(str(setup), "runas")]

    setup.write_bytes(b"swapped after the check")
    assert "changed" in virtual_mic.run_vb_cable_setup(setup, lambda *a: calls.append(a))
    assert len(calls) == 1


@pytest.mark.parametrize("tamper", [
    lambda folder: (folder / "version.dll").write_bytes(b"planted"),  # Windows looks beside the exe for DLLs
    lambda folder: (folder / "vbMmeCable64_win10.inf").write_bytes(b"swapped"),
    lambda folder: (folder / "readme.txt").unlink(),
])
def test_nothing_runs_as_admin_from_a_folder_someone_changed(tmp_path, pinned, tamper):
    setup = _unpacked(tmp_path)
    tamper(tmp_path)
    calls = []
    assert "changed" in virtual_mic.run_vb_cable_setup(setup, lambda *a: calls.append(a))
    assert calls == []


def test_saying_no_to_uac_is_reported_plainly(tmp_path, pinned):
    setup = _unpacked(tmp_path)

    def declined(path, verb):
        e = OSError("The operation was canceled by the user")
        e.winerror = 1223
        raise e
    assert virtual_mic.run_vb_cable_setup(setup, declined) == "Windows didn't get permission to install VB-Cable."


class _NoCable:
    @staticmethod
    def query_devices():
        return [{"name": "Speakers (Realtek)", "max_output_channels": 2}]


def _windows_mic(monkeypatch, opened="", downloaded=("setup.exe", "")):
    backend = WindowsMic.__new__(WindowsMic)
    backend.setup_opened, backend._sd, backend._device = False, _NoCable, None
    calls = []
    monkeypatch.setattr(virtual_mic, "download_vb_cable", lambda: (calls.append("download") or downloaded))
    monkeypatch.setattr(virtual_mic, "run_vb_cable_setup", lambda s: (calls.append(("run", s)) or opened))
    return backend, calls


def test_the_card_offers_the_install_and_asks_first(qapp, monkeypatch):
    backend, calls = _windows_mic(monkeypatch)
    p = _plugin(backend)
    p._bus.phones_changed.emit(1)  # the card wakes up once a phone is paired
    p.set_config({"enabled": True})
    p.on_stream_start("url", _Ctrl())
    assert p._action_btn.text() == "Install VB-Cable"
    assert "VB-Audio" in p._status.text()

    monkeypatch.setattr(p, "_confirm_install", lambda: False)
    p._action_btn.click()
    assert calls == []  # nothing downloads without a yes

    monkeypatch.setattr(p, "_confirm_install", lambda: True)
    p._action_btn.click()
    assert calls == ["download", ("run", "setup.exe")]
    assert "restart the computer" in p._status.text()
    assert p._action_btn.text() == "Open setup again"  # in case it was closed without installing
    p._action_btn.click()
    assert calls == ["download", ("run", "setup.exe")] * 2


def test_a_failed_install_falls_back_to_the_link(qapp, monkeypatch):
    backend, _calls = _windows_mic(monkeypatch, downloaded=(None, "Couldn't download VB-Cable: offline"))
    p = _plugin(backend)
    p._bus.phones_changed.emit(1)  # the card wakes up once a phone is paired
    p.set_config({"enabled": True})
    p.on_stream_start("url", _Ctrl())
    monkeypatch.setattr(p, "_confirm_install", lambda: True)
    p._action_btn.click()
    assert p._status.text() == "Couldn't download VB-Cable: offline"
    assert p._action_btn.text() == "Get VB-Cable" and p._action_url == virtual_mic.VB_CABLE_URL
    assert backend.setup_opened is False


def test_a_failure_after_the_mic_was_switched_off_stays_quiet(qapp, monkeypatch):
    backend, _calls = _windows_mic(monkeypatch)
    p = _plugin(backend)
    p._bus.phones_changed.emit(1)
    p.set_config({"enabled": True})
    p._installing = True
    p._set_enabled(False)
    p._on_installed("Couldn't download VB-Cable: offline")
    assert p._status.text() == "" and not p._installing


def test_the_question_names_vb_audio_and_says_what_they_ask(qapp, monkeypatch):
    backend, _calls = _windows_mic(monkeypatch)
    p = _plugin(backend)
    shown = {}

    def fake_exec(box):
        shown["text"] = box.informativeText()
        shown["buttons"] = [b.text() for b in box.buttons()]
    monkeypatch.setattr(microphone.QMessageBox, "exec", fake_exec)
    assert p._confirm_install() is False  # closed without a choice
    assert virtual_mic.VB_CABLE_ORIGIN in shown["text"]
    assert virtual_mic.VB_CABLE_DONATIONWARE in shown["text"]
    assert "Download and install" in shown["buttons"]


def test_an_earlier_tries_folder_is_cleared_first(tmp_path, monkeypatch, pinned):
    monkeypatch.setattr(virtual_mic.tempfile, "gettempdir", lambda: str(tmp_path))
    monkeypatch.setattr(virtual_mic.tempfile, "tempdir", str(tmp_path))
    old = tmp_path / "telescope-vbcable-old"
    old.mkdir()
    (old / "VBCABLE_Setup_x64.exe").write_bytes(b"x")
    os.utime(old, (time.time() - 2 * 86400,) * 2)
    recent = tmp_path / "telescope-vbcable-recent"  # its setup may still be open
    recent.mkdir()
    keep = tmp_path / "something-else"
    keep.mkdir()
    setup, err = virtual_mic.download_vb_cable(urlopen=_serving(GOOD))
    assert err == "" and setup.parent.parent == tmp_path
    assert not old.exists() and keep.exists() and recent.exists()


def test_a_hidden_file_in_the_pack_isn_t_unpacked_and_one_planted_later_blocks_the_setup(tmp_path, monkeypatch):
    pack = _pack({virtual_mic.VB_CABLE_SETUP: SETUP, "readme.txt": b"hi", ".hidden": b"x"})
    monkeypatch.setattr(virtual_mic, "VB_CABLE_PACK_SHA256", hashlib.sha256(pack).hexdigest())
    monkeypatch.setattr(virtual_mic, "VB_CABLE_SETUP_SHA256", hashlib.sha256(SETUP).hexdigest())
    setup, err = virtual_mic.download_vb_cable(tmp_path, _serving(pack))
    assert err == "" and not (tmp_path / ".hidden").exists()
    calls = []
    assert virtual_mic.run_vb_cable_setup(setup, lambda *a: calls.append(a)) == ""
    (tmp_path / ".hidden").write_bytes(b"planted")
    assert "changed" in virtual_mic.run_vb_cable_setup(setup, lambda *a: calls.append(a))
    assert len(calls) == 1
