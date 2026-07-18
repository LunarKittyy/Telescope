import subprocess
from pathlib import Path

import pytest

import telescope.platform as platform_api


def test_run_returns_process_result(monkeypatch):
    completed = subprocess.CompletedProcess(["tool"], 7, "out", "err")
    monkeypatch.setattr(platform_api.subprocess, "run", lambda *args, **kwargs: completed)

    assert platform_api._run(["tool"], timeout=2) == (7, "out", "err")


@pytest.mark.parametrize(
    "exc,expected",
    [
        (FileNotFoundError(), (-1, "", "Not found: missing")),
        (subprocess.TimeoutExpired(["missing"], 1), (-2, "", "Timed out")),
    ],
)
def test_run_normalizes_expected_subprocess_failures(monkeypatch, exc, expected):
    monkeypatch.setattr(
        platform_api.subprocess,
        "run",
        lambda *args, **kwargs: (_ for _ in ()).throw(exc),
    )
    assert platform_api._run(["missing"]) == expected


def test_platform_tools_dir_uses_executable_when_frozen(monkeypatch, tmp_path):
    monkeypatch.setattr(platform_api.sys, "frozen", True, raising=False)
    monkeypatch.setattr(platform_api.sys, "executable", str(tmp_path / "Telescope.exe"))
    assert platform_api.platform_tools_dir() == tmp_path / "platform-tools"


def test_platform_tools_dir_uses_desktop_source_tree(monkeypatch):
    monkeypatch.delattr(platform_api.sys, "frozen", raising=False)
    # Anchored on this test file's own location (desktop/tests/..) rather than
    # a literal "desktop" name, so it holds regardless of the checkout's folder name.
    desktop_root = Path(__file__).resolve().parent.parent
    assert platform_api.platform_tools_dir() == desktop_root / "platform-tools"


def test_bundled_apk_path_returns_existing_file(monkeypatch, tmp_path):
    monkeypatch.setattr(platform_api.sys, "frozen", True, raising=False)
    monkeypatch.setattr(platform_api.sys, "executable", str(tmp_path / "Telescope.exe"))
    apk = tmp_path / "Telescope.apk"
    apk.write_bytes(b"apk")

    assert platform_api.bundled_apk_path() == apk
    apk.unlink()
    assert platform_api.bundled_apk_path() is None


def test_adb_exe_prefers_bundled_binary(monkeypatch, tmp_path):
    bundled = tmp_path / ("adb.exe" if platform_api.IS_WINDOWS else "adb")
    bundled.write_text("")
    monkeypatch.setattr(platform_api, "platform_tools_dir", lambda: tmp_path)
    monkeypatch.setattr(platform_api.shutil, "which", lambda _name: "/path/adb")

    assert platform_api.adb_exe() == str(bundled)


def test_adb_exe_falls_back_to_path(monkeypatch, tmp_path):
    monkeypatch.setattr(platform_api, "platform_tools_dir", lambda: tmp_path)
    monkeypatch.setattr(platform_api.shutil, "which", lambda name: f"/usr/bin/{name}")

    assert platform_api.adb_exe() == "/usr/bin/adb"
    assert platform_api.adb_available() is True


def test_adb_devices_returns_only_authorized_serials(monkeypatch):
    output = (
        "List of devices attached\n"
        "phone-1\tdevice\n"
        "phone-2\tunauthorized\n"
        "emulator-5554\tdevice\n"
        "noise\n"
    )
    monkeypatch.setattr(platform_api, "adb_exe", lambda: "adb")
    monkeypatch.setattr(platform_api, "_run", lambda _cmd: (0, output, ""))

    assert platform_api.adb_devices() == ["phone-1", "emulator-5554"]


def test_adb_devices_returns_empty_on_command_failure(monkeypatch):
    monkeypatch.setattr(platform_api, "adb_exe", lambda: "adb")
    monkeypatch.setattr(platform_api, "_run", lambda _cmd: (1, "", "bad"))
    assert platform_api.adb_devices() == []


@pytest.mark.parametrize(
    "serial,expected",
    [
        (None, ["adb", "forward"]),
        ("phone", ["adb", "-s", "phone", "forward"]),
    ],
)
def test_with_serial(serial, expected):
    assert platform_api._with_serial(["adb", "forward"], serial) == expected


def test_adb_forward_builds_serial_specific_command(monkeypatch):
    calls = []
    monkeypatch.setattr(platform_api, "adb_exe", lambda: "adb")
    monkeypatch.setattr(
        platform_api,
        "_run",
        lambda cmd: calls.append(cmd) or (0, "", ""),
    )

    assert platform_api.adb_forward(8080, serial="phone") == (True, "Port 8080 forwarded")
    assert calls == [["adb", "-s", "phone", "forward", "tcp:8080", "tcp:8080"]]


def test_adb_forward_surfaces_error_and_unforward_ignores_it(monkeypatch):
    calls = []
    monkeypatch.setattr(platform_api, "adb_exe", lambda: "adb")
    monkeypatch.setattr(
        platform_api,
        "_run",
        lambda cmd: calls.append(cmd) or (1, "", "forward failed"),
    )

    assert platform_api.adb_forward(9000) == (False, "forward failed")
    assert platform_api.adb_unforward(9000, serial="serial") is None
    assert calls[-1] == ["adb", "-s", "serial", "forward", "--remove", "tcp:9000"]


def test_adb_reverse_builds_serial_specific_command(monkeypatch):
    calls = []
    monkeypatch.setattr(platform_api, "adb_exe", lambda: "adb")
    monkeypatch.setattr(
        platform_api,
        "_run",
        lambda cmd: calls.append(cmd) or (0, "", ""),
    )

    assert platform_api.adb_reverse(8765, serial="phone") == (True, "Port 8765 reversed")
    assert calls == [["adb", "-s", "phone", "reverse", "tcp:8765", "tcp:8765"]]


def test_adb_reverse_surfaces_error_and_unreverse_ignores_it(monkeypatch):
    calls = []
    monkeypatch.setattr(platform_api, "adb_exe", lambda: "adb")
    monkeypatch.setattr(
        platform_api,
        "_run",
        lambda cmd: calls.append(cmd) or (1, "", "reverse failed"),
    )

    assert platform_api.adb_reverse(9000) == (False, "reverse failed")
    assert platform_api.adb_unreverse(9000, serial="serial") is None
    assert calls[-1] == ["adb", "-s", "serial", "reverse", "--remove", "tcp:9000"]


def test_adb_broadcast_pair_builds_serial_specific_command(monkeypatch):
    calls = []
    monkeypatch.setattr(platform_api, "adb_exe", lambda: "adb")
    monkeypatch.setattr(
        platform_api,
        "_run",
        lambda cmd: calls.append(cmd) or (0, "", ""),
    )

    assert platform_api.adb_broadcast_pair("cGF5bG9hZA==", serial="phone") == (True, "Broadcast sent")
    assert calls == [[
        "adb", "-s", "phone", "shell", "am", "broadcast",
        "-a", "com.telescope.action.PAIR", "-p", "com.telescope",
        "--es", "payload", "cGF5bG9hZA==",
    ]]


def test_adb_broadcast_pair_surfaces_error(monkeypatch):
    monkeypatch.setattr(platform_api, "adb_exe", lambda: "adb")
    monkeypatch.setattr(platform_api, "_run", lambda cmd: (1, "", "device offline"))

    assert platform_api.adb_broadcast_pair("cGF5bG9hZA==") == (False, "device offline")
