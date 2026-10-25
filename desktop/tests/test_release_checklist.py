"""The release checklist's map of files to sections (read by .github/checklist_due.py) names every file in the repo."""

import subprocess
import sys
from pathlib import Path

SCRIPT = Path(__file__).resolve().parents[2] / ".github" / "checklist_due.py"


def test_every_file_is_in_a_checklist_section_or_listed_as_shared_or_ignored():
    r = subprocess.run([sys.executable, str(SCRIPT), "--check-map"], capture_output=True, text=True)
    assert r.returncode == 0, r.stdout + r.stderr
