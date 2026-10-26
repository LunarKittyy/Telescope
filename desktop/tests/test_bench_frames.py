"""scripts/bench_frames.py keeps running as plugins change: every row it times is there, on a tiny frame."""

import json
import subprocess
import sys
from pathlib import Path

DESKTOP = Path(__file__).resolve().parent.parent


def test_the_frame_benchmark_runs_through_every_section(tmp_path):
    out = tmp_path / "bench.json"
    subprocess.run([sys.executable, "scripts/bench_frames.py", "--frames", "2", "--size", "320x180", "--seconds", "0.2",
                    "--json", str(out)], cwd=DESKTOP, check=True, capture_output=True, timeout=300)
    rows = json.loads(out.read_text())
    for row in ("background (idle)", "colour (idle)", "privacy blur: others", "fun pack: crt",
                "colour grading: full grade (3D LUT)", "worker._publish and the preview's GUI side",
                "GUI timer lateness at 60 fps", "4 streams, every frame new", "MJPEG decode"):
        assert row in rows, row
