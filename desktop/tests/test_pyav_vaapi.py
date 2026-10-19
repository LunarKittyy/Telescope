import sys
from pathlib import Path

import pytest

from telescope import pyav_vaapi


@pytest.fixture
def home(tmp_path, monkeypatch):
    """Telescope's data folder in a temp dir, on Linux, with the standard PyAV at 18.1.0."""
    monkeypatch.setenv("XDG_DATA_HOME", str(tmp_path))
    monkeypatch.setattr(pyav_vaapi, "supported", lambda: True)
    monkeypatch.setattr(pyav_vaapi, "standard_version", lambda: "18.1.0")
    monkeypatch.setattr(pyav_vaapi, "active", None)
    monkeypatch.setattr(pyav_vaapi, "problem", "")
    monkeypatch.setattr(sys, "path", list(sys.path))
    return tmp_path / "Telescope" / "pyav-vaapi"


def test_nothing_picked_means_the_standard_pyav_and_no_check(home, monkeypatch):
    monkeypatch.setattr(pyav_vaapi, "probe", lambda folder: pytest.fail("checked without a pick"))
    assert pyav_vaapi.activate() is None and pyav_vaapi.problem == ""


def test_a_pick_that_checks_out_goes_first_on_the_path(home, monkeypatch):
    folder = home / "18.1.0"
    pyav_vaapi.choose(pyav_vaapi.BUILD, folder)
    monkeypatch.setattr(pyav_vaapi, "probe", lambda f: (True, "18.1.0", ""))
    assert pyav_vaapi.activate() == {"kind": "build", "version": "18.1.0", "path": str(folder)}
    assert sys.path[0] == str(folder)


def test_a_build_for_another_pyav_version_is_left_out(home, monkeypatch):
    pyav_vaapi.choose(pyav_vaapi.BUILD, home / "17.0.0")
    monkeypatch.setattr(pyav_vaapi, "probe", lambda f: pytest.fail("a stale build isn't even checked"))
    assert pyav_vaapi.activate() is None
    assert "17.0.0" in pyav_vaapi.problem and "18.1.0" in pyav_vaapi.problem


@pytest.mark.parametrize("why,says", [("old", "older than 14.0"), ("novaapi", "no VA-API"),
                                      ("noopen", "didn't open"), ("load", "didn't load")])
def test_a_pick_that_fails_its_check_is_left_out_and_says_why(home, monkeypatch, why, says):
    pyav_vaapi.choose(pyav_vaapi.SYSTEM, home / "system")
    monkeypatch.setattr(pyav_vaapi, "probe", lambda f: (False, "11.0.0", why))
    before = list(sys.path)
    assert pyav_vaapi.activate() is None
    assert says in pyav_vaapi.problem and sys.path == before


def test_forgetting_goes_back_to_the_standard_one(home):
    pyav_vaapi.choose(pyav_vaapi.SYSTEM, home / "system")
    pyav_vaapi.forget()
    assert pyav_vaapi.chosen() is None


def test_a_garbled_choice_file_counts_as_none(home):
    home.mkdir(parents=True)
    (home / "choice.json").write_text("{nope")
    assert pyav_vaapi.chosen() is None


def test_the_distros_pyav_is_found_and_linked_on_its_own(home, tmp_path, monkeypatch):
    site = tmp_path / "usr" / "lib" / "python3" / "dist-packages"
    (site / "av").mkdir(parents=True)
    (site / "av" / "__init__.py").write_text("")
    (site / "numpy").mkdir()
    monkeypatch.setattr(pyav_vaapi, "_system_dirs", lambda: [tmp_path / "missing", site])
    package = pyav_vaapi.find_system()
    assert package == site / "av"
    folder = pyav_vaapi.link_system(package)
    assert (folder / "av").resolve() == (site / "av").resolve()
    assert not (folder / "numpy").exists()  # only av comes along
    assert pyav_vaapi.link_system(package) == folder  # linking again replaces the link


@pytest.mark.parametrize("ids,start", [({"nobara", "fedora", "rhel"}, "sudo dnf install"),
                                       ({"ubuntu", "debian"}, "sudo apt install"), ({"arch"}, "sudo pacman -S"),
                                       ({"gentoo"}, "")])
def test_the_install_command_fits_the_distro(monkeypatch, ids, start):
    monkeypatch.setattr(pyav_vaapi, "_distro", lambda: ids)
    command = pyav_vaapi.install_command()
    assert command.startswith(start) if start else command == ""


def test_missing_tools_are_named(monkeypatch):
    monkeypatch.setattr(pyav_vaapi.shutil, "which", lambda name: None)
    monkeypatch.setattr(pyav_vaapi.sysconfig, "get_paths", lambda: {"include": "/nowhere"})
    assert pyav_vaapi.missing_tools() == ["a C compiler", "pkg-config", "Python's development files"]


@pytest.mark.parametrize("version", ["12.3.0", "13.1.0", ""])
def test_unsupported_build_versions_are_rejected_before_running_pip(home, monkeypatch, version):
    monkeypatch.setattr(pyav_vaapi.subprocess, "Popen", lambda *a, **kw: pytest.fail("ran pip"))
    ok, message = pyav_vaapi.build(version, lambda line: None)
    assert not ok and "14 or newer" in message
    assert not home.exists()


def test_version_compare_reads_odd_versions():
    assert pyav_vaapi._version_tuple("14.2.0") >= pyav_vaapi.MIN_VERSION
    assert pyav_vaapi._version_tuple("11.0.0") < pyav_vaapi.MIN_VERSION
    assert pyav_vaapi._version_tuple("15.0.0rc1") == (15, 0)


@pytest.mark.skipif(not sys.platform.startswith("linux"), reason="the check runs a Linux PyAV")
def test_the_check_says_when_a_folder_has_no_pyav(tmp_path):
    ok, _version, why = pyav_vaapi.probe(tmp_path)
    assert not ok and why == "load"  # the venv's own PyAV loads instead, which doesn't count


def test_a_build_pins_cython_so_a_newer_one_cant_break_it(home, monkeypatch):
    seen = {}

    class Done:
        stdout, returncode = iter(["ERROR: stop here"]), 1

        def __init__(self, cmd, env=None, **_kwargs):
            seen["env"] = env

        def wait(self):
            pass

    monkeypatch.setattr(pyav_vaapi.subprocess, "Popen", Done)
    assert pyav_vaapi.build("18.1.0", lambda line: None)[0] is False
    pins = Path(seen["env"]["PIP_CONSTRAINT"])
    assert pins.read_text().strip() == f"cython=={pyav_vaapi.BUILD_CYTHON}"
