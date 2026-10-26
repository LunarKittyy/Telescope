"""The window's plugin update check: telling about new releases and, with auto-update on, installing them."""

import hashlib
import io
import json
import sys
import urllib.error
import zipfile

import pytest

from telescope import plugin_loader
from telescope.widgets.plugin_updater import PluginUpdater

REPO_URL = "https://github.com/someone/extra"


@pytest.fixture
def plugins(config_home, tmp_path, monkeypatch):
    folder = tmp_path / "plugins"
    folder.mkdir(mode=0o755)
    monkeypatch.setattr(plugin_loader, "plugins_dir", lambda: folder)
    yield folder
    for name in [m for m in sys.modules if m.startswith("telescope_ext.")]:
        del sys.modules[name]


def _manifest(name, version):
    return json.dumps({"name": name, "title": name.title(), "version": version, "api": 1, "class": "P",
                       "repo": REPO_URL})


_CODE = "from telescope.plugin import TelescopePlugin\n\nclass P(TelescopePlugin):\n    name = {name!r}\n"


def _install(folder, name="extra", version="1.0", on=True):
    target = folder / name
    target.mkdir()
    (target / "plugin.json").write_text(_manifest(name, version))
    (target / "__init__.py").write_text(_CODE.format(name=name))
    if on:
        plugin_loader.set_enabled(next(i for i in plugin_loader.find_installed(folder=folder) if i.name == name), True)


def _zip_bytes(name="extra", version="1.1") -> bytes:
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w") as zf:
        zf.writestr(f"{name}/plugin.json", _manifest(name, version))
        zf.writestr(f"{name}/__init__.py", _CODE.format(name=name))
    return buf.getvalue()


class _Reply(io.BytesIO):
    headers = {}


class _GitHub:
    def __init__(self, tag="v1.1", data=None, offline=False, corrupt=False):
        self.data = _zip_bytes() if data is None else data
        self.tag, self.offline, self.corrupt, self.asked = tag, offline, corrupt, []

    def __call__(self, url, timeout, headers=None):
        self.asked.append(url)
        if self.offline:
            raise urllib.error.URLError("offline")
        if url.startswith("https://api.github.com/"):
            return _Reply(json.dumps({"tag_name": self.tag, "html_url": f"{REPO_URL}/releases/tag/{self.tag}", "assets": [
                {"name": "extra.zip", "size": len(self.data), "browser_download_url": f"{REPO_URL}/x/extra.zip",
                 "digest": "sha256:" + hashlib.sha256(self.data).hexdigest()}]}).encode())
        return _Reply(b"x" * len(self.data) if self.corrupt else self.data)


def _check(qapp, updater):
    updater.maybe_check()
    updater.wait()
    qapp.processEvents()


def _item(folder, name="extra"):
    return next(i for i in plugin_loader.find_installed(folder=folder) if i.name == name)


def test_a_newer_release_is_only_mentioned_by_default(qapp, plugins):
    _install(plugins)
    github = _GitHub()
    updater = PluginUpdater(opener=github)
    _check(qapp, updater)
    assert updater.newer(_item(plugins)).tag == "v1.1"
    assert _item(plugins).version == "1.0" and len(github.asked) == 1  # nothing downloaded


def test_with_auto_update_on_a_plugin_thats_on_is_replaced_and_stays_on(qapp, plugins):
    _install(plugins)
    plugin_loader.set_plugin_switch("auto_update", True)
    updater = PluginUpdater(opener=_GitHub())
    _check(qapp, updater)
    item = _item(plugins)
    assert item.version == "1.1" and item.enabled and updater.updated == {"extra": "1.1"}
    assert updater.newer(item) is None


def test_auto_update_leaves_a_plugin_thats_off_alone(qapp, plugins):
    _install(plugins, on=False)
    plugin_loader.set_plugin_switch("auto_update", True)
    github = _GitHub()
    updater = PluginUpdater(opener=github)
    _check(qapp, updater)
    assert _item(plugins).version == "1.0" and updater.newer(_item(plugins)) is not None and len(github.asked) == 1


def test_auto_update_refuses_a_zip_holding_another_plugin(qapp, plugins):
    _install(plugins)
    plugin_loader.set_plugin_switch("auto_update", True)
    updater = PluginUpdater(opener=_GitHub(data=_zip_bytes(name="sneaky")))
    _check(qapp, updater)
    assert [i.name for i in plugin_loader.find_installed(folder=plugins)] == ["extra"]
    assert _item(plugins).version == "1.0" and _item(plugins).enabled and updater.updated == {}


def test_a_release_auto_update_tried_isnt_downloaded_again(qapp, plugins):
    _install(plugins)
    plugin_loader.set_plugin_switch("auto_update", True)
    github = _GitHub(data=_zip_bytes(name="sneaky"))
    _check(qapp, PluginUpdater(opener=github))
    updater = PluginUpdater(opener=github)
    updater._cache.repos.clear()
    updater._cache.save()  # forget the check but not what was tried
    _check(qapp, updater)
    assert [u for u in github.asked if u.endswith("extra.zip")] == [f"{REPO_URL}/x/extra.zip"]


def test_offline_nothing_happens_and_nothing_is_said(qapp, plugins):
    _install(plugins)
    plugin_loader.set_plugin_switch("auto_update", True)
    updater = PluginUpdater(opener=_GitHub(offline=True))
    changed = []
    updater.changed.connect(lambda: changed.append(True))
    _check(qapp, updater)
    assert updater.newer(_item(plugins)) is None and _item(plugins).version == "1.0" and changed == [True]


def test_with_the_check_off_github_isnt_asked(qapp, plugins):
    _install(plugins)
    plugin_loader.set_plugin_switch("check_updates", False)
    plugin_loader.set_plugin_switch("auto_update", True)
    github = _GitHub()
    _check(qapp, PluginUpdater(opener=github))
    assert github.asked == []


def test_a_download_that_fails_isnt_tried_again_every_hour(qapp, plugins):
    _install(plugins)
    plugin_loader.set_plugin_switch("auto_update", True)
    github = _GitHub(corrupt=True)  # doesn't match its digest
    updater = PluginUpdater(opener=github)
    _check(qapp, updater)
    _check(qapp, updater)  # the next hourly tick, the release still cached as newer
    assert [u for u in github.asked if u.endswith("extra.zip")] == [f"{REPO_URL}/x/extra.zip"]
    assert _item(plugins).version == "1.0" and updater.newer(_item(plugins)).tag == "v1.1"


def test_a_release_whose_zip_isnt_newer_isnt_installed(qapp, plugins):
    _install(plugins, version="1.1")
    plugin_loader.set_plugin_switch("auto_update", True)
    updater = PluginUpdater(opener=_GitHub(tag="v1.2", data=_zip_bytes(version="1.0")))  # mislabelled
    _check(qapp, updater)
    item = _item(plugins)
    assert item.version == "1.1" and item.enabled and updater.updated == {}
