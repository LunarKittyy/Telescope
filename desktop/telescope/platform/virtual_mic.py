"""The virtual microphone other apps record from.

Linux: a PulseAudio / PipeWire null sink that Telescope plays into, remapped as a source named
"Telescope Microphone". Nothing to install beyond pactl and pacat, and it's gone after a reboot
(or when the mic is switched off).
Windows: VB-Audio Virtual Cable, which has to be installed separately; Telescope plays into
"CABLE Input" and apps record from "CABLE Output".
"""

import shutil
import subprocess
from typing import Callable, Optional

SINK = "telescope_mic_sink"
SOURCE = "telescope_mic"
SOURCE_NAME = "Telescope Microphone"
LINUX_TOOLS = ("pactl", "pacat")

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


def linux_setup(run: Callable = _run) -> tuple:
    """Create the sink and source unless they're there already.
    Returns (module ids to unload later, error text or "")."""
    ok, out = run(["pactl", "list", "short", "sources"])
    if not ok:
        return [], "The sound server didn't answer (pactl list failed)."
    if any(line.split("\t")[1:2] == [SOURCE] for line in out.splitlines()):
        return [], ""
    loaded = []
    for args in (
        ["module-null-sink", f"sink_name={SINK}", 'sink_properties=device.description="Telescope (mic feed)"'],
        ["module-remap-source", f"master={SINK}.monitor", f"source_name={SOURCE}",
         f'source_properties=device.description="{SOURCE_NAME}"'],
    ):
        ok, out = run(["pactl", "load-module", *args])
        if not ok or not out.strip().isdigit():
            linux_teardown(loaded, run)
            return [], f"Couldn't create the virtual microphone: {out or 'pactl failed'}"
        loaded.append(int(out.strip()))
    return loaded, ""


def linux_teardown(module_ids: list, run: Callable = _run):
    for mid in reversed(module_ids):
        run(["pactl", "unload-module", str(mid)])


def pacat_command() -> list:
    return ["pacat", "--playback", f"--device={SINK}", "--format=s16le", "--rate=48000",
            "--channels=1", "--latency-msec=40", "--client-name=Telescope"]


def find_vb_cable(devices: list) -> Optional[int]:
    """Index of VB-Cable's playback end in sounddevice.query_devices(), or None."""
    for i, d in enumerate(devices):
        if VB_CABLE_PLAYBACK.lower() in str(d.get("name", "")).lower() and d.get("max_output_channels", 0) > 0:
            return i
    return None
