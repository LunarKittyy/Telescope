from types import SimpleNamespace

import pytest

from telescope import pyav_vaapi
from telescope.widgets import vaapi_setup
from telescope.widgets.vaapi_setup import VaapiSetupDialog


class _SyncThread:
    def __init__(self, target, **_kwargs):
        self._run = target

    def start(self):
        self._run()


@pytest.fixture
def setup(qapp, tmp_path, monkeypatch):
    """The dialog with a temp data folder, no distro PyAV and every build tool there."""
    # Deliver the check's signal while this test's mocks are still installed.
    # A real worker queues it until a later test processes Qt events.
    monkeypatch.setattr(vaapi_setup, "threading", SimpleNamespace(Thread=_SyncThread))
    monkeypatch.setenv("XDG_DATA_HOME", str(tmp_path))
    monkeypatch.setattr(pyav_vaapi, "standard_version", lambda: "18.1.0")
    monkeypatch.setattr(pyav_vaapi, "active", None)
    monkeypatch.setattr(pyav_vaapi, "problem", "")
    monkeypatch.setattr(pyav_vaapi, "find_system", lambda: None)
    monkeypatch.setattr(pyav_vaapi, "missing_tools", lambda: [])
    monkeypatch.setattr(pyav_vaapi, "probe", lambda folder: (True, "18.1.0", ""))
    restarts = []
    made = []

    def make():
        dlg = VaapiSetupDialog(None, lambda: restarts.append(True) or True)
        made.append(dlg)
        return dlg
    yield make, restarts, tmp_path / "Telescope" / "pyav-vaapi"
    for dlg in made:
        dlg.close()


def test_with_the_tools_there_it_offers_a_build(setup):
    make, _restarts, _root = setup
    dlg = make()
    assert dlg._system_line.text() == "None installed" and dlg._system_btn.isHidden()
    assert "Builds PyAV 18.1.0" in dlg._build_line.text() and dlg._build_btn.text() == "Build"
    assert dlg._tools.isHidden() and dlg._restart_note.isHidden() and dlg._using_strip.isHidden()


def test_missing_tools_come_with_the_command_that_installs_them(setup, monkeypatch):
    make, _restarts, _root = setup
    monkeypatch.setattr(pyav_vaapi, "missing_tools", lambda: ["a C compiler", "FFmpeg's development files"])
    monkeypatch.setattr(pyav_vaapi, "install_command", lambda: "sudo dnf install gcc ffmpeg-free-devel")
    dlg = make()
    assert dlg._build_line.text() == "Needs a C compiler and FFmpeg's development files. To install them:"
    assert not dlg._tools.isHidden() and dlg._command_text == "sudo dnf install gcc ffmpeg-free-devel"
    assert dlg._command.text() == "sudo dnf install gcc ffmpeg\u2011free\u2011devel"  # shown unbreakable, copied plain
    assert dlg._build_btn.isHidden()


def test_an_old_distro_pyav_says_what_it_needs(setup):
    make, _restarts, _root = setup
    dlg = make()
    dlg._on_system(("/x", False, "11.0.0", "old"))
    assert dlg._system_line.text() == "PyAV 11.0.0. It needs PyAV 14 or newer" and dlg._system_btn.isHidden()


def test_picking_the_distros_pyav_asks_for_a_restart(setup):
    make, restarts, root = setup
    dlg = make()
    dlg._on_system((root / "system", True, "19.0.1", ""))
    assert dlg._system_line.text() == "PyAV 19.0.1, with VA-API" and not dlg._system_btn.isHidden()
    dlg._system_btn.click()
    assert pyav_vaapi.chosen() == {"kind": "system", "path": str(root / "system")}
    assert dlg._using_line.text() == "From the next start" and not dlg._restart_btn.isHidden()
    dlg._restart_btn.click()
    assert restarts == [True]


def test_a_finished_build_is_picked_and_one_already_built_isnt_rebuilt(setup, monkeypatch):
    make, _restarts, root = setup
    dlg = make()
    dlg._building = True
    (root / "18.1.0" / "av").mkdir(parents=True)
    dlg._on_built(True, str(root / "18.1.0"))
    assert pyav_vaapi.chosen()["kind"] == "build" and dlg._build_btn.isHidden()
    pyav_vaapi.forget()
    monkeypatch.setattr(pyav_vaapi, "build", lambda *a: pytest.fail("rebuilt"))
    dlg = make()
    assert dlg._build_btn.text() == "Use it" and dlg._build_line.text() == "PyAV 18.1.0 is built"
    dlg._build_btn.click()
    assert pyav_vaapi.chosen()["path"] == str(root / "18.1.0")


def test_a_failed_build_says_why(setup):
    make, _restarts, _root = setup
    dlg = make()
    dlg._building = True
    dlg._on_built(False, "error: libavcodec/avcodec.h: No such file")
    assert dlg._build_line.text() == "The build failed: error: libavcodec/avcodec.h: No such file"
    assert dlg._build_btn.text() == "Try again" and pyav_vaapi.chosen() is None


def test_in_use_it_says_so_and_can_go_back(setup, monkeypatch):
    make, _restarts, root = setup
    pyav_vaapi.choose(pyav_vaapi.BUILD, root / "18.1.0")
    monkeypatch.setattr(pyav_vaapi, "active", {"kind": "build", "version": "18.1.0", "path": str(root / "18.1.0")})
    dlg = make()
    assert dlg._using_line.text() == "Telescope is using the built PyAV 18.1.0" and dlg._restart_note.isHidden()
    dlg._standard_btn.click()
    assert pyav_vaapi.chosen() is None and not dlg._restart_note.isHidden()


def test_a_rejected_existing_build_can_be_rebuilt_without_forgetting_it_first(setup, monkeypatch):
    make, _restarts, root = setup
    folder = root / "18.1.0"
    (folder / "av").mkdir(parents=True)
    pyav_vaapi.choose(pyav_vaapi.BUILD, folder)
    monkeypatch.setattr(pyav_vaapi, "problem", "it didn't load")
    monkeypatch.setattr(pyav_vaapi, "probe", lambda folder: (False, "", "load"))
    built = []
    monkeypatch.setattr(pyav_vaapi, "build", lambda v, on_line: built.append(v) or (True, str(folder)))
    dlg = make()
    assert dlg._build_btn.text() == "Rebuild" and not dlg._build_btn.isHidden()
    assert "no longer works" in dlg._build_line.text()
    dlg._build_btn.click()
    assert built == ["18.1.0"] and dlg._build_btn.isHidden()
    assert pyav_vaapi.chosen()["path"] == str(folder)


def test_an_existing_build_is_not_offered_until_its_check_finishes(setup, monkeypatch):
    make, _restarts, root = setup
    (root / "18.1.0" / "av").mkdir(parents=True)
    jobs = []

    class Pending:
        def __init__(self, target, **_kwargs):
            jobs.append(target)

        def start(self):
            pass
    monkeypatch.setattr(vaapi_setup, "threading", SimpleNamespace(Thread=Pending))
    dlg = make()
    assert "Checking" in dlg._build_line.text() and dlg._build_btn.isHidden()
    jobs.pop()()
    assert dlg._build_btn.text() == "Use it" and not dlg._build_btn.isHidden()


def test_replacing_the_active_build_still_asks_for_a_restart(setup, monkeypatch):
    make, _restarts, root = setup
    folder = root / "18.1.0"
    (folder / "av").mkdir(parents=True)
    pyav_vaapi.choose(pyav_vaapi.BUILD, folder)
    monkeypatch.setattr(pyav_vaapi, "active", {"kind": "build", "version": "18.1.0", "path": str(folder)})
    dlg = make()
    assert dlg._restart_btn.isHidden()
    dlg._on_built(True, str(folder))
    assert not dlg._restart_btn.isHidden()


@pytest.mark.parametrize("version", ["12.3.0", "13.1.0", ""])
def test_an_unsupported_pyav_explains_the_upgrade_instead_of_offering_a_build(setup, monkeypatch, version):
    make, _restarts, _root = setup
    monkeypatch.setattr(pyav_vaapi, "standard_version", lambda: version)
    monkeypatch.setattr(pyav_vaapi, "build", lambda *a: pytest.fail("built an unsupported PyAV"))
    dlg = make()
    assert "14 or newer" in dlg._build_line.text() and dlg._build_btn.isHidden()
    dlg._start_build()
    assert not dlg._building


def test_reopening_setup_keeps_one_build_and_its_progress(setup, monkeypatch):
    from telescope.plugin import EventBus
    from telescope.plugins.setup import SetupPlugin

    plugin = SetupPlugin()
    plugin.setup(SimpleNamespace(restart_app=lambda: True), EventBus())
    monkeypatch.setattr(VaapiSetupDialog, "exec", lambda self: 0)
    plugin._open_vaapi()
    first = plugin._vaapi_dlg
    jobs = []

    class Pending:
        def __init__(self, target, **_kwargs):
            jobs.append(target)

        def start(self):
            pass
    monkeypatch.setattr(vaapi_setup, "threading", SimpleNamespace(Thread=Pending))
    first._start_build()
    first._on_line("Building PyAV...")
    first.close()
    plugin._open_vaapi()
    plugin._vaapi_dlg._start_build()  # even a second request cannot start another job
    assert plugin._vaapi_dlg is first and first._building
    assert first._build_line.text() == "Building PyAV..." and len(jobs) == 1
    first._on_built(False, "stopped for the test")
    first.deleteLater()


def test_a_build_that_raises_still_finishes_and_can_be_retried(setup, monkeypatch):
    make, _restarts, _root = setup

    def fail(*_args):
        raise OSError("not enough disk space")
    monkeypatch.setattr(pyav_vaapi, "build", fail)
    dlg = make()
    dlg._start_build()
    assert not dlg._building and not dlg._build_btn.isHidden()
    assert "not enough disk space" in dlg._build_line.text()


def test_rechecking_finds_packages_installed_while_setup_was_closed(setup, monkeypatch):
    make, _restarts, root = setup
    dlg = make()
    assert dlg._system_line.text() == "None installed"
    package = root / "distro" / "av"
    package.mkdir(parents=True)
    monkeypatch.setattr(pyav_vaapi, "find_system", lambda: package)
    dlg.check_again()
    assert dlg._system_line.text() == "PyAV 18.1.0, with VA-API"
    assert not dlg._system_btn.isHidden()
