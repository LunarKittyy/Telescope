"""Plugins that don't come with Telescope: finding them, turning them on, and loading the ones that are on.

Each lives in a folder of its own under plugins_dir(): a plugin.json describing it and an __init__.py with its
TelescopePlugin class (docs/writing-plugins.md). Nothing in a folder runs until the user turns that plugin on, and turning it
on remembers a fingerprint of its files: a plugin that changes afterwards stays off until it's turned on again.

Which plugins are on is kept in the settings file, under "plugins": "off" lists the built-in ones turned off,
"noticed" the ones of those the user has been told about, and "added" maps each added one that's on to that
fingerprint.
"""

import hashlib
import importlib.util
import json
import logging
import os
import re
import shutil
import sys
import tempfile
import types
import zipfile
from dataclasses import dataclass, field
from pathlib import Path, PurePosixPath
from typing import Optional

from telescope import dev_profile
from telescope.config import config_path, load_config, save_config
from telescope.plugin import TelescopePlugin
from telescope.i18n import N_, PLUGIN_PACKAGE, add_plugin_catalog, tr

logger = logging.getLogger(__name__)

PLUGIN_API = 1
"""The plugin API this Telescope offers (HostServices, the hooks, EventBus). Raised when a change would break a plugin
written for the one before; OLDEST_PLUGIN_API is the oldest one still loaded."""
OLDEST_PLUGIN_API = 1

MANIFEST = "plugin.json"
_NAME = re.compile(r"[a-z][a-z0-9_]{1,31}\Z")
_CLASS = re.compile(r"[A-Za-z_][A-Za-z0-9_]{0,63}\Z")
_TEXT_MAX = 300  # longest title, author or description shown
_MAX_FILES = 500
_MAX_BYTES = 20 * 1024 * 1024  # a plugin folder, or what a .zip unpacks to
_CACHE = "__pycache__"  # Python writes these next to the code when it's imported; cleared before each load
_SHARED = N_("Other users on this computer could change it, so it isn't loaded.")
_LINK = N_("It contains a link to somewhere else, which plugins can't have.")
_PACKAGE = PLUGIN_PACKAGE  # loaded plugins are telescope_ext.<name>, never on sys.path


def plugins_dir() -> Path:
    """Where plugins are installed: a "plugins" folder next to the settings."""
    return config_path().parent / "plugins"


@dataclass
class Installed:
    """A plugin folder and what its plugin.json says; problem is why it can't be loaded ("" if it can)."""

    folder: Path
    name: str
    title: str = ""
    version: str = ""
    author: str = ""
    description: str = ""
    api: int = 0
    class_name: str = ""
    digest: str = ""
    problem: str = ""
    enabled: bool = False
    changed: bool = False  # turned on, but its files differ from when it was
    dev: bool = False  # loaded from where it's written by dev_desktop.py --plugin, always on
    error: str = field(default="", compare=False)  # what went wrong loading it this run


def _text(value) -> str:
    """Manifest text as one plain line: no control characters, capped."""
    text = "".join(ch for ch in str(value) if ch.isprintable()) if isinstance(value, (str, int, float)) else ""
    return text.strip()[:_TEXT_MAX]


def _files(folder: Path, skip_hidden: bool = False) -> list:
    """The plugin's files, relative to folder and sorted; raises ValueError for a link, compiled code, a file other
    users could change, or a folder too big. skip_hidden: leave out .git and the like (a plugin being written)."""
    found, total = [], 0
    for root, dirs, names in os.walk(folder, followlinks=False):
        here = Path(root)
        if skip_hidden:
            dirs[:] = [d for d in dirs if not d.startswith(".")]
            names = [n for n in names if not n.startswith(".")]
        for d in dirs:
            if (here / d).is_symlink():
                raise ValueError(tr(_LINK))
        dirs[:] = sorted(d for d in dirs if d != _CACHE)
        if _unsafe_permissions(here):
            raise ValueError(tr(_SHARED))
        for n in sorted(names):
            path = here / n
            if path.is_symlink():
                raise ValueError(tr(_LINK))
            if n.endswith((".pyc", ".pyo", ".so", ".pyd")):
                raise ValueError(tr("It contains compiled code (like .pyc files). Plugins have to be source code."))
            if _unsafe_permissions(path):
                raise ValueError(tr(_SHARED))
            total += path.stat().st_size
            found.append(path.relative_to(folder))
            if len(found) > _MAX_FILES or total > _MAX_BYTES:
                raise ValueError(tr("It's too big for a plugin (over 500 files or 20 MB)."))
    return found


def digest(folder: Path, skip_hidden: bool = False) -> str:
    """A fingerprint of every file in folder (names and contents), to notice a plugin that changed."""
    h = hashlib.sha256()
    for rel in _files(folder, skip_hidden):
        h.update(rel.as_posix().encode() + b"\0")
        h.update(hashlib.sha256((folder / rel).read_bytes()).digest())
    return h.hexdigest()


def _unsafe_permissions(path: Path) -> bool:
    """On Linux, whether another user could change path (so it could run as this one)."""
    if os.name != "posix":
        return False
    st = path.stat()
    if st.st_uid != os.getuid() or st.st_mode & 0o002:
        return True
    return bool(st.st_mode & 0o020) and not _private_group(st.st_gid)


def _private_group(gid: int) -> bool:
    """Whether gid is this user's own group with nobody else in it (Fedora's umask 002 makes folders group-writable)."""
    try:
        import grp
        import pwd
        group = grp.getgrgid(gid)
        me = pwd.getpwuid(os.getuid())
        if group.gr_name != me.pw_name or group.gr_mem:
            return False
        return not any(user.pw_gid == gid and user.pw_uid != me.pw_uid for user in pwd.getpwall())
    except (ImportError, KeyError):
        return False


def make_plugins_dir(folder: Optional[Path] = None) -> Path:
    """The plugins folder, made if it isn't there yet, readable and writable by this user alone."""
    folder = folder or plugins_dir()
    folder.mkdir(parents=True, exist_ok=True, mode=0o700)
    return folder


def _state(cfg: dict) -> tuple:
    """(the built-in plugins turned off, {added plugin that's on: its fingerprint}) as the settings have them."""
    raw = cfg.get("plugins")
    raw = raw if isinstance(raw, dict) else {}
    off, added = raw.get("off"), raw.get("added")
    off = {n for n in off if isinstance(n, str)} if isinstance(off, list) else set()
    added = ({k: v for k, v in added.items() if isinstance(k, str) and isinstance(v, str)}
             if isinstance(added, dict) else {})
    return off, added


def _noticed(cfg: dict) -> set:
    raw = cfg.get("plugins")
    noticed = raw.get("noticed") if isinstance(raw, dict) else None
    return {n for n in noticed if isinstance(n, str)} if isinstance(noticed, list) else set()


def _optional_on(cfg: dict) -> set:
    raw = cfg.get("plugins")
    on = raw.get("on") if isinstance(raw, dict) else None
    return {n for n in on if isinstance(n, str)} if isinstance(on, list) else set()


def _save_state(off: set, added: dict, noticed: Optional[set] = None, on: Optional[set] = None):
    """Write the plugin switches; raises OSError when the settings can't be read or written (nothing is lost)."""
    cfg = load_config(strict=True)
    noticed = _noticed(cfg) if noticed is None else noticed
    on = _optional_on(cfg) if on is None else on
    cfg["plugins"] = {"off": sorted(off), "noticed": sorted(noticed & off), "added": dict(sorted(added.items()))}
    if on:
        cfg["plugins"]["on"] = sorted(on)
    if not save_config(cfg):
        raise OSError(tr("Couldn't save the settings."))


def _read_enabled() -> dict:
    return _state(load_config())[1]


def builtin_off() -> set:
    """The names of the built-in plugins the user turned off."""
    return _state(load_config())[0]


def set_builtin_off(name: str, off: bool):
    """Turn a built-in plugin off (or back on) from the next start."""
    names, added = _state(load_config(strict=True))
    (names.add if off else names.discard)(name)
    _save_state(names, added)


def unnoticed_off() -> set:
    """Built-in plugins turned off that the user hasn't been reminded of yet (a banner at start)."""
    cfg = load_config()
    return _state(cfg)[0] - _noticed(cfg)


def notice_off():
    """The user has seen which built-in plugins are off: don't remind them again until another one is."""
    off, added = _state(load_config(strict=True))
    _save_state(off, added, noticed=off)


def all_builtin_on():
    """Turn every built-in plugin back on, from the next start (not the optional ones)."""
    _save_state(set(), _state(load_config(strict=True))[1])


def optional_on() -> set:
    """The names of the optional built-in plugins the user switched on (they start out off)."""
    return _optional_on(load_config())


def set_optional_on(name: str, on: bool):
    """Switch an optional built-in plugin on (or off again) from the next start."""
    cfg = load_config(strict=True)
    names = _optional_on(cfg)
    (names.add if on else names.discard)(name)
    _save_state(*_state(cfg), on=names)


def find_installed(reserved=(), folder: Optional[Path] = None) -> list:
    """Every plugin folder, by name, with whether it's on and why it can't load. reserved: names already taken (the
    built-in plugins')."""
    folder = folder or plugins_dir()
    dev = [_inspect(path, reserved, {}, False, dev=True) for path in dev_profile.plugin_folders()]
    if not folder.is_dir():
        return dev
    enabled = _read_enabled()
    shared = _unsafe_permissions(folder)
    found = []
    for path in sorted(p for p in folder.iterdir() if p.is_dir() and not p.name.startswith(".")):
        if path.name not in {d.name for d in dev}:  # the copy being written stands in for an installed one
            found.append(_inspect(path, reserved, enabled, shared))
    return found + dev


def _inspect(path: Path, reserved, enabled: dict, shared: bool, dev: bool = False) -> Installed:
    plugin = Installed(folder=path, name=path.name, title=path.name, dev=dev)

    def problem(text: str) -> Installed:
        plugin.problem = text
        return plugin

    if path.is_symlink():
        return problem(tr("It's a link to another folder, which plugins can't be."))
    if not _NAME.match(path.name):
        return problem(tr("The folder has to have the same name as the plugin, using only lowercase letters, digits and _ "
                       "(starting with a letter)."))
    try:
        manifest = json.loads((path / MANIFEST).read_text(encoding="utf-8"))
    except OSError:
        return problem(tr("It has no {file}.", file=MANIFEST))
    except ValueError:
        return problem(tr("Its {file} isn't valid JSON.", file=MANIFEST))
    if not isinstance(manifest, dict):
        return problem(tr("Its {file} isn't valid.", file=MANIFEST))
    plugin.title = _text(manifest.get("title")) or path.name
    plugin.version = _text(manifest.get("version"))
    plugin.author = _text(manifest.get("author"))
    plugin.description = _text(manifest.get("description"))
    api = manifest.get("api")
    plugin.api = api if isinstance(api, int) and not isinstance(api, bool) else 0
    plugin.class_name = manifest.get("class") if isinstance(manifest.get("class"), str) else ""
    try:
        plugin.digest = digest(path, skip_hidden=dev)
    except (OSError, ValueError) as exc:
        plugin.changed = path.name in enabled  # it was on: say why it didn't start
        plugin.enabled = dev
        return problem(str(exc) if isinstance(exc, ValueError) else tr("Its files couldn't be read."))
    plugin.enabled = dev or enabled.get(path.name) == plugin.digest
    plugin.changed = not dev and path.name in enabled and not plugin.enabled
    if manifest.get("name") != path.name:
        return problem(tr('Its {file} has to give the folder\'s name as "name".', file=MANIFEST))
    if path.name in reserved:
        return problem(tr("A plugin that comes with Telescope already has that name."))
    if plugin.api > PLUGIN_API:
        return problem(tr("It needs a newer Telescope (plugin API {wanted}; this one has {have}).",
                          wanted=plugin.api, have=PLUGIN_API))
    if plugin.api < OLDEST_PLUGIN_API:
        return problem(tr("It was made for an older Telescope (plugin API {api}) and needs an update.",
                          api=plugin.api or "?"))
    if not _CLASS.match(plugin.class_name):
        return problem(tr('Its {file} has to name its plugin class as "class".', file=MANIFEST))
    if not (path / "__init__.py").is_file():
        return problem(tr("It has no __init__.py."))
    if shared or _unsafe_permissions(path):
        return problem(tr(_SHARED))
    return plugin


def set_enabled(plugin: Installed, on: bool):
    """Turn an added plugin on (as its files are now) or off, from the next start."""
    off, added = _state(load_config(strict=True))
    if on:
        added[plugin.name] = digest(plugin.folder)
    else:
        added.pop(plugin.name, None)
    _save_state(off, added)


def load(plugin: Installed) -> TelescopePlugin:
    """Import a plugin that's on and make its object; raises with what went wrong."""
    if plugin.problem or not plugin.enabled:
        raise RuntimeError(plugin.problem or tr("It isn't turned on."))
    if _PACKAGE not in sys.modules:
        package = types.ModuleType(_PACKAGE)
        package.__path__ = []  # a namespace for the plugins, with nothing to import from on its own
        sys.modules[_PACKAGE] = package
    module_name = f"{_PACKAGE}.{plugin.name}"
    add_plugin_catalog(plugin.name, plugin.folder)  # its locale/<lang>.po, if it has one; never keeps it from loading
    if not plugin.dev:  # the author's own folder: their code, and their caches
        _clear_caches(plugin.folder)
    spec = importlib.util.spec_from_file_location(module_name, plugin.folder / "__init__.py",
                                                  submodule_search_locations=[str(plugin.folder)])
    module = importlib.util.module_from_spec(spec)
    sys.modules[module_name] = module  # its own modules import each other relatively, as from . import x
    try:
        spec.loader.exec_module(module)
    except BaseException:
        sys.modules.pop(module_name, None)
        raise
    cls = getattr(module, plugin.class_name, None)
    if not (isinstance(cls, type) and issubclass(cls, TelescopePlugin)):
        raise TypeError(tr("{name} isn't a TelescopePlugin class.", name=plugin.class_name))
    instance = cls()
    if instance.name != plugin.name:
        raise ValueError(tr("Its class has name = {have}; it has to be {want}, as in {file}.",
                            have=repr(instance.name), want=repr(plugin.name), file=MANIFEST))
    instance.title = instance.title or plugin.title
    instance.about = instance.about or plugin.description
    return instance


def _clear_caches(folder: Path):
    """Delete the plugin's __pycache__ folders, so what runs is compiled from the source that was checked."""
    for root, dirs, _names in os.walk(folder, followlinks=False):
        for d in dirs:
            path = Path(root) / d
            if d == _CACHE and not path.is_symlink():
                shutil.rmtree(path)
        dirs[:] = [d for d in dirs if d != _CACHE]


def load_enabled(reserved=(), folder: Optional[Path] = None) -> tuple:
    """(plugins to register, the Installed entries that are on but couldn't load, error filled in)."""
    loaded, failed = [], []
    for plugin in find_installed(reserved, folder):
        if not plugin.enabled and not plugin.changed:
            continue
        if plugin.dev and plugin.problem:
            plugin.error = plugin.problem
            failed.append(plugin)
            continue
        if plugin.changed:
            plugin.error = plugin.problem or tr("Its files changed since it was turned on. Turn it on again if you "
                                                "trust this version.")
            failed.append(plugin)
            continue
        try:
            loaded.append(load(plugin))
            logger.info("Loaded plugin %s %s", plugin.name, plugin.version)
        except (Exception, SystemExit) as exc:  # a sys.exit() in a plugin mustn't keep Telescope from starting
            logger.exception("Plugin %s couldn't load", plugin.name)
            plugin.error = plugin.problem or f"{type(exc).__name__}: {exc}"[:_TEXT_MAX]
            failed.append(plugin)
    return loaded, failed


def install_zip(archive: Path, reserved=(), folder: Optional[Path] = None) -> Installed:
    """Unpack a plugin .zip (one folder, named as the plugin) into the plugins folder, turned off. Raises ValueError
    saying what's wrong with it, or FileExistsError if a plugin of that name is already there."""
    folder = folder or plugins_dir()
    try:
        with zipfile.ZipFile(archive) as zf:
            entries = zf.infolist()
            if len(entries) > _MAX_FILES or sum(e.file_size for e in entries) > _MAX_BYTES:
                raise ValueError(tr("It's too big for a plugin (over 500 files or 20 MB)."))
            tops = set()
            for e in entries:
                parts = PurePosixPath(e.filename).parts
                if (not parts or e.filename.startswith(("/", "\\")) or "\\" in e.filename
                        or any(p in ("..", "") or ":" in p for p in parts)):
                    raise ValueError(tr("It holds a file with a path that points outside the plugin."))
                if (e.external_attr >> 16) & 0o170000 == 0o120000:
                    raise ValueError(tr(_LINK))
                tops.add(parts[0])
            if len(tops) != 1:
                raise ValueError(tr("It has to hold one folder, named as the plugin."))
            name = tops.pop()
            if not _NAME.match(name):
                raise ValueError(tr("The folder has to have the same name as the plugin, using only lowercase letters, "
                                 "digits and _ (starting with a letter)."))
            if name in reserved:
                raise ValueError(tr("A plugin that comes with Telescope already has that name."))
            if (folder / name).exists():
                raise FileExistsError(name)
            make_plugins_dir(folder)
            staging = Path(tempfile.mkdtemp(prefix=".adding-", dir=folder))
            try:
                for e in entries:
                    target = staging.joinpath(*PurePosixPath(e.filename).parts)
                    if e.is_dir():
                        target.mkdir(parents=True, exist_ok=True)
                        continue
                    target.parent.mkdir(parents=True, exist_ok=True)
                    with zf.open(e) as src, open(target, "wb") as dst:
                        shutil.copyfileobj(src, dst)
                for root, dirs, names in os.walk(staging / name):
                    os.chmod(root, 0o755)
                    for n in names:
                        os.chmod(Path(root) / n, 0o644)
                os.replace(staging / name, folder / name)
            finally:
                shutil.rmtree(staging, ignore_errors=True)
    except zipfile.BadZipFile:
        raise ValueError(tr("It isn't a .zip file Telescope can open.")) from None
    return _inspect(folder / name, reserved, _read_enabled(), _unsafe_permissions(folder))


def remove(plugin: Installed, folder: Optional[Path] = None):
    """Delete a plugin's folder (it has to be one directly in the plugins folder) and forget it was on."""
    folder = folder or plugins_dir()
    target = plugin.folder
    if target.is_symlink() or target.resolve().parent != folder.resolve() or target.name != plugin.name:
        raise ValueError(tr("That isn't a folder in the plugins folder."))
    set_enabled(plugin, False)
    shutil.rmtree(target)
