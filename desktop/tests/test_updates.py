"""Update check, download and install, with a fake network and temporary install folders."""

import hashlib
import io
import json
import os
import tarfile
import urllib.error
import zipfile

import pytest

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
    archive = _zip(tmp_path / "Telescope-windows.zip", {
        "TelescopeDesktop.exe": b"new exe", "unitycapture/UnityCaptureFilter64.dll": b"same dll",
        "Telescope.apk": b"new apk",
    })
    result = updates.install_windows(archive, app)
    assert (app / "TelescopeDesktop.exe").read_bytes() == b"new exe"
    assert (app / "TelescopeDesktop.old.exe").read_bytes() == b"old exe"
    assert (app / "Telescope.apk").read_bytes() == b"new apk"
    assert result.relaunch == [str(app / "TelescopeDesktop.exe"), "--after-update"]
    assert not (app / updates.STAGING_DIR).exists()

    updates.clean_up_after_update(app)
    assert not (app / "TelescopeDesktop.old.exe").exists()


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
    assert updates.self_update_blocker(app) is None
    (tmp_path / ".git").mkdir()
    assert "git" in updates.self_update_blocker(app)
    (tmp_path / ".git").rmdir()
    monkeypatch.setattr(updates.os, "access", lambda *_a: False)
    assert "writable" in updates.self_update_blocker(app)
    monkeypatch.setattr(version, "CHANNEL", "dev")
    assert "source checkout" in updates.self_update_blocker(app)
