"""Update check, download and install for the desktop app. Qt-free; the network is injectable.

Each GitHub release carries a manifest.json (written by .github/write_manifest.py) with the version,
the build number and every file's SHA-256. Builds are compared by build number: the commit count on
master, so stable and nightly numbers are comparable and switching channel never downgrades.

Installing replaces the app's files in place and hands back the command that starts the new version:
- Windows (the PyInstaller bundle): a running exe can be renamed but not overwritten, so the old one
  becomes TelescopeDesktop.old.exe and is deleted on the next start.
- Linux (the source tarball): each top-level entry is swapped, the old ones kept in .previous/ until
  the swap completes; start.sh then installs any new Python requirements.
A source checkout (a .git folder) or a folder this user can't write to is never touched.
"""

import hashlib
import json
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

from telescope import version
from telescope.platform import IS_WINDOWS

CHANNELS = ("stable", "nightly")
MANIFEST_URLS = {
    "stable": f"https://github.com/{version.REPO}/releases/latest/download/manifest.json",
    "nightly": f"https://github.com/{version.REPO}/releases/download/nightly/manifest.json",
}
WINDOWS_ASSET = "Telescope-windows.zip"
LINUX_ASSET = "Telescope-linux.tar.gz"
EXE_NAME = "TelescopeDesktop.exe"
OLD_EXE_NAME = "TelescopeDesktop.old.exe"
STAGING_DIR = ".update-staging"
PREVIOUS_DIR = ".previous"
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
    except (OSError, ValueError) as exc:
        raise UpdateError("Couldn't check for updates. Check the internet connection.") from exc
    if len(raw) > MAX_MANIFEST_BYTES:
        raise UpdateError("The update information couldn't be read.")
    return parse_manifest(raw)


def is_newer(manifest: Optional[Manifest], build: Optional[int] = None) -> bool:
    current = version.BUILD if build is None else build
    return manifest is not None and current > 0 and manifest.build > current


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
    if version.CHANNEL == "dev" or (directory / ".git").exists() or (directory.parent / ".git").exists():
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
    except OSError as exc:
        partial.unlink(missing_ok=True)
        raise UpdateError("The download failed. Check the internet connection and try again.") from exc
    if done != asset.size or digest.hexdigest() != asset.sha256:
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


def _file_hash(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            digest.update(chunk)
    return digest.hexdigest()


@dataclass
class InstallResult:
    relaunch: list        # argv that starts the new version
    skipped: list = field(default_factory=list)  # files in use that kept their old version


def install_windows(archive: Path, directory: Path) -> InstallResult:
    staging = directory / STAGING_DIR
    _extract(archive, staging)
    new_exe = staging / EXE_NAME
    if not new_exe.is_file():
        shutil.rmtree(staging, ignore_errors=True)
        raise UpdateError("The download doesn't contain Telescope, so it wasn't installed.")

    current = directory / EXE_NAME
    old = directory / OLD_EXE_NAME
    try:
        old.unlink(missing_ok=True)
        if current.exists():
            os.replace(current, old)  # allowed while it runs; overwriting isn't
        try:
            os.replace(new_exe, current)
        except OSError:
            if old.exists():
                os.replace(old, current)
            raise
    except OSError as exc:
        shutil.rmtree(staging, ignore_errors=True)
        raise UpdateError(f"Couldn't replace {EXE_NAME}: {exc.strerror or exc}") from exc

    skipped = []
    for src in sorted(p for p in staging.rglob("*") if p.is_file()):
        rel = src.relative_to(staging)
        dst = directory / rel
        if dst.exists() and _file_hash(dst) == _file_hash(src):
            continue
        dst.parent.mkdir(parents=True, exist_ok=True)
        try:
            os.replace(src, dst)
        except OSError:
            # e.g. a UnityCapture DLL loaded by OBS right now; it keeps working, just unchanged.
            skipped.append(str(rel))
    shutil.rmtree(staging, ignore_errors=True)
    return InstallResult([str(current), "--after-update"], skipped)


def install_linux(archive: Path, directory: Path) -> InstallResult:
    staging = directory / STAGING_DIR
    _extract(archive, staging)
    entries = sorted(p.name for p in staging.iterdir())
    if "main.py" not in entries or "telescope" not in entries:
        shutil.rmtree(staging, ignore_errors=True)
        raise UpdateError("The download doesn't contain Telescope, so it wasn't installed.")

    previous = directory / PREVIOUS_DIR
    if previous.exists():
        shutil.rmtree(previous)
    previous.mkdir()
    moved_old, moved_new = [], []
    try:
        for name in entries:
            if (directory / name).exists():
                os.replace(directory / name, previous / name)
                moved_old.append(name)
            os.replace(staging / name, directory / name)
            moved_new.append(name)
    except OSError as exc:
        for name in moved_new:  # put everything back the way it was
            _remove(directory / name)
        for name in moved_old:
            os.replace(previous / name, directory / name)
        raise UpdateError(f"Couldn't replace Telescope's files: {exc.strerror or exc}") from exc
    finally:
        shutil.rmtree(staging, ignore_errors=True)

    start = directory / "start.sh"
    if start.exists():
        start.chmod(0o755)
        return InstallResult([str(start), "--after-update"])
    return InstallResult([sys.executable, str(directory / "main.py"), "--after-update"])


def _remove(path: Path):
    if path.is_dir() and not path.is_symlink():
        shutil.rmtree(path)
    else:
        path.unlink(missing_ok=True)


def install(archive: Path, directory: Optional[Path] = None) -> InstallResult:
    directory = directory or install_dir()
    return install_windows(archive, directory) if IS_WINDOWS else install_linux(archive, directory)


def clean_up_after_update(directory: Optional[Path] = None):
    """Delete what the previous version left behind. Best effort: the old exe may still be exiting."""
    directory = directory or install_dir()
    for leftover in (directory / OLD_EXE_NAME, directory / STAGING_DIR):
        try:
            _remove(leftover)
        except OSError:
            pass
