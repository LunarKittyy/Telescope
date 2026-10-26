"""RNNoise noise reduction for the microphone: 48 kHz mono in 480-sample frames, the same audio /v1/audio sends.

The library comes from the pyrnnoise wheel and is loaded with ctypes straight from its folder. Importing pyrnnoise
itself would pull in audiolab and matplotlib, which it's installed without (requirements-nodeps.txt).
"""

import ctypes
import importlib.util
import logging
import os
import sys
import threading
from typing import Optional

import numpy as np

from telescope.i18n import tr

logger = logging.getLogger(__name__)

RATE = 48_000
FRAME = 480  # samples RNNoise takes at a time (10 ms)
DELAY = 2 * FRAME  # RNNoise's own lookahead: what comes out is 20 ms behind what went in

_lock = threading.Lock()
_loaded = False
_lib = None
_problem: Optional[tuple] = None  # (kind, error) when the library can't be had


def library_paths() -> list:
    """Where the library may be: the frozen build's pyrnnoise folder, then the installed package's."""
    name = "rnnoise.dll" if sys.platform == "win32" else "librnnoise.so"
    dirs = []
    frozen = getattr(sys, "_MEIPASS", None)
    if frozen:
        dirs.append(os.path.join(frozen, "pyrnnoise"))
    try:
        spec = importlib.util.find_spec("pyrnnoise")  # finds the folder without running its __init__
    except (ImportError, ValueError):
        spec = None
    if spec is not None and spec.submodule_search_locations:
        dirs.extend(spec.submodule_search_locations)
    return [os.path.join(d, name) for d in dirs]


def open_library(paths: list):
    """(library, None), or (None, (kind, error)) saying why it can't be loaded."""
    path = next((p for p in paths if os.path.isfile(p)), None)
    if path is None:
        return None, ("missing", "")
    try:
        lib = ctypes.CDLL(path)
        lib.rnnoise_create.argtypes = [ctypes.c_void_p]
        lib.rnnoise_create.restype = ctypes.c_void_p
        lib.rnnoise_destroy.argtypes = [ctypes.c_void_p]
        lib.rnnoise_destroy.restype = None
        lib.rnnoise_process_frame.argtypes = [ctypes.c_void_p, ctypes.c_void_p, ctypes.c_void_p]
        lib.rnnoise_process_frame.restype = ctypes.c_float
        lib.rnnoise_get_frame_size.restype = ctypes.c_int
        size = lib.rnnoise_get_frame_size()
        if size != FRAME:
            raise OSError(f"it works in frames of {size} samples, not {FRAME}")
    except (OSError, AttributeError) as e:
        logger.warning("Couldn't load RNNoise from %s: %s", path, e)
        return None, ("failed", str(e))
    logger.info("RNNoise loaded from %s", path)
    return lib, None


def load():
    """The RNNoise library, loaded once; None when it can't be, and problem() says why."""
    global _loaded, _lib, _problem
    with _lock:
        if not _loaded:
            _loaded = True
            _lib, _problem = open_library(library_paths())
        return _lib


def problem() -> str:
    """Why noise reduction isn't available, in the current language, or "" when it is."""
    load()
    if _problem is None:
        return ""
    kind, error = _problem
    if kind == "missing":
        return tr("The pyrnnoise package isn't installed.")
    return tr("Couldn't load RNNoise: {error}", error=error)


class Denoiser:
    """One stream's RNNoise state. process() takes float32 samples at 16-bit scale and gives back as many: whole
    frames (the writer's 10 ms chunks) go straight through, and anything else waits for the rest of its frame."""

    def __init__(self, lib):
        self._lib = lib
        self._state = lib.rnnoise_create(None)
        if not self._state:
            raise MemoryError("rnnoise_create failed")
        self._frame = np.zeros(FRAME, np.float32)  # the one buffer handed to the library
        self._in = np.zeros(0, np.float32)  # under a frame, waiting for the rest
        self._out = np.zeros(0, np.float32)  # cleaned, not handed back yet
        self._padded = False

    def process(self, samples: np.ndarray) -> np.ndarray:
        pending = np.concatenate((self._in, samples)) if self._in.size else samples
        whole = pending.size - pending.size % FRAME
        done = np.empty(whole, np.float32)
        ptr = self._frame.ctypes.data
        for i in range(0, whole, FRAME):
            self._frame[:] = pending[i:i + FRAME]
            self._lib.rnnoise_process_frame(self._state, ptr, ptr)
            done[i:i + FRAME] = self._frame
        self._in = pending[whole:].copy()
        out = np.concatenate((self._out, done)) if self._out.size else done
        if self._in.size and not self._padded:
            self._padded = True  # chunks that don't line up with frames: run most of a frame later from here on
            out = np.concatenate((np.zeros(FRAME - 1, np.float32), out))
        n = samples.size
        self._out = out[n:]
        return out[:n]

    def close(self):
        if self._state:
            self._lib.rnnoise_destroy(self._state)
            self._state = None


def make() -> Optional[Denoiser]:
    """A fresh Denoiser, or None without the library."""
    lib = load()
    return Denoiser(lib) if lib is not None else None
