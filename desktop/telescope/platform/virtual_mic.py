"""The virtual microphone other apps record from.

Linux: a PulseAudio / PipeWire pipe source named "Telescope Microphone", fed through a FIFO. It's
an input only, so it never shows up as a speaker, and there's no playback stream for the desktop to
move onto the real speakers. Needs only pactl, and it's gone after a reboot (or when the mic is
switched off).
Windows: VB-Audio Virtual Cable, which has to be installed separately; Telescope plays into
"CABLE Input" and apps record from "CABLE Output".
"""

import os
import shutil
import subprocess
import tempfile
from typing import Callable, Optional

SOURCE = "telescope_mic"
SOURCE_NAME = "Telescope Microphone"
LINUX_TOOLS = ("pactl",)
# Tags of every module an earlier run may have left loaded (the first builds used a null sink
# plus a remapped source).
_OURS = (f"source_name={SOURCE}", "sink_name=telescope_mic_sink")

VB_CABLE_URL = "https://vb-audio.com/Cable/"
VB_CABLE_PLAYBACK = "CABLE Input"
VB_CABLE_RECORD = "CABLE Output"


def _run(cmd: list, timeout: float = 5) -> tuple:
    try:
        r = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
    except (OSError, subprocess.TimeoutExpired) as e:
        return False, str(e)
    return r.returncode == 0, (r.stdout if r.returncode == 0 else r.stderr).strip()


def linux_tools_missing(which: Callable = shutil.which) -> list:
    return [t for t in LINUX_TOOLS if which(t) is None]


def fifo_path() -> str:
    return os.path.join(os.environ.get("XDG_RUNTIME_DIR") or tempfile.gettempdir(), "telescope-mic.fifo")


def linux_setup(run: Callable = _run, fifo: Optional[str] = None) -> tuple:
    """Create the source. Anything a crashed run left behind is unloaded first, so the FIFO is
    always the one this run writes to. Returns (module ids to unload later, error text or "")."""
    ok, out = run(["pactl", "list", "short", "modules"])
    if not ok:
        return [], "The sound server didn't answer (pactl list failed)."
    stale = [line.split("\t")[0] for line in out.splitlines()
             if any(tag in line for tag in _OURS)]
    linux_teardown([int(m) for m in stale if m.isdigit()], run)
    fifo = fifo or fifo_path()
    try:
        os.unlink(fifo)
    except OSError:
        pass
    base = ["pactl", "load-module", "module-pipe-source", f"source_name={SOURCE}", f"file={fifo}",
            "format=s16le", "rate=48000", "channels=1"]
    # The inner quotes keep the space; PulseAudio rejects the other quoting styles. If a sound
    # server rejects this one too, a mic listed as "telescope_mic" beats no mic.
    for extra in ([f"source_properties=\"device.description='{SOURCE_NAME}'\""], []):
        ok, out = run(base + extra)
        if ok and out.strip().isdigit():
            return [int(out.strip())], ""
    return [], f"Couldn't create the virtual microphone: {out or 'pactl failed'}"


def linux_teardown(module_ids: list, run: Callable = _run):
    for mid in reversed(module_ids):
        run(["pactl", "unload-module", str(mid)])


def find_vb_cable(devices: list) -> Optional[int]:
    """Index of VB-Cable's playback end in sounddevice.query_devices(), or None."""
    for i, d in enumerate(devices):
        if VB_CABLE_PLAYBACK.lower() in str(d.get("name", "")).lower() and d.get("max_output_channels", 0) > 0:
            return i
    return None
