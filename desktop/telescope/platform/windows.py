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

from telescope.platform import NO_WINDOW

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


def _known_folder(folder_id: str) -> Optional[Path]:
    # The system's own setting, unlike %ProgramFiles%, which whoever starts Telescope can point anywhere
    if sys.platform != "win32":
        return None
    import ctypes
    from ctypes import wintypes

    class GUID(ctypes.Structure):
        _fields_ = [("Data1", wintypes.DWORD), ("Data2", wintypes.WORD), ("Data3", wintypes.WORD),
                    ("Data4", ctypes.c_ubyte * 8)]
    raw = bytes.fromhex(folder_id.replace("-", ""))
    guid = GUID(int.from_bytes(raw[0:4], "big"), int.from_bytes(raw[4:6], "big"), int.from_bytes(raw[6:8], "big"),
                (ctypes.c_ubyte * 8)(*raw[8:]))
    out = ctypes.c_wchar_p()
    if ctypes.windll.shell32.SHGetKnownFolderPath(ctypes.byref(guid), 0, None, ctypes.byref(out)) != 0:
        return None
    try:
        return Path(out.value)
    finally:
        ctypes.windll.ole32.CoTaskMemFree(out)


_FOLDERID_PROGRAM_FILES = "905e63b6-c1bf-494e-b29c-65b732d3d21a"


def protected_unitycapture_dir() -> Path:
    # Only an admin can write here, unlike the app's folder, so nothing can swap a DLL between the hash check and every later load
    base = _known_folder(_FOLDERID_PROGRAM_FILES) or Path(r"C:\Program Files")
    return base / "Telescope" / "UnityCapture"


def _ps_quote(value) -> str:
    return "'" + str(value).replace("'", "''") + "'"


def _elevated_install_script(src: Path, devices: int = 1, reset: bool = False) -> str:
    """Runs as admin: find Program Files itself, refuse a Telescope folder there that isn't admin-only all the way down, copy the DLLs in, check their hashes there, register those copies."""
    files = "; ".join(f"{_ps_quote(name)} = {_ps_quote(digest)}" for name, digest in _EXPECTED_SHA256.items())
    # UnityCapture reads both from regsvr32's command line; the extra cameras get "Telescope #2" and on
    count = f"'/i:UnityCaptureDevices={int(devices)}', " if devices > 1 else ""
    # Registering never takes cameras away, so going back to fewer unregisters them all first
    unregister = "$p = Start-Process $regsvr32 -ArgumentList '/s', '/u', $dll -Wait -PassThru\n    " if reset else ""
    return f"""$ErrorActionPreference = 'Stop'
# Anything unexpected gets its own exit code, so it isn't mistaken for a cancelled admin prompt
trap {{ exit {_EXIT_ERROR} }}
# Started from PowerShell 7, this inherits its module path and can't load its own modules: use the system's
$env:PSModulePath = [Environment]::GetEnvironmentVariable('PSModulePath', 'Machine')
# .NET directly rather than Get-FileHash and Get-Acl, so nothing here depends on loading a module
function Get-Sha256($path) {{
    $stream = [IO.File]::OpenRead($path)
    try {{ return -join ([Security.Cryptography.SHA256]::Create().ComputeHash($stream) | ForEach-Object {{ $_.ToString('X2') }}) }}
    finally {{ $stream.Dispose() }}
}}
$src = {_ps_quote(src)}
# Resolved here, as admin, from the system's settings: nothing the unelevated app passes decides where the DLLs go
$telescope = Join-Path ([Environment]::GetFolderPath('ProgramFiles')) 'Telescope'
$dst = Join-Path $telescope 'UnityCapture'
$files = [ordered]@{{ {files} }}
$icacls = Join-Path ([Environment]::SystemDirectory) 'icacls.exe'
$sid = [Security.Principal.SecurityIdentifier]
# Administrators, SYSTEM, TrustedInstaller
$trusted = @('S-1-5-32-544', 'S-1-5-18', 'S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464')
# Write data or attributes, delete, change permissions or owner, and generic all/write
$writes = 0x2 -bor 0x4 -bor 0x10 -bor 0x40 -bor 0x100 -bor 0x10000 -bor 0x40000 -bor 0x80000 -bor 0x10000000 -bor 0x40000000

# Admin-owned, no link anywhere, and nobody else can change it: checked, never repaired, so a link is never followed
function Test-AdminOnly($path) {{
    $item = Get-Item -LiteralPath $path -Force
    if (([int]$item.Attributes -band 0x400) -ne 0) {{ return $false }}  # FILE_ATTRIBUTE_REPARSE_POINT
    $acl = $item.GetAccessControl()
    if ($trusted -notcontains $acl.GetOwner($sid).Value) {{ return $false }}
    foreach ($rule in $acl.GetAccessRules($true, $true, $sid)) {{
        if ($rule.AccessControlType -ne 'Allow' -or ([int]$rule.PropagationFlags -band 2) -ne 0) {{ continue }}  # 2: InheritOnly
        if (([int]$rule.FileSystemRights -band $writes) -ne 0 -and $trusted -notcontains $rule.IdentityReference.Value) {{ return $false }}
    }}
    if ($item.PSIsContainer) {{
        foreach ($child in [IO.Directory]::EnumerateFileSystemEntries($path)) {{
            if (-not (Test-AdminOnly $child)) {{ return $false }}
        }}
    }}
    return $true
}}

# Only for what this script just made: Windows can be set to make the creating account the owner instead
function Set-AdminOwner($path) {{
    & $icacls $path /setowner '*S-1-5-32-544' /Q | Out-Null
    if ($LASTEXITCODE -ne 0) {{ exit {_EXIT_UNSAFE_FOLDER} }}
}}

if ((Test-Path -LiteralPath $telescope) -and -not (Test-AdminOnly $telescope)) {{ exit {_EXIT_UNSAFE_FOLDER} }}
foreach ($dir in @($telescope, $dst)) {{
    if (-not (Test-Path -LiteralPath $dir)) {{
        New-Item -ItemType Directory -Path $dir | Out-Null
        Set-AdminOwner $dir
    }}
}}
foreach ($name in $files.Keys) {{
    $target = Join-Path $dst $name
    # A copy already there and right is kept: a camera app may have it loaded, so it can't be overwritten
    if (-not (Test-Path -LiteralPath $target) -or (Get-Sha256 $target) -ne $files[$name]) {{
        Copy-Item -LiteralPath (Join-Path $src $name) -Destination $target -Force
        Set-AdminOwner $target
    }}
}}
# Again over the finished tree, so nothing that changed while it was being made gets registered
if (-not (Test-AdminOnly $telescope)) {{ exit {_EXIT_UNSAFE_FOLDER} }}
foreach ($name in $files.Keys) {{
    $target = Join-Path $dst $name
    if ((Get-Sha256 $target) -ne $files[$name]) {{
        Remove-Item -LiteralPath $target -Force
        exit {_EXIT_CHECKSUM}
    }}
}}
$regsvr32 = Join-Path ([Environment]::SystemDirectory) 'regsvr32.exe'
foreach ($name in $files.Keys) {{
    $dll = '"' + (Join-Path $dst $name) + '"'
    {unregister}$p = Start-Process $regsvr32 -ArgumentList '/s', {count}'"/i:UnityCaptureName={UC_NAME}"', $dll -Wait -PassThru
    if ($p.ExitCode -ne 0) {{ exit {_EXIT_REGSVR32} }}
}}
exit 0
"""


def _system_dir() -> Optional[Path]:
    # From Windows itself rather than PATH or %SystemRoot%, so the PowerShell started is the real one
    if sys.platform != "win32":
        return None
    import ctypes
    buf = ctypes.create_unicode_buffer(260)
    n = ctypes.windll.kernel32.GetSystemDirectoryW(buf, len(buf))
    return Path(buf.value) if 0 < n < len(buf) else None


def _powershell() -> str:
    system = _system_dir()
    return str(system / "WindowsPowerShell" / "v1.0" / "powershell.exe") if system else "powershell"


_EXIT_CHECKSUM = 2
_EXIT_REGSVR32 = 3
_EXIT_UNSAFE_FOLDER = 4
_EXIT_ERROR = 5


def register_unitycapture(devices: int = 1, reset: bool = False) -> tuple:
    """Install and register the driver as admin; devices > 1 also registers that many cameras to stream to at once.
    reset: unregister every camera first, to go back to fewer."""
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
    script = _elevated_install_script(d, devices, reset)
    encoded = base64.b64encode(script.encode("utf-16-le")).decode("ascii")
    ps = ("$ErrorActionPreference = 'Stop'; "
          "$ps = Join-Path ([Environment]::SystemDirectory) 'WindowsPowerShell\\v1.0\\powershell.exe'; "
          f"$p = Start-Process $ps -ArgumentList '-NoProfile', '-NonInteractive', '-EncodedCommand', '{encoded}' "
          "-Verb RunAs -Wait -PassThru -WindowStyle Hidden; exit $p.ExitCode")
    try:
        r = subprocess.run(
            [_powershell(), "-NoProfile", "-Command", ps],
            capture_output=True, timeout=60, **NO_WINDOW,
        )
        if r.returncode == 0:
            return True, "Installed"
        if r.returncode == _EXIT_CHECKSUM:
            return False, "The driver copy in Program Files failed checksum verification - not registering"
        if r.returncode == _EXIT_ERROR:
            return False, "The driver install hit an error while running as admin - not registering"
        if r.returncode == _EXIT_UNSAFE_FOLDER:
            return False, "Program Files\\Telescope has links in it or other users can change it - delete that folder, then install again"
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
