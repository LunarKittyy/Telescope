"""Windows: fetch adb from Google when the person asks for it, rather than shipping it.

adb is under the Android SDK License, which doesn't allow passing it on, so Telescope doesn't bundle it. Instead it
reads Google's own SDK index (the one Android Studio's SDK manager uses) for the current Windows platform-tools,
downloads that zip from dl.google.com, checks it against the index's size and SHA-1, and keeps only what adb needs in
a folder of this user's (downloaded_dir()). Copies that still have the old bundled platform-tools keep using it.
"""

import hashlib
import logging
import os
import shutil
import tempfile
import threading
import time
import urllib.error
import xml.etree.ElementTree as ET
import zipfile
from dataclasses import dataclass
from pathlib import Path
from typing import Callable, Optional

from telescope import web
from telescope.i18n import tr

logger = logging.getLogger(__name__)

REPOSITORY = "https://dl.google.com/android/repository/"
INDEX_URL = REPOSITORY + "repository2-3.xml"
LICENSE_URL = "https://developer.android.com/studio/terms"
APPROX_SIZE = "8\u00a0MB"  # no-break space: it shows in a wrapped dialog
# What adb.exe needs next to it, plus Google's notices. The rest of platform-tools (fastboot, sqlite3, ...) is left out.
NEEDED = ("adb.exe", "AdbWinApi.dll", "AdbWinUsbApi.dll", "libwinpthread-1.dll", "NOTICE.txt", "source.properties")
_MAX_ZIP = 64 << 20   # the zip is about 8 MB; anything far bigger isn't it
_MAX_INDEX = 16 << 20
_lock = threading.Lock()  # Add phone and Advanced each have a Get adb; one download at a time


@dataclass
class Archive:
    url: str
    size: int
    sha1: str
    revision: str


def downloaded_dir() -> Path:
    """Where the downloaded adb lives: per user, outside the app's folder, so updates and moves leave it alone."""
    base = os.environ.get("LOCALAPPDATA") or str(Path.home() / "AppData" / "Local")
    return Path(base) / "Telescope" / "platform-tools"


def downloaded_adb() -> Optional[Path]:
    exe = downloaded_dir() / "adb.exe"
    return exe if exe.is_file() else None


def _local(tag: str) -> str:
    return tag.rsplit("}", 1)[-1]


def _child(element, name: str):
    return next((c for c in element if _local(c.tag) == name), None)


def find_archive(index: bytes, host_os: str = "windows") -> Archive:
    """The platform-tools zip for host_os in Google's SDK index. Raises ValueError if the index doesn't have one."""
    root = ET.fromstring(index)
    for package in root.iter():
        if _local(package.tag) != "remotePackage" or package.get("path") != "platform-tools":
            continue
        rev = _child(package, "revision")
        revision = ".".join((_child(rev, p).text or "").strip() for p in ("major", "minor", "micro")
                            if rev is not None and _child(rev, p) is not None)
        if not all(part.isdigit() for part in revision.split(".")):  # it's shown in the UI as the version
            revision = ""
        for archive in package.iter():
            if _local(archive.tag) != "archive":
                continue
            host = _child(archive, "host-os")
            complete = _child(archive, "complete")
            if host is None or host.text.strip() != host_os or complete is None:
                continue
            url, size, checksum = (_child(complete, n) for n in ("url", "size", "checksum"))
            if url is None or size is None or checksum is None or checksum.get("type") != "sha1":
                continue
            name = url.text.strip()
            if "/" in name or ":" in name or not name.endswith(".zip"):
                raise ValueError(f"unexpected archive name {name!r}")  # only files next to the index, on Google's host
            return Archive(REPOSITORY + name, int(size.text), checksum.text.strip().lower(), revision)
    raise ValueError(f"no platform-tools for {host_os} in Google's index")


def _fetch(urlopen: Callable, url: str, write: Callable, limit: int):
    got = 0
    with urlopen(url, timeout=30) as r:
        final = getattr(r, "geturl", lambda: url)()
        if not str(final).startswith("https://"):  # a redirect off HTTPS: the checksum would come from anyone
            raise ValueError(f"it was redirected to {final}")
        for chunk in iter(lambda: r.read(1 << 16), b""):
            got += len(chunk)
            if got > limit:
                raise ValueError("it's bigger than expected")
            write(chunk)


def downloaded_revision(dest: Optional[Path] = None) -> str:
    """The version Google's source.properties gives for the downloaded adb, or "" if it doesn't say."""
    try:
        for line in ((dest or downloaded_dir()) / "source.properties").read_text(errors="replace").splitlines():
            key, _, value = line.partition("=")
            if key.strip() == "Pkg.Revision":
                value = value.strip()
                return value if all(part.isdigit() for part in value.split(".")) else ""
    except OSError:
        pass
    return ""


def download_adb(urlopen: Callable = web.urlopen, dest: Optional[Path] = None,
                 progress: Optional[Callable[[str], None]] = None) -> tuple:
    """Download adb into dest (downloaded_dir()) unless it's already there; (True, version or "" if unknown) or
    (False, why not).
    Never leaves a half-made folder where adb_exe() looks."""
    dest = dest or downloaded_dir()
    with _lock:
        # Already there (the other dialog got it, say): its adb may be running, which keeps the folder from moving
        if (dest / "adb.exe").is_file():
            return True, downloaded_revision(dest)
        _clear_leftovers(dest.parent)
        return _download(urlopen, dest, progress or (lambda _msg: None))


def _clear_leftovers(parent: Path):
    """Work folders a download that was cut short (Telescope killed, say) left behind."""
    for old in parent.glob(".adb-*"):
        try:
            if time.time() - old.stat().st_mtime > 3600:
                shutil.rmtree(old, ignore_errors=True)
        except OSError:
            pass


def _download(urlopen: Callable, dest: Path, say: Callable[[str], None]) -> tuple:
    try:
        say(tr("Finding the current adb…"))
        index = bytearray()
        _fetch(urlopen, INDEX_URL, index.extend, _MAX_INDEX)
        archive = find_archive(bytes(index))
        if archive.size > _MAX_ZIP:
            raise ValueError(f"platform-tools is listed at {archive.size} bytes")
    except Exception as e:
        logger.warning("adb index: %s", e)
        return False, _offline(e) or tr("Google's list of downloads wasn't what Telescope expected. Try again later.")

    dest.parent.mkdir(parents=True, exist_ok=True)
    work = Path(tempfile.mkdtemp(prefix=".adb-", dir=dest.parent))  # same drive, so the final move is a rename
    try:
        zip_path = work / "platform-tools.zip"
        say(tr("Downloading adb {version} from Google…", version=archive.revision) if archive.revision
            else tr("Downloading adb from Google…"))
        try:
            with open(zip_path, "wb") as f:
                _fetch(urlopen, archive.url, f.write, archive.size)
        except Exception as e:
            logger.warning("adb download: %s", e)
            return False, _offline(e) or tr("Couldn't download adb. Try again.")
        if zip_path.stat().st_size != archive.size or _sha1(zip_path) != archive.sha1:
            return False, tr("The download didn't match Google's checksum, so it wasn't used. Try again.")
        say(tr("Unpacking…"))
        unpacked = work / "platform-tools"
        try:
            _unzip(zip_path, unpacked)
        except (OSError, zipfile.BadZipFile) as e:
            logger.warning("adb unpack: %s", e)
            return False, tr("Couldn't unpack adb. Try again.")
        if not (unpacked / "adb.exe").is_file():
            return False, tr("Google's download doesn't have adb.exe in it anymore.")
        ok, why = _put_in_place(unpacked, dest, work)
        return (True, archive.revision) if ok else (False, why)
    finally:
        shutil.rmtree(work, ignore_errors=True)


def _unzip(zip_path: Path, unpacked: Path):
    """Only what adb needs, from platform-tools/ in the zip (Google's layout) or from its top level."""
    unpacked.mkdir()
    with zipfile.ZipFile(zip_path) as z:
        names = set(z.namelist())
        for name in NEEDED:
            member = next((m for m in (f"platform-tools/{name}", name) if m in names), None)
            if member is None:
                continue
            with z.open(member) as src, open(unpacked / name, "wb") as dst:
                shutil.copyfileobj(src, dst)


def _put_in_place(unpacked: Path, dest: Path, work: Path) -> tuple:
    old = None
    try:
        if dest.exists():  # a folder without adb.exe in it: one that something else half emptied
            old = work / "old"
            os.replace(dest, old)
        os.replace(unpacked, dest)
    except OSError as e:
        if old is not None and not dest.exists():
            os.replace(old, dest)
        logger.warning("adb move: %s", e)
        return False, tr("Couldn't put adb in place. Is another program using it? Try again.")
    return True, ""


def use_own_adb(source: Path, dest: Optional[Path] = None) -> tuple:
    """Keep an adb the person got some other way (Google blocks the download in some countries): source is adb.exe,
    whose folder has the DLLs it needs, or a platform-tools zip. (True, version or "") or (False, why not)."""
    dest = dest or downloaded_dir()
    with _lock:
        dest.parent.mkdir(parents=True, exist_ok=True)
        _clear_leftovers(dest.parent)
        work = Path(tempfile.mkdtemp(prefix=".adb-", dir=dest.parent))
        try:
            unpacked = work / "platform-tools"
            try:
                if source.suffix.lower() == ".zip":
                    _unzip(source, unpacked)
                else:
                    unpacked.mkdir()
                    for name in NEEDED:
                        if (source.parent / name).is_file():
                            shutil.copyfile(source.parent / name, unpacked / name)
            except (OSError, zipfile.BadZipFile) as e:
                logger.warning("own adb: %s", e)
                return False, tr("Couldn't copy adb from there.")
            if not (unpacked / "adb.exe").is_file():
                return False, tr("That doesn't have adb.exe in it.")
            ok, why = _put_in_place(unpacked, dest, work)
            return (True, downloaded_revision(dest)) if ok else (False, why)
        finally:
            shutil.rmtree(work, ignore_errors=True)


def _offline(e: Exception) -> str:
    """A plain sentence for a network failure, "" for anything else."""
    if web.certificate_failed(e):
        return tr("Couldn't check Google's certificate, so nothing was downloaded. Security software or a network "
                  "that inspects HTTPS can cause this.")
    if isinstance(e, (urllib.error.URLError, TimeoutError, ConnectionError)) and not isinstance(
            e, urllib.error.HTTPError):
        return tr("Couldn't reach Google. Check the internet connection and try again.")
    if isinstance(e, urllib.error.HTTPError) and e.code in (403, 404, 451):
        return tr("Google wouldn't send adb to this computer (it said {code}). Google blocks Android downloads in "
                  "some countries. If you get platform-tools another way, pick its adb.exe or zip instead.", code=e.code)
    if isinstance(e, urllib.error.HTTPError):
        return tr("Google's server said {code}. Try again later.", code=e.code)
    return ""


def _sha1(path: Path) -> str:
    h = hashlib.sha1()  # what Google's index gives; the index itself comes over HTTPS from Google
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()
