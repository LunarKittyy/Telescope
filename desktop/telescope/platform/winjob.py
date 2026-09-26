"""Windows job object that kills its processes when Telescope exits, however it exits.

Windows has no parent-child lifetime for processes; a job with KILL_ON_JOB_CLOSE is the equivalent. Its
handle lives in this process, so Windows closes it (and kills everything in it) when Telescope quits or
crashes. Only processes added with kill_with_us() are in it; Telescope itself isn't, so programs it opens
(a browser, the log in a text editor, the relaunched app after an update) aren't touched.
"""

import logging

logger = logging.getLogger(__name__)

_JOB_OBJECT_EXTENDED_LIMIT_INFORMATION = 9
_JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE = 0x2000

_job = None


def _create_job():
    import ctypes
    from ctypes import wintypes

    class IoCounters(ctypes.Structure):
        _fields_ = [(name, ctypes.c_ulonglong) for name in (
            "ReadOperationCount", "WriteOperationCount", "OtherOperationCount",
            "ReadTransferCount", "WriteTransferCount", "OtherTransferCount")]

    class BasicLimits(ctypes.Structure):
        _fields_ = [("PerProcessUserTimeLimit", ctypes.c_int64), ("PerJobUserTimeLimit", ctypes.c_int64),
                    ("LimitFlags", wintypes.DWORD), ("MinimumWorkingSetSize", ctypes.c_size_t),
                    ("MaximumWorkingSetSize", ctypes.c_size_t), ("ActiveProcessLimit", wintypes.DWORD),
                    ("Affinity", ctypes.c_size_t), ("PriorityClass", wintypes.DWORD),
                    ("SchedulingClass", wintypes.DWORD)]

    class ExtendedLimits(ctypes.Structure):
        _fields_ = [("BasicLimitInformation", BasicLimits), ("IoInfo", IoCounters),
                    ("ProcessMemoryLimit", ctypes.c_size_t), ("JobMemoryLimit", ctypes.c_size_t),
                    ("PeakProcessMemoryUsed", ctypes.c_size_t), ("PeakJobMemoryUsed", ctypes.c_size_t)]

    k32 = ctypes.WinDLL("kernel32", use_last_error=True)
    k32.CreateJobObjectW.restype = wintypes.HANDLE
    k32.CreateJobObjectW.argtypes = [ctypes.c_void_p, wintypes.LPCWSTR]
    k32.SetInformationJobObject.argtypes = [wintypes.HANDLE, ctypes.c_int, ctypes.c_void_p, wintypes.DWORD]
    k32.AssignProcessToJobObject.argtypes = [wintypes.HANDLE, wintypes.HANDLE]
    k32.CloseHandle.argtypes = [wintypes.HANDLE]

    job = k32.CreateJobObjectW(None, None)
    if not job:
        raise OSError(ctypes.get_last_error(), "CreateJobObject failed")
    info = ExtendedLimits()
    info.BasicLimitInformation.LimitFlags = _JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
    if not k32.SetInformationJobObject(job, _JOB_OBJECT_EXTENDED_LIMIT_INFORMATION,
                                       ctypes.byref(info), ctypes.sizeof(info)):
        err = ctypes.get_last_error()
        k32.CloseHandle(job)
        raise OSError(err, "SetInformationJobObject failed")
    return k32, job


def kill_with_us(proc) -> bool:
    """Put a subprocess.Popen in the job, so it dies when Telescope does. False if Windows refused."""
    global _job
    try:
        if _job is None:
            _job = _create_job()
        k32, job = _job
        if not k32.AssignProcessToJobObject(job, int(proc._handle)):
            import ctypes
            raise OSError(ctypes.get_last_error(), "AssignProcessToJobObject failed")
        return True
    except Exception as exc:
        logger.warning("Couldn't tie adb's server to Telescope's lifetime: %s", exc)
        return False
