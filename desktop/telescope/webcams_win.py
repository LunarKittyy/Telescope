"""Windows COM for webcams, in plain ctypes so there's nothing to install.

dshow_paths() lists cameras in the order OpenCV's DirectShow backend opens them by (its Media Foundation backend
isn't in the OpenCV 5 wheels), each by the device path Qt Multimedia gives as its id, so a camera Qt lists is opened by
the right index. WindowsControls reaches a camera's picture settings through its Media Foundation source, found by
that same path: IAMVideoProcAmp (brightness, white balance, gain...) and IAMCameraControl (exposure, focus, zoom...),
each with the camera's own range, step and default.
"""

import ctypes
import logging
import uuid
from ctypes import POINTER, byref, c_long, c_uint32, c_void_p

from telescope.webcam_controls import Control

logger = logging.getLogger(__name__)

_AUTO, _MANUAL = 1, 2  # VideoProcAmp_Flags / CameraControl_Flags
# (our name, interface, property, unit); the auto switch for one is offered where the camera has both modes
_PROC_AMP, _CAMERA = "proc", "camera"
_PROPERTIES = (
    ("brightness", _PROC_AMP, 0, ""), ("contrast", _PROC_AMP, 1, ""), ("hue", _PROC_AMP, 2, ""),
    ("saturation", _PROC_AMP, 3, ""), ("sharpness", _PROC_AMP, 4, ""), ("gamma", _PROC_AMP, 5, ""),
    ("white_balance", _PROC_AMP, 7, "kelvin"), ("backlight", _PROC_AMP, 8, ""), ("gain", _PROC_AMP, 9, ""),
    ("anti_flicker", _PROC_AMP, 13, ""),  # KSPROPERTY_VIDEOPROCAMP_POWERLINE_FREQUENCY, numbered like V4L2's
    ("pan", _CAMERA, 0, ""), ("tilt", _CAMERA, 1, ""), ("zoom", _CAMERA, 3, "zoom"),
    ("exposure_time", _CAMERA, 4, "log2s"), ("focus", _CAMERA, 6, ""),
    ("frame_rate_drop", _CAMERA, 19, ""),  # KSPROPERTY_CAMERACONTROL_AUTO_EXPOSURE_PRIORITY
)
_AUTO_SWITCH = {"exposure_time": "exposure_auto", "white_balance": "white_balance_auto", "focus": "focus_auto"}
_SWITCHED = {auto: name for name, auto in _AUTO_SWITCH.items()}
_BY_NAME = {p[0]: p for p in _PROPERTIES}


def _guid(text: str):
    return (ctypes.c_byte * 16).from_buffer_copy(uuid.UUID(text).bytes_le)


IID_IMFMediaSource = "279a808d-aec7-40c8-9c6b-a6b492c78a66"
IID_IAMVideoProcAmp = "c6e13360-30ac-11d0-a18c-00a0c9118956"
IID_IAMCameraControl = "c6e13370-30ac-11d0-a18c-00a0c9118956"
MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE = "c60ac5fe-252a-478f-a0ef-bc8fa5f7cad3"
MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID = "8ac3587a-4ae7-42d8-99e0-0a6013eef90f"
MF_DEVSOURCE_ATTRIBUTE_SYMBOLIC_LINK = "58f0aad8-22bf-4f8a-bb3d-d2c4978c6e2f"
CLSID_SystemDeviceEnum = "62be5d10-60eb-11d0-bd3b-00a0c911ce86"
IID_ICreateDevEnum = "29840822-5b84-11d0-bd3b-00a0c911ce86"
CLSID_VideoInputDeviceCategory = "860bb310-5d01-11d0-bd3b-00a0c911ce86"
IID_IPropertyBag = "55272a00-42cb-11ce-8135-00aa004bb851"
_MF_VERSION, _MFSTARTUP_LITE = 0x00020070, 1
# vtable slots: IUnknown, IMFAttributes, IMFActivate, IAMVideoProcAmp/IAMCameraControl (same shape)
_QI, _RELEASE = 0, 2
_ATTR_GET_ALLOCATED_STRING, _ATTR_SET_GUID = 13, 24
_ACTIVATE_OBJECT, _SHUTDOWN_OBJECT = 33, 34
_GET_RANGE, _SET, _GET = 3, 4, 5
_CREATE_CLASS_ENUMERATOR, _ENUM_NEXT, _BIND_TO_STORAGE, _BAG_READ = 3, 3, 9, 3
_VT_BSTR, _VARIANT_SIZE = 8, 24


def _call(ptr, slot: int, *args, argtypes=()):
    """Call a COM method by its vtable slot; raises OSError on a failed HRESULT."""
    vtable = ctypes.cast(ptr, POINTER(POINTER(c_void_p))).contents
    fn = ctypes.WINFUNCTYPE(ctypes.HRESULT, c_void_p, *argtypes)(vtable[slot])
    return fn(ptr, *args)


def _release(ptr):
    if ptr:
        _call(ptr, _RELEASE)


def device_instance(path: str) -> str:
    """A device path without its interface class (the "#{guid}" at the end), which DirectShow and Media Foundation
    give differently for one camera."""
    path = path.strip().lower()
    cut = path.rfind("#{")
    return path[:cut] if cut > 0 else path


def _same_path(a: str, b: str) -> bool:
    return device_instance(a) == device_instance(b)


def dshow_paths() -> list:
    """The device path of each DirectShow camera, in the order OpenCV's DSHOW backend numbers them ("" for one
    without a path, like a virtual camera's filter)."""
    ole, oleaut = ctypes.WinDLL("ole32"), ctypes.WinDLL("oleaut32")
    ole.CoInitializeEx(None, 2)  # already set up on the GUI thread, which is fine
    devenum, monikers, out = c_void_p(), c_void_p(), []
    ole.CoCreateInstance.argtypes = (c_void_p, c_void_p, c_uint32, c_void_p, c_void_p)
    if ole.CoCreateInstance(_guid(CLSID_SystemDeviceEnum), None, 1, _guid(IID_ICreateDevEnum), byref(devenum)) != 0:
        return out
    try:
        if _call(devenum, _CREATE_CLASS_ENUMERATOR, _guid(CLSID_VideoInputDeviceCategory), byref(monikers), 0,
                 argtypes=(c_void_p, c_void_p, c_uint32)) != 0 or not monikers:
            return out  # S_FALSE: no cameras at all
        while True:
            moniker, fetched = c_void_p(), c_uint32()
            if _call(monikers, _ENUM_NEXT, 1, byref(moniker), byref(fetched),
                     argtypes=(c_uint32, c_void_p, c_void_p)) != 0 or not fetched.value:
                break
            bag, variant = c_void_p(), (ctypes.c_byte * _VARIANT_SIZE)()
            try:
                _call(moniker, _BIND_TO_STORAGE, None, None, _guid(IID_IPropertyBag), byref(bag),
                      argtypes=(c_void_p, c_void_p, c_void_p, c_void_p))
                _call(bag, _BAG_READ, "DevicePath", variant, None, argtypes=(ctypes.c_wchar_p, c_void_p, c_void_p))
                kind = ctypes.c_ushort.from_buffer(variant).value
                text = ctypes.c_void_p.from_buffer(variant, 8).value
                out.append(ctypes.wstring_at(text) if kind == _VT_BSTR and text else "")
                oleaut.VariantClear(variant)
            except OSError:
                out.append("")
            finally:
                _release(bag)
                _release(moniker)
    finally:
        _release(monikers)
        _release(devenum)
    return out


class WindowsControls:
    """The settings of the camera with this device path (Qt's id for it)."""

    def __init__(self, path: str):
        self._path = path
        self._activate = self._source = None
        self._ifaces: dict = {}

    def _open(self) -> bool:
        if self._ifaces:
            return True
        mf, mfplat, ole = ctypes.WinDLL("mf"), ctypes.WinDLL("mfplat"), ctypes.WinDLL("ole32")
        mfplat.MFStartup(_MF_VERSION, _MFSTARTUP_LITE)
        attrs = c_void_p()
        if mfplat.MFCreateAttributes(byref(attrs), 1) != 0:
            return False
        activates, count = POINTER(c_void_p)(), c_uint32()
        try:
            _call(attrs, _ATTR_SET_GUID, _guid(MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE),
                  _guid(MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID), argtypes=(c_void_p, c_void_p))
            if mf.MFEnumDeviceSources(attrs, byref(activates), byref(count)) != 0:
                return False
            found = [activates[i] for i in range(count.value)]
            pick = next((i for i, a in enumerate(found) if _same_path(self._link(a), self._path)), -1)
            for i, activate in enumerate(found):
                if i != pick:
                    _release(activate)
            if pick < 0:
                return False
            self._activate = found[pick]
            source = c_void_p()
            _call(self._activate, _ACTIVATE_OBJECT, _guid(IID_IMFMediaSource), byref(source),
                  argtypes=(c_void_p, c_void_p))
            self._source = source
            for kind, iid in ((_PROC_AMP, IID_IAMVideoProcAmp), (_CAMERA, IID_IAMCameraControl)):
                iface = c_void_p()
                try:
                    _call(source, _QI, _guid(iid), byref(iface), argtypes=(c_void_p, c_void_p))
                    self._ifaces[kind] = iface
                except OSError:
                    pass
            if not self._ifaces:
                self.close()  # nothing to set, so don't keep the device's source open
                return False
            return True
        except OSError:
            logger.debug("Couldn't open the webcam's settings", exc_info=True)
            self.close()
            return False
        finally:
            _release(attrs)
            if activates:
                ole.CoTaskMemFree(activates)

    @staticmethod
    def _link(activate) -> str:
        text, length = c_void_p(), c_uint32()
        try:
            _call(activate, _ATTR_GET_ALLOCATED_STRING, _guid(MF_DEVSOURCE_ATTRIBUTE_SYMBOLIC_LINK), byref(text),
                  byref(length), argtypes=(c_void_p, c_void_p, c_void_p))
        except OSError:
            return ""
        value = ctypes.wstring_at(text.value) if text.value else ""
        ctypes.WinDLL("ole32").CoTaskMemFree(text)
        return value

    def close(self):
        for iface in self._ifaces.values():
            _release(iface)
        self._ifaces = {}
        _release(self._source)
        if self._activate:
            try:
                _call(self._activate, _SHUTDOWN_OBJECT)
            except OSError:
                pass
            _release(self._activate)
        self._activate = self._source = None

    def _range(self, kind: str, prop: int):
        lo, hi, step, default, caps = (c_long() for _ in range(5))
        _call(self._ifaces[kind], _GET_RANGE, prop, byref(lo), byref(hi), byref(step), byref(default), byref(caps),
              argtypes=(c_long,) + (c_void_p,) * 5)
        return lo.value, hi.value, step.value, default.value, caps.value

    def _get(self, kind: str, prop: int):
        value, flags = c_long(), c_long()
        _call(self._ifaces[kind], _GET, prop, byref(value), byref(flags), argtypes=(c_long, c_void_p, c_void_p))
        return value.value, flags.value

    def list(self) -> list:
        try:
            if not self._open():
                return []
        except OSError:
            return []
        out, answered = [], False
        for name, kind, prop, unit in _PROPERTIES:
            if kind not in self._ifaces:
                continue
            try:
                lo, hi, step, default, caps = self._range(kind, prop)
                value, flags = self._get(kind, prop)
            except OSError:
                continue
            answered = True
            if hi <= lo or not caps & _MANUAL:
                continue
            auto_on = bool(flags & _AUTO)
            if name in _AUTO_SWITCH and caps & _AUTO:
                out.append(Control(_AUTO_SWITCH[name], "bool", 0, 1, 1, int(bool(caps & _AUTO)), int(auto_on)))
            if name == "anti_flicker":
                out.append(Control(name, "menu", lo, hi, 1, default, value, tuple(range(lo, hi + 1))))
            elif name == "frame_rate_drop" or (lo, hi) == (0, 1):
                out.append(Control(name, "bool", 0, 1, 1, default, value))
            else:
                out.append(Control(name, "int", lo, hi, max(1, step), default, value, (), False, unit))
        if not answered:
            self.close()  # unplugged since it was opened: open it afresh next time
        return out

    def set(self, name: str, value: int) -> bool:
        manual = _SWITCHED.get(name, name)
        entry = _BY_NAME.get(manual)
        try:
            if entry is None or not self._open() or entry[1] not in self._ifaces:
                return False
            kind, prop = entry[1], entry[2]
            if name in _SWITCHED:  # the auto switch: keep the value, change the mode
                value, mode = self._get(kind, prop)[0], _AUTO if value else _MANUAL
            else:
                mode = _MANUAL
            _call(self._ifaces[kind], _SET, prop, int(value), mode, argtypes=(c_long, c_long, c_long))
            return True
        except OSError:
            logger.debug("The webcam refused %s = %s", name, value, exc_info=True)
            self.close()  # unplugged or taken away: open it afresh next time
            return False
