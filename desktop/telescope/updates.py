"""Update check, download and install for the desktop app. Qt-free; the network is injectable.

Each GitHub release carries a manifest.json (written by .github/write_manifest.py) with the version,
the build number and every file's SHA-256. Builds are compared by build number: the commit count on
master, so stable and nightly numbers are comparable and switching channel never downgrades.

Installing replaces the app's files in place and hands back the command that starts the new version. The swap
itself is update_guard's, journaled so a start after it was cut short finishes it, and the old version is kept
until the new one has started once:
- Windows (the PyInstaller folder build): a running exe can be renamed but not overwritten, so the old one
  becomes TelescopeDesktop.old.exe. Its libraries can't be moved either while loaded, so each build keeps
  them in its own lib-<build> folder.
- Linux (the source tarball): each top-level entry is swapped, the old ones kept in .previous/; start.sh
  then installs any new Python requirements.
A source checkout (no CI build info, channel "dev") or a folder this user can't write to is never touched.
"""

import hashlib
import http.client
import json
import logging
import os
import shutil
import sys
import tarfile
import urllib.error
import urllib.request
import zipfile
from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable, Optional

import update_guard
from update_guard import EXE_NAME, LIB_PREFIX, OLD_EXE_NAME, PREVIOUS_DIR, STAGING_DIR
from telescope import version
from telescope.platform import IS_WINDOWS

logger = logging.getLogger(__name__)

CHANNELS = ("stable", "nightly")
MANIFEST_URLS = {
    "stable": f"https://github.com/{version.REPO}/releases/latest/download/manifest.json",
    "nightly": f"https://github.com/{version.REPO}/releases/download/nightly/manifest.json",
}
WINDOWS_ASSET = "Telescope-windows.zip"
LINUX_ASSET = "Telescope-linux.tar.gz"
REQUEST_TIMEOUT = 15
MAX_MANIFEST_BYTES = 256 * 1024


class UpdateError(Exception):
    """Something the user should read; the message is the UI text."""


@dataclass(frozen=True)
class Asset:
    name: str
    url: str
    sha256: str
    size: int


@dataclass(frozen=True)
class Manifest:
    version: str
    build: int
    channel: str
    version_name: str
    notes: str
    protocol: int
    assets: dict = field(default_factory=dict)  # name -> Asset

    @property
    def display_version(self) -> str:
        return self.version if self.channel == "stable" else f"{self.version} {self.channel} {self.build}"


def default_channel() -> str:
    """Nightly builds follow nightly; stable and source builds follow stable."""
    return "nightly" if version.CHANNEL == "nightly" else "stable"


def parse_manifest(raw: bytes) -> Manifest:
    try:
        data = json.loads(raw.decode("utf-8"))
        assets = {}
        for a in data["assets"]:
            asset = Asset(str(a["name"]), str(a["url"]), str(a["sha256"]).lower(), int(a["size"]))
            if not asset.url.startswith("https://") or len(asset.sha256) != 64:
                raise ValueError(f"bad asset {asset.name}")
            assets[asset.name] = asset
        return Manifest(
            version=str(data["version"]), build=int(data["build"]), channel=str(data["channel"]),
            version_name=str(data.get("versionName", data["version"])), notes=str(data.get("notes", "")),
            protocol=int(data.get("protocol", 0)), assets=assets,
        )
    except (ValueError, KeyError, TypeError, AttributeError) as exc:
        raise UpdateError("The update information couldn't be read.") from exc


def _open(url: str, timeout: float):
    req = urllib.request.Request(url, headers={"User-Agent": f"Telescope/{version.VERSION}"})
    return urllib.request.urlopen(req, timeout=timeout)


def fetch_manifest(channel: str, opener: Callable = _open) -> Optional[Manifest]:
    """The channel's current manifest, or None when the channel has no release yet (HTTP 404)."""
    try:
        with opener(MANIFEST_URLS[channel], REQUEST_TIMEOUT) as r:
            raw = r.read(MAX_MANIFEST_BYTES + 1)
    except urllib.error.HTTPError as exc:
        if exc.code == 404:
            return None
        raise UpdateError(f"Couldn't check for updates (HTTP {exc.code}).") from exc
    except (OSError, ValueError, http.client.HTTPException) as exc:  # HTTPException: a reply that isn't HTTP
        raise UpdateError("Couldn't check for updates. Check the internet connection.") from exc
    if len(raw) > MAX_MANIFEST_BYTES:
        raise UpdateError("The update information couldn't be read.")
    return parse_manifest(raw)


def is_newer(manifest: Optional[Manifest], build: Optional[int] = None, directory: Optional[Path] = None) -> bool:
    """Whether manifest is an update to offer: newer, and not the build that was rolled back for not starting."""
    current = version.BUILD if build is None else build
    if manifest is None or current <= 0 or manifest.build <= current:
        return False
    return manifest.build != update_guard.failed_build(directory or install_dir())


def was_rolled_back(manifest: Optional[Manifest], build: Optional[int] = None, directory: Optional[Path] = None) -> bool:
    """Whether manifest is the newer build that is held back because it was rolled back for not starting."""
    current = version.BUILD if build is None else build
    if manifest is None or current <= 0 or manifest.build <= current:
        return False
    return manifest.build == update_guard.failed_build(directory or install_dir())


def forget_rolled_back(directory: Optional[Path] = None):
    """Offer the rolled-back build again."""
    try:
        ((directory or install_dir()) / update_guard.FAILED).unlink(missing_ok=True)
    except OSError:
        logger.exception("Couldn't clear the rolled-back marker")


def platform_asset(manifest: Manifest) -> Optional[Asset]:
    return manifest.assets.get(WINDOWS_ASSET if IS_WINDOWS else LINUX_ASSET)


# ── Where this copy lives ─────────────────────────────────────────────────────

def install_dir() -> Path:
    """The folder holding this copy: the exe's folder, or the one with main.py."""
    if getattr(sys, "frozen", False):
        return Path(sys.executable).resolve().parent
    return Path(__file__).resolve().parent.parent


def self_update_blocker(directory: Optional[Path] = None) -> Optional[str]:
    """Why this copy can't replace itself (the UI then offers the release page), or None."""
    directory = directory or install_dir()
    if version.CHANNEL == "dev":  # not a .git check: a release unpacked inside a git-tracked folder is still a release
        return "This is a source checkout. Update it with git."
    if IS_WINDOWS and not getattr(sys, "frozen", False):
        return "Only the packaged app can update itself."
    if not os.access(directory, os.W_OK):
        return "Telescope's folder isn't writable, so it can't update itself."
    return None


# ── Download ──────────────────────────────────────────────────────────────────

def download(asset: Asset, dest_dir: Path, progress: Optional[Callable[[int, int], None]] = None,
             cancelled: Callable[[], bool] = lambda: False, opener: Callable = _open) -> Path:
    """Download and verify an asset; raises UpdateError. The file only gets its real name once verified."""
    dest_dir.mkdir(parents=True, exist_ok=True)
    final = dest_dir / asset.name
    partial = dest_dir / (asset.name + ".part")
    digest = hashlib.sha256()
    done = 0
    try:
        with opener(asset.url, REQUEST_TIMEOUT) as r, partial.open("wb") as f:
            while True:
                if cancelled():
                    raise UpdateError("Cancelled.")
                chunk = r.read(256 * 1024)
                if not chunk:
                    break
                done += len(chunk)
                if done > asset.size:
                    raise UpdateError("The download is bigger than it should be.")
                digest.update(chunk)
                f.write(chunk)
                if progress:
                    progress(done, asset.size)
    except UpdateError:
        partial.unlink(missing_ok=True)
        raise
    except (OSError, http.client.HTTPException) as exc:  # HTTPException: cut off part way (IncompleteRead)
        partial.unlink(missing_ok=True)
        raise UpdateError("The download failed. Check the internet connection and try again.") from exc
    if done < asset.size:  # the connection closed early, as when a wifi without internet drops it
        partial.unlink(missing_ok=True)
        raise UpdateError("The download was cut short. Check the internet connection and try again.")
    if digest.hexdigest() != asset.sha256:
        partial.unlink(missing_ok=True)
        raise UpdateError("The download didn't match its checksum, so it wasn't installed.")
    os.replace(partial, final)
    return final


# ── Install ───────────────────────────────────────────────────────────────────

def _safe_members_zip(archive: zipfile.ZipFile, root: Path):
    for name in archive.namelist():
        target = (root / name).resolve()
        if root.resolve() not in target.parents and target != root.resolve():
            raise UpdateError("The update contains unexpected paths, so it wasn't installed.")


def _extract(archive_path: Path, staging: Path):
    if staging.exists():
        shutil.rmtree(staging)
    staging.mkdir(parents=True)
    try:
        if archive_path.suffix == ".zip":
            with zipfile.ZipFile(archive_path) as z:
                _safe_members_zip(z, staging)
                z.extractall(staging)
        else:
            with tarfile.open(archive_path, "r:gz") as t:
                for m in t.getmembers():
                    target = (staging / m.name).resolve()
                    if staging.resolve() not in target.parents and target != staging.resolve():
                        raise UpdateError("The update contains unexpected paths, so it wasn't installed.")
                    if not (m.isfile() or m.isdir()):
                        raise UpdateError("The update contains unexpected files, so it wasn't installed.")
                if hasattr(tarfile, "data_filter"):
                    t.extractall(staging, filter="data")
                else:
                    t.extractall(staging)
    except (zipfile.BadZipFile, tarfile.TarError, OSError) as exc:
        raise UpdateError("The download couldn't be unpacked.") from exc


@dataclass
class InstallResult:
    relaunch: list        # argv that starts the new version
    skipped: list = field(default_factory=list)  # files in use that kept their old version


def _swap(directory: Path, journal: dict) -> list:
    """Journal the swap, then do it (see update_guard). A failure puts the old version back and raises UpdateError."""
    update_guard.write_journal(directory, dict(journal, state="swapping", from_build=version.BUILD))
    skipped = []
    try:
        update_guard.finish_swap(directory, journal, skipped)
    except OSError as exc:
        try:
            update_guard.roll_back(directory, journal, remember=False)
        except OSError:
            logger.exception("Putting the old version back failed; the next start tries again")
        raise UpdateError(f"Couldn't replace Telescope's files: {exc.strerror or exc}") from exc
    # The old version stays until the new one has started once (update_guard.confirm()).
    update_guard.write_journal(directory, dict(journal, state="trial", started=False))
    return skipped


def install_windows(archive: Path, directory: Path, build: int = 0) -> InstallResult:
    staging = directory / STAGING_DIR
    _extract(archive, staging)
    if not (staging / EXE_NAME).is_file():
        shutil.rmtree(staging, ignore_errors=True)
        raise UpdateError("The download doesn't contain Telescope, so it wasn't installed.")
    # A roll-back puts back whatever old exe it finds, so one left from an earlier update must go first
    for leftover in (update_guard.OLD_EXE_NAME, update_guard.FAILED_EXE_NAME):
        try:
            (directory / leftover).unlink(missing_ok=True)
        except OSError:
            shutil.rmtree(staging, ignore_errors=True)
            raise UpdateError(f"{leftover} is in use, so the update wasn't installed. Try again after a restart.")
    skipped = _swap(directory, {"platform": "windows", "to_build": build})
    return InstallResult([str(directory / EXE_NAME), "--after-update"], skipped)


def install_linux(archive: Path, directory: Path, build: int = 0) -> InstallResult:
    staging = directory / STAGING_DIR
    _extract(archive, staging)
    entries = sorted(p.name for p in staging.iterdir())
    if "main.py" not in entries or "telescope" not in entries:
        shutil.rmtree(staging, ignore_errors=True)
        raise UpdateError("The download doesn't contain Telescope, so it wasn't installed.")
    previous = directory / PREVIOUS_DIR
    if previous.exists():
        shutil.rmtree(previous)
    added = [name for name in entries if not (directory / name).exists()]
    _swap(directory, {"platform": "linux", "to_build": build, "entries": entries, "added": added})

    start = directory / "start.sh"
    if start.exists():
        start.chmod(0o755)
        return InstallResult([str(start), "--after-update"])
    return InstallResult([sys.executable, str(directory / "main.py"), "--after-update"])


def install(archive: Path, directory: Optional[Path] = None, build: int = 0) -> InstallResult:
    """build: the one being installed, so a roll-back knows not to offer it again."""
    directory = directory or install_dir()
    return (install_windows if IS_WINDOWS else install_linux)(archive, directory, build)


def clean_up_after_update(directory: Optional[Path] = None, running_lib: Optional[str] = None):
    """Delete what earlier versions left behind: the old exe, the staging folder and any lib-<build> folder but
    running_lib (this copy's, found by itself when it's the packaged app). Best effort: the old exe may still be
    exiting, and what it holds is tried again on the next start."""
    directory = directory or install_dir()
    if update_guard.read_journal(directory) is not None:
        return  # an update isn't confirmed yet: these are what it would roll back to
    if version.BUILD and update_guard.failed_build(directory) == version.BUILD:
        # This copy was rolled back while it was still starting (a second launch gave up waiting for it): the exe in
        # place is the old version again, and these include its libraries.
        return
    if running_lib is None and getattr(sys, "frozen", False):
        running_lib = Path(getattr(sys, "_MEIPASS", "")).name
    leftovers = [directory / OLD_EXE_NAME, directory / update_guard.FAILED_EXE_NAME, directory / STAGING_DIR,
                 directory / PREVIOUS_DIR]
    if running_lib and running_lib.startswith(LIB_PREFIX):
        leftovers += [p for p in directory.glob(LIB_PREFIX + "*") if p.is_dir() and p.name != running_lib]
    for leftover in leftovers:
        try:
            update_guard.remove(leftover)
        except OSError:
            pass
