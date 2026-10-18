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
    dlg._on_built(True, str(root / "18.1.0"))
    assert pyav_vaapi.chosen()["kind"] == "build" and dlg._build_btn.isHidden()
    pyav_vaapi.forget()
    (root / "18.1.0" / "av").mkdir(parents=True)
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
