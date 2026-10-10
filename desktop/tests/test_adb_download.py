"""Windows: adb fetched from Google on request instead of bundled. Google's index finds the zip, its size and SHA-1
check it, and only what adb needs is kept, in a folder that's only ever swapped in whole."""

import hashlib
import io
import urllib.error
import zipfile

import pytest

import telescope.platform as platform_api
from telescope.platform import adb_download
from telescope.platform.adb_download import NEEDED, Archive, download_adb, find_archive

# The shape of Google's repository2-3.xml (trimmed from the real one: namespaces, the unprefixed remotePackage)
INDEX = """<?xml version='1.0' encoding='utf-8'?>
<sdk:sdk-repository xmlns:sdk="http://schemas.android.com/sdk/android/repo/repository2/03"
    xmlns:common="http://schemas.android.com/repository/android/common/02"
    xmlns:generic="http://schemas.android.com/repository/android/generic/02"
    xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance">
  <remotePackage path="build-tools;34.0.0">
    <revision><major>34</major><minor>0</minor><micro>0</micro></revision>
    <archives><archive><complete><size>1</size><checksum type="sha1">00</checksum>
      <url>build-tools-win.zip</url></complete><host-os>windows</host-os></archive></archives>
  </remotePackage>
  <remotePackage path="platform-tools">
    <type-details xsi:type="generic:genericDetailsType" />
    <revision><major>37</major><minor>0</minor><micro>1</micro></revision>
    <uses-license ref="android-sdk-license" />
    <archives>
      <archive><complete><size>9054187</size><checksum type="sha1">477254aa5f903c15cf51001717bdf347fb6b53e0</checksum>
        <url>platform-tools_r37.0.1-linux.zip</url></complete><host-os>linux</host-os></archive>
      <archive><complete><size>{size}</size><checksum type="sha1">{sha1}</checksum>
        <url>{name}</url></complete><host-os>windows</host-os></archive>
    </archives>
  </remotePackage>
</sdk:sdk-repository>"""


def _zip(names=("adb.exe", "AdbWinApi.dll", "AdbWinUsbApi.dll", "libwinpthread-1.dll", "NOTICE.txt",
                "source.properties", "fastboot.exe", "sqlite3.exe")) -> bytes:
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w") as z:
        z.writestr("platform-tools/", b"")
        for name in names:
            z.writestr(f"platform-tools/{name}", name.encode())
    return buf.getvalue()


class _Google:
    """urlopen for dl.google.com: the index and one zip."""

    def __init__(self, zip_bytes=None, listed=None, name="platform-tools_r37.0.1-win.zip", fail=None):
        self.zip = _zip() if zip_bytes is None else zip_bytes
        listed = self.zip if listed is None else listed
        self.index = INDEX.format(size=len(listed), sha1=hashlib.sha1(listed).hexdigest(), name=name).encode()
        self.fail, self.urls = fail, []

    def __call__(self, url, timeout):
        self.urls.append(url)
        if self.fail and self.fail in url:
            raise urllib.error.URLError(TimeoutError("timed out"))
        if url == adb_download.INDEX_URL:
            return io.BytesIO(self.index)
        if url == adb_download.REPOSITORY + "platform-tools_r37.0.1-win.zip":
            return io.BytesIO(self.zip)
        raise AssertionError(f"unexpected {url}")


def test_the_index_gives_the_windows_zip_its_size_and_checksum():
    google = _Google()
    archive = find_archive(google.index)
    assert archive == Archive(adb_download.REPOSITORY + "platform-tools_r37.0.1-win.zip", len(google.zip),
                              hashlib.sha1(google.zip).hexdigest(), "37.0.1")
    assert find_archive(google.index, "linux").url.endswith("platform-tools_r37.0.1-linux.zip")


@pytest.mark.parametrize("name", ["../evil.zip", "https://elsewhere.example/pt.zip", "sub/pt.zip", "pt.exe"])
def test_the_index_can_only_point_next_to_itself(name):
    with pytest.raises(ValueError):
        find_archive(_Google(name=name).index)


def test_an_index_without_platform_tools_is_an_error():
    with pytest.raises(ValueError, match="no platform-tools"):
        find_archive(b"<sdk-repository><remotePackage path='emulator'/></sdk-repository>")


def test_it_downloads_from_google_and_keeps_only_what_adb_needs(tmp_path):
    google, dest = _Google(), tmp_path / "Telescope" / "platform-tools"
    said = []
    assert download_adb(google, dest, said.append) == (True, "37.0.1")
    assert google.urls == [adb_download.INDEX_URL, adb_download.REPOSITORY + "platform-tools_r37.0.1-win.zip"]
    assert sorted(p.name for p in dest.iterdir()) == sorted(NEEDED)
    assert (dest / "adb.exe").read_bytes() == b"adb.exe"
    assert sorted(p.name for p in dest.parent.iterdir()) == ["platform-tools"]  # no work folder left behind
    assert said and "37.0.1" in said[1]


def test_a_zip_that_doesn_t_match_the_index_isn_t_used(tmp_path):
    dest = tmp_path / "platform-tools"
    real = _zip()
    ok, why = download_adb(_Google(real, listed=real[:-1] + b"!"), dest)  # same size, one byte off
    assert not ok and "checksum" in why
    assert not dest.exists()
    assert list(tmp_path.iterdir()) == []


def test_a_bigger_download_than_listed_stops_early(tmp_path):
    google = _Google(listed=b"tiny")
    ok, why = download_adb(google, tmp_path / "platform-tools")
    assert not ok and "bigger than expected" in why


def test_a_zip_without_adb_isn_t_used(tmp_path):
    dest = tmp_path / "platform-tools"
    ok, why = download_adb(_Google(_zip(("fastboot.exe",))), dest)
    assert not ok and "adb.exe" in why and not dest.exists()


@pytest.mark.parametrize("fail", ["repository2-3.xml", "platform-tools_r37"])
def test_no_network_says_so_and_keeps_what_was_there(tmp_path, fail):
    dest = tmp_path / "platform-tools"
    dest.mkdir()
    (dest / "NOTICE.txt").write_bytes(b"old")
    ok, why = download_adb(_Google(fail=fail), dest)
    assert not ok and why == "Couldn't reach Google. Check the internet connection and try again."
    assert (dest / "NOTICE.txt").read_bytes() == b"old"
    assert sorted(p.name for p in tmp_path.iterdir()) == ["platform-tools"]


def test_a_half_emptied_folder_is_replaced_whole(tmp_path):
    dest = tmp_path / "platform-tools"
    dest.mkdir()
    (dest / "stale.dll").write_bytes(b"old")
    assert download_adb(_Google(), dest)[0]
    assert sorted(p.name for p in dest.iterdir()) == sorted(NEEDED)


def test_adb_that_s_already_there_isn_t_fetched_again(tmp_path):
    # The other dialog got it first, and its adb server may be running from that folder
    google, dest = _Google(), tmp_path / "platform-tools"
    assert download_adb(google, dest) == (True, "37.0.1") and len(google.urls) == 2
    (dest / "source.properties").write_text("Pkg.UserSrc=false\nPkg.Revision=37.0.1\n")
    assert download_adb(google, dest) == (True, "37.0.1") and len(google.urls) == 2
    (dest / "source.properties").unlink()
    assert download_adb(google, dest) == (True, "already downloaded")


def test_a_cut_short_download_s_work_folder_is_cleared_later(tmp_path):
    import os
    import time
    stale, fresh = tmp_path / ".adb-old", tmp_path / ".adb-running"
    for folder in (stale, fresh):
        folder.mkdir()
        (folder / "platform-tools.zip").write_bytes(b"part")
    os.utime(stale, (time.time() - 7200,) * 2)
    assert download_adb(_Google(), tmp_path / "platform-tools")[0]
    assert sorted(p.name for p in tmp_path.iterdir()) == [".adb-running", "platform-tools"]


def test_it_lives_in_local_app_data(monkeypatch, tmp_path):
    monkeypatch.setenv("LOCALAPPDATA", str(tmp_path))
    assert adb_download.downloaded_dir() == tmp_path / "Telescope" / "platform-tools"


def test_adb_is_found_in_the_app_folder_then_the_download_then_path(monkeypatch, tmp_path):
    monkeypatch.setattr(platform_api, "IS_WINDOWS", True)
    monkeypatch.setattr(platform_api, "platform_tools_dir", lambda: tmp_path / "app" / "platform-tools")
    monkeypatch.setenv("LOCALAPPDATA", str(tmp_path / "local"))
    monkeypatch.setattr(platform_api.shutil, "which", lambda name: r"C:\sdk\adb.exe")
    assert platform_api.adb_exe() == r"C:\sdk\adb.exe"
    downloaded = tmp_path / "local" / "Telescope" / "platform-tools" / "adb.exe"
    downloaded.parent.mkdir(parents=True)
    downloaded.write_bytes(b"adb")
    assert platform_api.adb_exe() == str(downloaded)
    bundled = tmp_path / "app" / "platform-tools" / "adb.exe"
    bundled.parent.mkdir(parents=True)
    bundled.write_bytes(b"adb")
    assert platform_api.adb_exe() == str(bundled)


def test_a_server_error_says_so(tmp_path):
    def urlopen(url, timeout):
        raise urllib.error.HTTPError(url, 503, "Service Unavailable", {}, None)
    assert download_adb(urlopen, tmp_path / "pt") == (False, "Google's server said 503. Try again later.")
