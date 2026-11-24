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
OLD = "#!/bin/sh\nexit 0\n"
# The new build as the guard sees it: the first start marks the journal started, the next marks it retried, and every start fails
CRASHES = """#!/bin/sh
cd "$(dirname "$0")" || exit 1
echo run >> runs
[ -f .update.json ] || exit 0
if grep -q '"started": true' .update.json; then
    grep -q '"retried": true' .update.json || sed -i 's/"state": "trial"/"state": "trial", "retried": true/' .update.json
else
    sed -i 's/"started": false/"started": true/' .update.json
fi
exit 3
"""
# The same, but the retry gets as far as confirming the build (the journal goes) before it dies
CONFIRMS = CRASHES.replace("exit 3\n", "[ -f runs ] && [ \"$(wc -l < runs)\" -ge 2 ] && rm -f .update.json\nexit 3\n")


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

    def run_start(self, body=FAILS):
        for name in ("start.sh",):
            shutil.copy(HERE / name, self.dir / name)
        self.program(self.dir, body)
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

    def old_build(self):
        (self.dir / ".previous" / "telescope-rs").write_text(OLD)
        (self.dir / ".previous" / "telescope-rs").chmod(0o755)

    def runs(self):
        return (self.dir / "runs").read_text().count("run")

    def test_start_sh_gives_a_crashing_first_start_one_more_try_then_rolls_back(self):
        self.journal(entries=["telescope-rs"], added=[], started=False)
        self.old_build()
        result = self.run_start(CRASHES)
        self.assertEqual(self.runs(), 2)
        self.assertEqual(result.returncode, 0)
        self.assertFalse((self.dir / ".update.json").exists())
        self.assertEqual((self.dir / "telescope-rs").read_text(), OLD)
        self.assertEqual(json.loads((self.dir / ".update-failed").read_text()), {"build": 5})

    def test_start_sh_rolls_back_at_once_when_the_guard_already_retried(self):
        self.journal(entries=["telescope-rs"], added=[], started=True, retried=True)
        self.old_build()
        self.run_start(CRASHES)
        self.assertEqual(self.runs(), 1)
        self.assertFalse((self.dir / ".update.json").exists())

    def test_start_sh_keeps_a_build_whose_retry_got_as_far_as_confirming(self):
        self.journal(entries=["telescope-rs"], added=[], started=False)
        result = self.run_start(CONFIRMS)
        self.assertEqual(result.returncode, 3)
        self.assertEqual(self.runs(), 2)
        self.assertFalse((self.dir / ".update-failed").exists())

    def run_apprun(self, journal_owner, body=FAILS):
        shutil.copy(HERE / "AppRun", self.dir / "AppRun")
        self.program(self.dir, body)
        image = self.dir / "Telescope-x86_64.AppImage"
        image.write_text("#!/bin/sh\nexit 0\n")
        image.chmod(image.stat().st_mode | stat.S_IEXEC)
        (self.dir / ".previous" / "Telescope-x86_64.AppImage").write_text("#!/bin/sh\nexit 0\n")
        (self.dir / ".previous" / "Telescope-x86_64.AppImage").chmod(0o755)
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

    def test_apprun_gives_a_crashing_first_start_one_more_try_then_rolls_back(self):
        self.journal(appimage="Telescope-x86_64.AppImage", started=False)
        result = self.run_apprun("Telescope-x86_64.AppImage", CRASHES)
        self.assertEqual(self.runs(), 2)
        self.assertEqual(result.returncode, 0)
        self.assertFalse((self.dir / ".update.json").exists())


if __name__ == "__main__":
    unittest.main()
