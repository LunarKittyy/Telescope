import hashlib
import subprocess
import sys
import urllib.request
from pathlib import Path

from telescope.platform import _run

# Pinned to a specific commit (not the mutable master branch) and verified by
# hash below, so a compromised or rewritten upstream branch can't silently
# swap in a different binary before it's registered with admin rights.
_UNITYCAPTURE_COMMIT = "3ed54c325e0ad71afcf4f246c07e5e17b3d7f2d2"
UNITYCAPTURE_URL_BASE = f"https://raw.githubusercontent.com/schellingb/UnityCapture/{_UNITYCAPTURE_COMMIT}/Install"

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


def download_unitycapture(progress_cb=None) -> tuple:
    d = unitycapture_dir()
    d.mkdir(parents=True, exist_ok=True)
    for bits in ("32", "64"):
        name = f"UnityCaptureFilter{bits}.dll"
        url  = f"{UNITYCAPTURE_URL_BASE}/{name}"
        dest = d / name
        try:
            if progress_cb:
                progress_cb(f"Downloading {name}...")
            urllib.request.urlretrieve(url, dest)
        except Exception as e:
            return False, f"Download failed: {e}"
        digest = _sha256(dest)
        if digest != _EXPECTED_SHA256[name]:
            dest.unlink(missing_ok=True)
            return False, f"{name} failed checksum verification (got {digest[:12]}...) - not registering"
    return True, "Downloaded"


def register_unitycapture() -> tuple:
    d = unitycapture_dir()
    for name, expected in _EXPECTED_SHA256.items():
        path = d / name
        if not path.exists() or _sha256(path) != expected:
            return False, f"{name} failed checksum verification - not registering"
    dll32 = str(d / "UnityCaptureFilter32.dll")
    dll64 = str(d / "UnityCaptureFilter64.dll")
    ps = (
        'Start-Process cmd.exe '
        f'-ArgumentList \'/c regsvr32 /s "{dll32}" && regsvr32 /s "{dll64}"\' '
        '-Verb RunAs -Wait -WindowStyle Hidden'
    )
    try:
        r = subprocess.run(
            ["powershell", "-NoProfile", "-Command", ps],
            capture_output=True, timeout=60,
        )
        if r.returncode == 0:
            return True, "Installed"
        return False, "Registration failed (cancelled or denied?)"
    except subprocess.TimeoutExpired:
        return False, "Timed out"
    except Exception as e:
        return False, str(e)


def uc_is_registered() -> bool:
    try:
        import winreg
        dll = str(unitycapture_dir() / "UnityCaptureFilter64.dll").lower()
        with winreg.OpenKey(winreg.HKEY_CLASSES_ROOT, "CLSID") as clsid_root:
            i = 0
            while True:
                try:
                    clsid = winreg.EnumKey(clsid_root, i)
                    try:
                        with winreg.OpenKey(clsid_root, f"{clsid}\\InprocServer32") as k:
                            val, _ = winreg.QueryValueEx(k, "")
                            if val.lower() == dll:
                                return True
                    except OSError:
                        pass
                    i += 1
                except OSError:
                    break
    except Exception:
        pass
    return False
