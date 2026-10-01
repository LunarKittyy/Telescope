import base64
import hashlib
import os
import re
import shutil
import subprocess
import sys
import urllib.request
from pathlib import Path
from typing import Optional

from telescope.platform import NO_WINDOW, _run

# Pinned commit with hash verification to prevent tampering before registration.
_UNITYCAPTURE_COMMIT = "3ed54c325e0ad71afcf4f246c07e5e17b3d7f2d2"
UNITYCAPTURE_URL_BASE = f"https://raw.githubusercontent.com/schellingb/UnityCapture/{_UNITYCAPTURE_COMMIT}/Install"

# What apps list the camera as. UnityCapture takes it at registration (its InstallCustomName.bat).
UC_NAME = "Telescope"
UC_DEFAULT_NAME = "Unity Video Capture"

_EXPECTED_SHA256 = {
    "UnityCaptureFilter32.dll": "aa3ebdf03dea7f3aab3dd7b724751f49ed71672256b57c6a19aa6809cabf30ba",
    "UnityCaptureFilter64.dll": "72812f5363d8ecb45632253f8c8c888844b1b62e27616f3c8cc21064ccde25e5",
}


def unitycapture_dir() -> Path:
    if getattr(sys, "frozen", False):
        return Path(sys.executable).parent / "unitycapture"
    return Path(__file__).parent.parent.parent / "unitycapture"


def _sha256(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def unitycapture_downloaded(d: Path) -> bool:
    # Both, since a bad one is deleted on its own: checking one would skip a download the other still needs
    return all((d / name).exists() for name in _EXPECTED_SHA256)


def download_unitycapture(progress_cb=None) -> tuple:
    d = unitycapture_dir()
    try:
        d.mkdir(parents=True, exist_ok=True)
    except OSError as e:
        return False, f"Couldn't create {d}: {e}"
    for bits in ("32", "64"):
        name = f"UnityCaptureFilter{bits}.dll"
        url  = f"{UNITYCAPTURE_URL_BASE}/{name}"
        dest = d / name
        part = d / (name + ".part")  # a cut-off download never sits where the callers look for a finished one
        try:
            if progress_cb:
                progress_cb(f"Downloading {name}...")
            with urllib.request.urlopen(url, timeout=30) as r, open(part, "wb") as f:  # urlretrieve never times out
                shutil.copyfileobj(r, f)
            digest = _sha256(part)
            if digest != _EXPECTED_SHA256[name]:
                return False, f"{name} failed checksum verification (got {digest[:12]}...) - not registering"
            os.replace(part, dest)
        except Exception as e:
            return False, f"Download failed: {e}"
        finally:
            try:
                part.unlink(missing_ok=True)
            except OSError:
                pass
    return True, "Downloaded"


def protected_unitycapture_dir() -> Path:
    # Only an admin can write here, unlike the app's folder, so nothing can swap a DLL between the hash check and every later load
    base = os.environ.get("ProgramW6432") or os.environ.get("ProgramFiles") or r"C:\Program Files"
    return Path(base) / "Telescope" / "UnityCapture"


def _ps_quote(value) -> str:
    return "'" + str(value).replace("'", "''") + "'"


def _elevated_install_script(src: Path, dst: Path) -> str:
    """Runs as admin: copy the DLLs into dst, check the copies' hashes there, register those copies."""
    files = "; ".join(f"{_ps_quote(name)} = {_ps_quote(digest)}" for name, digest in _EXPECTED_SHA256.items())
    return f"""$ErrorActionPreference = 'Stop'
$src = {_ps_quote(src)}
$dst = {_ps_quote(dst)}
$files = [ordered]@{{ {files} }}
New-Item -ItemType Directory -Force -Path $dst | Out-Null
foreach ($name in $files.Keys) {{
    $target = Join-Path $dst $name
    # A copy already there and right is kept: a camera app may have it loaded, so it can't be overwritten
    if (-not (Test-Path -LiteralPath $target) -or (Get-FileHash -Algorithm SHA256 -LiteralPath $target).Hash -ne $files[$name]) {{
        Copy-Item -LiteralPath (Join-Path $src $name) -Destination $target -Force
    }}
    if ((Get-FileHash -Algorithm SHA256 -LiteralPath $target).Hash -ne $files[$name]) {{
        Remove-Item -LiteralPath $target -Force
        exit {_EXIT_CHECKSUM}
    }}
}}
foreach ($name in $files.Keys) {{
    $dll = '"' + (Join-Path $dst $name) + '"'
    $p = Start-Process regsvr32.exe -ArgumentList '/s', '"/i:UnityCaptureName={UC_NAME}"', $dll -Wait -PassThru
    if ($p.ExitCode -ne 0) {{ exit {_EXIT_REGSVR32} }}
}}
exit 0
"""


_EXIT_CHECKSUM = 2
_EXIT_REGSVR32 = 3


def register_unitycapture() -> tuple:
    d = unitycapture_dir()
    for name, expected in _EXPECTED_SHA256.items():
        path = d / name
        if not path.exists() or _sha256(path) != expected:
            try:
                path.unlink(missing_ok=True)  # so "Try again" downloads it afresh instead of failing the same way
            except OSError:
                pass
            return False, f"{name} failed checksum verification - not registering"
    # The check above is only for a quick, clear failure; the one that counts runs as admin on the protected copy
    script = _elevated_install_script(d, protected_unitycapture_dir())
    encoded = base64.b64encode(script.encode("utf-16-le")).decode("ascii")
    ps = ("$ErrorActionPreference = 'Stop'; "
          f"$p = Start-Process powershell.exe -ArgumentList '-NoProfile', '-NonInteractive', '-EncodedCommand', '{encoded}' "
          "-Verb RunAs -Wait -PassThru -WindowStyle Hidden; exit $p.ExitCode")
    try:
        r = subprocess.run(
            ["powershell", "-NoProfile", "-Command", ps],
            capture_output=True, timeout=60, **NO_WINDOW,
        )
        if r.returncode == 0:
            return True, "Installed"
        if r.returncode == _EXIT_CHECKSUM:
            return False, "The driver copy in Program Files failed checksum verification - not registering"
        if r.returncode == _EXIT_REGSVR32:
            return False, "Windows refused to register the driver (regsvr32 failed)"
        return False, "Registration failed (cancelled or denied?)"
    except subprocess.TimeoutExpired:
        return False, "Timed out"
    except Exception as e:
        return False, str(e)


def _uc_registration() -> Optional[tuple]:
    """(name apps list it as, folder its DLL is in) for Telescope's UnityCapture filter, or None if it isn't registered."""
    try:
        import winreg
        folders = {str(f / "UnityCaptureFilter64.dll").lower(): f
                   for f in (unitycapture_dir(), protected_unitycapture_dir())}
        with winreg.OpenKey(winreg.HKEY_CLASSES_ROOT, "CLSID") as clsid_root:
            i = 0
            while True:
                try:
                    clsid = winreg.EnumKey(clsid_root, i)
                    try:
                        with winreg.OpenKey(clsid_root, f"{clsid}\\InprocServer32") as k:
                            val, _ = winreg.QueryValueEx(k, "")
                        if val.lower() in folders:
                            # The filter's own key carries its display name as the default value.
                            try:
                                with winreg.OpenKey(clsid_root, clsid) as k:
                                    name, _ = winreg.QueryValueEx(k, "")
                            except OSError:
                                name = ""
                            if not name.endswith(" Configuration"):  # the property page shares the DLL
                                return name or UC_DEFAULT_NAME, folders[val.lower()]
                    except OSError:
                        pass
                    i += 1
                except OSError:
                    break
    except Exception:
        pass
    return None


def uc_registered_name() -> Optional[str]:
    """The name Telescope's UnityCapture filter is registered under, or None if it isn't registered."""
    reg = _uc_registration()
    return reg[0] if reg else None


def uc_in_app_folder() -> bool:
    """Registered by an older Telescope straight from the app's folder, which this user's programs can write to."""
    reg = _uc_registration()
    return reg is not None and reg[1] != protected_unitycapture_dir()


def uc_is_registered() -> bool:
    return uc_registered_name() is not None


# The temp folders zip tools run a file from: Explorer's Temp1_<name>.zip, 7-Zip's 7zO..., WinRAR's Rar$EX...
_ARCHIVE_TEMP = re.compile(r"(temp\d+_.*\.zip|7zo\w+|rar\$ex\w*\..*)$", re.I)


def running_from_archive(app_dir: Path, temp_dir: Path) -> bool:
    """Whether Telescope was opened from inside a zip, so it runs from a copy in the temp folder that goes away."""
    try:
        rel = Path(app_dir).resolve().relative_to(Path(temp_dir).resolve())
    except ValueError:
        return False
    return any(_ARCHIVE_TEMP.match(part) for part in rel.parts)


def warn_running_from_archive():
    import ctypes
    text = ("Telescope is running from inside the zip, so it can't keep its files or update itself.\n\n"
            "Right-click the zip, choose Extract All, and open Telescope from the extracted folder.")
    ctypes.windll.user32.MessageBoxW(None, text, "Extract Telescope first", 0x30)  # MB_ICONWARNING
