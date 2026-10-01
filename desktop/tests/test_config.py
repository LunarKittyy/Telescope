import json
import os
import stat

import pytest

posix_only = pytest.mark.skipif(os.name != "posix", reason="Unix permissions")


def _mode(path):
    return stat.S_IMODE(os.stat(path).st_mode)


def test_fresh_config_is_empty_current_version(config_home):
    cfg = config_home.load_config()
    assert cfg == {"version": 3, "selected_device": None, "plugin_configs": {}, "devices": {}}


def test_save_and_reload_roundtrip(config_home):
    cfg = config_home.load_config()
    cfg["selected_device"] = "Phone1"
    assert config_home.save_config(cfg) is True

    reloaded = config_home.load_config()
    assert reloaded["selected_device"] == "Phone1"
    assert config_home.config_path().exists()


def test_save_is_atomic_no_leftover_tmp_file(config_home):
    cfg = config_home.load_config()
    config_home.save_config(cfg)
    tmp = config_home.config_path().with_suffix(".json.tmp")
    assert not tmp.exists()


@posix_only
def test_saved_config_is_private_whatever_the_umask(config_home):
    old = os.umask(0o022)
    try:
        assert config_home.save_config(config_home.load_config()) is True
    finally:
        os.umask(old)
    path = config_home.config_path()
    assert _mode(path) == 0o600
    assert _mode(path.parent) == 0o700


@posix_only
def test_saving_tightens_a_config_left_readable_by_others(config_home):
    path = config_home.config_path()
    path.parent.mkdir(parents=True)
    path.write_text("{}")
    os.chmod(path.parent, 0o755)
    os.chmod(path, 0o644)
    assert config_home.save_config(config_home.load_config()) is True
    assert _mode(path) == 0o600
    assert _mode(path.parent) == 0o700


@posix_only
def test_a_backup_of_a_bad_config_is_private(config_home):
    path = config_home.config_path()
    path.parent.mkdir(parents=True)
    path.write_text("not valid json")
    config_home.load_config()
    backup, = path.parent.glob(f"{path.name}.invalid-*")
    assert _mode(backup) == 0o600


@posix_only
def test_a_filesystem_that_refuses_chmod_still_saves(config_home, monkeypatch):
    def refuse(*_args):
        raise PermissionError("no Unix permissions here")
    monkeypatch.setattr(os, "chmod", refuse)
    monkeypatch.setattr(os, "fchmod", refuse)
    assert config_home.save_config(config_home.load_config()) is True


def test_malformed_json_falls_back_to_empty(config_home):
    path = config_home.config_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("not valid json", encoding="utf-8")

    cfg = config_home.load_config()
    assert cfg["version"] == 3


def test_malformed_json_is_backed_up_before_reset(config_home):
    path = config_home.config_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("not valid json", encoding="utf-8")

    config_home.load_config()

    backups = list(path.parent.glob(f"{path.name}.invalid-*"))
    assert len(backups) == 1
    assert backups[0].read_text(encoding="utf-8") == "not valid json"


def test_a_config_that_is_not_utf8_is_backed_up_not_fatal(config_home):
    path = config_home.config_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    raw = '{"version": 3, "selected_device": "Café"}'.encode("latin-1")
    path.write_bytes(raw)

    assert config_home.load_config()["selected_device"] is None
    assert next(path.parent.glob(f"{path.name}.invalid-*")).read_bytes() == raw


def test_a_config_saved_with_a_byte_order_mark_still_loads(config_home):
    path = config_home.config_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(b"\xef\xbb\xbf" + json.dumps({"version": 3, "selected_device": "Pixel"}).encode())
    assert config_home.load_config()["selected_device"] == "Pixel"


def test_stale_version_config_is_backed_up_before_reset(config_home):
    path = config_home.config_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    original = json.dumps({"version": 1, "selected_device": "Old"})
    path.write_text(original, encoding="utf-8")

    cfg = config_home.load_config()

    assert cfg == config_home._empty()
    backups = list(path.parent.glob(f"{path.name}.invalid-*"))
    assert len(backups) == 1
    assert backups[0].read_text(encoding="utf-8") == original


def test_fresh_install_is_not_backed_up(config_home):
    # No file exists yet - nothing to preserve, and no directory to scan.
    config_home.load_config()
    assert not config_home.config_path().parent.exists()


@pytest.mark.parametrize("raw", [[], "text", 42, {"version": "two"}])
def test_invalid_top_level_config_shapes_fall_back_to_empty(config_home, raw):
    assert config_home._migrate(raw) == config_home._empty()


def test_current_version_config_keeps_custom_keys_and_fills_in_missing_sections(config_home):
    current = {"version": 3, "custom": True}
    result = config_home._migrate(current)
    assert result["custom"] is True
    assert result["plugin_configs"] == {}
    assert result["devices"] == {}
    assert result["selected_device"] is None


def test_future_version_config_is_preserved(config_home):
    future = {
        "version": 99, "custom": True,
        "plugin_configs": {"connection": {"mode": "wifi"}},
        "devices": {"Phone": {"active_ip": "1.2.3.4"}},
        "selected_device": "Phone",
    }
    assert config_home._migrate(future) == future


@pytest.mark.parametrize("xdg_set", [True, False])
def test_linux_config_path_uses_xdg_or_home_fallback(config_home, monkeypatch, xdg_set):
    if not xdg_set:
        monkeypatch.delenv("XDG_CONFIG_HOME", raising=False)
    monkeypatch.setattr("sys.platform", "linux")
    path = config_home.config_path()
    assert path.name == "telescope_config.json"
    assert "telescope" in str(path.parent).lower()


def test_windows_config_path_uses_appdata(config_home, monkeypatch):
    monkeypatch.setattr("sys.platform", "win32")
    path = config_home.config_path()
    assert path.name == "telescope_config.json"
    assert "Telescope" in str(path.parent)


@pytest.mark.parametrize("version", [0, 1])
def test_config_older_than_current_version_resets_to_empty(config_home, version):
    old = {
        "version": version,
        "selected_device": "Phone",
        "plugin_configs": {"connection": {"devices_list": [{"name": "Phone", "ip": "1.2.3.4"}]}},
    }
    assert config_home._migrate(old) == config_home._empty()


def test_config_missing_version_resets_to_empty(config_home):
    assert config_home._migrate({"selected_device": "Phone"}) == config_home._empty()


def test_malformed_legacy_device_entry_does_not_crash_and_resets_to_empty(config_home):
    path = config_home.config_path()
    path.parent.mkdir(parents=True)
    path.write_text(json.dumps({
        "version": 1,
        "plugin_configs": {"connection": {"devices_list": [{}]}},
    }))
    assert config_home.load_config() == config_home._empty()


def test_malformed_plugin_configs_section_resets_alone(config_home):
    cfg = {
        "version": 3,
        "plugin_configs": ["not", "a", "dict"],
        "devices": {"Phone": {"active_ip": "1.2.3.4"}},
        "selected_device": "Phone",
    }
    result = config_home._migrate(cfg)
    assert result["plugin_configs"] == {}
    assert result["devices"] == {"Phone": {"active_ip": "1.2.3.4"}}
    assert result["selected_device"] == "Phone"


def test_malformed_devices_section_resets_alone(config_home):
    cfg = {
        "version": 3,
        "plugin_configs": {"connection": {"mode": "wifi"}},
        "devices": {"Phone": {"active_ip": 12345}},  # active_ip must be a string
        "selected_device": "Phone",
    }
    result = config_home._migrate(cfg)
    assert result["plugin_configs"] == {"connection": {"mode": "wifi"}}
    assert result["devices"] == {}
    assert result["selected_device"] == "Phone"


def test_devices_section_with_non_dict_entry_resets_alone(config_home):
    cfg = {"version": 3, "devices": {"Phone": "not-a-dict"}}
    result = config_home._migrate(cfg)
    assert result["devices"] == {}


def test_malformed_selected_device_resets_alone(config_home):
    cfg = {
        "version": 3,
        "plugin_configs": {"connection": {"mode": "wifi"}},
        "devices": {},
        "selected_device": 42,
    }
    result = config_home._migrate(cfg)
    assert result["selected_device"] is None
    assert result["plugin_configs"] == {"connection": {"mode": "wifi"}}


def test_valid_current_version_config_round_trips_through_migrate(config_home):
    cfg = {
        "version": 3,
        "plugin_configs": {"connection": {"mode": "usb"}},
        "devices": {"Phone": {"active_ip": "10.0.0.1", "plugin_configs": {"transforms": {"zoom": 2}}}},
        "selected_device": "Phone",
    }
    assert config_home._migrate(cfg) == cfg


def test_save_failure_returns_false_and_sets_version(config_home, monkeypatch):
    cfg = {"version": 0}
    monkeypatch.setattr(config_home.os, "replace", lambda *_args: (_ for _ in ()).throw(OSError("no space")))
    assert config_home.save_config(cfg) is False
    assert cfg["version"] == config_home.CONFIG_VERSION


def test_v2_config_keeps_global_settings_but_drops_pairing_and_per_phone_settings(config_home):
    config_home.config_path().parent.mkdir(parents=True, exist_ok=True)
    config_home.config_path().write_text(json.dumps({
        "version": 2,
        "selected_device": "Pixel",
        "plugin_configs": {"connection": {"devices": [{"name": "Pixel"}]}, "setup": {"canvas": [1920, 1080]}},
        "devices": {"Pixel": {"plugin_configs": {"camera_control": {"iso": 100}}}},
    }))
    cfg = config_home.load_config()
    assert cfg["version"] == 3
    assert cfg["plugin_configs"] == {"setup": {"canvas": [1920, 1080]}}
    assert cfg["devices"] == {}
    assert cfg["selected_device"] is None


def test_a_numpy_value_saves_as_plain_and_a_bad_one_keeps_the_saved_file(config_home):
    import numpy as np
    cfg = config_home.load_config()
    cfg["plugin_configs"]["x"] = {"zoom": np.float32(1.5), "pan": np.array([0, 1])}
    assert config_home.save_config(cfg) is True
    assert config_home.load_config()["plugin_configs"]["x"] == {"zoom": 1.5, "pan": [0, 1]}

    cfg["plugin_configs"]["x"] = {"bad": object()}
    assert config_home.save_config(cfg) is False
    assert config_home.load_config()["plugin_configs"]["x"] == {"zoom": 1.5, "pan": [0, 1]}


def test_a_briefly_locked_config_is_read_again_not_reset(config_home, monkeypatch):
    cfg = config_home.load_config()
    cfg["selected_device"] = "Phone1"
    config_home.save_config(cfg)
    real, calls = config_home.Path.read_bytes, []

    def flaky(self):
        calls.append(1)
        if len(calls) == 1:
            raise PermissionError("locked")
        return real(self)

    monkeypatch.setattr(config_home.Path, "read_bytes", flaky)
    monkeypatch.setattr(config_home.time, "sleep", lambda _s: None)
    assert config_home.load_config()["selected_device"] == "Phone1"
