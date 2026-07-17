from pathlib import Path

import pytest

import telescope.platform.linux as linux


def test_options_line_contains_every_runtime_parameter():
    line = linux._v4l2_options_line()
    assert line.startswith("options v4l2loopback ")
    for key, value in linux.V4L2_PARAMS.items():
        assert key in line
        assert value in line
    assert line.endswith("\n")


@pytest.mark.parametrize(
    "result,expected",
    [((0, "v4l2loopback 123 0\n", ""), True), ((0, "other 123 0\n", ""), False),
     ((1, "v4l2loopback", "bad"), False)],
)
def test_module_loaded(monkeypatch, result, expected):
    monkeypatch.setattr(linux, "_run", lambda _cmd: result)
    assert linux.v4l2_module_loaded() is expected


def test_device_and_combined_loaded_checks(monkeypatch):
    monkeypatch.setattr(linux.os.path, "exists", lambda path: path == linux.V4L2_PHONE_DEV)
    monkeypatch.setattr(linux, "v4l2_module_loaded", lambda: True)
    assert linux.v4l2_devices_ready() is True
    assert linux.v4l2_is_loaded() is True
    monkeypatch.setattr(linux, "v4l2_module_loaded", lambda: False)
    assert linux.v4l2_is_loaded() is False


@pytest.mark.parametrize("tool", ["pkexec", None])
def test_unload_selects_privilege_helper(monkeypatch, tool):
    calls = []
    monkeypatch.setattr(linux.shutil, "which", lambda _name: tool)
    monkeypatch.setattr(
        linux, "_run", lambda cmd, timeout: calls.append((cmd, timeout)) or (0, "", "")
    )

    assert linux.v4l2_unload() == (True, "Module unloaded")
    assert calls[0][0][0] == ("pkexec" if tool else "sudo")
    assert calls[0][1] == 30


def test_unload_and_reload_surface_fallback_errors(monkeypatch):
    monkeypatch.setattr(linux.shutil, "which", lambda _name: None)
    monkeypatch.setattr(linux, "_run", lambda *args, **kwargs: (1, "", ""))
    assert linux.v4l2_unload() == (False, "modprobe -r failed")
    assert linux.v4l2_reload() == (False, "reload failed")


def test_reload_uses_shared_parameters(monkeypatch):
    calls = []
    monkeypatch.setattr(linux.shutil, "which", lambda _name: "pkexec")
    monkeypatch.setattr(
        linux, "_run", lambda cmd, timeout: calls.append((cmd, timeout)) or (0, "", "")
    )

    assert linux.v4l2_reload() == (True, "/dev/video11 + /dev/video10")
    command = calls[0][0]
    assert command[:3] == ["pkexec", "sh", "-c"]
    for value in linux.V4L2_PARAMS.values():
        assert value in command[-1]
    assert calls[0][1] == 90


def test_load_refuses_existing_module_or_conflicting_device(monkeypatch):
    monkeypatch.setattr(linux, "v4l2_module_loaded", lambda: True)
    assert linux.v4l2_load()[0] is False
    assert "different config" in linux.v4l2_load()[1]

    monkeypatch.setattr(linux, "v4l2_module_loaded", lambda: False)
    monkeypatch.setattr(linux.os.path, "exists", lambda path: path == linux.V4L2_OBS_DEV)
    assert linux.v4l2_load() == (
        False,
        f"{linux.V4L2_OBS_DEV} already exists and is not a v4l2loopback device.",
    )


def test_load_invokes_modprobe_and_handles_failure(monkeypatch):
    calls = []
    monkeypatch.setattr(linux, "v4l2_module_loaded", lambda: False)
    monkeypatch.setattr(linux.os.path, "exists", lambda _path: False)
    monkeypatch.setattr(linux.shutil, "which", lambda _name: None)
    monkeypatch.setattr(
        linux, "_run", lambda cmd, timeout: calls.append((cmd, timeout)) or (0, "", "")
    )

    assert linux.v4l2_load() == (True, "Loaded: /dev/video11 + /dev/video10")
    assert calls[0][0][:3] == ["sudo", "modprobe", "v4l2loopback"]
    assert calls[0][1] == 60

    monkeypatch.setattr(linux, "_run", lambda *args, **kwargs: (1, "", "denied"))
    assert linux.v4l2_load() == (False, "denied")


def test_persist_status_reports_each_file(monkeypatch):
    monkeypatch.setattr(
        linux.os.path,
        "exists",
        lambda path: path == linux.V4L2_PERSIST_MODPROBE_CONF,
    )
    assert linux.v4l2_persist_status() == {
        "modprobe_conf": True,
        "modules_load_conf": False,
    }


def test_find_conflicting_configs_skips_ours_comments_and_read_errors(monkeypatch, tmp_path):
    ours = tmp_path / "ours.conf"
    comment = tmp_path / "comment.conf"
    conflict = tmp_path / "conflict.conf"
    missing = tmp_path / "missing.conf"
    ours.write_text("options v4l2loopback devices=2")
    comment.write_text("# options v4l2loopback devices=1\n")
    conflict.write_text("options v4l2loopback devices=1\n")
    monkeypatch.setattr(linux, "V4L2_PERSIST_MODPROBE_CONF", str(ours))
    monkeypatch.setattr(
        linux.glob, "glob", lambda _pattern: [str(missing), str(conflict), str(comment), str(ours)]
    )

    assert linux._find_conflicting_confs() == [str(conflict)]


def test_persist_enable_is_idempotent_and_refuses_conflicts(monkeypatch):
    monkeypatch.setattr(
        linux, "v4l2_persist_status", lambda: {"modprobe_conf": True, "modules_load_conf": False}
    )
    assert linux.v4l2_persist_enable()[0] is True

    monkeypatch.setattr(
        linux, "v4l2_persist_status", lambda: {"modprobe_conf": False, "modules_load_conf": False}
    )
    monkeypatch.setattr(linux, "_find_conflicting_confs", lambda: ["/etc/conflict.conf"])
    ok, msg = linux.v4l2_persist_enable()
    assert ok is False
    assert "/etc/conflict.conf" in msg


def test_persist_enable_writes_both_files_and_surfaces_errors(monkeypatch):
    calls = []
    monkeypatch.setattr(
        linux, "v4l2_persist_status", lambda: {"modprobe_conf": False, "modules_load_conf": False}
    )
    monkeypatch.setattr(linux, "_find_conflicting_confs", lambda: [])
    monkeypatch.setattr(linux.shutil, "which", lambda _name: "pkexec")
    monkeypatch.setattr(
        linux, "_run", lambda cmd, timeout: calls.append((cmd, timeout)) or (0, "", "")
    )

    assert linux.v4l2_persist_enable()[0] is True
    script = calls[0][0][-1]
    assert linux.V4L2_PERSIST_MODPROBE_CONF in script
    assert linux.V4L2_PERSIST_MODULES_CONF in script
    assert linux._v4l2_options_line() in script

    monkeypatch.setattr(linux, "_run", lambda *args, **kwargs: (1, "", ""))
    assert linux.v4l2_persist_enable() == (False, "Failed to write persistence files")


def test_persist_disable_noop_remove_and_error(monkeypatch):
    monkeypatch.setattr(linux.os.path, "exists", lambda _path: False)
    assert linux.v4l2_persist_disable() == (True, "Nothing to remove")

    monkeypatch.setattr(linux.os.path, "exists", lambda _path: True)
    monkeypatch.setattr(linux.shutil, "which", lambda _name: None)
    calls = []
    monkeypatch.setattr(
        linux, "_run", lambda cmd, timeout: calls.append(cmd) or (0, "", "")
    )
    ok, msg = linux.v4l2_persist_disable()
    assert ok is True
    assert msg.startswith("Removed ")
    assert calls[0][:3] == ["sudo", "rm", "-f"]

    monkeypatch.setattr(linux, "_run", lambda *args, **kwargs: (1, "", "denied"))
    assert linux.v4l2_persist_disable() == (False, "denied")
