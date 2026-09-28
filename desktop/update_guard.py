"""Makes installing an update survive being cut short, and undoes one that won't start. Standard library only, and
outside the telescope package: main.py runs it before importing anything else, when a half-swapped install may not
import at all.

telescope/updates.py unpacks an update into STAGING_DIR, writes the journal (.update.json) and calls finish_swap().
Every start then calls recover() first:
- "swapping": the install was cut short. finish_swap() is safe to run again, so it picks up where it stopped (or,
  if that fails, roll_back() puts the old version back).
- "trial": the new version is starting for the first time. The old one is kept until confirm(), which main.py calls
  once the window is up. A start that finds the trial already started (the last one never got that far) rolls back,
  and remembers the build so the updater doesn't offer it again.
- "rolling_back": a roll-back was cut short; it's safe to run again too.

Windows: the exe is swapped with two renames (a running exe can be renamed but not replaced), and nothing can run
while it's missing in between, so that one instant stays unprotected. Everything else is.
"""

import json
import os
import shutil
import socket
import subprocess
import sys
import time
from pathlib import Path
from typing import Optional

JOURNAL = ".update.json"
FAILED = ".update-failed"      # the build that was rolled back: {"build": n}
STAGING_DIR = ".update-staging"
PREVIOUS_DIR = ".previous"     # Linux: the replaced entries, until the new version is confirmed
EXE_NAME = "TelescopeDesktop.exe"
OLD_EXE_NAME = "TelescopeDesktop.old.exe"
FAILED_EXE_NAME = "TelescopeDesktop.failed.exe"
LIB_PREFIX = "lib-"            # + the build number: the folder the exe's libraries are in (telescope.spec)
INSTANCE_PORT = 47823          # telescope.app's single-instance port


# ── The journal ───────────────────────────────────────────────────────────────

def read_journal(directory: Path) -> Optional[dict]:
    try:
        data = json.loads((directory / JOURNAL).read_text())
    except (OSError, ValueError):
        return None
    return data if isinstance(data, dict) else None


def write_journal(directory: Path, data: dict):
    """Replace the journal in one step, so a cut-short write leaves the old one."""
    tmp = directory / (JOURNAL + ".tmp")
    with tmp.open("w") as f:
        json.dump(data, f)
        f.flush()
        os.fsync(f.fileno())
    os.replace(tmp, directory / JOURNAL)


def clear_journal(directory: Path):
    (directory / JOURNAL).unlink(missing_ok=True)


def failed_build(directory: Path) -> Optional[int]:
    try:
        return int(json.loads((directory / FAILED).read_text())["build"])
    except (OSError, ValueError, KeyError, TypeError):
        return None


def remove(path: Path):
    if path.is_dir() and not path.is_symlink():
        shutil.rmtree(path)
    else:
        path.unlink(missing_ok=True)


# ── Swapping the new version in (safe to run again after being cut short) ─────

def finish_swap(directory: Path, journal: dict, skipped: Optional[list] = None) -> bool:
    """Move what's left in the staging folder into place. True if the exe was swapped by this call (Windows: the
    running copy is then the old one under another name). Files in use that keep their old version go in skipped."""
    staging = directory / STAGING_DIR
    skipped = [] if skipped is None else skipped
    if journal.get("platform") == "windows":
        swapped = _finish_windows(directory, staging, skipped)
    else:
        swapped = _finish_linux(directory, staging)
    shutil.rmtree(staging, ignore_errors=True)
    return swapped


def _finish_windows(directory: Path, staging: Path, skipped: list) -> bool:
    if not staging.is_dir():
        return False
    # The libraries first, each folder in one rename, so the new exe never sits next to a half-copied one.
    for lib in sorted(p for p in staging.iterdir() if p.is_dir() and p.name.startswith(LIB_PREFIX)):
        if not (directory / lib.name).exists():
            os.replace(lib, directory / lib.name)
    swapped = False
    new_exe, current, old = staging / EXE_NAME, directory / EXE_NAME, directory / OLD_EXE_NAME
    if new_exe.exists():
        if current.exists():
            old.unlink(missing_ok=True)
            os.replace(current, old)  # allowed while it runs; overwriting isn't
        os.replace(new_exe, current)
        swapped = True
    for src in sorted(p for p in staging.rglob("*") if p.is_file()):
        dst = directory / src.relative_to(staging)
        if dst.exists() and _same(dst, src):
            continue
        dst.parent.mkdir(parents=True, exist_ok=True)
        try:
            os.replace(src, dst)
        except OSError:
            # e.g. a UnityCapture DLL loaded by OBS right now; it keeps working, just unchanged.
            skipped.append(str(src.relative_to(staging)))
    return swapped


def _finish_linux(directory: Path, staging: Path) -> bool:
    if not staging.is_dir():
        return False
    previous = directory / PREVIOUS_DIR
    previous.mkdir(exist_ok=True)
    for src in sorted(staging.iterdir()):
        dst, prev = directory / src.name, previous / src.name
        if src.is_dir() and not src.is_symlink():
            # A folder can't be replaced in one step: the old one moves aside, then the new one in.
            if dst.exists() and not prev.exists():
                os.replace(dst, prev)
            elif dst.exists():
                remove(dst)  # left from a cut-short run; the old version is safe in .previous
            os.replace(src, dst)
        else:
            # A file is replaced in one step, so start.sh and main.py are never missing.
            if src.name == "start.sh":
                src.chmod(0o755)  # before it's in place: a start.sh that won't run couldn't finish this
            if dst.exists() and not prev.exists():
                shutil.copy2(dst, prev)
            os.replace(src, dst)
    return False


def _same(a: Path, b: Path) -> bool:
    if a.stat().st_size != b.stat().st_size:
        return False
    with a.open("rb") as fa, b.open("rb") as fb:
        while True:
            ca, cb = fa.read(1 << 20), fb.read(1 << 20)
            if ca != cb:
                return False
            if not ca:
                return True


# ── Putting the old version back (safe to run again after being cut short) ───

def roll_back(directory: Path, journal: dict, remember: bool = True):
    """remember: the new build itself is to blame (it didn't start), so the updater stops offering it."""
    write_journal(directory, dict(journal, state="rolling_back"))
    if journal.get("platform") == "windows":
        current, old = directory / EXE_NAME, directory / OLD_EXE_NAME
        if old.exists():
            if current.exists():
                failed = directory / FAILED_EXE_NAME
                failed.unlink(missing_ok=True)
                os.replace(current, failed)  # may be the running copy: renaming is allowed
            os.replace(old, current)
        # The new lib folder may be the running one; the old version's clean-up deletes it.
    else:
        previous = directory / PREVIOUS_DIR
        for name in journal.get("entries", []):
            dst, prev = directory / name, previous / name
            if prev.exists():
                if dst.exists():
                    remove(dst)
                os.replace(prev, dst)
            elif name in journal.get("added", []) and dst.exists():
                remove(dst)
    shutil.rmtree(directory / STAGING_DIR, ignore_errors=True)
    if remember and journal.get("to_build"):
        (directory / FAILED).write_text(json.dumps({"build": journal["to_build"]}))
    clear_journal(directory)


# ── At start ──────────────────────────────────────────────────────────────────

def instance_socket() -> socket.socket:
    # Off Windows, reuse only skips the TIME_WAIT a "raise" leaves; on Windows it would let two copies share the port.
    s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 0 if sys.platform == "win32" else 1)
    return s


def another_copy_running() -> bool:
    s = instance_socket()
    try:
        s.bind(("127.0.0.1", INSTANCE_PORT))
        return False
    except OSError:
        return True
    finally:
        s.close()


def recover(directory: Path, wait: float = 0.0, running_elsewhere=another_copy_running) -> Optional[list]:
    """Deal with an update in progress. None: carry on starting. A list: the command that starts the right version
    of Telescope; the caller runs it and exits. wait: how long to give the copy that installed the update to exit."""
    journal = read_journal(directory)
    if journal is None:
        return None
    deadline = time.monotonic() + wait
    while running_elsewhere():
        if time.monotonic() >= deadline:
            return None  # the running copy owns the journal; this one just asks it to show itself
        time.sleep(0.25)
    state = journal.get("state")
    try:
        if state == "swapping":
            try:
                swapped = finish_swap(directory, journal)
            except OSError:
                roll_back(directory, journal, remember=False)
                return relaunch_command(directory)
            write_journal(directory, dict(journal, state="trial", started=False))
            # Windows: a copy that just renamed itself out of the way is the old version, so start the new one.
            return relaunch_command(directory, after_update=True) if swapped else None
        if state == "rolling_back" or (state == "trial" and journal.get("started")):
            roll_back(directory, journal)
            return relaunch_command(directory)
        if state == "trial":
            write_journal(directory, dict(journal, started=True))
    except OSError:
        pass  # can't fix it from here: start what's there, and try again next time
    return None


def confirm(directory: Path):
    """The new version started fine: stop being ready to roll it back. Its leftovers go with the next clean-up."""
    journal = read_journal(directory)
    if journal is not None and journal.get("state") == "trial":
        clear_journal(directory)


def relaunch_command(directory: Path, after_update: bool = False) -> list:
    extra = ["--after-update"] if after_update else []
    if getattr(sys, "frozen", False):
        return [str(directory / EXE_NAME)] + extra
    start = directory / "start.sh"
    if start.exists():
        return [str(start)] + extra
    return [sys.executable, str(directory / "main.py")] + extra


def launch(argv: list):
    """Start argv on its own, not as a child of this process (which is about to exit)."""
    # A packaged app passes its own library folder to child copies of itself; this makes the new one find its own.
    env = dict(os.environ, PYINSTALLER_RESET_ENVIRONMENT="1")
    kwargs = {"close_fds": True, "env": env}
    if sys.platform == "win32":
        kwargs["creationflags"] = subprocess.DETACHED_PROCESS | subprocess.CREATE_NEW_PROCESS_GROUP
    else:
        kwargs["start_new_session"] = True
    subprocess.Popen(argv, **kwargs)
