"""Typed contracts for desktop boundaries: validated phone state and device dicts."""

from dataclasses import dataclass, field
from typing import Optional


class PhoneStateError(ValueError):
    """Raised on /v1/state payload shape mismatch."""


def _require(raw: dict, key: str, types: tuple, what: str):
    """Type-check raw[key]; bool is distinct from int (prevent True/False as int)."""
    if key not in raw:
        raise PhoneStateError(f"{what}: missing '{key}'")
    value = raw[key]
    is_bool = isinstance(value, bool)
    if is_bool and bool not in types:
        raise PhoneStateError(f"{what}: '{key}' has wrong type (bool)")
    if not is_bool and not isinstance(value, types):
        raise PhoneStateError(f"{what}: '{key}' has wrong type ({type(value).__name__})")
    return value


def _require_str(raw: dict, key: str, what: str) -> str:
    return _require(raw, key, (str,), what)


def _require_bool(raw: dict, key: str, what: str) -> bool:
    return _require(raw, key, (bool,), what)


def _require_int(raw: dict, key: str, what: str) -> int:
    return _require(raw, key, (int,), what)


def _require_number(raw: dict, key: str, what: str) -> float:
    value = _require(raw, key, (int, float), what)
    return float(value)


def _optional_number(raw: dict, key: str) -> Optional[float]:
    value = raw.get(key)
    if value is None:
        return None
    if not isinstance(value, (int, float)) or isinstance(value, bool):
        raise PhoneStateError(f"'{key}' has wrong type ({type(value).__name__})")
    return float(value)


def _optional_int(raw: dict, key: str) -> Optional[int]:
    value = raw.get(key)
    if value is None:
        return None
    if not isinstance(value, int) or isinstance(value, bool):
        raise PhoneStateError(f"'{key}' has wrong type ({type(value).__name__})")
    return value


@dataclass(frozen=True)
class CameraCapabilities:
    id: str
    logical_id: Optional[str]
    label: str
    current: bool
    has_ois: bool
    iso_min: int
    iso_max: int
    shutter_min_ns: int
    shutter_max_ns: int
    supports_manual_sensor: bool
    supports_manual_wb: bool
    supports_manual_focus: bool
    min_focus_distance: float
    ae_comp_min: int
    ae_comp_max: int
    ae_comp_step: float
    supports_flash: bool
    hw_level: str

    @classmethod
    def from_dict(cls, raw: dict) -> "CameraCapabilities":
        if not isinstance(raw, dict):
            raise PhoneStateError(f"camera entry is not an object ({type(raw).__name__})")
        w = "camera entry"
        logical_id = raw.get("logicalId")
        if logical_id is not None and not isinstance(logical_id, str):
            raise PhoneStateError(f"{w}: 'logicalId' has wrong type")
        return cls(
            id=_require_str(raw, "id", w),
            logical_id=logical_id,
            label=_require_str(raw, "label", w),
            current=_require_bool(raw, "current", w),
            has_ois=_require_bool(raw, "hasOis", w),
            iso_min=_require_int(raw, "isoMin", w),
            iso_max=_require_int(raw, "isoMax", w),
            shutter_min_ns=_require_int(raw, "shutterMinNs", w),
            shutter_max_ns=_require_int(raw, "shutterMaxNs", w),
            supports_manual_sensor=_require_bool(raw, "supportsManualSensor", w),
            supports_manual_wb=_require_bool(raw, "supportsManualWB", w),
            supports_manual_focus=_require_bool(raw, "supportsManualFocus", w),
            min_focus_distance=_require_number(raw, "minFocusDistance", w),
            ae_comp_min=_require_int(raw, "aeCompMin", w),
            ae_comp_max=_require_int(raw, "aeCompMax", w),
            ae_comp_step=_require_number(raw, "aeCompStep", w),
            supports_flash=_require_bool(raw, "supportsFlash", w),
            hw_level=_require_str(raw, "hwLevel", w),
        )


@dataclass(frozen=True)
class PhoneState:
    """Decoded /v1/state response; raw dict kept for backward-compat with dict-consuming plugins."""

    cameras: tuple = field(default_factory=tuple)
    auto: bool = True
    iso: Optional[int] = None
    shutter_ns: Optional[int] = None
    wb_manual: bool = False
    ois: bool = True
    focus_mode: str = "continuous"
    focus_distance: float = 0.0
    ae_comp: int = 0
    nr_mode: int = 1
    edge_mode: int = 1
    black_level_lock: bool = False
    torch: bool = False
    battery: Optional[int] = None
    charging: Optional[bool] = None
    battery_temp_c: Optional[float] = None
    raw: dict = field(default_factory=dict)

    @property
    def is_empty(self) -> bool:
        return not self.raw

    @classmethod
    def empty(cls) -> "PhoneState":
        return cls()

    @classmethod
    def from_dict(cls, raw: dict) -> "PhoneState":
        """Decode /v1/state response; empty dict means no data (not error); shape mismatches raise PhoneStateError."""
        if not raw:
            return cls.empty()
        if not isinstance(raw, dict):
            raise PhoneStateError(f"state is not an object ({type(raw).__name__})")
        w = "state"
        cams_raw = raw.get("cameras", [])
        if not isinstance(cams_raw, list):
            raise PhoneStateError(f"{w}: 'cameras' has wrong type ({type(cams_raw).__name__})")
        cameras = tuple(CameraCapabilities.from_dict(c) for c in cams_raw)
        return cls(
            cameras=cameras,
            auto=_require_bool(raw, "auto", w),
            iso=_optional_int(raw, "iso"),
            shutter_ns=_optional_int(raw, "shutter_ns"),
            wb_manual=_require_bool(raw, "wb_manual", w),
            ois=_require_bool(raw, "ois", w),
            focus_mode=_require_str(raw, "focus_mode", w),
            focus_distance=_require_number(raw, "focus_distance", w),
            ae_comp=_require_int(raw, "ae_comp", w),
            nr_mode=_require_int(raw, "nr_mode", w),
            edge_mode=_require_int(raw, "edge_mode", w),
            black_level_lock=_require_bool(raw, "black_level_lock", w),
            torch=_require_bool(raw, "torch", w),
            battery=_require_int(raw, "battery", w),
            charging=_require_bool(raw, "charging", w),
            battery_temp_c=_require_number(raw, "battery_temp_c", w),
            raw=raw,
        )


@dataclass(frozen=True)
class StreamSettings:
    resolution: str = "Pass-through"
    fps: int = 30
    jpeg_quality: int = 85
    phone_fps: int = 30


@dataclass(frozen=True)
class DeviceProfile:
    """Paired phone: name, known IPs, and QR-pairing bearer token (None if manually added only)."""

    name: str
    ips: tuple = field(default_factory=tuple)
    token: Optional[str] = None

    @classmethod
    def from_dict(cls, raw: dict) -> "DeviceProfile":
        if not isinstance(raw, dict):
            raise ValueError(f"device entry is not an object ({type(raw).__name__})")
        name = raw.get("name")
        if not isinstance(name, str) or not name.strip():
            raise ValueError("device entry: missing or empty 'name'")
        ips_raw = raw.get("ips", [])
        if not isinstance(ips_raw, list) or not all(isinstance(ip, str) for ip in ips_raw):
            raise ValueError("device entry: 'ips' must be a list of strings")
        token = raw.get("token")
        if token is not None and not isinstance(token, str):
            raise ValueError("device entry: 'token' must be a string")
        return cls(name=name, ips=tuple(ips_raw), token=token)

    def to_dict(self) -> dict:
        d = {"name": self.name, "ips": list(self.ips)}
        if self.token is not None:
            d["token"] = self.token
        return d
