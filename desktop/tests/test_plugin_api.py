"""The plugin API, frozen: every hook, host method, bus signal and helper a plugin can use, checked against
plugin_api.txt so nobody breaks plugins by accident (ARCHITECTURE.md, "Keeping plugins working").

After a change on purpose, write the new list with UPDATE_PLUGIN_API=1 python -m pytest tests/test_plugin_api.py"""

import inspect
import os
from pathlib import Path

from telescope import colour, i18n, overlays, plugin, sensors, shortcuts, stills
from telescope.plugin_loader import OLDEST_PLUGIN_API, PLUGIN_API
from telescope.widgets import banner

FROZEN = Path(__file__).with_name("plugin_api.txt")

# What docs/writing-plugins.md and plugin.py offer to plugins; anything else in Telescope can change freely
_PROTOCOLS = (plugin.HostSettings, plugin.HostStreams, plugin.HostCamera, plugin.HostSources, plugin.HostShell,
              plugin.HostView, plugin.HostVision, plugin.HostMotion, plugin.HostPreview, plugin.HostStills,
              plugin.HostColour, plugin.HostSensors, plugin.StreamSource)
_CLASSES = (plugin.TelescopePlugin, plugin.StreamsBehind, plugin.SettingsSection, colour.ColourStage,
            overlays.PreviewLayer, overlays.Pick,
            banner.Issue, banner.BannerAction, shortcuts.Param,
            shortcuts.ShortcutAction, stills.StillCaps, stills.StillResult, stills.StillProgress, sensors.SensorReading,
            sensors.SensorStatus)
_FUNCTIONS = (banner.copy_action, shortcuts.button_action, shortcuts.slider_action, shortcuts.log_slider_action,
              shortcuts.choice_action, i18n.tr, i18n.trn, i18n.N_)


def _value(value) -> str:
    if value is plugin.UNCHANGED:
        return "UNCHANGED"
    if isinstance(value, (str, int, float, bool, type(None), tuple)):
        return repr(value)
    if "DEFAULT_FACTORY" in type(value).__name__:  # a dataclass field(default_factory=...)
        return "<factory>"
    return getattr(value, "__name__", type(value).__name__)  # an object's repr has its address in it


def _signature(fn) -> str:
    """Names, kinds and defaults only: type hints don't break a plugin and their text differs between Pythons."""
    parts, star_done = [], False
    for p in inspect.signature(fn).parameters.values():
        if p.kind is p.VAR_POSITIONAL:
            parts.append(f"*{p.name}")
            star_done = True
            continue
        if p.kind is p.VAR_KEYWORD:
            parts.append(f"**{p.name}")
            continue
        if p.kind is p.KEYWORD_ONLY and not star_done:
            parts.append("*")
            star_done = True
        parts.append(p.name if p.default is p.empty else f"{p.name}={_value(p.default)}")
    return f"({', '.join(parts)})"


def _members(cls) -> list:
    lines = []
    for name, value in vars(cls).items():
        if name.startswith("_"):
            continue
        if callable(value) and not isinstance(value, type):
            lines.append(f"{cls.__name__}.{name}{_signature(value)}")
        elif not isinstance(value, (staticmethod, classmethod, property)):
            lines.append(f"{cls.__name__}.{name} = {_value(value)}")
    for name in getattr(cls, "__annotations__", {}):
        if not name.startswith("_") and name not in vars(cls):
            lines.append(f"{cls.__name__}.{name}")
    if ("__dataclass_fields__" in vars(cls) or "__init__" in vars(cls)) and not getattr(cls, "_is_protocol", False):
        lines.append(f"{cls.__name__}{_signature(cls)}")
    return lines


def current() -> str:
    lines = [f"PLUGIN_API = {PLUGIN_API}", f"OLDEST_PLUGIN_API = {OLDEST_PLUGIN_API}"]
    for proto in _PROTOCOLS + _CLASSES:
        lines += _members(proto)
    for name, value in vars(plugin.EventBus).items():
        if type(value).__name__ == "pyqtSignal":
            lines.append(f"EventBus.{name}({value.signatures[0].rstrip(')').split('(')[-1]})")
    lines += [f"{fn.__module__.rsplit('.', 1)[-1]}.{fn.__name__}{_signature(fn)}" for fn in _FUNCTIONS]
    return "\n".join(sorted(lines)) + "\n"


def test_the_plugin_api_only_changes_on_purpose():
    now = current()
    if os.environ.get("UPDATE_PLUGIN_API"):
        FROZEN.write_text(now, encoding="utf-8")
    was = FROZEN.read_text(encoding="utf-8")
    if now == was:
        return
    gone = sorted(set(was.splitlines()) - set(now.splitlines()))
    new = sorted(set(now.splitlines()) - set(was.splitlines()))
    advice = ("Only additions, which plugins don't mind. Write the new list with UPDATE_PLUGIN_API=1." if not gone else
              "Something plugins may use changed or went away, which breaks the ones using it. Keep the old one "
              "working next to the new one if you can (ARCHITECTURE.md, \"Keeping plugins working\"). Only if you "
              "can't, raise PLUGIN_API in plugin_loader.py. Then write the new list with UPDATE_PLUGIN_API=1.")
    raise AssertionError("The plugin API changed.\n" + "".join(f"\n  - {line}" for line in gone)
                         + "".join(f"\n  + {line}" for line in new) + f"\n\n{advice}")

