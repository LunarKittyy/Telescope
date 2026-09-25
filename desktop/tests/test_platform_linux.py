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


def _tools(monkeypatch, *present):
    monkeypatch.setattr(linux.shutil, "which", lambda name: name if name in present else None)


def _record(monkeypatch, *results):
    """_run returns each result in turn (the last one repeats); the commands are recorded."""
    calls, queue = [], list(results)

    def run(cmd, timeout):
        calls.append((cmd, timeout))
        return queue.pop(0) if len(queue) > 1 else queue[0]
    monkeypatch.setattr(linux, "_run", run)
    return calls


def test_pkexec_runs_the_whole_operation_behind_one_prompt(monkeypatch):
    _tools(monkeypatch, "pkexec", "sudo")
    calls = _record(monkeypatch, (0, "", ""))
    assert linux.v4l2_unload() == (True, "Module unloaded")
    assert calls == [(["pkexec", "sh", "-c", "modprobe -r v4l2loopback"], 30)]


def test_cancelling_the_prompt_is_not_an_error_to_explain(monkeypatch):
    _tools(monkeypatch, "pkexec", "sudo")
    calls = _record(monkeypatch, (126, "", ""))
    assert linux.v4l2_unload() == (False, linux.CANCELLED)
    assert len(calls) == 1


def test_no_polkit_agent_tries_cached_sudo_then_hands_back_the_command(monkeypatch):
    _tools(monkeypatch, "pkexec", "sudo")
    calls = _record(monkeypatch, (127, "", "no agent"), (1, "", "sudo: a password is required"))
    result = linux.v4l2_unload()
    assert result == (False, linux.NO_PROMPT)
    assert result.command == "sudo modprobe -r v4l2loopback"
    assert calls[1][0] == ["sudo", "-n", "sh", "-c", "modprobe -r v4l2loopback"]

    calls = _record(monkeypatch, (127, "", ""), (0, "", ""))
    assert linux.v4l2_unload().ok is True


def test_sudo_is_never_run_where_it_could_ask_for_a_password(monkeypatch):
    _tools(monkeypatch, "sudo")
    calls = _record(monkeypatch, (1, "", "sudo: a password is required"))
    result = linux.v4l2_reload()
    assert all(cmd[:2] == ["sudo", "-n"] for cmd, _ in calls)
    assert result.command.startswith("sudo modprobe -r v4l2loopback && sleep 0.5 && sudo modprobe v4l2loopback")

    _tools(monkeypatch)
    calls = _record(monkeypatch, (0, "", ""))
    assert linux.v4l2_unload().command == "sudo modprobe -r v4l2loopback"
    assert calls == []


def test_other_failures_pass_through(monkeypatch):
    _tools(monkeypatch, "pkexec")
    _record(monkeypatch, (1, "", ""))
    assert linux.v4l2_unload() == (False, "modprobe -r failed")
    assert linux.v4l2_reload() == (False, "reload failed")
    assert linux.v4l2_unload().command is None


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


def _clean_slate(monkeypatch, conflicts=()):
    monkeypatch.setattr(linux, "v4l2_module_loaded", lambda: False)
    monkeypatch.setattr(linux.os.path, "exists", lambda _path: False)
    monkeypatch.setattr(linux, "_find_conflicting_confs", lambda: list(conflicts))


def test_load_invokes_modprobe_and_handles_failure(monkeypatch):
    _clean_slate(monkeypatch)
    _tools(monkeypatch, "pkexec")
    calls = _record(monkeypatch, (0, "", ""))
    assert linux.v4l2_load() == (True, "Loaded: /dev/video11 + /dev/video10")
    script = calls[0][0][3]
    assert script.startswith("modprobe v4l2loopback ") and "printf" not in script
    assert calls[0][1] == 60

    _record(monkeypatch, (1, "", "denied"))
    assert linux.v4l2_load() == (False, "denied")
    _record(monkeypatch, (1, "", "modprobe: FATAL: Module v4l2loopback not found"))
    assert "isn't installed" in linux.v4l2_load()[1]


def test_setup_with_persist_writes_the_boot_config_in_the_same_prompt(monkeypatch, tmp_path):
    _clean_slate(monkeypatch)
    _tools(monkeypatch, "pkexec")
    calls = _record(monkeypatch, (0, "", ""))
    assert linux.v4l2_setup(persist=True).ok
    assert len(calls) == 1
    script = calls[0][0][3]
    assert linux.V4L2_PERSIST_MODPROBE_CONF in script and linux.V4L2_PERSIST_MODULES_CONF in script
    assert script.index("printf") < script.index("modprobe v4l2loopback")

    # The script writes exactly the options line (checked by running it against a temp dir).
    import subprocess
    local = script.replace("/etc/modprobe.d/", f"{tmp_path}/a-").replace("/etc/modules-load.d/", f"{tmp_path}/b-")
    subprocess.run(["sh", "-c", local.split(" && modprobe")[0]], check=True)
    assert (tmp_path / "a-99-telescope-v4l2loopback.conf").read_text() == linux._v4l2_options_line()
    assert (tmp_path / "b-99-telescope-v4l2loopback.conf").read_text() == "v4l2loopback\n"


def test_setup_leaves_someone_elses_boot_config_alone(monkeypatch):
    _clean_slate(monkeypatch, conflicts=["/etc/modprobe.d/other.conf"])
    _tools(monkeypatch, "pkexec")
    calls = _record(monkeypatch, (0, "", ""))
    assert linux.v4l2_setup(persist=True).ok
    assert "printf" not in calls[0][0][3]


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
    monkeypatch.setattr(
        linux, "v4l2_persist_status", lambda: {"modprobe_conf": False, "modules_load_conf": False}
    )
    monkeypatch.setattr(linux, "_find_conflicting_confs", lambda: [])
    _tools(monkeypatch, "pkexec")
    calls = _record(monkeypatch, (0, "", ""))
    assert linux.v4l2_persist_enable()[0] is True
    script = calls[0][0][-1]
    assert linux.V4L2_PERSIST_MODPROBE_CONF in script
    assert linux.V4L2_PERSIST_MODULES_CONF in script
    assert linux._v4l2_options_line().rstrip() in script

    _record(monkeypatch, (1, "", ""))
    assert linux.v4l2_persist_enable() == (False, "Failed to write persistence files")


def test_persist_disable_noop_remove_and_error(monkeypatch):
    monkeypatch.setattr(linux.os.path, "exists", lambda _path: False)
    assert linux.v4l2_persist_disable() == (True, "Nothing to remove")

    monkeypatch.setattr(linux.os.path, "exists", lambda _path: True)
    _tools(monkeypatch, "pkexec")
    calls = _record(monkeypatch, (0, "", ""))
    ok, msg = linux.v4l2_persist_disable()
    assert ok is True
    assert msg.startswith("Removed ")
    assert calls[0][0][3].startswith("rm -f ")

    _record(monkeypatch, (1, "", "denied"))
    assert linux.v4l2_persist_disable() == (False, "denied")
