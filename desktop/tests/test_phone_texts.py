"""Texts the phone words in English next to a code: the desktop words them itself, and still shows the English from a
phone that sends no code or one it doesn't know (docs/protocol.md, "Texts the desktop words itself")."""

import io
import json
import urllib.error

import pytest

from telescope import i18n
from telescope.audio import _phone_reason
from telescope.plugins.stream_output import codec_error_text
from telescope.sources import camera_error_text
from telescope.widgets.lens_panel import lens_label, lens_name, split_lens_label


@pytest.fixture
def swedish():
    i18n.load("sv")
    yield
    i18n.load("en")


def test_lens_label_from_fields_matches_the_one_cut_from_the_english_label():
    new = {"facing": "back", "focalMm": 24, "hasOis": True, "lensKind": "auto", "label": "Back ~24mm OIS [auto]"}
    assert lens_label(new) == "Auto ~24mm OIS"
    assert lens_label({**new, "label": "something else"}) == "Auto ~24mm OIS"
    assert lens_label({"label": "Back ~24mm OIS [auto]"}) == "Auto ~24mm OIS"
    assert lens_label({"facing": "front", "focalMm": 25, "hasOis": False, "label": "Front ~25mm"}) == "F/~25mm"
    assert lens_label({"facing": "external", "focalMm": 0, "hasOis": False}) == "Ext ?"
    assert lens_label({"facing": "back", "focalMm": 13, "hasOis": False, "lensKind": "physical"}) == "~13mm"


def test_lens_names_are_worded_in_the_language_but_keep_their_english_key(swedish):
    assert lens_label({"facing": "front", "focalMm": 25}) == "F/~25mm"
    assert split_lens_label("F/~25mm") == ("25", "mm", "Fram")
    assert split_lens_label("Auto ~24mm OIS") == ("24", "mm", "Auto · OIS")
    assert lens_name("F/~25mm") == "Fram ~25mm"


def test_codec_error_is_worded_from_the_code_and_size(swedish):
    state = {"codec_error": "English", "codec_error_code": "camera_cant_feed",
             "codec_error_width": 3840, "codec_error_height": 2160}
    assert codec_error_text(state) == "Kameran kan inte mata H.264 vid 3840x2160 på den här telefonen"
    assert codec_error_text({"codec_error": "The phone's H.264 encoder stopped",
                             "codec_error_code": "encoder_stopped"}) == "Telefonens H.264-kodare slutade fungera"


def test_codec_error_falls_back_to_the_english_text(swedish):
    assert codec_error_text({"codec_error": "Old phone text"}) == "Old phone text"
    assert codec_error_text({"codec_error": "New code", "codec_error_code": "from_the_future"}) == "New code"
    assert codec_error_text({"codec_error": "No size", "codec_error_code": "camera_stopped"}) == "No size"


def test_camera_error_is_worded_from_the_code(swedish):
    assert camera_error_text({"camera_error": "English", "camera_error_code": "camera_in_use"}).startswith("Kameran slogs")
    assert camera_error_text({"camera_error": "English", "camera_error_code": "x"}) == "English"
    assert camera_error_text({"camera_error": "English"}) == "English"


def _refusal(body: dict):
    return urllib.error.HTTPError("http://p/v1/audio", 403, "Forbidden", {}, io.BytesIO(json.dumps(body).encode()))


def test_mic_refusal_is_worded_from_the_code(swedish):
    assert _phone_reason(_refusal({"error": "English", "code": "mic_in_use"})) == "Telefonens mikrofon används"
    assert _phone_reason(_refusal({"error": "Only English"})) == "Only English"
    assert _phone_reason(_refusal({"error": "Unknown", "code": ["not", "a", "string"]})) == "Unknown"
