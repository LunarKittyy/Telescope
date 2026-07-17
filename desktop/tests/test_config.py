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


def test_legacy_config_is_migrated_once(config_home, monkeypatch, tmp_path):
    # _LEGACY_CONFIG_FILE is derived from config.py's own real path, not from
    # XDG_CONFIG_HOME/APPDATA, so it must be patched separately here - otherwise
    # this test would read/write the actual repo tree instead of tmp_path.
    legacy = tmp_path / "legacy" / "telescope_config.json"
    monkeypatch.setattr(config_home, "_LEGACY_CONFIG_FILE", legacy)
    legacy.parent.mkdir(parents=True, exist_ok=True)
    legacy.write_text(json.dumps({"version": 2, "selected_device": "Legacy"}), encoding="utf-8")

    cfg = config_home.load_config()
    assert cfg["selected_device"] == "Legacy"
    assert config_home.config_path().exists()


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


def test_v0_migration_maps_batt_alert_to_battery_alert(config_home):
    v0 = {"ip": "1.2.3.4", "port": 8080, "batt_alert": 15, "temp_alert": 40}
    migrated = config_home._migrate(dict(v0))

    mon_cfg = migrated["devices"]["Phone"]["plugin_configs"]["monitoring"]
    assert mon_cfg == {"battery_alert": 15, "temp_alert": 40}


def test_v0_migration_without_ip_key_is_a_noop_besides_version(config_home):
    cfg = {"some": "thing"}
    migrated = config_home._migrate_v0_to_v1(dict(cfg))
    assert migrated["version"] == 1
    assert migrated["some"] == "thing"
