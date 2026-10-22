"""The vision pack: MediaPipe and its models as one download from the release, checked and unpacked. No Qt here."""
import hashlib
import http.client
import json
import logging
import os
import platform
import shutil
import sys
import urllib.request
import zipfile
from pathlib import Path
from typing import Callable, Optional

from telescope import dev_profile, version, web

# No OpenCV or MediaPipe in here: the release builds the pack with a bare Python (scripts/build_vision_pack.py)
SEGMENTER_MODEL = "selfie_segmenter_landscape.tflite"
POSE_MODEL = "pose_landmarker_lite.task"


def default_dir() -> Path:
    """Where vision packs are unpacked, one folder per pack."""
    if dev_profile.active():
        return dev_profile.folder() / "vision"
    if sys.platform == "win32":
        base = os.environ.get("LOCALAPPDATA") or str(Path.home())
        return Path(base) / "Telescope" / "vision"
    base = os.environ.get("XDG_DATA_HOME") or str(Path.home() / ".local" / "share")
    return Path(base) / "telescope" / "vision"

logger = logging.getLogger(__name__)

PACK_INFO = Path(__file__).with_name("vision_pack.json")  # written by scripts/build_vision_pack.py --write
_DONE = ".verified"  # written last, holding the pack's id: a folder without it is a download that didn't finish
_TIMEOUT = 30
_SLACK = 1.1  # zips of the same files can come out a little bigger on another machine's zlib


class PackError(Exception):
    """Why the pack couldn't be downloaded or unpacked, said for the person who clicked Download."""


def platform_key() -> Optional[str]:
    """Which pack this computer needs, or None when there isn't one for it (only 64-bit Intel/AMD for now)."""
    if platform.machine().lower() not in ("x86_64", "amd64"):
        return None
    return "windows" if sys.platform == "win32" else "linux" if sys.platform.startswith("linux") else None


def expected(info: Optional[dict] = None) -> Optional[dict]:
    """This platform's entry in vision_pack.json: {"id", "size", "files"}, or None without one."""
    key = platform_key()
    if info is None:
        try:
            info = json.loads(PACK_INFO.read_text())
        except (OSError, ValueError):
            return None
    entry = info.get(key) if key and isinstance(info, dict) else None
    return entry if isinstance(entry, dict) and isinstance(entry.get("id"), str) else None


def pack_id() -> Optional[str]:
    entry = expected()
    return entry["id"][:16] if entry else None


def asset_name() -> Optional[str]:
    key, pid = platform_key(), pack_id()
    return f"Telescope-vision-{key}-{pid}.zip" if key and pid else None


def download_size() -> int:
    entry = expected()
    return int(entry.get("size", 0)) if entry else 0


def tree_hash(folder: Path) -> str:
    """What's in a folder, as one sha256 over each file's path and sha256 (the zip itself can differ by zlib)."""
    lines = []
    # WindowsPath sorts without case; use the same case-sensitive component order as PosixPath.
    for path in sorted((p for p in folder.rglob("*") if p.is_file()),
                       key=lambda p: p.relative_to(folder).parts):
        rel = path.relative_to(folder).as_posix()
        if rel == _DONE:
            continue
        digest = hashlib.sha256()
        with path.open("rb") as f:
            for chunk in iter(lambda: f.read(1024 * 1024), b""):
                digest.update(chunk)
        lines.append(f"{rel}\0{digest.hexdigest()}\n")
    return hashlib.sha256("".join(lines).encode()).hexdigest()


def installed(vision_dir: Path) -> bool:
    pid = pack_id()
    if not pid:
        return False
    try:
        return (vision_dir / pid / _DONE).read_text().strip() == pid
    except OSError:
        return False


def _open(url: str, timeout: float):
    req = urllib.request.Request(url, headers={"User-Agent": f"Telescope/{version.VERSION}"})
    return web.urlopen(req, timeout)


def download(vision_dir: Path, progress: Optional[Callable[[int, int], None]] = None,
             cancelled: Callable[[], bool] = lambda: False, opener: Callable = _open,
             url: Optional[str] = None) -> Path:
    """Download this version's pack from its release, check it and unpack it; raises PackError. Older packs go."""
    entry, name, pid = expected(), asset_name(), pack_id()
    if entry is None or name is None:
        raise PackError("There's no download for this computer yet.")
    url = url or version.release_asset_url(name)
    size = int(entry.get("size", 0))
    vision_dir.mkdir(parents=True, exist_ok=True)
    archive = vision_dir / f".{pid}.zip.part"
    staging = vision_dir / f".{pid}.unpacking"
    done = 0
    try:
        with opener(url, _TIMEOUT) as r, archive.open("wb") as f:
            while True:
                if cancelled():
                    raise PackError("Cancelled.")
                chunk = r.read(256 * 1024)
                if not chunk:
                    break
                done += len(chunk)
                if size and done > size * _SLACK:
                    raise PackError("The download is bigger than it should be.")
                f.write(chunk)
                if progress:
                    progress(done, size)
        _unpack(archive, staging)
        if tree_hash(staging) != entry["id"]:
            raise PackError("The download didn't match what this version of Telescope expects, so it wasn't used.")
        (staging / _DONE).write_text(pid)
        final = vision_dir / pid
        if final.exists():
            shutil.rmtree(final)
        os.replace(staging, final)
    except PackError:
        raise
    except (OSError, http.client.HTTPException) as exc:  # HTTPException: cut off part way (IncompleteRead)
        logger.warning("Vision pack download failed: %s", exc)
        raise PackError("The download failed. Check the internet connection and try again.") from exc
    finally:
        archive.unlink(missing_ok=True)
        shutil.rmtree(staging, ignore_errors=True)
    remove(vision_dir, keep=pid)
    return final


def _unpack(archive: Path, staging: Path):
    shutil.rmtree(staging, ignore_errors=True)
    staging.mkdir(parents=True)
    root = staging.resolve()
    try:
        with zipfile.ZipFile(archive) as z:
            for info in z.infolist():
                target = (root / info.filename).resolve()
                if root not in target.parents:
                    raise PackError("The download contains unexpected paths, so it wasn't used.")
            z.extractall(staging)
    except zipfile.BadZipFile as exc:
        raise PackError("The download couldn't be unpacked.") from exc


def remove(vision_dir: Path, keep: Optional[str] = None):
    """Delete the packs (and leftovers of unfinished downloads), all but keep."""
    if not vision_dir.is_dir():
        return
    for entry in vision_dir.iterdir():
        if entry.name == keep:
            continue
        try:
            shutil.rmtree(entry) if entry.is_dir() else entry.unlink()
        except OSError:
            logger.warning("Couldn't remove %s", entry)
