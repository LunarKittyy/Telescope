#!/usr/bin/env python3
"""Run this checkout as a dev desktop client next to the real Telescope: its own config (and so its own pairings and computer identity), log and mic, no menu or sign-in entry, and USB pairing aimed at the dev phone app (com.telescope.dev, from `./gradlew installDev`).

Usage: python scripts/dev_desktop.py [--keep] [--plugin FOLDER ...] [Qt options]
  default          a fresh profile, deleted when the app quits
  --keep           reuse one profile across runs, so the dev phone stays paired (delete it with --reset)
  --plugin FOLDER  load the plugin in FOLDER as it is, always on, and restart whenever one of its files is saved
                   (docs/plugins.md); give it more than once for several
"""

import getpass
import os
import shutil
import signal
import subprocess
import sys
import tempfile
from pathlib import Path

DESKTOP = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(DESKTOP))

from telescope import dev_profile  # noqa: E402


def kept_folder() -> Path:
    return Path(tempfile.gettempdir()) / f"telescope-dev-profile-{getpass.getuser()}"


def run_app(args, env) -> int:
    """Run Telescope until it quits; stop it too if this script is stopped, so no copy is left behind."""
    child = subprocess.Popen([sys.executable, str(DESKTOP / "main.py"), *args], env=env, cwd=DESKTOP)
    try:
        return child.wait()
    finally:
        if child.poll() is None:
            child.terminate()
            try:
                child.wait(10)
            except subprocess.TimeoutExpired:
                child.kill()


def take_plugins(argv) -> tuple:
    """(the --plugin folders, the other arguments); raises SystemExit saying what's wrong with a folder."""
    folders, rest = [], []
    words = iter(argv)
    for word in words:
        if word != "--plugin" and not word.startswith("--plugin="):
            rest.append(word)
            continue
        value = word.partition("=")[2] or next(words, "")
        folder = Path(value).expanduser().resolve()
        if not value or not (folder / "plugin.json").is_file():
            raise SystemExit(f"--plugin needs a plugin's folder (the one with plugin.json in it), not {value!r}")
        folders.append(folder)
    return folders, rest


def main(argv) -> int:
    keep, reset = "--keep" in argv, "--reset" in argv
    plugins, rest = take_plugins([a for a in argv if a not in ("--keep", "--reset")])
    if reset:
        shutil.rmtree(kept_folder(), ignore_errors=True)
        print(f"Removed {kept_folder()}")
        if not keep:
            return 0
    if keep:
        folder = kept_folder()
        folder.mkdir(mode=0o700, exist_ok=True)
    else:
        folder = Path(tempfile.mkdtemp(prefix="telescope-dev-"))
    print(f"Dev profile: {folder}", flush=True)
    if hasattr(signal, "SIGTERM"):
        signal.signal(signal.SIGTERM, lambda *_: sys.exit(143))  # still tidy up: the app and a throwaway profile
    env = dict(os.environ, **{dev_profile.ENV: str(folder), dev_profile.SUPERVISED_ENV: "1",
                              dev_profile.PLUGINS_ENV: os.pathsep.join(map(str, plugins))})
    env.pop(dev_profile.RESUME_ENV, None)
    for plugin in plugins:
        print(f"Plugin: {plugin} (Telescope restarts when you save one of its files)", flush=True)
    try:
        args = rest
        while True:
            code = run_app(args, env)
            if code not in (dev_profile.RESTART_EXIT, dev_profile.RESTART_STREAMING):
                return code
            env[dev_profile.RESUME_ENV] = "1" if code == dev_profile.RESTART_STREAMING else ""
            args = [a for a in rest if a != "--minimized"] + ["--restarted"]
            print("Restarting Telescope", flush=True)
    except KeyboardInterrupt:
        return 130
    finally:
        if not keep:
            shutil.rmtree(folder, ignore_errors=True)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
