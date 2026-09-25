import json
import logging
import os
import sys
from datetime import datetime
from pathlib import Path

logger = logging.getLogger(__name__)

CONFIG_VERSION = 3
_APP_NAME = "Telescope"
_CONFIG_FILENAME = "telescope_config.json"

# Plugin configs that are stored per-device rather than globally
DEVICE_LOCAL_PLUGINS = frozenset({"camera_control", "stream_output", "transforms", "monitoring", "presets"})


def config_path() -> Path:
    """Stable per-user config file location (XDG_CONFIG_HOME or ~/.config)."""
    if sys.platform == "win32":
        base = os.environ.get("APPDATA") or str(Path.home())
        return Path(base) / _APP_NAME / _CONFIG_FILENAME
    base = os.environ.get("XDG_CONFIG_HOME") or str(Path.home() / ".config")
    return Path(base) / _APP_NAME.lower() / _CONFIG_FILENAME


def load_config() -> dict:
    path = config_path()
    try:
        text = path.read_text(encoding="utf-8")
    except FileNotFoundError:
        return _empty()
    except OSError:
        logger.exception("Failed to read config from %s - starting fresh", path)
        return _empty()

    try:
        raw = json.loads(text)
    except json.JSONDecodeError:
        logger.exception("Config at %s is not valid JSON - backing up and starting fresh", path)
        _backup_invalid_file(path, text)
        return _empty()

    raw = _upgrade_from_v2(raw)
    if not _is_whole_config_valid(raw):
        logger.warning(
            "Config at %s is missing, malformed, or an unsupported older version - "
            "backing up and starting fresh", path,
        )
        _backup_invalid_file(path, text)
        return _empty()

    return _validate_sections(raw)


def save_config(cfg: dict) -> bool:
    """Write cfg atomically; return True on success so failures can be surfaced."""
    path = config_path()
    cfg["version"] = CONFIG_VERSION
    try:
        path.parent.mkdir(parents=True, exist_ok=True)
        tmp_path = path.with_suffix(path.suffix + ".tmp")
        with open(tmp_path, "w", encoding="utf-8") as f:
            f.write(json.dumps(cfg, indent=2))
            f.flush()
            os.fsync(f.fileno())
        os.replace(tmp_path, path)
        return True
    except OSError:
        logger.exception("Failed to save config to %s", path)
        return False


def _backup_invalid_file(path: Path, original_text: str) -> None:
    """Preserve discarded config (unparseable, wrong shape, unsupported version) as timestamped backup."""
    timestamp = datetime.now().strftime("%Y%m%dT%H%M%S")
    backup_path = path.with_name(f"{path.name}.invalid-{timestamp}")
    try:
        backup_path.write_text(original_text, encoding="utf-8")
        logger.info("Backed up invalid config to %s", backup_path)
    except OSError:
        logger.exception("Failed to back up invalid config to %s", backup_path)


def _empty() -> dict:
    return {"version": CONFIG_VERSION, "selected_device": None, "plugin_configs": {}, "devices": {}}


def _upgrade_from_v2(cfg):
    """v2 -> v3: phones are now keyed by id and paired per computer, so the old pairing and the
    per-device settings keyed by phone name are dropped. Global settings carry over."""
    if isinstance(cfg, dict) and cfg.get("version") == 2:
        cfg = dict(cfg)
        pcfg = cfg.get("plugin_configs")
        if isinstance(pcfg, dict):
            cfg["plugin_configs"] = {k: v for k, v in pcfg.items() if k != "connection"}
        cfg["devices"] = {}
        cfg["selected_device"] = None
        cfg["version"] = CONFIG_VERSION
        logger.info("Migrated config from version 2; phones need pairing again")
    return cfg


def _is_whole_config_valid(cfg) -> bool:
    if not isinstance(cfg, dict):
        return False
    version = cfg.get("version", 0)
    return isinstance(version, int) and not isinstance(version, bool) and version >= CONFIG_VERSION



def _valid_device_settings_entry(v) -> bool:
    """Validate per-device settings (active IP + plugin configs), distinct from roster entry."""
    if not isinstance(v, dict):
        return False
    active_ip = v.get("active_ip")
    if active_ip is not None and not isinstance(active_ip, str):
        return False
    if "plugin_configs" in v and not isinstance(v["plugin_configs"], dict):
        return False
    return True


def _validate_sections(cfg: dict) -> dict:
    """Validate each section independently; bad sections reset to defaults, rest retained."""
    result = dict(cfg)

    if not isinstance(result.get("plugin_configs"), dict):
        if "plugin_configs" in result:
            logger.warning("Config 'plugin_configs' section is malformed - resetting to defaults")
        result["plugin_configs"] = {}

    devices = result.get("devices")
    valid_devices = isinstance(devices, dict) and all(
        isinstance(k, str) and _valid_device_settings_entry(v) for k, v in devices.items()
    )
    if not valid_devices:
        if "devices" in result:
            logger.warning("Config 'devices' section is malformed - resetting to defaults")
        result["devices"] = {}

    selected = result.get("selected_device")
    if selected is not None and not isinstance(selected, str):
        logger.warning("Config 'selected_device' is malformed - resetting to default")
        result["selected_device"] = None
    elif "selected_device" not in result:
        result["selected_device"] = None

    return result


def _migrate(cfg: dict) -> dict:
    """Only v2 is upgraded; anything older is discarded for a fresh config."""
    cfg = _upgrade_from_v2(cfg)
    if not _is_whole_config_valid(cfg):
        return _empty()
    return _validate_sections(cfg)
