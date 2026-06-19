import os
import shutil

from phonecam.platform import _run

V4L2_PHONE_DEV = "/dev/video11"
V4L2_OBS_DEV   = "/dev/video10"


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
    priv = ["pkexec"] if shutil.which("pkexec") else ["sudo"]
    cmd = (
        "modprobe -r v4l2loopback && sleep 0.5 && "
        "modprobe v4l2loopback devices=2 video_nr=10,11 "
        "card_label='OBS Virtual Camera,Phone Camera' exclusive_caps=1"
    )
    rc, _, err = _run(priv + ["sh", "-c", cmd], timeout=90)
    if rc != 0:
        return False, err or "reload failed"
    return True, f"{V4L2_PHONE_DEV} + {V4L2_OBS_DEV}"


def v4l2_load() -> tuple:
    """Load v4l2loopback with PhoneCam's parameters.
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
    priv = ["pkexec"] if shutil.which("pkexec") else ["sudo"]
    rc, _, err = _run(priv + ["modprobe", "v4l2loopback",
        "devices=2", "video_nr=10,11",
        "card_label=OBS Virtual Camera,Phone Camera",
        "exclusive_caps=1"], timeout=60)
    return (True, f"Loaded: {V4L2_PHONE_DEV} + {V4L2_OBS_DEV}") \
        if rc == 0 else (False, err or "modprobe failed")
