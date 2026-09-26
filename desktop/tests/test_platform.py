import subprocess
from pathlib import Path

import pytest

import telescope.platform as platform_api


def test_run_returns_process_result(monkeypatch):
    completed = subprocess.CompletedProcess(["tool"], 7, "out", "err")
    monkeypatch.setattr(platform_api.subprocess, "run", lambda *args, **kwargs: completed)

    assert platform_api._run(["tool"], timeout=2) == (7, "out", "err")


def test_run_hides_the_console_window_on_windows(monkeypatch):
    seen = {}
    monkeypatch.setattr(platform_api, "NO_WINDOW", {"creationflags": 0x08000000})
    monkeypatch.setattr(platform_api.subprocess, "run",
                        lambda *args, **kwargs: seen.update(kwargs) or subprocess.CompletedProcess([], 0, "", ""))
    platform_api._run(["adb", "devices"])
    assert seen["creationflags"] == 0x08000000


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
    # Anchored to test file location, not literal "desktop", to work with any checkout folder name.
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


def test_adb_helpers_degrade_when_adb_is_missing(monkeypatch):
    # No adb on PATH: USB-mode status probes call these every few seconds and must not raise.
    monkeypatch.setattr(platform_api, "adb_exe", lambda: None)
    assert platform_api.adb_devices() == []
    ok, err = platform_api.adb_forward(8766)
    assert not ok and "adb" in err


@pytest.mark.parametrize(
    "result,expected",
    [
        ((0, "Performing Streamed Install\nSuccess\n", ""), (True, "")),
        ((1, "", "adb: failed to install x.apk: Failure [INSTALL_FAILED_UPDATE_INCOMPATIBLE: ...]"),
         (False, "The Telescope app on the phone was signed differently. Uninstall it on the phone, then try again.")),
        ((1, "", "Failure [INSTALL_FAILED_VERSION_DOWNGRADE]"), (False, "The phone already has a newer Telescope app.")),
        ((1, "one\n", "Failure [bad apk]\n"), (False, "Failure [bad apk]")),
        ((1, "", ""), (False, "adb install failed")),
    ],
)
def test_adb_install_explains_the_common_failures(monkeypatch, result, expected):
    calls = []
    monkeypatch.setattr(platform_api, "adb_exe", lambda: "adb")
    monkeypatch.setattr(platform_api, "_run", lambda cmd, timeout: calls.append(cmd) or result)
    assert platform_api.adb_install("SER", Path("x.apk")) == expected
    assert calls == [["adb", "-s", "SER", "install", "-r", "x.apk"]]


class _FakeServer:
    def __init__(self):
        self.alive, self.terminated = True, False

    def poll(self):
        return None if self.alive else 0

    def terminate(self):
        self.terminated, self.alive = True, False

    def wait(self, timeout=None):
        return 0


@pytest.fixture
def own_server(monkeypatch):
    monkeypatch.setattr(platform_api, "OWN_ADB_SERVER", True)
    monkeypatch.setattr(platform_api, "_adb_server", None)
    monkeypatch.setattr(platform_api, "adb_exe", lambda: "C:/Telescope/platform-tools/adb.exe")


def test_telescope_starts_adbs_server_as_its_own_child_once(own_server):
    started, tied, up = [], [], [False]

    def popen(cmd, **kwargs):
        started.append(cmd)
        up[0] = True
        return _FakeServer()

    for _ in range(3):
        platform_api.ensure_adb_server(running=lambda: up[0], popen=popen, tie=tied.append, sleep=lambda _s: None)
    assert started == [["C:/Telescope/platform-tools/adb.exe", "nodaemon", "server"]]
    assert len(tied) == 1

    server = tied[0]
    platform_api.stop_adb_server()
    assert server.terminated


def test_a_server_someone_else_runs_is_left_alone(own_server):
    started = []
    platform_api.ensure_adb_server(running=lambda: True, popen=lambda *a, **k: started.append(a),
                                   tie=lambda _p: None)
    assert started == []
    platform_api.stop_adb_server()  # nothing of ours to stop


def test_off_windows_adb_runs_its_own_server(monkeypatch):
    monkeypatch.setattr(platform_api, "OWN_ADB_SERVER", False)
    platform_api.ensure_adb_server(running=lambda: False, popen=lambda *a, **k: pytest.fail("started a server"))


def test_adb_commands_make_sure_the_server_is_up_first(monkeypatch):
    calls = []
    monkeypatch.setattr(platform_api, "ensure_adb_server", lambda: calls.append("server"))
    monkeypatch.setattr(platform_api.subprocess, "run",
                        lambda *a, **k: calls.append("run") or subprocess.CompletedProcess([], 0, "", ""))
    platform_api._run(["C:/pt/adb.exe", "devices"])
    platform_api._run(["lsmod"])
    assert calls == ["server", "run", "run"]


def test_the_windows_job_fails_softly_elsewhere():
    from telescope.platform import winjob
    if platform_api.IS_WINDOWS:
        pytest.skip("real job objects on Windows")
    assert winjob.kill_with_us(_FakeServer()) is False
