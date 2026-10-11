"""An unzipped Windows copy updating through the installer: running the setup, finding the installed copy, and the
installed copy deleting the unzipped one, never anything else."""

import subprocess
import time as time_module
import sys

import pytest

import update_guard
from telescope import updates
from telescope.updates import UpdateError


@pytest.fixture(autouse=True)
def _local_app_data(tmp_path, monkeypatch):
    """Where a kept adb would go (platform/adb_download.py), away from the real profile."""
    monkeypatch.setenv("LOCALAPPDATA", str(tmp_path / "LocalAppData"))
    return tmp_path / "LocalAppData"


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


def test_the_move_clears_the_installed_folder_s_old_journal_and_notes_the_old_copy(tmp_path):
    installed, old = _installed(tmp_path), tmp_path / "Downloads" / "Telescope"
    old.mkdir(parents=True)
    update_guard.write_journal(installed, {"platform": "windows", "to_build": 150, "state": "trial", "started": True})
    updates.install_with_setup(tmp_path / "s.exe", old, run=_Run(), locate=lambda: installed)
    assert update_guard.read_journal(installed) is None  # else its first start rolls the fresh install back
    assert updates.pending_move(installed) == old  # in case its first start doesn't get to tidy up
    assert update_guard.failed_build(old) is None  # nothing has failed to start yet


def test_once_the_installed_copy_is_started_the_old_one_holds_that_build_back(tmp_path):
    old = tmp_path / "Telescope"
    old.mkdir()
    updates.mark_started_elsewhere(200, old)
    assert update_guard.failed_build(old) == 200  # it's only seen if the installed copy didn't start


def test_a_pending_move_is_finished_at_a_later_start_and_then_forgotten(tmp_path):
    installed, old = _installed(tmp_path), _unzipped(tmp_path / "Downloads" / "Telescope")
    (installed / updates.PENDING_MOVE).write_text(str(old), encoding="utf-8")
    real = update_guard.remove
    held = {"on": True}

    def remove(path):
        if held["on"] and path.name == "Telescope.apk":
            raise PermissionError(32, "in use")
        real(path)
    import pytest as _p
    mp = _p.MonkeyPatch()
    mp.setattr(update_guard, "remove", remove)
    mp.setattr(time_module, "sleep", lambda s: None)
    try:
        assert not updates.finish_move(old, installed)
        assert updates.pending_move(installed) == old  # still there: the next start tries again
        held["on"] = False
        assert updates.finish_move(updates.pending_move(installed), installed)
    finally:
        mp.undo()
    assert not old.exists() and updates.pending_move(installed) is None


@pytest.mark.parametrize("contents", [b"C:\\Users\\\xc3", b"\xff\xfe", b"", b"relative\\path", b"C:\\a\x00b"])
def test_a_damaged_pending_move_note_is_ignored_and_removed(tmp_path, contents):
    installed = _installed(tmp_path)
    (installed / updates.PENDING_MOVE).write_bytes(contents)  # a cut-off write, say
    assert updates.pending_move(installed) is None
    assert not (installed / updates.PENDING_MOVE).exists()  # not tried again at every start


def test_the_pending_move_note_is_written_whole(tmp_path):
    installed, old = _installed(tmp_path), tmp_path / "Downloads" / "Telescope"
    old.mkdir(parents=True)
    updates.install_with_setup(tmp_path / "s.exe", old, run=_Run(), locate=lambda: installed)
    assert sorted(p.name for p in installed.glob(".moved-from*")) == [".moved-from"]


def test_a_pending_move_to_a_folder_that_s_gone_is_forgotten(tmp_path):
    installed = _installed(tmp_path)
    (installed / updates.PENDING_MOVE).write_text(str(tmp_path / "gone"), encoding="utf-8")
    assert not updates.finish_move(tmp_path / "gone", installed)
    assert updates.pending_move(installed) is None


def test_an_old_copy_s_bundled_adb_is_kept_for_the_installed_one(tmp_path, _local_app_data):
    old = _unzipped(tmp_path / "Telescope")
    assert updates.remove_unzipped_copy(old, _installed(tmp_path), pause=0)
    kept = _local_app_data / "Telescope" / "platform-tools"
    assert sorted(p.name for p in kept.iterdir()) == ["AdbWinApi.dll", "NOTICE", "adb.exe"]
    assert not old.exists()


def test_a_downloaded_adb_isn_t_replaced_by_an_old_copy_s(tmp_path, _local_app_data):
    kept = _local_app_data / "Telescope" / "platform-tools"
    kept.mkdir(parents=True)
    (kept / "adb.exe").write_bytes(b"newer")
    assert updates.remove_unzipped_copy(_unzipped(tmp_path / "Telescope"), _installed(tmp_path), pause=0)
    assert (kept / "adb.exe").read_bytes() == b"newer"


def test_a_user_s_own_platform_tools_isn_t_taken_as_telescope_s_adb(tmp_path, _local_app_data):
    downloads = _unzipped(tmp_path / "Downloads")
    (downloads / "platform-tools" / "fastboot.exe").write_bytes(b"theirs")
    updates.remove_unzipped_copy(downloads, _installed(tmp_path), pause=0)
    assert not (_local_app_data / "Telescope").exists()


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
    for name in ("lib-123/PyQt6/Qt6Core.dll", "lib-99/PyQt6/Qt6Core.dll", "lib-99/python313.dll",
                 "platform-tools/adb.exe", "platform-tools/AdbWinApi.dll", "platform-tools/NOTICE",
                 "unitycapture/UnityCaptureFilter64.dll", "unitycapture/LICENSE", ".update-staging/x.dll"):
        (folder / name).parent.mkdir(parents=True, exist_ok=True)
        (folder / name).write_bytes(b"x")
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


def test_the_user_s_own_folders_with_telescope_like_names_stay(tmp_path):
    downloads = _unzipped(tmp_path / "Downloads")
    for name in ("platform-tools/fastboot.exe", "lib-photos/cat.jpg", "lib-7/notes.txt", "unitycapture/mine.txt"):
        (downloads / name).parent.mkdir(parents=True, exist_ok=True)
        (downloads / name).write_bytes(b"theirs")
    assert updates.remove_unzipped_copy(downloads, _installed(tmp_path), pause=0)
    assert sorted(str(p.relative_to(downloads)).replace("\\", "/") for p in downloads.rglob("*")) == [
        "lib-7", "lib-7/notes.txt", "lib-photos", "lib-photos/cat.jpg",
        "platform-tools", "platform-tools/AdbWinApi.dll", "platform-tools/NOTICE", "platform-tools/adb.exe",
        "platform-tools/fastboot.exe",
        "unitycapture", "unitycapture/LICENSE", "unitycapture/UnityCaptureFilter64.dll", "unitycapture/mine.txt"]


def test_the_exe_goes_last_so_a_cut_short_clean_up_still_runs(tmp_path, monkeypatch):
    old = _unzipped(tmp_path / "Telescope")
    real, order = update_guard.remove, []

    def remove(path):
        order.append(path.name)
        if path.name == "Telescope.apk":
            raise PermissionError(32, "in use")
        real(path)
    monkeypatch.setattr(update_guard, "remove", remove)
    assert not updates.remove_unzipped_copy(old, _installed(tmp_path), tries=2, pause=0)
    assert "TelescopeDesktop.exe" not in order
    assert (old / "TelescopeDesktop.exe").exists()
    monkeypatch.setattr(update_guard, "remove", lambda path: (order.append(path.name), real(path)))
    order.clear()
    assert updates.remove_unzipped_copy(old, _installed(tmp_path / "x"), pause=0)
    assert order[-1] == "TelescopeDesktop.exe"


def test_a_folder_it_can_t_look_into_keeps_the_exe(tmp_path, monkeypatch):
    old = _unzipped(tmp_path / "Telescope")
    real = updates._ours

    def ours(entry):
        if entry.name == "platform-tools":
            raise PermissionError(5, "Access is denied")
        return real(entry)
    monkeypatch.setattr(updates, "_ours", ours)
    assert not updates.remove_unzipped_copy(old, _installed(tmp_path), tries=2, pause=0)
    assert (old / "TelescopeDesktop.exe").exists()


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
    monkeypatch.setenv("LOCALAPPDATA", str(tmp_path / "AppData" / "Local"))
    setups_folder = tmp_path / "AppData" / "Local" / "Programs" / "Telescope"
    setups_folder.mkdir(parents=True)
    assert not updates.is_unzipped_copy(setups_folder)  # the setup would close this copy halfway through
    monkeypatch.setattr(sys, "frozen", False)
    assert not updates.is_unzipped_copy(tmp_path / "nope")
    monkeypatch.setattr(updates, "IS_WINDOWS", False)
    monkeypatch.setattr(sys, "frozen", True)
    assert not updates.is_unzipped_copy(tmp_path / "nope")


def test_a_copy_on_a_usb_stick_or_a_share_stays_where_it_is(tmp_path, monkeypatch):
    import types
    monkeypatch.setattr(updates, "IS_WINDOWS", True)
    monkeypatch.setattr(sys, "frozen", True, raising=False)
    drive = {"type": 3}  # fixed
    kernel32 = types.SimpleNamespace(GetDriveTypeW=lambda root: drive["type"])
    import ctypes
    monkeypatch.setattr(ctypes, "windll", types.SimpleNamespace(kernel32=kernel32), raising=False)
    assert updates.is_unzipped_copy(tmp_path)
    for kind in (2, 4):  # removable, network
        drive["type"] = kind
        assert not updates.is_unzipped_copy(tmp_path)


def test_main_takes_the_moved_from_folder():
    import main
    args, rest = main.parse_args(["--after-update", "--moved-from", r"C:\Users\l\Downloads\Telescope", "-style", "x"])
    assert args.after_update and args.moved_from == r"C:\Users\l\Downloads\Telescope"
    assert rest == ["-style", "x"]


def _link_to(path, exe):
    """Enough of a .lnk for the search: the target's path as UTF-16, among other bytes."""
    path.write_bytes(b"L\x00\x00\x00" + b"\x01" * 40 + str(exe).encode("utf-16-le") + b"\x00\x00")
    return path


def test_a_desktop_shortcut_to_the_old_copy_gets_the_installed_one_s_in_its_place(tmp_path):
    desktop = tmp_path / "Desktop"
    desktop.mkdir()
    installed, old = _installed(tmp_path), _unzipped(tmp_path / "Downloads" / "Telescope")
    dead = _link_to(desktop / "TelescopeDesktop.exe - Shortcut.lnk", str(old / "TelescopeDesktop.exe").upper())
    other = _link_to(desktop / "Notes.lnk", tmp_path / "notes.exe")
    run = _Run()
    updates.install_with_setup(tmp_path / "s.exe", old, run=run, locate=lambda: installed, folders=[desktop])
    assert run.calls[0][-1] == "/MERGETASKS=desktopicon"
    assert updates.remove_unzipped_copy(old, installed, pause=0, folders=[desktop])
    assert not dead.exists() and other.exists()


def test_no_desktop_shortcut_means_no_desktop_icon(tmp_path):
    installed, old = _installed(tmp_path), tmp_path / "Telescope"
    old.mkdir()
    run = _Run()
    updates.install_with_setup(tmp_path / "s.exe", old, run=run, locate=lambda: installed, folders=[tmp_path])
    assert "/MERGETASKS=desktopicon" not in run.calls[0]


def test_a_shortcut_stays_while_the_old_exe_does(tmp_path, monkeypatch):
    desktop = tmp_path / "Desktop"
    desktop.mkdir()
    old = _unzipped(tmp_path / "Telescope")
    link = _link_to(desktop / "Telescope.lnk", old / "TelescopeDesktop.exe")
    real = update_guard.remove

    def remove(path):
        if path.name == "TelescopeDesktop.exe":
            raise PermissionError(32, "in use")
        real(path)
    monkeypatch.setattr(update_guard, "remove", remove)
    assert not updates.remove_unzipped_copy(old, _installed(tmp_path), tries=2, pause=0, folders=[desktop])
    assert link.exists()


@pytest.mark.parametrize("encode", [lambda t: t.encode("utf-16-le"), lambda t: b"\x00" + t.encode("utf-16-le"),
                                    lambda t: t.encode("latin-1")])
def test_shortcuts_are_found_whatever_the_case_and_encoding_of_the_path(tmp_path, encode):
    exe = tmp_path / "Åsa" / "Downloads" / "Telescope" / "TelescopeDesktop.exe"
    (tmp_path / "x.lnk").write_bytes(b"L\x00\x00\x00" + encode(str(exe).upper()) + b"\x00\x00")
    assert updates.shortcuts_to(exe, [tmp_path]) == [tmp_path / "x.lnk"]
    assert updates.shortcuts_to(tmp_path / "Other" / "TelescopeDesktop.exe", [tmp_path]) == []


def test_lib_folders_without_a_build_number_or_without_qt_stay(tmp_path):
    downloads = _unzipped(tmp_path / "Downloads")
    for name in ("lib-abc/PyQt6/x.dll", "lib-456/notes.txt"):
        (downloads / name).parent.mkdir(parents=True, exist_ok=True)
        (downloads / name).write_bytes(b"theirs")
    assert updates.remove_unzipped_copy(downloads, _installed(tmp_path), pause=0)
    assert sorted(p.name for p in downloads.iterdir()) == ["lib-456", "lib-abc"]


def test_the_setup_installing_over_the_unzipped_folder_notes_no_move(tmp_path):
    old = _unzipped(tmp_path / "Telescope")
    result = updates.install_with_setup(tmp_path / "s.exe", old, run=_Run(), locate=lambda: old)
    assert result.relaunch == [str(old / "TelescopeDesktop.exe"), "--after-update"]
    assert updates.pending_move(old) is None
    assert not updates.remove_unzipped_copy(old, old, pause=0)  # and it wouldn't delete itself either
    assert (old / "TelescopeDesktop.exe").exists()


def test_an_installed_folder_without_the_exe_is_an_error(tmp_path):
    folder = tmp_path / "Programs" / "Telescope"
    folder.mkdir(parents=True)
    old = tmp_path / "old"
    old.mkdir()
    with pytest.raises(UpdateError, match="wasn't where expected"):
        updates.install_with_setup(tmp_path / "s.exe", old, run=_Run(), locate=lambda: folder)
    assert not (folder / updates.PENDING_MOVE).exists()


def test_symlinks_in_the_unzipped_copy_are_never_followed(tmp_path):
    outside = tmp_path / "Outside"
    (outside / "PyQt6").mkdir(parents=True)
    (outside / "PyQt6" / "keep.dll").write_bytes(b"mine")
    (outside / "adb.exe").write_bytes(b"mine")
    (outside / "telescope.log").write_bytes(b"mine")
    old = _unzipped(tmp_path / "Telescope")
    try:
        (old / "lib-456").symlink_to(outside, target_is_directory=True)
        (old / "platform-tools-link").symlink_to(outside, target_is_directory=True)
        (old / "THIRD_PARTY_NOTICES.txt").unlink()
        (old / "THIRD_PARTY_NOTICES.txt").symlink_to(outside / "adb.exe")
        (old / "telescope.log").symlink_to(outside / "telescope.log")
    except (OSError, NotImplementedError):
        pytest.skip("symlinks aren't available here")
    assert updates.remove_unzipped_copy(old, _installed(tmp_path), pause=0)
    assert sorted(p.name for p in outside.rglob("*")) == ["PyQt6", "adb.exe", "keep.dll", "telescope.log"]
    assert (outside / "telescope.log").read_bytes() == b"mine"  # the log link goes, not what it points at
    assert sorted(p.name for p in old.iterdir()) == ["THIRD_PARTY_NOTICES.txt", "lib-456", "platform-tools-link"]
