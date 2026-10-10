"""The virtual microphone other apps record from.

Linux: a PulseAudio / PipeWire pipe source named "Telescope Microphone", fed through a FIFO. It's
an input only, so it never shows up as a speaker, and there's no playback stream for the desktop to
move onto the real speakers. Needs only pactl, and it's gone after a reboot (or when the mic is
switched off).
Windows: VB-Audio Virtual Cable, which has to be installed separately; Telescope plays into
"CABLE Input" and apps record from "CABLE Output". The mic card can fetch VB-Audio's own pack and open its
setup, but VB-Cable isn't part of Telescope: its readme doesn't allow putting it inside another setup without
VB-Audio's agreement, so their setup runs as it is, with its own windows, and the person installing it sees whose
it is first.
"""

import hashlib
import logging
import os
import shutil
import stat
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
import zipfile
from pathlib import Path
from typing import Callable, Optional

from telescope import dev_profile

logger = logging.getLogger(__name__)

# A dev profile gets its own source, so setting it up doesn't unload the real app's
SOURCE = "telescope_dev_mic" if dev_profile.active() else "telescope_mic"
SOURCE_NAME = "Telescope Microphone (dev)" if dev_profile.active() else "Telescope Microphone"
LINUX_TOOLS = ("pactl",)
# Tags of every module an earlier run may have left loaded (the first builds used a null sink
# plus a remapped source).
_OURS = (f"source_name={SOURCE}",) + (() if dev_profile.active() else ("sink_name=telescope_mic_sink",))

VB_CABLE_URL = "https://vb-audio.com/Cable/"
# The pack VB-Audio's page links, pinned: a newer one fails the check and the card goes back to the link above
VB_CABLE_PACK_URL = "https://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack45.zip"
VB_CABLE_PACK_SHA256 = "b950e39f01af1d04ea623c8f6d8eb9b6ea5c477c637295fabf20631c85116bfb"
VB_CABLE_SETUP = "VBCABLE_Setup_x64.exe"  # signed by Vincent Burel, so UAC names VB-Audio's author
VB_CABLE_SETUP_SHA256 = "734c35dfa6d98f48782a451633ceb471166ec70d60482fd89a1123d0ee3c4f41"
# What VB-Audio asks anyone passing VB-Cable on to say (the pack's readme.txt)
VB_CABLE_ORIGIN = "The origin of VB-CABLE : www.vb-cable.com."
VB_CABLE_DONATIONWARE = "VB-CABLE is a donationware, all participations are welcome."
_ERROR_CANCELLED = 1223  # ERROR_CANCELLED: no to the UAC prompt
_PACK = "VBCABLE_Driver_Pack.zip"
_MAX_PACK = 64 << 20  # the pack is a few MB; anything far bigger isn't it
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
    # XDG_RUNTIME_DIR is private to this user by spec; without it, a folder of our own, never shared /tmp itself
    runtime = os.environ.get("XDG_RUNTIME_DIR")
    return os.path.join(runtime or _private_temp_dir(), f"{SOURCE.replace('_', '-')}.fifo")


def _private_temp_dir() -> str:
    path = os.path.join(tempfile.gettempdir(), f"telescope-{os.getuid()}")
    try:
        os.mkdir(path, 0o700)
    except FileExistsError:
        pass
    # Someone else could have made it first in a shared /tmp: use it only if it's a real folder, ours, and closed
    st = os.lstat(path)
    if not stat.S_ISDIR(st.st_mode) or st.st_uid != os.getuid() or st.st_mode & 0o077:
        raise OSError(f"{path} isn't a private folder of this user's, so the microphone won't use it")
    return path


def _stale_modules(out: str) -> list:
    stale = [line.split("\t")[0] for line in out.splitlines() if any(tag in line for tag in _OURS)]
    return [int(m) for m in stale if m.isdigit()]


def remove_stale_source(run: Callable = _run):
    """Unload a source a crashed run left behind. Quiet when there is no sound server or no pactl."""
    ok, out = run(["pactl", "list", "short", "modules"])
    if ok:
        linux_teardown(_stale_modules(out), run)


def linux_setup(run: Callable = _run, fifo: Optional[str] = None) -> tuple:
    """Create the source. Anything a crashed run left behind is unloaded first, so the FIFO is
    always the one this run writes to. Returns (module ids to unload later, error text or "")."""
    ok, out = run(["pactl", "list", "short", "modules"])
    if not ok:
        return [], "The sound server didn't answer (pactl list failed)."
    linux_teardown(_stale_modules(out), run)
    try:
        fifo = fifo or fifo_path()
    except OSError as e:
        return [], f"Couldn't create the virtual microphone: {e}"
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


def _sha256(path: Path) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def download_vb_cable(folder: Optional[Path] = None, urlopen: Callable = urllib.request.urlopen) -> tuple:
    """Fetch VB-Audio's pack and unpack it; (setup exe path, "") or (None, why not)."""
    if folder is None:
        # An earlier try's folder, each a few MB. Only ones a day old: a newer one's setup may still be open.
        for old in Path(tempfile.gettempdir()).glob("telescope-vbcable-*"):
            try:
                if time.time() - old.stat().st_mtime > 86400:
                    shutil.rmtree(old, ignore_errors=True)
            except OSError:
                pass
    folder = Path(folder or tempfile.mkdtemp(prefix="telescope-vbcable-"))
    pack = folder / _PACK
    try:
        got = 0
        with urlopen(VB_CABLE_PACK_URL, timeout=30) as r, open(pack, "wb") as f:
            for chunk in iter(lambda: r.read(1 << 16), b""):
                got += len(chunk)
                if got > _MAX_PACK:
                    raise ValueError("it's much bigger than VB-Audio's pack")
                f.write(chunk)
    except urllib.error.HTTPError as e:  # reached, but it said no
        logger.warning("VB-Cable download: %s", e)
        return None, "vb-audio.com didn't hand over VB-Cable. Try again later, or get it from their site."
    except urllib.error.URLError as e:
        logger.warning("VB-Cable download: %s", e)
        return None, "Couldn't reach vb-audio.com. Check the internet connection and try again."
    except Exception as e:
        logger.warning("VB-Cable download: %s", e)
        return None, "Couldn't download VB-Cable. Try again, or get it from their site."
    if _sha256(pack) != VB_CABLE_PACK_SHA256:
        return None, "VB-Audio's download has changed since this version of Telescope. Get it from their site instead."
    try:
        with zipfile.ZipFile(pack) as z:
            for name in z.namelist():
                # A flat pack: anything with a folder in its name isn't what was checked, so it's left out
                if name != Path(name).name or name.startswith("."):
                    continue
                with z.open(name) as src, open(folder / name, "wb") as dst:
                    shutil.copyfileobj(src, dst)
    except (OSError, zipfile.BadZipFile) as e:
        logger.warning("VB-Cable unpack: %s", e)
        return None, "Couldn't unpack VB-Cable. Try again, or get it from their site."
    setup = folder / VB_CABLE_SETUP
    if not setup.exists():
        return None, "VB-Audio's download doesn't have the setup it used to. Get it from their site instead."
    return setup, ""


def _folder_is_the_pack(folder: Path) -> bool:
    """The setup runs as admin and reads the files beside it (and Windows looks there for DLLs), so the whole
    folder has to be the pinned pack's files and nothing else, not just the setup."""
    pack = folder / _PACK
    try:
        if _sha256(pack) != VB_CABLE_PACK_SHA256:
            return False
        with zipfile.ZipFile(pack) as z:
            wanted = {n: z.read(n) for n in z.namelist() if n == Path(n).name and not n.startswith(".")}
        present = {p.name for p in folder.iterdir()}
        if present != set(wanted) | {_PACK}:
            return False
        return all((folder / name).read_bytes() == data for name, data in wanted.items())
    except (OSError, zipfile.BadZipFile):
        return False


def run_vb_cable_setup(setup: Path, start: Optional[Callable] = None) -> str:
    """Open VB-Audio's setup as admin, after checking it and everything beside it once more; "" once it's open,
    else why not."""
    if _sha256(setup) != VB_CABLE_SETUP_SHA256 or not _folder_is_the_pack(setup.parent):
        return "VB-Cable's setup changed after it was checked, so it wasn't opened."
    start = start or os.startfile  # Windows only; ShellExecute's runas shows UAC for the setup itself
    try:
        start(str(setup), "runas", cwd=str(setup.parent))  # the setup's own folder, wherever it looks for its files
    except OSError as e:
        if getattr(e, "winerror", None) == _ERROR_CANCELLED:
            return "Windows didn't get permission to install VB-Cable."
        logger.warning("VB-Cable setup: %s", e)
        return "Couldn't open VB-Cable's setup. Try again, or get it from their site."
    return ""
