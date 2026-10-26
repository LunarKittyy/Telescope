"""The update check for added plugins, run by the window: a while after start, then hourly to see whether a day has
passed (plugin_updates.py does the asking). With the auto-update switch on, it also installs a newer release of a
plugin that's on and switches it on again, from the next start."""

import json
import logging
import shutil
import tempfile
import threading
import zipfile
from pathlib import Path
from typing import Callable, Optional

from PyQt6.QtCore import QObject, QTimer, pyqtSignal

from telescope import plugin_loader, plugin_updates

logger = logging.getLogger(__name__)

CHECK = "check_updates"
AUTO = "auto_update"
_FIRST_CHECK_MS = 30_000  # after the app's own update check, and well clear of starting up
_TICK_MS = 60 * 60 * 1000


def _zip_version(path: Path, name: str) -> str:
    """The version the plugin.json in a downloaded .zip gives, "" when it can't be read."""
    try:
        with zipfile.ZipFile(path) as zf:
            manifest = json.loads(zf.read(f"{name}/{plugin_loader.MANIFEST}").decode("utf-8"))
    except (OSError, KeyError, ValueError, zipfile.BadZipFile):
        return ""
    version = manifest.get("version") if isinstance(manifest, dict) else None
    return version if isinstance(version, str) else ""


class PluginUpdater(QObject):
    changed = pyqtSignal()  # a check found something, or a plugin was updated
    _done = pyqtSignal(object, object)  # the Cache, {plugin name: (downloaded .zip, release tag)}

    def __init__(self, reserved=(), parent=None, opener: Optional[Callable] = None):
        super().__init__(parent)
        self._reserved = set(reserved)
        self._opener = opener
        self._cache = plugin_updates.Cache()
        self._thread: Optional[threading.Thread] = None
        self.updated: dict = {}  # plugin name: the version auto-update installed this run
        self._done.connect(self._on_done)
        self._timer = QTimer(self)
        self._timer.timeout.connect(self.maybe_check)

    def start(self, first_ms: int = _FIRST_CHECK_MS):
        self._timer.start(_TICK_MS)
        first = QTimer(self)  # owned, so it goes with the window
        first.setSingleShot(True)
        first.timeout.connect(self.maybe_check)
        first.start(first_ms)

    def checking(self) -> bool:
        return plugin_loader.plugin_switch(CHECK, True)

    def auto(self) -> bool:
        return self.checking() and plugin_loader.plugin_switch(AUTO, False)

    def running(self) -> bool:
        return self._thread is not None and self._thread.is_alive()

    def newer(self, item) -> Optional[plugin_updates.Release]:
        """The release newer than the installed item that the last check found, or None."""
        repo = plugin_updates.github_repo(item.repo)
        release = self._cache.release(repo) if repo and self.checking() else None
        return release if release and plugin_updates.is_newer(release.tag, item.version) else None

    def maybe_check(self):
        """Check the repos that are due, in the background; quiet whatever happens."""
        try:
            if not self.checking() or self.running():
                return
            installed = [i for i in plugin_loader.find_installed(self._reserved) if not i.dev]
        except OSError:
            logger.warning("Couldn't list the plugins to check for updates", exc_info=True)
            return
        repos = {r for r in (plugin_updates.github_repo(i.repo) for i in installed) if r}
        if not repos:
            return
        auto = [(i.name, plugin_updates.github_repo(i.repo), i.version) for i in installed
                if self.auto() and i.enabled and not i.problem and plugin_updates.github_repo(i.repo)]
        self._thread = threading.Thread(target=self._run, args=(repos, auto), daemon=True, name="plugin-updates")
        self._thread.start()

    def _run(self, repos: set, auto: list):
        extra = {"opener": self._opener} if self._opener else {}
        cache = plugin_updates.check(repos, plugin_updates.Cache(self._cache.path), **extra)
        downloads, failed = {}, False
        for name, repo, installed in auto:
            release = cache.release(repo)
            if (release is None or not plugin_updates.is_newer(release.tag, installed)
                    or cache.auto_installed.get(name) == release.tag):
                continue
            asset = release.zip_for(name)
            if asset is None:
                logger.info("The latest release of %s has no .zip to update %s from", repo, name)
                continue
            folder = Path(tempfile.mkdtemp(prefix="telescope-plugin-"))
            try:
                downloads[name] = (plugin_updates.download(asset, folder, **extra), release.tag)
            except plugin_updates.DownloadError as exc:
                logger.info("Couldn't update %s: %s", name, exc)
                shutil.rmtree(folder, ignore_errors=True)
                cache.auto_installed[name] = release.tag  # not downloaded again every hour
                failed = True
        if failed:
            cache.save()  # the GUI thread only writes it once this thread is done
        try:
            self._done.emit(cache, downloads)
        except RuntimeError:  # the window went away first
            for path, _tag in downloads.values():
                shutil.rmtree(path.parent, ignore_errors=True)

    def _on_done(self, cache, downloads: dict):
        self._cache = cache
        for name, (path, tag) in downloads.items():
            try:
                self._install(name, path, tag)
            finally:
                shutil.rmtree(path.parent, ignore_errors=True)
        if downloads:
            cache.save()
        self.changed.emit()

    def _install(self, name: str, path: Path, tag: str):
        """Put a downloaded update in place and switch it on, if auto-update is still on and so is the plugin."""
        self._cache.auto_installed[name] = tag  # tried once: a release that doesn't install isn't fetched daily
        try:
            current = next((i for i in plugin_loader.find_installed(self._reserved) if i.name == name), None)
            if not self.auto() or current is None or current.dev or not current.enabled:
                return
            if plugin_loader.zip_plugin_name(path) != name:
                raise ValueError(f"its .zip holds another plugin, not {name}")
            version = _zip_version(path, name)
            if not plugin_updates.is_newer(version, current.version):
                raise ValueError(f"its .zip is version {version or '?'}, not newer than {current.version or '?'}")
            item = plugin_loader.install_zip(path, self._reserved, replace=True)
            if item.problem:
                logger.warning("Updated %s to %s, but it can't load: %s", name, tag, item.problem)
            else:
                plugin_loader.set_enabled(item, True)
                logger.info("Updated %s to %s by itself", name, tag)
            self.updated[name] = item.version or tag
        except (OSError, ValueError) as exc:
            logger.warning("Couldn't update %s to %s: %s", name, tag, exc)

    def wait(self, timeout: float = 5.0):
        """For tests: let a running check finish."""
        if self._thread is not None:
            self._thread.join(timeout)
