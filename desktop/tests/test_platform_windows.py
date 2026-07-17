import hashlib
import subprocess
import sys
import types
from pathlib import Path

import pytest

import telescope.platform.windows as windows


def test_unitycapture_dir_frozen_and_source(monkeypatch, tmp_path):
    monkeypatch.setattr(windows.sys, "frozen", True, raising=False)
    monkeypatch.setattr(windows.sys, "executable", str(tmp_path / "Telescope.exe"))
    assert windows.unitycapture_dir() == tmp_path / "unitycapture"

    monkeypatch.delattr(windows.sys, "frozen", raising=False)
    # Anchored on this test file's own location (desktop/tests/..) rather than
    # a literal "desktop" name, so it holds regardless of the checkout's folder name.
    desktop_root = Path(__file__).resolve().parent.parent
    assert windows.unitycapture_dir() == desktop_root / "unitycapture"


def test_sha256_reads_entire_file(tmp_path):
    path = tmp_path / "large.bin"
    data = b"a" * ((1 << 20) + 17)
    path.write_bytes(data)
    assert windows._sha256(path) == hashlib.sha256(data).hexdigest()


def test_download_unitycapture_verifies_both_files(monkeypatch, tmp_path):
    progress = []
    payloads = {"32": b"thirty-two", "64": b"sixty-four"}
    monkeypatch.setattr(windows, "unitycapture_dir", lambda: tmp_path)
    monkeypatch.setattr(
        windows,
        "_EXPECTED_SHA256",
        {f"UnityCaptureFilter{bits}.dll": hashlib.sha256(data).hexdigest()
         for bits, data in payloads.items()},
    )

    def retrieve(url, dest):
        bits = "32" if "32.dll" in url else "64"
        Path(dest).write_bytes(payloads[bits])

    monkeypatch.setattr(windows.urllib.request, "urlretrieve", retrieve)

    assert windows.download_unitycapture(progress.append) == (True, "Downloaded")
    assert progress == [
        "Downloading UnityCaptureFilter32.dll...",
        "Downloading UnityCaptureFilter64.dll...",
    ]


def test_download_unitycapture_handles_network_and_checksum_failures(monkeypatch, tmp_path):
    monkeypatch.setattr(windows, "unitycapture_dir", lambda: tmp_path)
    monkeypatch.setattr(
        windows.urllib.request,
        "urlretrieve",
        lambda *_args: (_ for _ in ()).throw(OSError("offline")),
    )
    ok, msg = windows.download_unitycapture()
    assert ok is False
    assert "offline" in msg

    monkeypatch.setattr(
        windows.urllib.request,
        "urlretrieve",
        lambda _url, dest: Path(dest).write_bytes(b"wrong"),
    )
    ok, msg = windows.download_unitycapture()
    assert ok is False
    assert "checksum" in msg
    assert not (tmp_path / "UnityCaptureFilter32.dll").exists()


def test_register_refuses_missing_or_tampered_files(monkeypatch, tmp_path):
    monkeypatch.setattr(windows, "unitycapture_dir", lambda: tmp_path)
    ok, msg = windows.register_unitycapture()
    assert ok is False
    assert "checksum" in msg


def test_register_invokes_elevated_powershell(monkeypatch, tmp_path):
    payloads = {name: name.encode() for name in windows._EXPECTED_SHA256}
    expected = {name: hashlib.sha256(data).hexdigest() for name, data in payloads.items()}
    for name, data in payloads.items():
        (tmp_path / name).write_bytes(data)
    monkeypatch.setattr(windows, "unitycapture_dir", lambda: tmp_path)
    monkeypatch.setattr(windows, "_EXPECTED_SHA256", expected)
    calls = []
    monkeypatch.setattr(
        windows.subprocess,
        "run",
        lambda cmd, **kwargs: calls.append((cmd, kwargs)) or subprocess.CompletedProcess(cmd, 0),
    )

    assert windows.register_unitycapture() == (True, "Installed")
    assert calls[0][0][:3] == ["powershell", "-NoProfile", "-Command"]
    assert "regsvr32" in calls[0][0][-1]
    assert calls[0][1]["timeout"] == 60


@pytest.mark.parametrize(
    "effect,expected",
    [
        (subprocess.CompletedProcess([], 1), (False, "Registration failed (cancelled or denied?)")),
        (subprocess.TimeoutExpired([], 60), (False, "Timed out")),
        (OSError("powershell missing"), (False, "powershell missing")),
    ],
)
def test_register_normalizes_process_failures(monkeypatch, tmp_path, effect, expected):
    payloads = {name: name.encode() for name in windows._EXPECTED_SHA256}
    expected_hashes = {name: hashlib.sha256(data).hexdigest() for name, data in payloads.items()}
    for name, data in payloads.items():
        (tmp_path / name).write_bytes(data)
    monkeypatch.setattr(windows, "unitycapture_dir", lambda: tmp_path)
    monkeypatch.setattr(windows, "_EXPECTED_SHA256", expected_hashes)

    def run(*_args, **_kwargs):
        if isinstance(effect, BaseException):
            raise effect
        return effect

    monkeypatch.setattr(windows.subprocess, "run", run)
    assert windows.register_unitycapture() == expected


def test_uc_is_registered_scans_registry_and_handles_absence(monkeypatch, tmp_path):
    dll = tmp_path / "UnityCaptureFilter64.dll"
    monkeypatch.setattr(windows, "unitycapture_dir", lambda: tmp_path)

    class Key:
        def __init__(self, path):
            self.path = path

        def __enter__(self):
            return self

        def __exit__(self, *_args):
            pass

    fake = types.SimpleNamespace(HKEY_CLASSES_ROOT="HKCR")
    fake.OpenKey = lambda _root, path: Key(path)
    fake.EnumKey = lambda _root, idx: ["first", "second"][idx] if idx < 2 else (_ for _ in ()).throw(OSError())
    fake.QueryValueEx = lambda key, _name: (
        str(dll) if key.path.startswith("second") else "other.dll",
        None,
    )
    monkeypatch.setitem(sys.modules, "winreg", fake)
    assert windows.uc_is_registered() is True

    fake.OpenKey = lambda *_args: (_ for _ in ()).throw(OSError("no registry"))
    assert windows.uc_is_registered() is False
