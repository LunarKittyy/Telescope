import io
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
    # Anchored to test file location, not literal "desktop", to work with any checkout folder name.
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

    def fetch(url, timeout):
        assert timeout
        return io.BytesIO(payloads["32" if "32.dll" in url else "64"])

    monkeypatch.setattr(windows.urllib.request, "urlopen", fetch)

    assert windows.download_unitycapture(progress.append) == (True, "Downloaded")
    assert progress == [
        "Downloading UnityCaptureFilter32.dll...",
        "Downloading UnityCaptureFilter64.dll...",
    ]


def test_download_unitycapture_handles_network_and_checksum_failures(monkeypatch, tmp_path):
    monkeypatch.setattr(windows, "unitycapture_dir", lambda: tmp_path)
    monkeypatch.setattr(
        windows.urllib.request,
        "urlopen",
        lambda *_args, **_kw: (_ for _ in ()).throw(OSError("offline")),
    )
    ok, msg = windows.download_unitycapture()
    assert ok is False
    assert "offline" in msg

    monkeypatch.setattr(
        windows.urllib.request,
        "urlopen",
        lambda _url, timeout: io.BytesIO(b"wrong"),
    )
    ok, msg = windows.download_unitycapture()
    assert ok is False
    assert "checksum" in msg
    assert not (tmp_path / "UnityCaptureFilter32.dll").exists()


def test_a_cut_off_download_leaves_nothing_for_the_next_try_to_trip_over(monkeypatch, tmp_path):
    monkeypatch.setattr(windows, "unitycapture_dir", lambda: tmp_path)

    class Cut(io.BytesIO):
        def read(self, *_a):
            raise OSError("connection reset")

    monkeypatch.setattr(windows.urllib.request, "urlopen", lambda _url, timeout: Cut())
    ok, _msg = windows.download_unitycapture()
    assert ok is False
    assert list(tmp_path.iterdir()) == []


def test_register_drops_a_tampered_file_so_a_retry_downloads_it_again(monkeypatch, tmp_path):
    monkeypatch.setattr(windows, "unitycapture_dir", lambda: tmp_path)
    (tmp_path / "UnityCaptureFilter32.dll").write_bytes(b"half")
    assert windows.register_unitycapture()[0] is False
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
    assert calls[0][0][-1].count('"/i:UnityCaptureName=Telescope"') == 2
    assert calls[0][1]["timeout"] == 60


def test_register_unitycapture_survives_an_apostrophe_in_the_folder(monkeypatch, tmp_path):
    folder = tmp_path / "O'Brien"
    folder.mkdir()
    payloads = {name: name.encode() for name in windows._EXPECTED_SHA256}
    for name, data in payloads.items():
        (folder / name).write_bytes(data)
    monkeypatch.setattr(windows, "unitycapture_dir", lambda: folder)
    monkeypatch.setattr(windows, "_EXPECTED_SHA256",
                        {name: hashlib.sha256(data).hexdigest() for name, data in payloads.items()})
    calls = []
    monkeypatch.setattr(windows.subprocess, "run",
                        lambda cmd, **kwargs: calls.append(cmd) or subprocess.CompletedProcess(cmd, 0))

    windows.register_unitycapture()

    ps = calls[0][-1]
    quoted = ps[ps.index("-ArgumentList '") + len("-ArgumentList '"):ps.rindex("' -Verb")]
    assert "O''Brien" in quoted and "'" not in quoted.replace("''", "")


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


def test_registered_name_reads_the_filter_key_and_skips_the_property_page(monkeypatch, tmp_path):
    dll = str(tmp_path / "UnityCaptureFilter64.dll")
    monkeypatch.setattr(windows, "unitycapture_dir", lambda: tmp_path)
    registry = {
        "{props}\\InprocServer32": dll, "{props}": "Unity Video Capture Configuration",
        "{filter}\\InprocServer32": dll, "{filter}": "Telescope",
    }

    class Key:
        def __init__(self, path):
            self.path = path

        def __enter__(self):
            return self

        def __exit__(self, *_args):
            pass

    keys = ["{other}", "{props}", "{filter}"]
    fake = types.SimpleNamespace(HKEY_CLASSES_ROOT="HKCR")

    def open_key(_root, path):
        if path != "CLSID" and path not in registry:
            raise OSError(path)
        return Key(path)
    fake.OpenKey = open_key
    fake.EnumKey = lambda _root, idx: keys[idx] if idx < len(keys) else (_ for _ in ()).throw(OSError())
    fake.QueryValueEx = lambda key, _name: (registry[key.path], 1)
    monkeypatch.setitem(sys.modules, "winreg", fake)
    assert windows.uc_registered_name() == "Telescope"

    registry["{filter}"] = "Unity Video Capture"
    assert windows.uc_registered_name() == "Unity Video Capture"
    del registry["{filter}"]  # no name stored: UnityCapture's default
    assert windows.uc_registered_name() == windows.UC_DEFAULT_NAME


@pytest.mark.parametrize("inside, archived", [
    ("Temp1_Telescope-windows.zip/Telescope", True),       # Explorer's Run from the zip
    ("7zO4A8C1B2E", True),                                 # 7-Zip
    ("Rar$EXa1234.5678/Telescope", True),                  # WinRAR
    ("Telescope", False),                                  # extracted into temp on purpose
    ("_MEI12345", False),                                  # PyInstaller's own unpack folder
])
def test_running_from_archive_spots_zip_tools_temp_folders(tmp_path, inside, archived):
    app = tmp_path / inside
    app.mkdir(parents=True)
    assert windows.running_from_archive(app, tmp_path) is archived


def test_running_from_archive_ignores_folders_outside_temp(tmp_path):
    app = tmp_path / "Downloads" / "Temp1_Telescope-windows.zip"
    app.mkdir(parents=True)
    assert not windows.running_from_archive(app, tmp_path / "Temp")
