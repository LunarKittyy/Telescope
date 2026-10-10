"""An unzipped Windows copy updating through the installer: running the setup, finding the installed copy, and the
installed copy deleting the unzipped one, never anything else."""

import subprocess
import sys

import pytest

import update_guard
from telescope import updates
from telescope.updates import UpdateError


class _Run:
    def __init__(self, code=0, exc=None):
        self.code, self.exc, self.calls = code, exc, []

    def __call__(self, argv, timeout, **kwargs):
        self.calls.append(argv)
        if self.exc:
            raise self.exc
        return subprocess.CompletedProcess(argv, self.code)


def _installed(tmp_path):
    folder = tmp_path / "Programs" / "Telescope"
    folder.mkdir(parents=True)
    (folder / "TelescopeDesktop.exe").write_bytes(b"exe")
    (folder / "unins000.exe").write_bytes(b"un")
    return folder


def test_the_setup_runs_silently_and_the_installed_copy_takes_over(tmp_path):
    installed, old = _installed(tmp_path), tmp_path / "Downloads" / "Telescope"
    old.mkdir(parents=True)
    run = _Run()
    result = updates.install_with_setup(tmp_path / "TelescopeSetup.exe", old, run=run, locate=lambda: installed)
    assert run.calls == [[str(tmp_path / "TelescopeSetup.exe"), "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART",
                          "/SP-"]]
    assert result.relaunch == [str(installed / "TelescopeDesktop.exe"), "--after-update", "--moved-from", str(old)]


@pytest.mark.parametrize("run, locate, says", [
    (_Run(code=5), None, r"stopped \(code 5\)"),
    (_Run(exc=subprocess.TimeoutExpired("setup", 600)), None, "took too long"),
    (_Run(exc=PermissionError(13, "Access is denied")), None, "Couldn't start the installer"),
    (_Run(), lambda: None, "wasn't where expected"),
])
def test_a_failed_install_leaves_this_copy_as_it_is(tmp_path, run, locate, says):
    old = tmp_path / "old"
    old.mkdir()
    (old / "TelescopeDesktop.exe").write_bytes(b"exe")
    with pytest.raises(UpdateError, match=says):
        updates.install_with_setup(tmp_path / "s.exe", old, run=run, locate=locate or (lambda: _installed(tmp_path)))
    assert (old / "TelescopeDesktop.exe").exists()


def test_installed_over_this_same_folder_deletes_nothing(tmp_path):
    installed = _installed(tmp_path)
    result = updates.install_with_setup(tmp_path / "s.exe", installed, run=_Run(), locate=lambda: installed)
    assert "--moved-from" not in result.relaunch


def test_the_installed_location_comes_from_the_setup_s_uninstall_entry():
    class Reg:
        HKEY_CURRENT_USER = "HKCU"
        opened = []

        class _Key:
            def __enter__(self): return self
            def __exit__(self, *a): return False

        def OpenKey(self, root, path):
            self.opened.append((root, path))
            return self._Key()

        def QueryValueEx(self, key, name):
            assert name == "InstallLocation"
            return r"C:\Users\l\AppData\Local\Programs\Telescope" + "\\", 1
    reg = Reg()
    assert updates.installed_location(reg) is not None
    assert reg.opened == [("HKCU", r"Software\Microsoft\Windows\CurrentVersion\Uninstall"
                                   r"\{BF097F77-F8B9-4F70-B23C-0B500DB1A07C}_is1")]

    class Missing(Reg):
        def OpenKey(self, root, path):
            raise FileNotFoundError(path)
    assert updates.installed_location(Missing()) is None


def test_the_app_id_matches_the_installer_script():
    from pathlib import Path
    iss = (Path(__file__).resolve().parents[2] / "installer" / "Telescope.iss").read_text()
    assert "AppId={" + updates.SETUP_APP_ID + "\n" in iss  # Inno writes AppId={{GUID}, the first brace escaped


def _unzipped(folder, extras=()):
    folder.mkdir(parents=True)
    for name in ("TelescopeDesktop.exe", "THIRD_PARTY_NOTICES.txt", "Telescope.apk", "TelescopeDesktop.old.exe",
                 ".update.json"):
        (folder / name).write_bytes(b"x")
    for sub in ("lib-123/PyQt6", "lib-99", "platform-tools", "unitycapture", ".update-staging"):
        (folder / sub).mkdir(parents=True)
        (folder / sub / "f.dll").write_bytes(b"x")
    for name in extras:
        (folder / name).write_bytes(b"theirs")
    return folder


def test_the_unzipped_copy_goes_once_the_installed_one_runs(tmp_path):
    old = _unzipped(tmp_path / "Telescope")
    assert updates.remove_unzipped_copy(old, _installed(tmp_path), pause=0)
    assert not old.exists()


def test_a_zip_unpacked_into_downloads_only_loses_telescope_s_files(tmp_path):
    downloads = _unzipped(tmp_path / "Downloads", extras=("holiday.jpg", "Telescope-windows.zip", "lib.txt"))
    (downloads / "Games").mkdir()
    assert updates.remove_unzipped_copy(downloads, _installed(tmp_path), pause=0)
    assert sorted(p.name for p in downloads.iterdir()) == ["Games", "Telescope-windows.zip", "holiday.jpg", "lib.txt"]


def _another_installed_copy(tmp, _current):
    folder = _unzipped(tmp / "Other")
    (folder / "unins000.exe").write_bytes(b"u")
    return folder


def _not_telescope(tmp, _current):
    folder = tmp / "NotTelescope"
    folder.mkdir()
    (folder / "notes.txt").write_bytes(b"x")
    return folder


@pytest.mark.parametrize("make", [
    lambda tmp, current: current,  # the copy that's running
    _another_installed_copy,       # its own uninstaller removes it
    _not_telescope,                # no TelescopeDesktop.exe in it
])
def test_it_refuses_anything_that_isnt_an_unzipped_telescope(tmp_path, make):
    current = _installed(tmp_path)
    target = make(tmp_path, current)
    before = sorted(str(p) for p in target.rglob("*"))
    assert not updates.remove_unzipped_copy(target, current, pause=0)
    assert sorted(str(p) for p in target.rglob("*")) == before


def test_files_still_held_by_the_exiting_copy_are_tried_again(tmp_path, monkeypatch):
    old = _unzipped(tmp_path / "Telescope")
    real, held = update_guard.remove, {"TelescopeDesktop.exe": 2}

    def remove(path):
        if held.get(path.name, 0) > 0:
            held[path.name] -= 1
            raise PermissionError(32, "in use")
        real(path)
    monkeypatch.setattr(update_guard, "remove", remove)
    assert updates.remove_unzipped_copy(old, _installed(tmp_path), pause=0)
    assert not old.exists()

    old = _unzipped(tmp_path / "Telescope2")
    held["TelescopeDesktop.exe"] = 99  # never let go
    assert not updates.remove_unzipped_copy(old, _installed(tmp_path / "x"), tries=3, pause=0)
    assert (old / "TelescopeDesktop.exe").exists()


def test_only_the_packaged_windows_app_counts_as_unzipped(tmp_path, monkeypatch):
    monkeypatch.setattr(updates, "IS_WINDOWS", True)
    monkeypatch.setattr(sys, "frozen", True, raising=False)
    assert updates.is_unzipped_copy(tmp_path)
    (tmp_path / "unins000.exe").write_bytes(b"u")
    assert not updates.is_unzipped_copy(tmp_path)
    monkeypatch.setattr(sys, "frozen", False)
    assert not updates.is_unzipped_copy(tmp_path / "nope")
    monkeypatch.setattr(updates, "IS_WINDOWS", False)
    monkeypatch.setattr(sys, "frozen", True)
    assert not updates.is_unzipped_copy(tmp_path / "nope")


def test_main_takes_the_moved_from_folder():
    import main
    args, rest = main.parse_args(["--after-update", "--moved-from", r"C:\Users\l\Downloads\Telescope", "-style", "x"])
    assert args.after_update and args.moved_from == r"C:\Users\l\Downloads\Telescope"
    assert rest == ["-style", "x"]
