import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

DESKTOP = Path(__file__).resolve().parent.parent

# Run in a fresh interpreter: the profile is read when each module is imported, as it is when the app starts
_PROBE = """
import json
import update_guard
from telescope import app, config, diagnostics, platform
from telescope.platform import virtual_mic
print(json.dumps({
    "config": str(config.config_path()),
    "logs": str(diagnostics.log_dir()),
    "instance_port": app._INSTANCE_PORT,
    "real_instance_port": update_guard.INSTANCE_PORT,
    "mic_source": virtual_mic.SOURCE,
    "mic_ours": list(virtual_mic._OURS),
    "fifo": virtual_mic.fifo_path(),
    "pair_package": platform.PAIR_BROADCAST_PACKAGE,
}))
"""


def _probe(tmp_path, profile=None) -> dict:
    env = dict(os.environ, QT_QPA_PLATFORM="offscreen", XDG_CONFIG_HOME=str(tmp_path / "xdg"),
               XDG_RUNTIME_DIR=str(tmp_path / "run"), APPDATA=str(tmp_path / "appdata"))
    env.pop("TELESCOPE_DEV_PROFILE", None)
    if profile is not None:
        env["TELESCOPE_DEV_PROFILE"] = str(profile)
    out = subprocess.run([sys.executable, "-c", _PROBE], env=env, cwd=DESKTOP, capture_output=True, text=True,
                         timeout=60, check=True)
    return json.loads(out.stdout.strip().splitlines()[-1])


def test_a_dev_profile_keeps_everything_apart_from_the_real_app(tmp_path):
    real = _probe(tmp_path)
    dev = _probe(tmp_path, tmp_path / "profile")
    assert Path(dev["config"]).parent == tmp_path / "profile"
    assert Path(dev["logs"]).parent == tmp_path / "profile"
    assert dev["instance_port"] != real["instance_port"] == real["real_instance_port"]
    assert dev["mic_source"] != real["mic_source"]
    assert dev["fifo"] != real["fifo"]
    assert dev["pair_package"] == "com.telescope.dev" and real["pair_package"] == "com.telescope"


def test_setting_up_the_dev_mic_never_unloads_the_real_one(tmp_path):
    dev = _probe(tmp_path, tmp_path / "profile")
    real_module = "12\tmodule-pipe-source\tsource_name=telescope_mic file=/run/telescope-mic.fifo"
    first_build = "7\tmodule-null-sink\tsink_name=telescope_mic_sink"
    assert not any(tag in line for tag in dev["mic_ours"] for line in (real_module, first_build))


@pytest.mark.skipif(sys.platform == "win32", reason="keeps the real app's paths readable in the assertion")
def test_without_a_profile_nothing_changes(tmp_path):
    real = _probe(tmp_path)
    assert real["config"] == str(tmp_path / "xdg" / "telescope" / "telescope_config.json")
    assert real["fifo"].endswith("telescope-mic.fifo")
    assert real["mic_source"] == "telescope_mic"


def _launcher():
    import importlib.util
    spec = importlib.util.spec_from_file_location("dev_desktop", DESKTOP / "scripts" / "dev_desktop.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def test_the_launcher_runs_the_app_in_a_profile_and_throws_a_fresh_one_away(tmp_path, monkeypatch):
    launcher = _launcher()
    monkeypatch.setattr(launcher.tempfile, "tempdir", str(tmp_path))
    seen = []

    def call(cmd, env, cwd):
        folder = Path(env["TELESCOPE_DEV_PROFILE"])
        (folder / "telescope_config.json").write_text("{}")
        seen.append((cmd, folder))
        return 0
    monkeypatch.setattr(launcher.subprocess, "call", call)

    assert launcher.main(["-platform", "offscreen"]) == 0
    cmd, folder = seen[0]
    assert cmd[1].endswith("main.py") and cmd[2:] == ["-platform", "offscreen"]
    assert not folder.exists()

    assert launcher.main(["--keep"]) == 0
    kept = seen[1][1]
    assert kept == launcher.kept_folder() and (kept / "telescope_config.json").exists()
    assert launcher.main(["--keep"]) == 0 and seen[2][1] == kept
    assert launcher.main(["--reset"]) == 0
    assert not kept.exists()
