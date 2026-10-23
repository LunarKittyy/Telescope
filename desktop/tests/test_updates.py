"""Update check, download and install, with a fake network and temporary install folders."""

import contextlib
import hashlib
import io
import json
import os
import socket
import struct
import tarfile
import threading
import time
import urllib.error
import urllib.request
import zipfile

import pytest

import update_guard
from telescope import updates, version
from telescope.updates import Asset, UpdateError


def _manifest(build=120, channel="nightly", assets=None, **extra):
    data = {
        "schema": 1, "version": "0.6.0", "build": build, "channel": channel,
        "versionName": f"0.6.0-{channel}.{build}", "commit": "abc", "protocol": 2,
        "notes": "https://example.invalid/notes",
        "assets": assets if assets is not None else [
            {"name": "Telescope-linux.tar.gz", "url": "https://h/x.tar.gz", "sha256": "a" * 64, "size": 1}],
    }
    data.update(extra)
    return json.dumps(data).encode()


class _Resp(io.BytesIO):
    def __enter__(self):
        return self

    def __exit__(self, *exc):
        return False


def _opener(body=None, error=None):
    seen = []

    def opener(url, timeout):
        seen.append(url)
        if error is not None:
            raise error
        return _Resp(body)
    opener.seen = seen
    return opener


# ── Manifest ─────────────────────────────────────────────────────────────────

def test_parses_a_manifest_and_names_its_version():
    m = updates.parse_manifest(_manifest())
    assert m.build == 120 and m.display_version == "0.6.0 nightly 120"
    assert m.assets["Telescope-linux.tar.gz"].size == 1
    assert updates.parse_manifest(_manifest(channel="stable")).display_version == "0.6.0"


@pytest.mark.parametrize("raw", [
    b"not json",
    b"[]",
    _manifest(assets=[{"name": "x", "url": "http://insecure/x", "sha256": "a" * 64, "size": 1}]),
    _manifest(assets=[{"name": "x", "url": "https://h/x", "sha256": "short", "size": 1}]),
    json.dumps({"version": "1"}).encode(),
])
def test_rejects_a_manifest_it_cant_trust(raw):
    with pytest.raises(UpdateError):
        updates.parse_manifest(raw)


def test_fetch_uses_the_channel_url_and_treats_404_as_no_release():
    opener = _opener(_manifest())
    assert updates.fetch_manifest("nightly", opener).build == 120
    assert opener.seen == [updates.MANIFEST_URLS["nightly"]]
    assert "releases/latest/download" in updates.MANIFEST_URLS["stable"]
    missing = urllib.error.HTTPError("u", 404, "nf", {}, None)
    assert updates.fetch_manifest("stable", _opener(error=missing)) is None
    with pytest.raises(UpdateError, match="internet"):
        updates.fetch_manifest("stable", _opener(error=urllib.error.URLError("down")))


def test_newer_compares_build_numbers_and_never_offers_a_dev_build_an_update():
    m = updates.parse_manifest(_manifest(build=120))
    assert updates.is_newer(m, build=100)
    assert not updates.is_newer(m, build=120)
    assert not updates.is_newer(m, build=130)  # switching to an older channel doesn't downgrade
    assert not updates.is_newer(m, build=0)    # source checkout
    assert not updates.is_newer(None, build=100)


def test_default_channel_follows_the_build(monkeypatch):
    monkeypatch.setattr(version, "CHANNEL", "nightly")
    assert updates.default_channel() == "nightly"
    monkeypatch.setattr(version, "CHANNEL", "dev")
    assert updates.default_channel() == "stable"


# ── Download ─────────────────────────────────────────────────────────────────

def _asset_for(payload, name="Telescope-linux.tar.gz", size=None):
    return Asset(name, "https://h/" + name, hashlib.sha256(payload).hexdigest(),
                 len(payload) if size is None else size)


def test_download_verifies_and_reports_progress(tmp_path):
    payload = b"x" * 600_000
    seen = []
    path = updates.download(_asset_for(payload), tmp_path, lambda d, t: seen.append(d), opener=_opener(payload))
    assert path.read_bytes() == payload and seen[-1] == len(payload)
    assert not (tmp_path / "Telescope-linux.tar.gz.part").exists()


@pytest.mark.parametrize("served,asset_payload,size,message", [
    (b"tampered", b"original", None, "checksum"),
    (b"much longer than promised", b"short", 5, "bigger"),
])
def test_download_refuses_what_doesnt_match(tmp_path, served, asset_payload, size, message):
    with pytest.raises(UpdateError, match=message):
        updates.download(_asset_for(asset_payload, size=size), tmp_path, opener=_opener(served))
    assert list(tmp_path.iterdir()) == []


def test_download_can_be_cancelled(tmp_path):
    with pytest.raises(UpdateError, match="Cancelled"):
        updates.download(_asset_for(b"abc"), tmp_path, cancelled=lambda: True, opener=_opener(b"abc"))


@pytest.mark.parametrize("old_payload", [b"old", b"old build much larger than new", b"older"])
def test_download_update_recovers_when_a_nightly_is_replaced(tmp_path, monkeypatch, old_payload):
    payload = b"newer"
    old_asset, new_asset = _asset_for(old_payload), _asset_for(payload)
    old = updates.Manifest("1", 120, "nightly", "", "", 2, {old_asset.name: old_asset})
    latest = updates.Manifest("1", 121, "nightly", "", "", 2, {new_asset.name: new_asset})
    opener = _opener(payload)
    download = updates.download
    monkeypatch.setattr(updates, "download", lambda *a, **kw: download(*a, **kw, opener=opener))
    monkeypatch.setattr(updates, "fetch_manifest", lambda channel: latest)
    monkeypatch.setattr(updates, "was_rolled_back", lambda manifest: False)
    selected, path = updates.download_update(old, old_asset, tmp_path)
    assert selected == latest and path.read_bytes() == payload
    assert len(opener.seen) == 2
    assert sorted(p.name for p in tmp_path.iterdir()) == [new_asset.name]


@pytest.mark.parametrize("replacement", ["same", "older", "missing", "channel", "rolled back", "corrupt"])
def test_download_update_does_not_accept_an_invalid_replacement(tmp_path, monkeypatch, replacement):
    asset = _asset_for(b"old")
    old = updates.Manifest("1", 120, "nightly", "", "", 2, {asset.name: asset})
    latest = updates.Manifest("1", 120 if replacement == "same" else 119 if replacement == "older" else 121,
                              "stable" if replacement == "channel" else "nightly", "", "", 2,
                              {} if replacement == "missing" else old.assets)
    seen = []

    def fail(*args, **kwargs):
        seen.append(args[0])
        raise UpdateError("checksum mismatch")

    monkeypatch.setattr(updates, "download", fail)
    monkeypatch.setattr(updates, "fetch_manifest", lambda channel: latest)
    monkeypatch.setattr(updates, "was_rolled_back", lambda manifest: replacement == "rolled back")
    with pytest.raises(UpdateError, match="checksum"):
        updates.download_update(old, asset, tmp_path)
    assert len(seen) == (2 if replacement == "corrupt" else 1)


def test_download_update_does_not_refresh_after_success_or_cancellation(tmp_path, monkeypatch):
    asset = _asset_for(b"old")
    old = updates.Manifest("1", 120, "nightly", "", "", 2, {asset.name: asset})
    monkeypatch.setattr(updates, "fetch_manifest", lambda channel: pytest.fail("unexpected refresh"))
    download = updates.download
    monkeypatch.setattr(updates, "download", lambda *a, **kw: download(*a, **kw, opener=_opener(b"old")))
    assert updates.download_update(old, asset, tmp_path)[0] == old
    with pytest.raises(UpdateError, match="Cancelled"):
        updates.download_update(old, asset, tmp_path, cancelled=lambda: True)


# ── Offline ──────────────────────────────────────────────────────────────────
# A wifi can reach the phone and nothing else. Every way a connection with no internet behind it fails has to
# end in an UpdateError the UI can show, quickly, with nothing left behind.

def _no_answer(conn, stopped):
    stopped.wait(5)


def _stalls_mid_body(conn, stopped):
    conn.recv(4096)
    conn.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n{")
    stopped.wait(5)


def _cut_off(conn, _stopped):
    conn.recv(4096)
    conn.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\n{")


def _not_http(conn, _stopped):
    conn.recv(4096)
    conn.sendall(b"hello\r\n\r\n")


def _reset(conn, _stopped):
    conn.recv(4096)
    conn.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, struct.pack("ii", 1, 0))


def _login_page(conn, _stopped):
    conn.recv(4096)
    body = b"<html>Log in to use this wifi</html>"
    conn.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: %d\r\n\r\n%s" % (len(body), body))


@pytest.fixture
def offline(monkeypatch):
    """A URL that behaves like a network without internet, and an opener for it."""
    monkeypatch.setattr(updates, "REQUEST_TIMEOUT", 0.5)
    sockets = []
    threads = []
    errors = []
    stopped = threading.Event()

    def make(behaviour):
        if behaviour == "refused":
            s = socket.socket()
            s.bind(("127.0.0.1", 0))
            url = f"http://127.0.0.1:{s.getsockname()[1]}/x"
            s.close()
        elif behaviour == "no dns":
            url = "http://telescope-update-test.invalid/x"
        else:
            s = socket.socket()
            s.bind(("127.0.0.1", 0))
            s.listen()
            s.settimeout(5)
            sockets.append(s)

            def serve():
                try:
                    conn, _ = s.accept()
                    with conn:
                        conn.settimeout(5)
                        behaviour(conn, stopped)
                except ConnectionError:
                    pass
                except Exception as exc:
                    errors.append(exc)
            thread = threading.Thread(target=serve, daemon=True)
            threads.append(thread)
            thread.start()
            url = f"http://127.0.0.1:{s.getsockname()[1]}/x"
        return lambda _url, timeout: urllib.request.urlopen(url, timeout=timeout)
    yield make
    stopped.set()
    for thread in threads:
        thread.join(6)
    for s in sockets:
        s.close()
    assert not any(thread.is_alive() for thread in threads)
    assert errors == []


_OFFLINE = ["refused", "no dns", _no_answer, _stalls_mid_body, _cut_off, _not_http, _reset, _login_page]


@pytest.mark.parametrize("behaviour", _OFFLINE)
def test_a_check_without_internet_fails_with_a_message(offline, behaviour):
    started = time.monotonic()
    with pytest.raises(UpdateError):
        updates.fetch_manifest("stable", offline(behaviour))
    assert time.monotonic() - started < 3


@pytest.mark.parametrize("behaviour", _OFFLINE)
def test_a_download_without_internet_fails_and_leaves_nothing(offline, behaviour, tmp_path):
    started = time.monotonic()
    with pytest.raises(UpdateError) as err:
        updates.download(_asset_for(b"z" * 1000), tmp_path, opener=offline(behaviour))
    assert time.monotonic() - started < 3
    assert list(tmp_path.iterdir()) == []
    if behaviour is not _login_page:
        assert "internet connection" in str(err.value)


# ── Install: Windows ─────────────────────────────────────────────────────────

def _zip(path, files):
    with zipfile.ZipFile(path, "w") as z:
        for name, data in files.items():
            z.writestr(name, data)
    return path


def test_windows_install_renames_the_running_exe_and_replaces_changed_files(tmp_path):
    app = tmp_path / "app"
    (app / "unitycapture").mkdir(parents=True)
    (app / "TelescopeDesktop.exe").write_bytes(b"old exe")
    (app / "unitycapture" / "UnityCaptureFilter64.dll").write_bytes(b"same dll")
    (app / "Telescope.apk").write_bytes(b"old apk")
    archive = _zip(tmp_path / "Telescope-windows-portable.zip", {
        "TelescopeDesktop.exe": b"new exe", "unitycapture/UnityCaptureFilter64.dll": b"same dll",
        "Telescope.apk": b"new apk",
    })
    result = updates.install_windows(archive, app)
    assert (app / "TelescopeDesktop.exe").read_bytes() == b"new exe"
    assert (app / "TelescopeDesktop.old.exe").read_bytes() == b"old exe"
    assert (app / "Telescope.apk").read_bytes() == b"new apk"
    assert result.relaunch == [str(app / "TelescopeDesktop.exe"), "--after-update"]
    assert not (app / updates.STAGING_DIR).exists()

    assert (app / "TelescopeDesktop.old.exe").exists()  # kept until the new version has started once
    update_guard.confirm(app)
    updates.clean_up_after_update(app)
    assert not (app / "TelescopeDesktop.old.exe").exists()


def test_the_first_folder_build_installs_over_the_single_exe(tmp_path):
    # What the updater in today's single-exe release does with a folder-build zip.
    app = tmp_path / "app"
    app.mkdir()
    (app / "TelescopeDesktop.exe").write_bytes(b"onefile exe")
    archive = _zip(tmp_path / "Telescope-windows-portable.zip", {
        "TelescopeDesktop.exe": b"folder exe", "lib-130/python311.dll": b"py", "lib-130/PyQt6/Qt6/x.dll": b"qt",
    })
    updates.install_windows(archive, app)
    assert (app / "TelescopeDesktop.exe").read_bytes() == b"folder exe"
    assert (app / "lib-130" / "PyQt6" / "Qt6" / "x.dll").read_bytes() == b"qt"

    update_guard.confirm(app)
    updates.clean_up_after_update(app, running_lib="lib-130")
    assert not (app / "TelescopeDesktop.old.exe").exists()
    assert (app / "lib-130").is_dir()


def test_a_folder_build_update_adds_its_own_libs_and_the_old_ones_go_on_the_next_start(tmp_path):
    app = tmp_path / "app"
    (app / "lib-130").mkdir(parents=True)
    (app / "lib-130" / "python311.dll").write_bytes(b"old py")  # loaded by the running copy: never touched
    (app / "TelescopeDesktop.exe").write_bytes(b"exe 130")
    (app / "platform-tools").mkdir()
    (app / "platform-tools" / "adb.exe").write_bytes(b"adb")
    archive = _zip(tmp_path / "Telescope-windows-portable.zip", {
        "TelescopeDesktop.exe": b"exe 131", "lib-131/python311.dll": b"new py", "platform-tools/adb.exe": b"adb",
    })
    result = updates.install_windows(archive, app)
    assert result.skipped == []
    assert (app / "lib-130" / "python311.dll").read_bytes() == b"old py"
    assert (app / "lib-131" / "python311.dll").read_bytes() == b"new py"

    updates.clean_up_after_update(app, running_lib="lib-131")
    assert (app / "lib-130").exists()  # kept until the new version has started once
    update_guard.confirm(app)
    updates.clean_up_after_update(app, running_lib="lib-131")
    assert not (app / "lib-130").exists()
    assert (app / "lib-131").is_dir()
    assert (app / "platform-tools" / "adb.exe").exists()


def test_the_new_lib_folder_is_in_place_before_the_exe_is_swapped(tmp_path, monkeypatch):
    app = tmp_path / "app"
    app.mkdir()
    (app / "TelescopeDesktop.exe").write_bytes(b"exe 130")
    archive = _zip(tmp_path / "Telescope-windows-portable.zip", {
        "TelescopeDesktop.exe": b"exe 131", "lib-131/a.dll": b"a", "lib-131/sub/b.dll": b"b",
    })
    seen = []
    real_replace = os.replace

    def replace(src, dst):
        if str(dst).endswith("TelescopeDesktop.exe") and "131" in open(src, "rb").read().decode():
            seen.append(sorted(p.name for p in (app / "lib-131").rglob("*")))
        return real_replace(src, dst)
    monkeypatch.setattr(updates.os, "replace", replace)

    updates.install_windows(archive, app)
    assert seen == [["a.dll", "b.dll", "sub"]]


def test_clean_up_leaves_lib_folders_alone_without_knowing_which_one_is_running(tmp_path):
    app = tmp_path / "app"
    (app / "lib-130").mkdir(parents=True)
    (app / "lib-131").mkdir()

    updates.clean_up_after_update(app)  # a source checkout: no running_lib
    updates.clean_up_after_update(app, running_lib="_internal")

    assert (app / "lib-130").is_dir() and (app / "lib-131").is_dir()


def test_windows_install_refuses_an_archive_without_the_app(tmp_path):
    app = tmp_path / "app"
    app.mkdir()
    (app / "TelescopeDesktop.exe").write_bytes(b"old exe")
    with pytest.raises(UpdateError, match="doesn't contain"):
        updates.install_windows(_zip(tmp_path / "w.zip", {"readme.txt": b"hi"}), app)
    assert (app / "TelescopeDesktop.exe").read_bytes() == b"old exe"


def test_an_archive_escaping_its_folder_is_refused(tmp_path):
    app = tmp_path / "app"
    app.mkdir()
    with pytest.raises(UpdateError, match="unexpected"):
        updates.install_windows(_zip(tmp_path / "w.zip", {"../evil.exe": b"x"}), app)
    assert not (tmp_path / "evil.exe").exists()


# ── Install: Linux ───────────────────────────────────────────────────────────

def _tarball(path, files):
    with tarfile.open(path, "w:gz") as t:
        for name, data in files.items():
            info = tarfile.TarInfo(name)
            info.size = len(data)
            t.addfile(info, io.BytesIO(data))
    return path


def _linux_app(tmp_path):
    app = tmp_path / "app"
    (app / "telescope").mkdir(parents=True)
    (app / "main.py").write_text("old")
    (app / "telescope" / "app.py").write_text("old")
    (app / "telescope" / "gone.py").write_text("removed in the new version")
    (app / "start.sh").write_text("old")
    (app / "notes-the-user-kept.txt").write_text("mine")
    return app


NEW_LINUX = {"main.py": b"new", "telescope/app.py": b"new", "start.sh": b"#!/bin/sh\n", "requirements.txt": b"x"}


def test_linux_install_swaps_the_app_and_keeps_the_old_one(tmp_path):
    app = _linux_app(tmp_path)
    result = updates.install_linux(_tarball(tmp_path / "l.tar.gz", NEW_LINUX), app)
    assert (app / "main.py").read_text() == "new"
    assert (app / "telescope" / "app.py").read_text() == "new"
    assert not (app / "telescope" / "gone.py").exists()  # whole package swapped, no stale modules
    assert (app / "notes-the-user-kept.txt").read_text() == "mine"
    assert (app / ".previous" / "main.py").read_text() == "old"
    assert os.access(app / "start.sh", os.X_OK)
    assert result.relaunch == [str(app / "start.sh"), "--after-update"]


def test_linux_install_rolls_back_when_a_swap_fails(tmp_path, monkeypatch):
    app = _linux_app(tmp_path)
    real_replace = os.replace

    def flaky_replace(src, dst):
        if str(dst).endswith("start.sh") and ".update-staging" in str(src):
            raise PermissionError(13, "Permission denied")
        return real_replace(src, dst)
    monkeypatch.setattr(updates.os, "replace", flaky_replace)
    with pytest.raises(UpdateError, match="Permission denied"):
        updates.install_linux(_tarball(tmp_path / "l.tar.gz", NEW_LINUX), app)
    assert (app / "main.py").read_text() == "old"
    assert (app / "telescope" / "gone.py").exists()
    assert (app / "start.sh").read_text() == "old"
    assert not (app / "requirements.txt").exists()


# ── When not to self-update ──────────────────────────────────────────────────

def test_a_source_checkout_or_read_only_folder_only_gets_a_link(tmp_path, monkeypatch):
    monkeypatch.setattr(version, "CHANNEL", "nightly")
    monkeypatch.setattr(updates, "IS_WINDOWS", False)
    app = tmp_path / "app"
    app.mkdir()
    (tmp_path / ".git").mkdir()  # a release unpacked inside a git-tracked folder
    assert updates.self_update_blocker(app) is None
    monkeypatch.setattr(updates.os, "access", lambda *_a: False)
    assert "writable" in updates.self_update_blocker(app)
    monkeypatch.setattr(version, "CHANNEL", "dev")
    assert "source checkout" in updates.self_update_blocker(app)


def test_a_rolled_back_build_is_named_and_can_be_tried_again(tmp_path):
    (tmp_path / ".update-failed").write_text('{"build": 200}')
    m = updates.Manifest("0.6.0", 200, "nightly", "n", "", 2, {})
    assert updates.was_rolled_back(m, build=150, directory=tmp_path)
    assert not updates.is_newer(m, build=150, directory=tmp_path)
    assert not updates.was_rolled_back(updates.Manifest("0.6.0", 201, "nightly", "n", "", 2, {}), build=150, directory=tmp_path)
    assert not updates.was_rolled_back(m, build=200, directory=tmp_path)  # already running it
    updates.forget_rolled_back(tmp_path)
    assert updates.is_newer(m, build=150, directory=tmp_path)


class _FakeWinreg:
    """The uninstall entry Inno writes, in memory."""
    HKEY_CURRENT_USER = "HKCU"
    KEY_QUERY_VALUE, KEY_SET_VALUE = 1, 2
    REG_SZ, REG_DWORD = 1, 4

    def __init__(self, values=None):
        self.values = values

    @contextlib.contextmanager
    def OpenKey(self, root, key, reserved=0, access=0):
        if self.values is None or key != updates.UNINSTALL_KEY:
            raise FileNotFoundError(key)
        yield self.values

    def QueryValueEx(self, values, name):
        if name not in values:
            raise FileNotFoundError(name)
        return values[name], self.REG_SZ

    def SetValueEx(self, values, name, reserved, kind, value):
        values[name] = value


def test_note_installed_version_updates_the_apps_entry(tmp_path, monkeypatch):
    monkeypatch.setattr(updates.version, "VERSION", "3.4.1")
    reg = _FakeWinreg({"InstallLocation": str(tmp_path), "DisplayVersion": "3.3.1",
                       "MajorVersion": 3, "MinorVersion": 3})
    assert updates.note_installed_version(tmp_path, winreg=reg)
    assert reg.values["DisplayVersion"] == "3.4.1"
    assert (reg.values["MajorVersion"], reg.values["MinorVersion"]) == (3, 4)
    assert not updates.note_installed_version(tmp_path, winreg=reg)  # already right: nothing written


def test_note_installed_version_leaves_another_copys_entry(tmp_path, monkeypatch):
    monkeypatch.setattr(updates.version, "VERSION", "3.4.1")
    installed = tmp_path / "Programs" / "Telescope"
    installed.mkdir(parents=True)
    reg = _FakeWinreg({"InstallLocation": str(installed), "DisplayVersion": "3.3.1"})
    assert not updates.note_installed_version(tmp_path, winreg=reg)
    assert reg.values["DisplayVersion"] == "3.3.1"
    assert not updates.note_installed_version(tmp_path, winreg=_FakeWinreg())  # not installed at all
