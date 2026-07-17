import json
import logging
import os
import sys
from datetime import datetime
from pathlib import Path

logger = logging.getLogger(__name__)

CONFIG_VERSION = 2
_APP_NAME = "Telescope"
_CONFIG_FILENAME = "telescope_config.json"

# Plugin configs that are stored per-device rather than globally
DEVICE_LOCAL_PLUGINS = frozenset({"camera_control", "stream_output", "transforms", "monitoring"})


def config_path() -> Path:
    """Stable per-user config file location, independent of where the
    executable/script happens to run from."""
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

    if not _is_whole_config_valid(raw):
        logger.warning(
            "Config at %s is missing, malformed, or an unsupported older version - "
            "backing up and starting fresh", path,
        )
        _backup_invalid_file(path, text)
        return _empty()

    return _validate_sections(raw)


def save_config(cfg: dict) -> bool:
    """Write *cfg* atomically. Returns True on success so the caller can
    surface a persistence failure instead of silently losing settings."""
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
    """Preserves a config file that's about to be discarded (unparseable,
    wrong shape, or an unsupported older version) as a timestamped sibling,
    so a save-over-with-defaults doesn't just lose whatever was there."""
    timestamp = datetime.now().strftime("%Y%m%dT%H%M%S")
    backup_path = path.with_name(f"{path.name}.invalid-{timestamp}")
    try:
        backup_path.write_text(original_text, encoding="utf-8")
        logger.info("Backed up invalid config to %s", backup_path)
    except OSError:
        logger.exception("Failed to back up invalid config to %s", backup_path)


# ── Validation ───────────────────────────────────────────────────────────────

def _empty() -> dict:
    return {"version": CONFIG_VERSION, "selected_device": None, "plugin_configs": {}, "devices": {}}


def _is_whole_config_valid(cfg) -> bool:
    if not isinstance(cfg, dict):
        return False
    version = cfg.get("version", 0)
    return isinstance(version, int) and not isinstance(version, bool) and version >= CONFIG_VERSION


def _valid_device_settings_entry(v) -> bool:
    """Validates one entry of the top-level `devices` dict: per-device
    settings (active IP + device-local plugin configs), not to be confused
    with a connection-plugin device *roster* entry (name/ips/token)."""
    if not isinstance(v, dict):
        return False
    active_ip = v.get("active_ip")
    if active_ip is not None and not isinstance(active_ip, str):
        return False
    if "plugin_configs" in v and not isinstance(v["plugin_configs"], dict):
        return False
    return True


def _validate_sections(cfg: dict) -> dict:
    """Validates each top-level section independently. A current-version
    config no longer gets an all-or-nothing pass: a malformed section resets
    to its default while the rest of the config (and any valid sections) is
    retained, instead of the whole config being discarded over one bad
    section."""
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
    """No compatibility path for configs older than CONFIG_VERSION: this is a
    single-user, manually-updated app, so an out-of-date config is just
    discarded in favor of a fresh one rather than carrying per-version
    migration code. A current-version config has its sections validated
    independently instead - see _validate_sections."""
    if not _is_whole_config_valid(cfg):
        return _empty()
    return _validate_sections(cfg)
