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


@pytest.mark.parametrize("raw", [[], "text", 42, {"version": "two"}])
def test_invalid_top_level_config_shapes_fall_back_to_empty(config_home, raw):
    assert config_home._migrate(raw) == config_home._empty()


def test_current_and_future_versions_are_preserved(config_home):
    current = {"version": 2, "custom": True}
    future = {"version": 99, "custom": True}
    assert config_home._migrate(current) is current
    assert config_home._migrate(future) is future


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


def test_failed_legacy_copy_falls_back_to_fresh_config(config_home, monkeypatch, tmp_path):
    legacy = tmp_path / "legacy.json"
    legacy.write_text('{"selected_device": "Legacy"}')
    monkeypatch.setattr(config_home, "_LEGACY_CONFIG_FILE", legacy)
    real_write_text = config_home.Path.write_text

    def fail_destination(path, *args, **kwargs):
        if path == config_home.config_path():
            raise OSError("read only")
        return real_write_text(path, *args, **kwargs)

    monkeypatch.setattr(config_home.Path, "write_text", fail_destination)
    assert config_home.load_config() == config_home._empty()


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


def test_save_failure_returns_false_and_sets_version(config_home, monkeypatch):
    cfg = {"version": 0}
    monkeypatch.setattr(config_home.os, "replace", lambda *_args: (_ for _ in ()).throw(OSError("no space")))
    assert config_home.save_config(cfg) is False
    assert cfg["version"] == config_home.CONFIG_VERSION
