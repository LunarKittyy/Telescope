import json
import logging
import os
import sys
from pathlib import Path

logger = logging.getLogger(__name__)

CONFIG_VERSION = 2
_APP_NAME = "Telescope"
_CONFIG_FILENAME = "telescope_config.json"

# Legacy location: next to the source tree / frozen exe. PyInstaller one-file
# builds run from a temporary extraction directory that's wiped after every
# launch, so this was never a stable per-user storage location - kept around
# only so an existing install can be migrated once to the real config path.
_LEGACY_CONFIG_FILE = Path(__file__).parent.parent / _CONFIG_FILENAME

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
    if not path.exists() and _LEGACY_CONFIG_FILE.exists():
        try:
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(_LEGACY_CONFIG_FILE.read_text(encoding="utf-8"), encoding="utf-8")
            logger.info("Migrated legacy config from %s to %s", _LEGACY_CONFIG_FILE, path)
        except OSError:
            logger.exception("Failed to migrate legacy config from %s", _LEGACY_CONFIG_FILE)

    try:
        raw = json.loads(path.read_text(encoding="utf-8"))
    except FileNotFoundError:
        return _empty()
    except (OSError, json.JSONDecodeError):
        logger.exception("Failed to read config from %s - starting fresh", path)
        return _empty()
    return _migrate(raw)


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


# ── Migration ─────────────────────────────────────────────────────────────────

def _empty() -> dict:
    return {"version": CONFIG_VERSION, "selected_device": None, "plugin_configs": {}, "devices": {}}


def _migrate(cfg: dict) -> dict:
    """No compatibility path for configs older than CONFIG_VERSION: this is a
    single-user, manually-updated app, so an out-of-date config is just
    discarded in favor of a fresh one rather than carrying migration code."""
    if not isinstance(cfg, dict):
        return _empty()
    version = cfg.get("version", 0)
    if not isinstance(version, int) or version < CONFIG_VERSION:
        return _empty()
    return cfg
