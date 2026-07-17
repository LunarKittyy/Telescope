import pytest

from telescope.models import CameraCapabilities, DeviceProfile, PhoneState, PhoneStateError

_VALID_CAMERA = {
    "id": "0", "logicalId": None, "label": "Back", "current": True,
    "hasOis": True, "isoMin": 50, "isoMax": 3200,
    "shutterMinNs": 100_000, "shutterMaxNs": 1_000_000_000,
    "supportsManualSensor": True, "supportsManualWB": True,
    "supportsManualFocus": False, "minFocusDistance": 0.0,
    "aeCompMin": -8, "aeCompMax": 8, "aeCompStep": 0.167,
    "supportsFlash": True, "hwLevel": "FULL",
}

_VALID_STATE = {
    "cameras": [_VALID_CAMERA], "auto": True, "wb_manual": False, "ois": True,
    "focus_mode": "continuous", "focus_distance": 0.0, "ae_comp": 0,
    "nr_mode": 1, "edge_mode": 1, "black_level_lock": False, "torch": False,
    "battery": 80, "charging": False, "battery_temp_c": 25.0,
}


def test_camera_capabilities_from_dict_round_trips_all_fields():
    cam = CameraCapabilities.from_dict(_VALID_CAMERA)
    assert cam.id == "0"
    assert cam.logical_id is None
    assert cam.has_ois is True
    assert cam.iso_min == 50
    assert cam.hw_level == "FULL"


def test_camera_capabilities_rejects_missing_field():
    bad = dict(_VALID_CAMERA)
    del bad["isoMin"]
    with pytest.raises(PhoneStateError):
        CameraCapabilities.from_dict(bad)


def test_camera_capabilities_rejects_wrong_type():
    bad = dict(_VALID_CAMERA)
    bad["isoMin"] = "50"  # must be an int, not a string
    with pytest.raises(PhoneStateError):
        CameraCapabilities.from_dict(bad)


def test_camera_capabilities_rejects_bool_where_int_expected():
    bad = dict(_VALID_CAMERA)
    bad["isoMin"] = True  # bool is technically an int subclass - must still be rejected
    with pytest.raises(PhoneStateError):
        CameraCapabilities.from_dict(bad)


def test_phone_state_empty_dict_is_not_an_error():
    state = PhoneState.from_dict({})
    assert state.is_empty is True
    assert state == PhoneState.empty()


def test_phone_state_decodes_full_valid_payload():
    state = PhoneState.from_dict(_VALID_STATE)
    assert state.is_empty is False
    assert len(state.cameras) == 1
    assert state.cameras[0].id == "0"
    assert state.battery == 80
    assert state.raw == _VALID_STATE


@pytest.mark.parametrize("missing_key", ["auto", "wb_manual", "battery", "focus_mode"])
def test_phone_state_rejects_missing_required_field(missing_key):
    bad = dict(_VALID_STATE)
    del bad[missing_key]
    with pytest.raises(PhoneStateError):
        PhoneState.from_dict(bad)


def test_phone_state_rejects_non_list_cameras():
    bad = {**_VALID_STATE, "cameras": "not-a-list"}
    with pytest.raises(PhoneStateError):
        PhoneState.from_dict(bad)


def test_phone_state_rejects_malformed_camera_entry():
    bad = {**_VALID_STATE, "cameras": [{"id": "0"}]}
    with pytest.raises(PhoneStateError):
        PhoneState.from_dict(bad)


def test_phone_state_optional_iso_and_shutter_default_to_none():
    state = PhoneState.from_dict(_VALID_STATE)
    assert state.iso is None
    assert state.shutter_ns is None

    with_manual = {**_VALID_STATE, "iso": 400, "shutter_ns": 8_000_000}
    state2 = PhoneState.from_dict(with_manual)
    assert state2.iso == 400
    assert state2.shutter_ns == 8_000_000


def test_device_profile_round_trips_through_dict():
    profile = DeviceProfile(name="Phone", ips=("10.0.0.1", "100.64.0.1"), token="tok-123")
    d = profile.to_dict()
    assert d == {"name": "Phone", "ips": ["10.0.0.1", "100.64.0.1"], "token": "tok-123"}
    assert DeviceProfile.from_dict(d) == profile


def test_device_profile_token_is_optional_and_omitted_from_dict():
    profile = DeviceProfile.from_dict({"name": "Phone", "ips": ["10.0.0.1"]})
    assert profile.token is None
    assert "token" not in profile.to_dict()


@pytest.mark.parametrize("raw,reason", [
    ({}, "missing name"),
    ({"name": ""}, "empty name"),
    ({"name": "Phone", "ips": "not-a-list"}, "ips not a list"),
    ({"name": "Phone", "ips": [1, 2]}, "ips not strings"),
    ({"name": "Phone", "ips": ["1.2.3.4"], "token": 42}, "token not a string"),
    ("not-a-dict", "not a dict"),
])
def test_device_profile_rejects_malformed_entries(raw, reason):
    with pytest.raises(ValueError):
        DeviceProfile.from_dict(raw)
