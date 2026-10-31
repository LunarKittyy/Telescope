#!/usr/bin/env python3
"""The package's main.py. The Python app's updater only accepts a tarball with main.py and telescope/ in it, and anything still
starting `python main.py` lands here, so this hands over to start.sh and does nothing else."""

import os
import shutil
import sys
from pathlib import Path

start = Path(__file__).resolve().parent / "start.sh"
bash = shutil.which("bash") or "/bin/bash"
os.execv(bash, [bash, str(start), *sys.argv[1:]])
