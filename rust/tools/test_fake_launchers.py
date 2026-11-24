#!/usr/bin/env python3
"""start.sh and AppRun against a fake telescope-rs: a journal that belongs to another launcher is left alone."""
import json
import os
import shutil
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
FAILS = "#!/bin/sh\nexit 3\n"


class LauncherTest(unittest.TestCase):
    def setUp(self):
        self.dir = Path(tempfile.mkdtemp())
        self.addCleanup(shutil.rmtree, self.dir, True)

    def program(self, folder, body):
        path = folder / "telescope-rs"
        path.write_text(body)
        path.chmod(path.stat().st_mode | stat.S_IEXEC)

    def journal(self, **fields):
        (self.dir / ".update.json").write_text(json.dumps({"state": "trial", "to_build": 5, **fields}, indent=2))
        (self.dir / ".previous").mkdir()
        (self.dir / ".previous" / "telescope-rs").write_text("old")

    def run_start(self):
        for name in ("start.sh",):
            shutil.copy(HERE / name, self.dir / name)
        self.program(self.dir, FAILS)
        return subprocess.run([str(self.dir / "start.sh")], capture_output=True, text=True, timeout=30)

    def test_start_sh_keeps_an_appimages_rollback_state(self):
        self.journal(appimage="Telescope-x86_64.AppImage")
        result = self.run_start()
        self.assertEqual(result.returncode, 3)
        self.assertTrue((self.dir / ".previous").is_dir())
        self.assertTrue((self.dir / ".update.json").is_file())

    def test_start_sh_still_rolls_a_failed_trial_back(self):
        self.journal(entries=["telescope-rs"], added=[])
        self.run_start()
        self.assertFalse((self.dir / ".update.json").exists())
        self.assertEqual((self.dir / "telescope-rs").read_text(), "old")

    def run_apprun(self, journal_owner):
        shutil.copy(HERE / "AppRun", self.dir / "AppRun")
        self.program(self.dir, FAILS)
        image = self.dir / "Telescope-x86_64.AppImage"
        image.write_text("#!/bin/sh\nexit 0\n")
        image.chmod(image.stat().st_mode | stat.S_IEXEC)
        (self.dir / ".previous" / "Telescope-x86_64.AppImage").write_text("#!/bin/sh\nexit 0\n")
        env = dict(os.environ, APPDIR=str(self.dir), APPIMAGE=str(image))
        return subprocess.run(["sh", str(self.dir / "AppRun")], capture_output=True, text=True, timeout=30, env=env)

    def test_apprun_ignores_another_appimages_journal(self):
        self.journal(appimage="Other.AppImage")
        (self.dir / ".previous").mkdir(exist_ok=True)
        result = self.run_apprun("Other.AppImage")
        self.assertEqual(result.returncode, 3)
        self.assertTrue((self.dir / ".update.json").is_file())

    def test_apprun_rolls_back_its_own_failed_trial(self):
        self.journal(appimage="Telescope-x86_64.AppImage")
        self.run_apprun("Telescope-x86_64.AppImage")
        self.assertFalse((self.dir / ".update.json").exists())


if __name__ == "__main__":
    unittest.main()
