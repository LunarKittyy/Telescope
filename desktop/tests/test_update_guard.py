"""An update cut short at any step, and one that won't start, with temporary install folders."""

import io
import os
import sys
from pathlib import Path
import tarfile
import zipfile

import pytest

import update_guard
from telescope import updates


class _Killed(BaseException):
    """The process dying: nothing after it runs, not even except-clauses for Exception."""


def _no_one_else(*_a):
    return False


# ── Windows ──────────────────────────────────────────────────────────────────

def _windows_app(tmp_path):
    app = tmp_path / "app"
    (app / "lib-130").mkdir(parents=True)
    (app / "lib-130" / "python311.dll").write_bytes(b"py 130")
    (app / "TelescopeDesktop.exe").write_bytes(b"exe 130")
    (app / "Telescope.apk").write_bytes(b"apk 130")
    return app


def _windows_zip(tmp_path):
    path = tmp_path / "Telescope-windows.zip"
    with zipfile.ZipFile(path, "w") as z:
        for name, data in {"TelescopeDesktop.exe": b"exe 131", "lib-131/python311.dll": b"py 131",
                           "lib-131/PyQt6/qt.dll": b"qt 131", "Telescope.apk": b"apk 131"}.items():
            z.writestr(name, data)
    return path


def _assert_windows_new(app):
    assert (app / "TelescopeDesktop.exe").read_bytes() == b"exe 131"
    assert (app / "lib-131" / "python311.dll").read_bytes() == b"py 131"
    assert (app / "lib-131" / "PyQt6" / "qt.dll").read_bytes() == b"qt 131"
    assert (app / "Telescope.apk").read_bytes() == b"apk 131"
    assert not (app / update_guard.STAGING_DIR).exists()


def _count_moves(monkeypatch, run):
    calls = []
    real = os.replace
    monkeypatch.setattr(os, "replace", lambda s, d: calls.append(1) or real(s, d))
    run()
    monkeypatch.setattr(os, "replace", real)
    return len(calls)


def _kill_at(monkeypatch, k):
    real, seen = os.replace, []

    def replace(src, dst):
        seen.append(1)
        if len(seen) == k:
            raise _Killed()
        return real(src, dst)
    monkeypatch.setattr(os, "replace", replace)
    return real


def test_a_windows_install_cut_short_anywhere_is_finished_by_the_next_start(tmp_path, monkeypatch):
    total = _count_moves(monkeypatch, lambda: updates.install_windows(
        _windows_zip(tmp_path), _windows_app(tmp_path / "count"), 131))
    for k in range(1, total + 1):
        root = tmp_path / f"k{k}"
        root.mkdir()
        app = _windows_app(root)
        real = _kill_at(monkeypatch, k)
        try:
            updates.install_windows(_windows_zip(root), app, 131)
        except _Killed:
            pass
        monkeypatch.setattr(os, "replace", real)

        # Only the instant between the exe's two renames leaves nothing to start (the test runs the guard anyway);
        # everything else starts a copy that finishes the job.
        exe_was_new = (app / "TelescopeDesktop.exe").exists() and \
            (app / "TelescopeDesktop.exe").read_bytes() == b"exe 131"
        if update_guard.read_journal(app) is None:
            # Cut short before the journal was written: the update never began, and the old version is intact.
            assert update_guard.recover(app, running_elsewhere=_no_one_else) is None
            assert (app / "TelescopeDesktop.exe").read_bytes() == b"exe 130"
            continue
        relaunch = update_guard.recover(app, running_elsewhere=_no_one_else)
        _assert_windows_new(app)
        # A start by the old exe moves it aside for the new one, and hands over to that.
        assert (relaunch is None) == exe_was_new, k
        assert relaunch is None or relaunch[-1] == "--after-update"
        assert update_guard.read_journal(app)["state"] == "trial"
        # The old version is still there to go back to.
        assert (app / "lib-130" / "python311.dll").read_bytes() == b"py 130"


def test_a_windows_update_that_never_confirms_rolls_back_and_isnt_offered_again(tmp_path, monkeypatch):
    app = _windows_app(tmp_path)
    updates.install_windows(_windows_zip(tmp_path), app, 131)

    assert update_guard.recover(app, running_elsewhere=_no_one_else) is None  # first start of the new version
    assert update_guard.read_journal(app)["started"] is True
    # It crashed before confirming, so the next start goes back (once it's had its time to start up).
    monkeypatch.setattr(update_guard, "TRIAL_GRACE_S", 0)
    relaunch = update_guard.recover(app, running_elsewhere=_no_one_else)

    assert relaunch is not None and "--after-update" not in relaunch
    assert (app / "TelescopeDesktop.exe").read_bytes() == b"exe 130"
    assert (app / "TelescopeDesktop.failed.exe").read_bytes() == b"exe 131"
    assert update_guard.read_journal(app) is None
    assert update_guard.failed_build(app) == 131
    monkeypatch.setattr(updates.version, "BUILD", 130)
    assert not updates.is_newer(updates.parse_manifest(_manifest(131)), directory=app)
    assert updates.is_newer(updates.parse_manifest(_manifest(132)), directory=app)

    # The old version's clean-up takes the failed one's leftovers.
    updates.clean_up_after_update(app, running_lib="lib-130")
    assert not (app / "TelescopeDesktop.failed.exe").exists() and not (app / "lib-131").exists()


def test_an_old_exe_left_from_an_earlier_update_is_never_rolled_back_to(tmp_path, monkeypatch):
    app = _windows_app(tmp_path)
    (app / "TelescopeDesktop.old.exe").write_bytes(b"exe stale")  # its lib folder is long gone
    real = os.replace

    def lib_move_fails(src, dst):
        if Path(src).name.startswith("lib-"):
            raise OSError("in use")
        return real(src, dst)

    monkeypatch.setattr(os, "replace", lib_move_fails)
    with pytest.raises(updates.UpdateError):
        updates.install_windows(_windows_zip(tmp_path), app, 131)
    monkeypatch.setattr(os, "replace", real)

    assert (app / "TelescopeDesktop.exe").read_bytes() == b"exe 130"


def test_a_confirmed_update_stays_and_its_leftovers_go(tmp_path):
    app = _windows_app(tmp_path)
    updates.install_windows(_windows_zip(tmp_path), app, 131)
    assert update_guard.recover(app, running_elsewhere=_no_one_else) is None

    update_guard.confirm(app)
    assert update_guard.recover(app, running_elsewhere=_no_one_else) is None
    updates.clean_up_after_update(app, running_lib="lib-131")

    _assert_windows_new(app)
    assert not (app / "TelescopeDesktop.old.exe").exists() and not (app / "lib-130").exists()


def test_a_second_copy_starting_leaves_the_update_to_the_running_one(tmp_path):
    app = _windows_app(tmp_path)
    updates.install_windows(_windows_zip(tmp_path), app, 131)
    update_guard.write_journal(app, dict(update_guard.read_journal(app), started=True))

    assert update_guard.recover(app, running_elsewhere=lambda: True) is None
    assert (app / "TelescopeDesktop.exe").read_bytes() == b"exe 131"  # not rolled back under the running copy


def test_a_second_launch_while_the_new_version_is_still_loading_leaves_it_alone(tmp_path, monkeypatch):
    app = _windows_app(tmp_path)
    updates.install_windows(_windows_zip(tmp_path), app, 131)
    assert update_guard.recover(app, running_elsewhere=_no_one_else) is None  # the new version starts importing
    monkeypatch.setattr(update_guard.time, "sleep", lambda _s: None)
    bound_yet = iter([False, False, True])  # the double-click lands before it binds the port

    assert update_guard.recover(app, running_elsewhere=lambda: next(bound_yet)) is None
    assert (app / "TelescopeDesktop.exe").read_bytes() == b"exe 131"
    assert update_guard.failed_build(app) is None


def test_a_first_start_rolled_back_underneath_it_leaves_the_old_version_whole(tmp_path, monkeypatch):
    app = _windows_app(tmp_path)
    updates.install_windows(_windows_zip(tmp_path), app, 131)
    assert update_guard.recover(app, running_elsewhere=_no_one_else) is None  # the new version starts, slowly
    # A second launch gives up waiting for it to bind the port, and puts the old version back.
    monkeypatch.setattr(update_guard, "TRIAL_GRACE_S", 0)
    assert update_guard.recover(app, running_elsewhere=_no_one_else) is not None
    assert (app / "TelescopeDesktop.exe").read_bytes() == b"exe 130"

    # The slow first start carries on regardless (renamed, still 131) and gets as far as its clean-up.
    monkeypatch.setattr(updates.version, "BUILD", 131)
    updates.clean_up_after_update(app, running_lib="lib-131")
    assert (app / "lib-130" / "python311.dll").read_bytes() == b"py 130"  # the exe in place still starts


def test_after_an_update_the_new_copy_waits_for_the_old_one_to_exit(tmp_path):
    app = _windows_app(tmp_path)
    updates.install_windows(_windows_zip(tmp_path), app, 131)
    still_running = iter([True, True, False])

    assert update_guard.recover(app, wait=5, running_elsewhere=lambda: next(still_running)) is None
    assert update_guard.read_journal(app)["started"] is True


def test_a_roll_back_cut_short_is_finished_by_the_next_start(tmp_path, monkeypatch):
    app = _windows_app(tmp_path)
    updates.install_windows(_windows_zip(tmp_path), app, 131)
    journal = dict(update_guard.read_journal(app), started=True)
    update_guard.write_journal(app, journal)
    real = _kill_at(monkeypatch, 3)  # the journal, then the new exe aside, then dies before the old one is back
    with pytest.raises(_Killed):
        update_guard.roll_back(app, journal)
    monkeypatch.setattr(os, "replace", real)

    assert update_guard.recover(app, running_elsewhere=_no_one_else) is not None
    assert (app / "TelescopeDesktop.exe").read_bytes() == b"exe 130"
    assert update_guard.read_journal(app) is None


def test_clean_up_waits_while_an_update_is_unconfirmed(tmp_path):
    app = _windows_app(tmp_path)
    updates.install_windows(_windows_zip(tmp_path), app, 131)
    updates.clean_up_after_update(app, running_lib="lib-131")
    assert (app / "TelescopeDesktop.old.exe").exists() and (app / "lib-130").exists()


# ── Linux ────────────────────────────────────────────────────────────────────

def _linux_app(tmp_path):
    app = tmp_path / "app"
    (app / "telescope").mkdir(parents=True)
    (app / "main.py").write_text("main 130")
    (app / "update_guard.py").write_text("guard 130")
    (app / "start.sh").write_text("start 130")
    (app / "telescope" / "app.py").write_text("app 130")
    (app / "telescope" / "gone.py").write_text("only in 130")
    (app / "notes.txt").write_text("the user's")
    return app


NEW = {"main.py": b"main 131", "update_guard.py": b"guard 131", "start.sh": b"start 131",
       "telescope/app.py": b"app 131", "requirements.txt": b"new in 131"}


def _tarball(path):
    with tarfile.open(path, "w:gz") as t:
        for name, data in NEW.items():
            info = tarfile.TarInfo(name)
            info.size = len(data)
            t.addfile(info, io.BytesIO(data))
    return path


def _tree(app):
    return {p.relative_to(app).as_posix(): p.read_text() for p in sorted(app.rglob("*"))
            if p.is_file() and not any(part.startswith((".update", ".previous")) for part in p.relative_to(app).parts)}


NEW_TREE = {"main.py": "main 131", "update_guard.py": "guard 131", "start.sh": "start 131",
            "telescope/app.py": "app 131", "requirements.txt": "new in 131", "notes.txt": "the user's"}
OLD_TREE = {"main.py": "main 130", "update_guard.py": "guard 130", "start.sh": "start 130",
            "telescope/app.py": "app 130", "telescope/gone.py": "only in 130", "notes.txt": "the user's"}


def test_a_linux_install_cut_short_anywhere_is_finished_by_the_next_start(tmp_path, monkeypatch):
    moves = []
    real = os.replace
    monkeypatch.setattr(os, "replace", lambda s, d: moves.append(1) or real(s, d))
    updates.install_linux(_tarball(tmp_path / "count.tar.gz"), _linux_app(tmp_path / "count"), 131)
    monkeypatch.setattr(os, "replace", real)

    for k in range(1, len(moves) + 1):
        root = tmp_path / f"k{k}"
        root.mkdir()
        app = _linux_app(root)
        _kill_at(monkeypatch, k)
        try:
            updates.install_linux(_tarball(root / "u.tar.gz"), app, 131)
        except _Killed:
            pass
        monkeypatch.setattr(os, "replace", real)
        # start.sh, main.py and the guard are files, replaced in one step: whatever happened, the next start runs.
        for name in ("start.sh", "main.py", "update_guard.py"):
            assert (app / name).exists(), (k, name)

        began = update_guard.read_journal(app) is not None
        assert update_guard.recover(app, running_elsewhere=_no_one_else) is None
        # Cut short before the journal was written, the update never began; after, it's always finished.
        assert _tree(app) == (NEW_TREE if began else OLD_TREE), k


def test_a_linux_update_that_never_confirms_rolls_back(tmp_path, monkeypatch):
    app = _linux_app(tmp_path)
    updates.install_linux(_tarball(tmp_path / "u.tar.gz"), app, 131)
    assert update_guard.recover(app, running_elsewhere=_no_one_else) is None
    assert _tree(app) == NEW_TREE
    monkeypatch.setattr(update_guard, "TRIAL_GRACE_S", 0)

    relaunch = update_guard.recover(app, running_elsewhere=_no_one_else)

    assert relaunch == [str(app / "start.sh")]
    assert _tree(app) == OLD_TREE  # including the module 131 dropped, and without the file 131 added
    assert update_guard.failed_build(app) == 131


def test_a_linux_install_that_fails_outright_is_undone_but_not_blamed_on_the_build(tmp_path, monkeypatch):
    app = _linux_app(tmp_path)
    real = os.replace

    def flaky(src, dst):
        if str(dst).endswith("telescope") and update_guard.STAGING_DIR in str(src):
            raise PermissionError(13, "Permission denied")
        return real(src, dst)
    monkeypatch.setattr(os, "replace", flaky)
    with pytest.raises(updates.UpdateError, match="Permission denied"):
        updates.install_linux(_tarball(tmp_path / "u.tar.gz"), app, 131)
    monkeypatch.setattr(os, "replace", real)

    assert _tree(app) == OLD_TREE
    assert update_guard.read_journal(app) is None
    assert update_guard.failed_build(app) is None


def _manifest(build):
    return (f'{{"schema": 1, "version": "0.6.0", "build": {build}, "channel": "nightly", "versionName": "x",'
            f' "commit": "abc", "protocol": 2, "notes": "", "assets": []}}').encode()


# ── A logout during the first start is not a failed start ────────────────────

_POSIX_ONLY = pytest.mark.skipif(sys.platform == "win32", reason="no SIGHUP or os.kill semantics on Windows")


def _terminate_handler(monkeypatch, app):
    handlers, signalled = {}, []
    monkeypatch.setattr(update_guard.sys, "platform", "linux")
    monkeypatch.setattr(update_guard.signal, "signal", lambda sig, h: handlers.__setitem__(sig, h))
    monkeypatch.setattr(update_guard.os, "kill", lambda pid, sig: signalled.append(sig))
    update_guard.confirm_on_terminate(app)
    return handlers, signalled


@_POSIX_ONLY
def test_sigterm_during_the_first_start_confirms_instead_of_rolling_back(tmp_path, monkeypatch):
    app = _windows_app(tmp_path)
    updates.install_windows(_windows_zip(tmp_path), app, 131)
    assert update_guard.recover(app, running_elsewhere=_no_one_else) is None  # the new version starts
    handlers, signalled = _terminate_handler(monkeypatch, app)

    handlers[update_guard.signal.SIGTERM](update_guard.signal.SIGTERM, None)

    assert update_guard.read_journal(app) is None
    assert signalled == [update_guard.signal.SIGTERM]  # then it dies as the signal meant
    assert update_guard.recover(app, running_elsewhere=_no_one_else) is None  # nothing to roll back
    assert update_guard.failed_build(app) is None
    assert (app / "TelescopeDesktop.exe").read_bytes() == b"exe 131"


@_POSIX_ONLY
def test_a_start_that_dies_with_no_signal_still_rolls_back(tmp_path, monkeypatch):
    app = _windows_app(tmp_path)
    updates.install_windows(_windows_zip(tmp_path), app, 131)
    update_guard.recover(app, running_elsewhere=_no_one_else)
    _terminate_handler(monkeypatch, app)  # installed, but never run: a hard kill or a crash
    monkeypatch.setattr(update_guard, "TRIAL_GRACE_S", 0)

    assert update_guard.recover(app, running_elsewhere=_no_one_else) is not None
    assert update_guard.failed_build(app) == 131


@_POSIX_ONLY
def test_sigterm_with_no_update_in_progress_leaves_no_journal_behind(tmp_path, monkeypatch):
    handlers, signalled = _terminate_handler(monkeypatch, tmp_path)
    handlers[update_guard.signal.SIGHUP](update_guard.signal.SIGHUP, None)
    assert signalled == [update_guard.signal.SIGHUP] and not (tmp_path / update_guard.JOURNAL).exists()


def test_windows_installs_no_terminate_handler(tmp_path, monkeypatch):
    handlers = {}
    monkeypatch.setattr(update_guard.sys, "platform", "win32")
    monkeypatch.setattr(update_guard.signal, "signal", lambda sig, h: handlers.__setitem__(sig, h))
    update_guard.confirm_on_terminate(tmp_path)
    assert handlers == {}


def test_the_guard_sees_a_running_copy_through_the_same_address_the_app_binds(monkeypatch):
    import socket
    if not hasattr(socket, "AF_UNIX"):
        pytest.skip("Unix sockets")
    monkeypatch.setattr(update_guard.sys, "platform", "linux")
    monkeypatch.setattr(update_guard.os, "getuid", lambda: 4242425)
    family, address = update_guard.instance_address()
    assert not update_guard.another_copy_running()
    holder = update_guard.instance_socket(family)
    holder.bind(address)
    holder.listen(1)
    try:
        assert update_guard.another_copy_running()
        monkeypatch.setattr(update_guard.os, "getuid", lambda: 4242426)  # another user's copy doesn't count
        assert not update_guard.another_copy_running()
    finally:
        holder.close()


def test_right_after_an_update_a_copy_from_before_the_per_user_lock_counts(monkeypatch):
    if sys.platform == "win32":
        pytest.skip("the per-user lock is a Unix socket on Linux only")
    monkeypatch.setattr(update_guard.os, "getuid", lambda: 4242427)
    holder = update_guard.instance_socket(update_guard.socket.AF_INET)
    try:
        holder.bind(("127.0.0.1", 0))
    except OSError:
        pytest.skip("no loopback")
    monkeypatch.setattr(update_guard, "INSTANCE_PORT", holder.getsockname()[1])
    holder.listen(1)
    try:
        assert not update_guard.another_copy_running()
        assert update_guard.another_copy_running(legacy=True)
    finally:
        holder.close()
