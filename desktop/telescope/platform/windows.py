import subprocess
import sys
import urllib.request
from pathlib import Path

from telescope.platform import _run

UNITYCAPTURE_URL_BASE = "https://github.com/schellingb/UnityCapture/raw/master/Install"


def unitycapture_dir() -> Path:
    if getattr(sys, "frozen", False):
        return Path(sys.executable).parent / "unitycapture"
    return Path(__file__).parent.parent.parent / "unitycapture"


def download_unitycapture(progress_cb=None) -> tuple:
    d = unitycapture_dir()
    d.mkdir(parents=True, exist_ok=True)
    for bits in ("32", "64"):
        url  = f"{UNITYCAPTURE_URL_BASE}/UnityCaptureFilter{bits}.dll"
        dest = d / f"UnityCaptureFilter{bits}.dll"
        try:
            if progress_cb:
                progress_cb(f"Downloading {dest.name}...")
            urllib.request.urlretrieve(url, dest)
        except Exception as e:
            return False, f"Download failed: {e}"
    return True, "Downloaded"


def register_unitycapture() -> tuple:
    d = unitycapture_dir()
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
