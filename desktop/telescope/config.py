import json
import logging
import os
import sys
import time
from datetime import datetime
from pathlib import Path
from typing import Callable, Optional

from telescope import dev_profile

logger = logging.getLogger(__name__)

CONFIG_VERSION = 3
_APP_NAME = "Telescope"
_CONFIG_FILENAME = "telescope_config.json"

# Plugin configs that are stored per-device rather than globally
DEVICE_LOCAL_PLUGINS = frozenset({"camera_control", "stream_output", "transforms", "monitoring", "presets", "microphone"})


_last_backed_up: Optional[tuple] = None
_reset_notice: Optional[str] = None


def config_path() -> Path:
    """Stable per-user config file location (XDG_CONFIG_HOME or ~/.config), or the dev profile's folder."""
    if dev_profile.active():
        return dev_profile.folder() / _CONFIG_FILENAME
    if sys.platform == "win32":
        base = os.environ.get("APPDATA") or str(Path.home())
        return Path(base) / _APP_NAME / _CONFIG_FILENAME
    base = os.environ.get("XDG_CONFIG_HOME") or str(Path.home() / ".config")
    return Path(base) / _APP_NAME.lower() / _CONFIG_FILENAME


def load_config(strict: bool = False) -> dict:
    """strict: raise OSError when the file is there but can't be read, for callers about to save over it."""
    path = config_path()
    for attempt in range(3):
        try:
            data = path.read_bytes()
            break
        except FileNotFoundError:
            return _empty()
        except OSError:
            # Often brief (a virus scanner or sync app holding the file); starting fresh would save over it later
            if attempt == 2:
                if strict:
                    raise
                logger.exception("Failed to read config from %s - starting fresh", path)
                return _empty()
            time.sleep(0.2)

    try:
        raw = json.loads(data.decode("utf-8-sig"))  # -sig: Notepad saves a byte order mark
    except (ValueError, RecursionError):  # not JSON, not UTF-8, a number too long to read, or nested too deep
        logger.exception("Config at %s is not valid JSON - backing up and starting fresh", path)
        _backup_invalid_file(path, data)
        return _empty()

    raw = _upgrade_from_v2(raw)
    if not _is_whole_config_valid(raw):
        logger.warning(
            "Config at %s is missing, malformed, or an unsupported older version - "
            "backing up and starting fresh", path,
        )
        _backup_invalid_file(path, data)
        return _empty()

    return _validate_sections(raw)


def save_config(cfg: dict) -> bool:
    """Write cfg atomically; return True on success so failures can be surfaced."""
    path = config_path()
    cfg["version"] = CONFIG_VERSION
    try:
        text = json.dumps(cfg, indent=2, default=_plain)  # before opening, so a bad value can't leave a half-written file
    except (TypeError, ValueError):
        logger.exception("Config has a value that can't be saved; keeping the last saved config")
        return False
    try:
        _private_dir(path.parent)
        tmp_path = path.with_suffix(path.suffix + ".tmp")
        with _open_private(tmp_path) as f:
            f.write(text)
            f.flush()
            os.fsync(f.fileno())
        os.replace(tmp_path, path)
        return True
    except OSError:
        logger.exception("Failed to save config to %s", path)
        return False


def _private_dir(path: Path) -> None:
    # The config holds pairing tokens: only this user may read it (no-op on Windows, where %APPDATA% is already per-user)
    path.mkdir(parents=True, exist_ok=True, mode=0o700)
    if os.name == "posix":
        _tighten(lambda: os.chmod(path, 0o700), path)


def _open_private(path: Path, mode: str = "w"):
    # 0600 from creation, and again for a file left from before, since the mode argument only applies to new files
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC | getattr(os, "O_BINARY", 0), 0o600)
    try:
        if os.name == "posix":
            _tighten(lambda: os.fchmod(fd, 0o600), path)
        return open(fd, mode, encoding=None if "b" in mode else "utf-8")
    except BaseException:
        os.close(fd)
        raise


def _tighten(chmod: Callable, path: Path) -> None:
    # A filesystem without Unix permissions (a FAT drive, some sync folders) shouldn't stop the config saving
    try:
        chmod()
    except OSError:
        logger.warning("Couldn't restrict permissions on %s", path)


def _plain(value):
    # A plugin handing back a numpy number or array is saved as the plain value it stands for
    if hasattr(value, "tolist"):
        return value.tolist()
    raise TypeError(f"{type(value).__name__} can't be saved in the config")


def take_reset_notice() -> Optional[str]:
    """Once, after a config was set aside: where the copy went ("" when it couldn't be kept). None: nothing happened."""
    global _reset_notice
    notice, _reset_notice = _reset_notice, None
    return notice


def _backup_invalid_file(path: Path, original: bytes) -> None:
    """Preserve discarded config (unparseable, wrong shape, unsupported version) as timestamped backup."""
    global _last_backed_up, _reset_notice
    if _last_backed_up == (path, original):
        return  # every load before the first save sees the same file; one copy is enough
    _last_backed_up = (path, original)
    _reset_notice = ""
    timestamp = datetime.now().strftime("%Y%m%dT%H%M%S")
    backup_path = path.with_name(f"{path.name}.invalid-{timestamp}")
    try:
        with _open_private(backup_path, "wb") as f:
            f.write(original)
        _reset_notice = str(backup_path)
        logger.info("Backed up invalid config to %s", backup_path)
    except OSError:
        logger.exception("Failed to back up invalid config to %s", backup_path)


def _empty() -> dict:
    return {"version": CONFIG_VERSION, "selected_device": None, "plugin_configs": {}, "devices": {}}


# LEGACY MIGRATION PATH: configs from v2.3 and earlier; remove once the next stable release has been out a while.
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
    cfg = _upgrade_from_v2(cfg)  # LEGACY MIGRATION PATH
    if not _is_whole_config_valid(cfg):
        return _empty()
    return _validate_sections(cfg)
