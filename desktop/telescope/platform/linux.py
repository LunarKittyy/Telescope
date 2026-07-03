import glob
import os
import shutil

from telescope.platform import _run

V4L2_PHONE_DEV = "/dev/video11"
V4L2_OBS_DEV   = "/dev/video10"

# Shared v4l2loopback module parameters. Both the runtime load/reload path
# and the persistent on-disk config writer build their strings from this so
# they can never drift apart.
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


def v4l2_devices_ready() -> bool:
    return os.path.exists(V4L2_PHONE_DEV)


def v4l2_is_loaded() -> bool:
    return v4l2_module_loaded() and v4l2_devices_ready()


def v4l2_unload() -> tuple:
    """Unload v4l2loopback. Fails if any consumer still holds the device open."""
    priv = ["pkexec"] if shutil.which("pkexec") else ["sudo"]
    rc, _, err = _run(priv + ["modprobe", "-r", "v4l2loopback"], timeout=30)
    if rc != 0:
        return False, err or "modprobe -r failed"
    return True, "Module unloaded"


def v4l2_reload() -> tuple:
    """Unload and reload v4l2loopback in a single elevated invocation (one password prompt)."""
    p = V4L2_PARAMS
    priv = ["pkexec"] if shutil.which("pkexec") else ["sudo"]
    cmd = (
        "modprobe -r v4l2loopback && sleep 0.5 && "
        f"modprobe v4l2loopback devices={p['devices']} video_nr={p['video_nr']} "
        f"card_label='{p['card_label']}' exclusive_caps={p['exclusive_caps']}"
    )
    rc, _, err = _run(priv + ["sh", "-c", cmd], timeout=90)
    if rc != 0:
        return False, err or "reload failed"
    return True, f"{V4L2_PHONE_DEV} + {V4L2_OBS_DEV}"


def v4l2_load() -> tuple:
    """Load v4l2loopback with Telescope's parameters.
    Never unloads an already-running module -- that could break other setups.
    """
    if v4l2_module_loaded():
        return False, (
            f"v4l2loopback is loaded with a different config "
            f"and {V4L2_PHONE_DEV} is unavailable. "
            f"Run: sudo modprobe -r v4l2loopback"
        )
    for dev in (V4L2_PHONE_DEV, V4L2_OBS_DEV):
        if os.path.exists(dev):
            return False, f"{dev} already exists and is not a v4l2loopback device."
    p = V4L2_PARAMS
    priv = ["pkexec"] if shutil.which("pkexec") else ["sudo"]
    rc, _, err = _run(priv + ["modprobe", "v4l2loopback",
        f"devices={p['devices']}", f"video_nr={p['video_nr']}",
        f"card_label={p['card_label']}",
        f"exclusive_caps={p['exclusive_caps']}"], timeout=60)
    return (True, f"Loaded: {V4L2_PHONE_DEV} + {V4L2_OBS_DEV}") \
        if rc == 0 else (False, err or "modprobe failed")


# ── Persistent config (opt-in) ───────────────────────────────────────────
#
# v4l2_load()/v4l2_reload() above only configure the module for the current
# boot. These functions let the user additionally write that same config to
# disk so it survives a reboot, without touching any other app's config.

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
    """Write modprobe.d + modules-load.d files so v4l2loopback comes up
    pre-configured with Telescope's params on every future boot/module load.
    No-ops (returns success) if either file already exists, rather than
    clobbering it.
    """
    status = v4l2_persist_status()
    if status["modprobe_conf"] or status["modules_load_conf"]:
        return True, f"Already persisted: {V4L2_PERSIST_MODPROBE_CONF}"

    conflicts = _find_conflicting_confs()
    if conflicts:
        return False, (
            "Existing v4l2loopback config found in " + ", ".join(conflicts) +
            " - remove it first"
        )

    priv = ["pkexec"] if shutil.which("pkexec") else ["sudo"]
    script = (
        f"cat > {V4L2_PERSIST_MODPROBE_CONF} << 'EOF'\n{_v4l2_options_line()}EOF\n"
        f"cat > {V4L2_PERSIST_MODULES_CONF} << 'EOF'\nv4l2loopback\nEOF\n"
    )
    rc, _, err = _run(priv + ["sh", "-c", script], timeout=30)
    if rc != 0:
        return False, err or "Failed to write persistence files"
    return True, f"Wrote {V4L2_PERSIST_MODPROBE_CONF} and {V4L2_PERSIST_MODULES_CONF}"


def v4l2_persist_disable() -> tuple:
    """Remove Telescope's persistence files, if present. Does not unload the
    currently-running module."""
    existing = [p for p in (V4L2_PERSIST_MODPROBE_CONF, V4L2_PERSIST_MODULES_CONF)
                if os.path.exists(p)]
    if not existing:
        return True, "Nothing to remove"
    priv = ["pkexec"] if shutil.which("pkexec") else ["sudo"]
    rc, _, err = _run(priv + ["rm", "-f"] + existing, timeout=30)
    if rc != 0:
        return False, err or "Failed to remove persistence files"
    return True, f"Removed {', '.join(existing)}"
