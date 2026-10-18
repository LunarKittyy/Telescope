"""A second PyAV with VA-API, for AMD and Intel graphics cards on Linux.

PyAV's own wheel leaves VA-API out. Advanced can set one up beside it: the distro's PyAV when it's new enough, or one
built here against the system FFmpeg. It lives in its own folder and is put ahead of the venv's at startup, after a
check in a separate process, so one that's broken can never stop Telescope from starting. Nothing here imports av.
"""

import importlib.metadata
import json
import os
import shutil
import subprocess
import sys
import sysconfig
from pathlib import Path
from typing import Callable, Optional

MIN_VERSION = (14, 0)  # PyAV's hardware decoding came in 14
SYSTEM, BUILD = "system", "build"
_PROBE_TIMEOUT = 20
_PKG_CONFIG_LIBS = ("libavcodec", "libavdevice", "libavfilter", "libavformat", "libavutil", "libswresample", "libswscale")

# Checks a folder's PyAV in a fresh interpreter: loads from there, is new enough, and opens VA-API
_PROBE = r"""
import sys
sys.path.insert(0, sys.argv[1])
import av
if not av.__file__.startswith(sys.argv[1]):
    print("elsewhere"); sys.exit()
print(av.__version__)
from av.codec.hwaccel import HWAccel, hwdevices_available
if "vaapi" not in hwdevices_available():
    print("novaapi"); sys.exit()
try:
    av.CodecContext.create("h264", "r", hwaccel=HWAccel(device_type="vaapi", allow_software_fallback=False))
except Exception:
    print("noopen"); sys.exit()
print("ok")
"""

# What this run uses, set by activate(): {"kind", "version", "path"} or None for the standard one
active: Optional[dict] = None
# Why a PyAV picked in Advanced isn't in use this run, or ""
problem = ""


def root() -> Path:
    return Path(os.environ.get("XDG_DATA_HOME") or Path.home() / ".local" / "share") / "Telescope" / "pyav-vaapi"


def supported() -> bool:
    return sys.platform.startswith("linux") and not getattr(sys, "frozen", False)


def standard_version() -> str:
    """The venv's own PyAV version (what the build matches), read without importing it."""
    try:
        return importlib.metadata.version("av")
    except importlib.metadata.PackageNotFoundError:
        return ""


def _version_tuple(version: str) -> tuple:
    parts = []
    for part in version.split(".")[:2]:
        digits = "".join(c for c in part if c.isdigit())
        parts.append(int(digits) if digits else 0)
    return tuple(parts)


# ── The choice ────────────────────────────────────────────────────────────────

def chosen() -> Optional[dict]:
    """What Advanced picked: {"kind": "system" or "build", "path": folder}, or None."""
    try:
        data = json.loads((root() / "choice.json").read_text())
    except (OSError, ValueError):
        return None
    return data if isinstance(data, dict) and data.get("kind") in (SYSTEM, BUILD) and data.get("path") else None


def choose(kind: str, folder: Path):
    root().mkdir(parents=True, exist_ok=True)
    (root() / "choice.json").write_text(json.dumps({"kind": kind, "path": str(folder)}))


def forget():
    """Back to the standard PyAV from the next start. A build stays, so turning it on again doesn't rebuild."""
    (root() / "choice.json").unlink(missing_ok=True)


# ── Checking one ──────────────────────────────────────────────────────────────

def probe(folder: Path) -> tuple:
    """(ok, version, why not) for the PyAV in folder, checked in a separate process."""
    try:
        out = subprocess.run([sys.executable, "-c", _PROBE, str(folder)], capture_output=True, text=True,
                             timeout=_PROBE_TIMEOUT).stdout.split()
    except (OSError, subprocess.SubprocessError):
        return False, "", "check"
    if not out:
        return False, "", "load"
    if out[0] == "elsewhere":
        return False, "", "load"
    version = out[0]
    if _version_tuple(version) < MIN_VERSION:
        return False, version, "old"
    result = out[1] if len(out) > 1 else "load"
    return result == "ok", version, "" if result == "ok" else result


def activate() -> Optional[dict]:
    """At startup, before anything imports av: put the picked PyAV first on the path if it still checks out."""
    global active, problem
    active, problem = None, ""
    pick = chosen() if supported() else None
    if pick is None:
        return None
    folder = Path(pick["path"])
    if pick["kind"] == BUILD and folder.name != standard_version():
        problem = f"built for PyAV {folder.name}, and this version of Telescope uses {standard_version()}"
        return None
    ok, version, why = probe(folder)
    if not ok:
        problem = {"old": f"PyAV {version} is older than {'.'.join(map(str, MIN_VERSION))}",
                   "novaapi": f"PyAV {version} has no VA-API", "noopen": "VA-API didn't open"}.get(why, "it didn't load")
        return None
    sys.path.insert(0, str(folder))
    active = {"kind": pick["kind"], "version": version, "path": str(folder)}
    return active


# ── The distro's PyAV ─────────────────────────────────────────────────────────

def _system_dirs() -> list:
    """Where the system Python keeps packages, the venv's base first."""
    base = sys.base_prefix
    version = f"python{sys.version_info.major}.{sys.version_info.minor}"
    dirs = []
    for key in ("platlib", "purelib"):
        try:
            dirs.append(sysconfig.get_path(key, vars={"base": base, "platbase": base}))
        except KeyError:
            pass
    for lib in ("lib64", "lib"):
        dirs.append(os.path.join(base, lib, version, "site-packages"))
    dirs.append(os.path.join(base, "lib", "python3", "dist-packages"))  # Debian and Ubuntu
    seen, out = set(), []
    for d in dirs:
        if d and d not in seen:
            seen.add(d)
            out.append(Path(d))
    return out


def find_system() -> Optional[Path]:
    """The distro's av package folder, if it has one."""
    for d in _system_dirs():
        if (d / "av" / "__init__.py").is_file():
            return d / "av"
    return None


def link_system(package: Path) -> Path:
    """A folder holding just the distro's av, so the rest of its packages stay out of Telescope's way."""
    folder = root() / SYSTEM
    folder.mkdir(parents=True, exist_ok=True)
    link = folder / "av"
    if link.is_symlink() or link.exists():
        link.unlink()
    link.symlink_to(package, target_is_directory=True)
    return folder


# ── Building one ──────────────────────────────────────────────────────────────

def missing_tools() -> list:
    """What a build needs that isn't here, in words."""
    missing = []
    if not (shutil.which("cc") or shutil.which("gcc")):
        missing.append("a C compiler")
    if not shutil.which("pkg-config"):
        missing.append("pkg-config")
    elif subprocess.run(["pkg-config", "--exists", *_PKG_CONFIG_LIBS], capture_output=True).returncode != 0:
        missing.append("FFmpeg's development files")
    include = sysconfig.get_paths().get("include", "")
    if not (include and Path(include, "Python.h").is_file()):
        missing.append("Python's development files")
    return missing


def _distro() -> set:
    try:
        text = Path("/etc/os-release").read_text()
    except OSError:
        return set()
    ids = set()
    for line in text.splitlines():
        key, _, value = line.partition("=")
        if key in ("ID", "ID_LIKE"):
            ids.update(value.strip().strip('"').split())
    return ids


def install_command() -> str:
    """The one command that installs what a build needs on this distro, or "" when it's not one we know."""
    ids = _distro()
    if ids & {"fedora", "rhel", "centos"}:
        return "sudo dnf install gcc pkgconf-pkg-config python3-devel ffmpeg-free-devel"
    if ids & {"debian", "ubuntu"}:
        return ("sudo apt install gcc pkg-config python3-dev libavcodec-dev libavdevice-dev libavfilter-dev "
                "libavformat-dev libavutil-dev libswresample-dev libswscale-dev")
    if "arch" in ids:
        return "sudo pacman -S --needed gcc pkgconf ffmpeg"
    return ""


def build(version: str, on_line: Callable[[str], None]) -> tuple:
    """Build PyAV `version` against the system FFmpeg into its own folder. (ok, message); runs for a minute or two."""
    final = root() / version
    work = root() / f".building-{version}"
    shutil.rmtree(work, ignore_errors=True)
    work.parent.mkdir(parents=True, exist_ok=True)
    # No cache: a wheel built earlier may be linked to an FFmpeg the system has since replaced
    cmd = [sys.executable, "-m", "pip", "install", "--disable-pip-version-check", "--no-cache-dir", "--no-deps",
           "--no-binary", "av", "--target", str(work), f"av=={version}"]
    tail = []
    try:
        proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        for line in proc.stdout:
            line = line.strip()
            if line:
                tail = (tail + [line])[-20:]
                on_line(line)
        proc.wait()
    except OSError as exc:
        return False, str(exc)
    if proc.returncode != 0:
        shutil.rmtree(work, ignore_errors=True)
        return False, next((l for l in reversed(tail) if "error" in l.lower()), tail[-1] if tail else "pip failed")
    ok, _, why = probe(work)
    if not ok:
        shutil.rmtree(work, ignore_errors=True)
        return False, "it built, but VA-API didn't open with it" if why in ("novaapi", "noopen") else "it built, but didn't load"
    shutil.rmtree(final, ignore_errors=True)
    work.rename(final)
    return True, str(final)
