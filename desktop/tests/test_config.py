import json

import pytest


def test_fresh_config_is_empty_v2(config_home):
    cfg = config_home.load_config()
    assert cfg == {"version": 2, "selected_device": None, "plugin_configs": {}, "devices": {}}


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


def test_malformed_json_falls_back_to_empty(config_home):
    path = config_home.config_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("not valid json", encoding="utf-8")

    cfg = config_home.load_config()
    assert cfg["version"] == 2


def test_malformed_json_is_backed_up_before_reset(config_home):
    path = config_home.config_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text("not valid json", encoding="utf-8")

    config_home.load_config()

    backups = list(path.parent.glob(f"{path.name}.invalid-*"))
    assert len(backups) == 1
    assert backups[0].read_text(encoding="utf-8") == "not valid json"


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
    current = {"version": 2, "custom": True}
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
        "version": 2,
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
        "version": 2,
        "plugin_configs": {"connection": {"mode": "wifi"}},
        "devices": {"Phone": {"active_ip": 12345}},  # active_ip must be a string
        "selected_device": "Phone",
    }
    result = config_home._migrate(cfg)
    assert result["plugin_configs"] == {"connection": {"mode": "wifi"}}
    assert result["devices"] == {}
    assert result["selected_device"] == "Phone"


def test_devices_section_with_non_dict_entry_resets_alone(config_home):
    cfg = {"version": 2, "devices": {"Phone": "not-a-dict"}}
    result = config_home._migrate(cfg)
    assert result["devices"] == {}


def test_malformed_selected_device_resets_alone(config_home):
    cfg = {
        "version": 2,
        "plugin_configs": {"connection": {"mode": "wifi"}},
        "devices": {},
        "selected_device": 42,
    }
    result = config_home._migrate(cfg)
    assert result["selected_device"] is None
    assert result["plugin_configs"] == {"connection": {"mode": "wifi"}}


def test_valid_current_version_config_round_trips_through_migrate(config_home):
    cfg = {
        "version": 2,
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
