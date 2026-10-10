"""The mic card's VB-Cable install: VB-Audio's pack fetched, checked and its own setup opened, with a fake
download and a fake ShellExecute (the real pack is checked by hand, see virtual_mic's pinned hashes)."""

import hashlib
import io
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


def test_a_failed_download_says_so(tmp_path):
    def offline(url, timeout):
        raise OSError("no route to host")
    setup, err = virtual_mic.download_vb_cable(tmp_path, offline)
    assert setup is None and "no route to host" in err


def test_the_setup_opens_as_admin_only_when_its_hash_still_matches(tmp_path, pinned):
    setup = tmp_path / virtual_mic.VB_CABLE_SETUP
    setup.write_bytes(SETUP)
    calls = []
    assert virtual_mic.run_vb_cable_setup(setup, lambda path, verb: calls.append((path, verb))) == ""
    assert calls == [(str(setup), "runas")]

    setup.write_bytes(b"swapped after the check")
    assert "changed" in virtual_mic.run_vb_cable_setup(setup, lambda *a: calls.append(a))
    assert len(calls) == 1


def test_saying_no_to_uac_is_reported_plainly(tmp_path, pinned):
    setup = tmp_path / virtual_mic.VB_CABLE_SETUP
    setup.write_bytes(SETUP)

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
    assert p._action_btn.text() == "Get VB-Cable"  # in case their setup didn't work out


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
