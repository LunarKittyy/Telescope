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
    if not cfg:
        return _empty()
    version = cfg.get("version", 0)
    if version >= CONFIG_VERSION:
        return cfg
    if version == 0:
        cfg = _migrate_v0_to_v1(cfg)
    if cfg.get("version", 0) < 2:
        cfg = _migrate_v1_to_v2(cfg)
    return cfg


def _migrate_v0_to_v1(cfg: dict) -> dict:
    """Old flat format (single ip field) → Phase 3 plugin_configs format."""
    # If no ip key this isn't v0
    if "ip" not in cfg:
        cfg["version"] = 1
        return cfg
    old_ip   = cfg.pop("ip", "")
    old_name = "Phone"
    legacy_keys = ("resolution", "fps", "flip_h", "flip_v", "rotation",
                   "exp_manual", "iso", "shutter_ns", "ois",
                   "jpeg_quality", "phone_fps", "batt_alert", "temp_alert")
    dev_flat = {k: cfg.pop(k) for k in legacy_keys if k in cfg}
    if "batt_alert" in dev_flat:
        dev_flat["battery_alert"] = dev_flat.pop("batt_alert")
    result = {
        "version": 1,
        "plugin_configs": {
            "connection": {
                "mode":            cfg.get("mode", "usb"),
                "port":            str(cfg.get("port", 8080)),
                "devices_list":    [{"name": old_name, "ip": old_ip}],
                "selected_device": old_name,
            },
            "stream_output": {k: dev_flat[k] for k in ("resolution", "fps", "jpeg_quality", "phone_fps") if k in dev_flat},
            "transforms":    {k: dev_flat[k] for k in ("flip_h", "flip_v", "rotation") if k in dev_flat},
            "camera_control":{k: dev_flat[k] for k in ("exp_manual", "iso", "shutter_ns", "ois") if k in dev_flat},
            "monitoring":    {k: dev_flat[k] for k in ("battery_alert", "temp_alert") if k in dev_flat},
        },
    }
    if "unitycapture_installed" in cfg:
        result["unitycapture_installed"] = cfg["unitycapture_installed"]
    return result


def _migrate_v1_to_v2(cfg: dict) -> dict:
    """Phase 3 flat plugin_configs → per-device plugin_configs."""
    old_pcfg = cfg.get("plugin_configs", {})
    conn_cfg = old_pcfg.get("connection", {})

    selected      = conn_cfg.get("selected_device") or cfg.get("selected_device")
    devices_list  = conn_cfg.get("devices_list", [])

    # Strip selected_device out of the connection plugin slice
    new_conn_cfg = {k: v for k, v in conn_cfg.items() if k != "selected_device"}

    # Per-device plugin configs: take from the old global pool for the selected device
    per_device_pcfg = {k: v for k, v in old_pcfg.items() if k in DEVICE_LOCAL_PLUGINS}

    devices: dict = {}
    for d in devices_list:
        name = d["name"]
        devices[name] = {
            "plugin_configs": per_device_pcfg if name == selected else {}
        }
    if selected and selected not in devices:
        devices[selected] = {"plugin_configs": per_device_pcfg}

    result = {
        "version":         CONFIG_VERSION,
        "selected_device": selected,
        "plugin_configs":  {"connection": new_conn_cfg},
        "devices":         devices,
    }
    if uc := cfg.get("unitycapture_installed"):
        result["unitycapture_installed"] = uc
    return result
