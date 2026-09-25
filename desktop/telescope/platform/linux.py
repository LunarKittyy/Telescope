import glob
import os
import shlex
import shutil
from typing import Optional

from telescope.platform import _run

V4L2_PHONE_DEV = "/dev/video11"
V4L2_OBS_DEV   = "/dev/video10"

# v4l2loopback params used by runtime load and persistent config (single source of truth).
V4L2_PARAMS = {
    "devices": "2",
    "video_nr": "10,11",
    "card_label": "OBS Virtual Camera,Phone Camera",
    "exclusive_caps": "1",
}

V4L2_PERSIST_MODPROBE_CONF = "/etc/modprobe.d/99-telescope-v4l2loopback.conf"
V4L2_PERSIST_MODULES_CONF  = "/etc/modules-load.d/99-telescope-v4l2loopback.conf"


def _v4l2_options_line() -> str:
    """The `options v4l2loopback ...` line as written to modprobe.d."""
    p = V4L2_PARAMS
    return (
        f'options v4l2loopback devices={p["devices"]} video_nr={p["video_nr"]} '
        f'card_label="{p["card_label"]}" exclusive_caps={p["exclusive_caps"]}\n'
    )


def v4l2_module_loaded() -> bool:
    rc, out, _ = _run(["lsmod"])
    return rc == 0 and "v4l2loopback" in out


def v4l2_module_installed() -> bool:
    """Whether the v4l2loopback package is installed (loadable), loaded or not."""
    if v4l2_module_loaded():
        return True
    rc, _, _ = _run(["modinfo", "v4l2loopback"])
    if rc == -1:  # no modinfo on PATH: can't tell, so don't claim it's missing
        return True
    return rc == 0


def v4l2_devices_ready() -> bool:
    return os.path.exists(V4L2_PHONE_DEV)


def v4l2_is_loaded() -> bool:
    return v4l2_module_loaded() and v4l2_devices_ready()


class PrivResult(tuple):
    """(ok, message) from a root command. `command` is set when no password prompt could be shown:
    the line to run in a terminal instead."""

    def __new__(cls, ok: bool, message: str, command: Optional[str] = None):
        result = super().__new__(cls, (ok, message))
        result.command = command
        return result

    @property
    def ok(self) -> bool:
        return self[0]

    @property
    def message(self) -> str:
        return self[1]


def as_text(result) -> tuple:
    """(ok, message) for a status label, with the command to run by hand appended when there is one."""
    ok, message = result
    command = getattr(result, "command", None)
    return ok, f"{message}\n{command}" if command else message


NO_PROMPT = "Telescope couldn't ask for your password here. Run this in a terminal, then try again."
CANCELLED = "Cancelled."


# A root operation is a list of steps: ("run", "shell command"), ("write", path, one line of text)
# or ("pause", "sleep N"), which needs no root.
# The same steps become the script pkexec runs and the command a person can paste into a terminal.

def _script(steps) -> str:
    parts = []
    for step in steps:
        if step[0] == "write":
            parts.append(f"printf '%s\\n' {shlex.quote(step[2])} > {step[1]}")
        else:
            parts.append(step[1])
    return " && ".join(parts)


def manual_command(steps) -> str:
    parts = []
    for step in steps:
        if step[0] == "write":
            parts.append(f"echo {shlex.quote(step[2])} | sudo tee {step[1]} > /dev/null")
        elif step[0] == "pause":
            parts.append(step[1])
        else:
            parts.append(f"sudo {step[1]}")
    return " && ".join(parts)


def _privileged(steps, timeout: int, failed: str) -> PrivResult:
    """Run a shell script as root with one password prompt.

    pkexec shows a graphical prompt. Without it (or without a polkit agent, exit 127), sudo can only
    be used non-interactively: a GUI app has no terminal to ask in, and a blocking sudo would just
    hang until the timeout. So it tries cached credentials and otherwise hands back the command.
    """
    script = _script(steps)
    if shutil.which("pkexec"):
        rc, _, err = _run(["pkexec", "sh", "-c", script], timeout=timeout)
        if rc == 0:
            return PrivResult(True, "")
        if rc == 126:
            return PrivResult(False, CANCELLED)
        if rc != 127:
            return PrivResult(False, err.strip() or failed)
    if shutil.which("sudo"):
        rc, _, err = _run(["sudo", "-n", "sh", "-c", script], timeout=timeout)
        if rc == 0:
            return PrivResult(True, "")
        if "password" not in (err or "").lower():
            return PrivResult(False, err.strip() or failed)
    return PrivResult(False, NO_PROMPT, manual_command(steps))


def _modprobe_step():
    p = V4L2_PARAMS
    return ("run", f"modprobe v4l2loopback devices={p['devices']} video_nr={p['video_nr']} "
                   f"card_label={shlex.quote(p['card_label'])} exclusive_caps={p['exclusive_caps']}")


def _persist_steps() -> list:
    return [("write", V4L2_PERSIST_MODPROBE_CONF, _v4l2_options_line().rstrip()),
            ("write", V4L2_PERSIST_MODULES_CONF, "v4l2loopback")]


def v4l2_unload() -> PrivResult:
    """Unload v4l2loopback. Fails if any consumer still holds the device open."""
    r = _privileged([("run", "modprobe -r v4l2loopback")], 30, "modprobe -r failed")
    return PrivResult(True, "Module unloaded") if r.ok else r


def v4l2_reload() -> PrivResult:
    """Unload and reload v4l2loopback in a single elevated invocation (one password prompt)."""
    r = _privileged([("run", "modprobe -r v4l2loopback"), ("pause", "sleep 0.5"), _modprobe_step()],
                    90, "reload failed")
    return PrivResult(True, f"{V4L2_PHONE_DEV} + {V4L2_OBS_DEV}") if r.ok else r


def v4l2_setup(persist: bool = False) -> PrivResult:
    """Load v4l2loopback with Telescope's parameters, and with persist also at every boot, behind one
    password prompt. Never unloads an already-running module, since that could break other setups."""
    if v4l2_module_loaded():
        return PrivResult(False, (
            f"v4l2loopback is loaded with a different config "
            f"and {V4L2_PHONE_DEV} is unavailable. "
            f"Run: sudo modprobe -r v4l2loopback"
        ))
    for dev in (V4L2_PHONE_DEV, V4L2_OBS_DEV):
        if os.path.exists(dev):
            return PrivResult(False, f"{dev} already exists and is not a v4l2loopback device.")
    status = v4l2_persist_status()
    # Someone else's modprobe.d options would fight ours at boot; leave theirs alone.
    persist = persist and not (status["modprobe_conf"] or status["modules_load_conf"]) \
        and not _find_conflicting_confs()
    steps = (_persist_steps() if persist else []) + [_modprobe_step()]
    r = _privileged(steps, 60, "modprobe failed")
    if r.ok:
        return PrivResult(True, f"Loaded: {V4L2_PHONE_DEV} + {V4L2_OBS_DEV}")
    if "not found" in r.message.lower():
        return PrivResult(False, (
            "v4l2loopback isn't installed. Install it via your package manager "
            "(v4l2loopback-dkms on Debian/Ubuntu/Arch, v4l2loopback via RPM Fusion "
            "on Fedora/Nobara), then try again."
        ))
    return r


def v4l2_load() -> PrivResult:
    """Load v4l2loopback for this boot only."""
    return v4l2_setup(persist=False)


# ── Persistent config (opt-in) ───────────────────────────────────────────
# v4l2_load()/v4l2_reload() configure for current boot only; these persist to disk.

def v4l2_persist_status() -> dict:
    """Whether Telescope's own persistence files currently exist."""
    return {
        "modprobe_conf": os.path.exists(V4L2_PERSIST_MODPROBE_CONF),
        "modules_load_conf": os.path.exists(V4L2_PERSIST_MODULES_CONF),
    }


def _find_conflicting_confs() -> list:
    """Other modprobe.d files (not ours) that already set v4l2loopback options."""
    conflicts = []
    ours = os.path.abspath(V4L2_PERSIST_MODPROBE_CONF)
    for path in sorted(glob.glob("/etc/modprobe.d/*.conf")):
        if os.path.abspath(path) == ours:
            continue
        try:
            with open(path, "r") as f:
                content = f.read()
        except OSError:
            continue
        for line in content.splitlines():
            stripped = line.strip()
            if stripped and not stripped.startswith("#") and "v4l2loopback" in stripped:
                conflicts.append(path)
                break
    return conflicts


def v4l2_persist_enable() -> tuple:
    """Write modprobe.d and modules-load.d files for v4l2loopback; no-ops if already persisted."""
    status = v4l2_persist_status()
    if status["modprobe_conf"] or status["modules_load_conf"]:
        return True, f"Already persisted: {V4L2_PERSIST_MODPROBE_CONF}"

    conflicts = _find_conflicting_confs()
    if conflicts:
        return False, (
            "Existing v4l2loopback config found in " + ", ".join(conflicts) +
            " - remove it first"
        )

    r = _privileged(_persist_steps(), 30, "Failed to write persistence files")
    if not r.ok:
        return r
    return PrivResult(True, f"Wrote {V4L2_PERSIST_MODPROBE_CONF} and {V4L2_PERSIST_MODULES_CONF}")


def v4l2_persist_disable() -> tuple:
    """Remove Telescope's persistence files; does not unload the running module."""
    existing = [p for p in (V4L2_PERSIST_MODPROBE_CONF, V4L2_PERSIST_MODULES_CONF)
                if os.path.exists(p)]
    if not existing:
        return True, "Nothing to remove"
    r = _privileged([("run", "rm -f " + " ".join(shlex.quote(p) for p in existing))], 30,
                    "Failed to remove persistence files")
    if not r.ok:
        return r
    return PrivResult(True, f"Removed {', '.join(existing)}")
