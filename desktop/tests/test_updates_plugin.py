import time
"""The update button, dialog and flow on top of telescope/updates.py."""

import pytest
from PyQt6.QtWidgets import QWidget

import telescope.plugins.updates as plugin_module
from telescope import updates, version
from telescope.plugin import EventBus
from telescope.plugins.updates import UpdatesPlugin
from telescope.updates import Asset, InstallResult, Manifest, UpdateError

NEWER = Manifest("0.6.0", 200, "nightly", "0.6.0-nightly.200", "https://notes", 2,
                 {"Telescope-linux.tar.gz": Asset("Telescope-linux.tar.gz", "https://x", "a" * 64, 10),
                  "Telescope-windows.zip": Asset("Telescope-windows.zip", "https://y", "b" * 64, 10)})


class _Host(QWidget):
    def __init__(self):
        super().__init__()
        self.streaming = False
        self.quits = 0
        self.saves = 0

    def is_streaming(self):
        return self.streaming

    def schedule_save(self):
        self.saves += 1

    def quit_app(self):
        self.quits += 1


@pytest.fixture
def env(qapp, monkeypatch):
    monkeypatch.setattr(version, "CHANNEL", "nightly")
    monkeypatch.setattr(version, "BUILD", 150)
    monkeypatch.setattr(updates, "self_update_blocker", lambda directory=None: None)
    fetched = {"result": NEWER, "channels": []}

    def fetch(channel):
        fetched["channels"].append(channel)
        if isinstance(fetched["result"], Exception):
            raise fetched["result"]
        return fetched["result"]
    monkeypatch.setattr(updates, "fetch_manifest", fetch)
    monkeypatch.setattr(UpdatesPlugin, "_spawn_check",
                        lambda self, cid, channel: self._on_checked(cid, *_fetch_now(fetch, channel)))
    host = _Host()
    bus = EventBus()
    plugin = UpdatesPlugin()
    plugin.setup(host, bus)
    plugin._timer.stop()
    button = plugin.create_header_widget()
    return plugin, host, bus, button, fetched


def _fetch_now(fetch, channel):
    try:
        return fetch(channel), ""
    except UpdateError as exc:
        return None, str(exc)


def test_a_newer_build_shows_the_update_button(env):
    plugin, _host, _bus, button, fetched = env
    assert button.isHidden()
    plugin._maybe_auto_check()
    assert fetched["channels"] == ["nightly"]
    assert not button.isHidden()
    assert "0.6.0 nightly 200" in button.toolTip()


def test_the_daily_check_waits_a_day_and_skips_source_checkouts(env, monkeypatch):
    plugin, _host, _bus, _button, fetched = env
    plugin._maybe_auto_check()
    plugin._maybe_auto_check()
    assert len(fetched["channels"]) == 1
    plugin._last_check = 0
    monkeypatch.setattr(version, "CHANNEL", "dev")
    plugin._maybe_auto_check()
    assert len(fetched["channels"]) == 1


def test_each_launch_checks_even_if_the_last_check_was_recent(env):
    plugin, _host, _bus, _button, fetched = env
    plugin.set_config({"last_check": time.time() - 60})  # saved by the previous run
    plugin._maybe_auto_check()
    assert fetched["channels"] == ["nightly"]
    plugin._maybe_auto_check()
    assert len(fetched["channels"]) == 1  # then daily


def test_background_failures_stay_quiet_but_a_manual_check_explains(env):
    plugin, _host, _bus, button, fetched = env
    fetched["result"] = UpdateError("Couldn't check for updates. Check the internet connection.")
    plugin.check(manual=False)
    assert plugin.status_text() == ("", "status_dim") and button.isHidden()
    plugin.check(manual=True)
    assert plugin.status_text()[1] == "status_err"


def test_a_launch_without_internet_checks_again_on_the_next_tick(env):
    plugin, _host, _bus, button, fetched = env
    fetched["result"] = UpdateError("Couldn't check for updates. Check the internet connection.")
    plugin._maybe_auto_check()
    assert plugin._last_check == 0  # a failed check doesn't count as today's
    fetched["result"] = NEWER
    plugin._maybe_auto_check()
    assert len(fetched["channels"]) == 2 and not button.isHidden()


def test_up_to_date_and_no_release_yet(env):
    plugin, _host, _bus, button, fetched = env
    fetched["result"] = Manifest("0.5.0", 150, "nightly", "x", "", 2, NEWER.assets)
    plugin.check(manual=True)
    assert plugin.status_text()[0] == "You have the latest version." and button.isHidden()
    fetched["result"] = None
    plugin.set_channel("stable")
    assert fetched["channels"][-1] == "stable"
    assert plugin.status_text()[0] == "There's no stable release yet."


def test_updating_downloads_installs_relaunches_and_quits(env, monkeypatch, tmp_path):
    plugin, host, _bus, _button, _fetched = env
    plugin.check()
    relaunched = []
    monkeypatch.setattr(UpdatesPlugin, "_relaunch", staticmethod(relaunched.append))
    monkeypatch.setattr(updates, "download", lambda asset, dest, progress, cancelled: tmp_path / asset.name)
    monkeypatch.setattr(updates, "install", lambda archive: InstallResult(["start.sh", "--after-update"]))
    monkeypatch.setattr(UpdatesPlugin, "_spawn_install", lambda self, asset, via_setup=False: self._on_installed(
        updates.install(updates.download(asset, tmp_path, None, None)), ""))
    plugin.update_now()
    assert relaunched == [["start.sh", "--after-update"]]
    assert host.quits == 1


def test_an_update_that_cant_restart_stays_open_and_says_so(env, monkeypatch):
    plugin, host, _bus, _button, _fetched = env
    plugin.check()

    def cant_start(_argv):
        raise PermissionError("blocked")

    monkeypatch.setattr(UpdatesPlugin, "_relaunch", staticmethod(cant_start))
    monkeypatch.setattr(UpdatesPlugin, "_spawn_install",
                        lambda self, asset, via_setup=False: self._on_installed(InstallResult(["start.sh"]), ""))
    plugin.update_now()
    assert host.quits == 0 and not plugin.busy
    assert "reopen Telescope" in plugin.status_text()[0]


def test_an_update_that_lands_during_a_stream_restarts_only_once_it_stops(env, monkeypatch, qapp):
    plugin, host, bus, _button, _fetched = env
    plugin.check()
    relaunched = []
    monkeypatch.setattr(UpdatesPlugin, "_relaunch", staticmethod(relaunched.append))
    monkeypatch.setattr(UpdatesPlugin, "_spawn_install", lambda self, asset, via_setup=False: None)  # still downloading
    plugin.update_now()
    host.streaming = True  # an app opened the camera while it downloaded
    plugin._on_installed(InstallResult(["start.sh"]), "")
    assert relaunched == [] and host.quits == 0
    assert "when you stop streaming" in plugin.status_text()[0]

    host.streaming = False
    bus.stream_stopped.emit()
    bus.stream_stopped.emit()  # quitting stops again
    qapp.processEvents()
    assert relaunched == [["start.sh"]] and host.quits == 1


def test_no_update_while_streaming(env, monkeypatch):
    plugin, host, bus, _button, _fetched = env
    plugin.check()
    host.streaming = True
    started = []
    monkeypatch.setattr(UpdatesPlugin, "_spawn_install", lambda self, asset, via_setup=False: started.append(asset))
    plugin.open_dialog()
    assert not plugin._dlg._update_btn.isEnabled()
    assert "Stop streaming to update" in plugin._dlg._status.text()
    plugin.update_now()
    assert started == []
    host.streaming = False
    bus.stream_stopped.emit()
    assert plugin._dlg._update_btn.isEnabled()
    plugin._dlg.close()


def test_a_failed_update_says_why_and_keeps_running(env, monkeypatch):
    plugin, host, _bus, _button, _fetched = env
    plugin.check()
    monkeypatch.setattr(UpdatesPlugin, "_spawn_install",
                        lambda self, asset, via_setup=False: self._on_installed(None, "The download didn't match its checksum."))
    plugin.update_now()
    assert plugin.status_text() == ("The download didn't match its checksum.", "status_err")
    assert host.quits == 0 and not plugin.busy


def test_a_copy_that_cant_replace_itself_links_to_the_release(env, monkeypatch):
    plugin, _host, _bus, _button, _fetched = env
    plugin.check()
    monkeypatch.setattr(updates, "self_update_blocker", lambda directory=None: "This is a source checkout. Update it with git.")
    opened = []
    monkeypatch.setattr(plugin_module.QDesktopServices, "openUrl", lambda url: opened.append(url.toString()))
    plugin.open_dialog()
    assert plugin._dlg._update_btn.text() == "Open download page"
    plugin.update_now()
    assert opened == ["https://notes"]
    plugin._dlg.close()


def test_config_round_trips_and_defaults_to_the_build_s_channel(env):
    plugin, _host, _bus, _button, _fetched = env
    plugin.set_config({"channel": "stable", "auto_check": False, "last_check": 5.0})
    assert plugin.get_config() == {"channel": "stable", "auto_check": False, "last_check": 5.0,
                                   "keep_portable": False}
    plugin.set_config({"channel": "bogus", "last_check": "x", "keep_portable": "yes"})
    assert plugin.get_config() == {"channel": "nightly", "auto_check": True, "last_check": 0.0,
                                   "keep_portable": False}
    plugin.set_config({"keep_portable": True})
    assert plugin.get_config()["keep_portable"] is True


def test_a_newer_phone_app_opens_the_dialog(env):
    plugin, _host, bus, _button, _fetched = env
    bus.update_requested.emit()
    assert plugin._dlg is not None and plugin._dlg.isVisible()
    plugin._dlg.close()


def test_the_relaunched_app_unpacks_its_own_copy():
    launched = []
    UpdatesPlugin._relaunch(["Telescope.exe", "--after-update"], popen=lambda argv, **kw: launched.append((argv, kw)))
    (argv, kwargs), = launched
    assert argv == ["Telescope.exe", "--after-update"]
    assert kwargs["env"]["PYINSTALLER_RESET_ENVIRONMENT"] == "1"


def test_the_dialog_says_a_build_was_rolled_back_and_lets_the_user_try_again(env, monkeypatch, tmp_path):
    plugin, _host, _bus, button, _fetched = env
    (tmp_path / ".update-failed").write_text('{"build": 200}')
    monkeypatch.setattr(updates, "install_dir", lambda: tmp_path)

    plugin.check(manual=True)
    text, kind = plugin.status_text()
    assert "0.6.0 nightly 200 didn't start after updating, so it's held back" in text and kind == "status_warn"
    assert plugin.available is None and button.isHidden()

    dlg = plugin_module.UpdatesDialog(plugin)
    dlg.show()
    dlg.refresh()
    assert not dlg._retry.isHidden()

    plugin.try_again()
    assert plugin.rolled_back is None and plugin.available is NEWER
    assert not (tmp_path / ".update-failed").exists()
    assert not button.isHidden()
    dlg.close()



# ── An unzipped Windows copy moves to the installed one ──────────────────────

SETUP = Asset("TelescopeSetup.exe", "https://z", "c" * 64, 10)
WITH_SETUP = Manifest(NEWER.version, NEWER.build, NEWER.channel, NEWER.version_name, NEWER.notes, NEWER.protocol,
                      {**NEWER.assets, "TelescopeSetup.exe": SETUP})


@pytest.fixture
def unzipped(env, monkeypatch):
    """An unzipped Windows copy whose camera driver lives in Program Files, with a release that has the setup."""
    plugin, host, bus, button, fetched = env
    fetched["result"] = WITH_SETUP
    monkeypatch.setattr(updates, "is_unzipped_copy", lambda directory=None: True)
    monkeypatch.setattr(updates, "IS_WINDOWS", True)  # so the in-place route picks the Windows zip
    from telescope.platform import windows
    monkeypatch.setattr(windows, "uc_in_app_folder", lambda: False)
    return env


def test_an_unzipped_copy_updates_through_the_installer_and_says_so(unzipped, monkeypatch, tmp_path):
    plugin, host, _bus, _button, _fetched = unzipped
    plugin.check()
    assert plugin.moves_on_update()
    assert "installs it for your user" in plugin.status_text()[0]
    spawned = []
    monkeypatch.setattr(UpdatesPlugin, "_spawn_install",
                        lambda self, asset, via_setup=False: spawned.append((asset.name, via_setup)))
    plugin.update_now()
    assert spawned == [("TelescopeSetup.exe", True)]


def test_the_installer_route_relaunches_the_installed_copy(unzipped, monkeypatch, tmp_path):
    plugin, host, _bus, _button, _fetched = unzipped
    plugin.check()
    relaunched, builds = [], []
    monkeypatch.setattr(UpdatesPlugin, "_relaunch", staticmethod(relaunched.append))
    argv = ["C:/Users/l/AppData/Local/Programs/Telescope/TelescopeDesktop.exe", "--after-update",
            "--moved-from", "C:/Users/l/Downloads/Telescope"]
    monkeypatch.setattr(updates, "install_with_setup", lambda setup: InstallResult(argv))
    monkeypatch.setattr(updates, "mark_started_elsewhere", lambda build: builds.append(build))
    monkeypatch.setattr(updates, "install", lambda *a, **k: pytest.fail("the zip route ran"))
    monkeypatch.setattr(updates, "download", lambda asset, dest, progress=None, cancelled=None: tmp_path / asset.name)
    monkeypatch.setattr(plugin_module, "stop_adb_server", lambda: None)
    # The real worker, run inline
    monkeypatch.setattr(plugin_module.threading, "Thread",
                        lambda target, daemon: type("T", (), {"start": staticmethod(target)})())
    plugin.update_now()
    assert relaunched == [argv]
    assert builds == [200]  # marked in this folder once the installed copy is started, should it not start
    assert host.quits == 1


def test_keeping_it_portable_updates_in_place(unzipped, monkeypatch):
    plugin, _host, _bus, _button, _fetched = unzipped
    plugin.check()
    plugin.open_dialog()
    assert not plugin._dlg._portable_row.isHidden()
    plugin._dlg._portable.setChecked(True)
    assert plugin.keep_portable and not plugin.moves_on_update()
    assert "installs it for your user" not in plugin.status_text()[0]
    spawned = []
    monkeypatch.setattr(UpdatesPlugin, "_spawn_install",
                        lambda self, asset, via_setup=False: spawned.append((asset.name, via_setup)))
    plugin.update_now()
    assert spawned == [("Telescope-windows.zip", False)]
    plugin._dlg.close()


def test_no_move_without_a_setup_or_with_the_driver_in_this_folder(unzipped, monkeypatch):
    plugin, _host, _bus, _button, fetched = unzipped
    fetched["result"] = NEWER  # a release from before the installer
    plugin.check()
    assert not plugin.moves_on_update()
    fetched["result"] = WITH_SETUP
    plugin.check()
    from telescope.platform import windows
    monkeypatch.setattr(windows, "uc_in_app_folder", lambda: True)  # deleting this folder would take the camera
    assert not plugin.moves_on_update()


def test_an_installed_copy_never_moves(env, monkeypatch):
    plugin, _host, _bus, _button, fetched = env
    fetched["result"] = WITH_SETUP
    monkeypatch.setattr(updates, "is_unzipped_copy", lambda directory=None: False)
    plugin.check()
    assert not plugin.moves_on_update()
    plugin.open_dialog()
    assert plugin._dlg._portable_row.isHidden()
    plugin._dlg.close()


def test_a_move_needs_no_writable_folder(unzipped, monkeypatch):
    plugin, _host, _bus, _button, _fetched = unzipped
    monkeypatch.setattr(updates, "self_update_blocker", lambda directory=None: "Telescope's folder isn't writable.")
    plugin.check()
    assert plugin.update_blocker() is None
