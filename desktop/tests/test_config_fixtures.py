"""The app still does with each saved settings file what tests/fixtures/configs says (scripts/config_fixtures.py)."""

import json
import sys

import pytest

from scripts import config_fixtures

pytestmark = pytest.mark.skipif(sys.platform == "win32", reason="the expected files are recorded on Linux")


@pytest.mark.parametrize("folder", config_fixtures.fixtures(), ids=lambda f: f.name)
def test_the_app_ends_up_where_the_fixture_says(qapp, folder):
    expected = json.loads((folder / config_fixtures.EXPECTED_NAME).read_text(encoding="utf-8"))
    # When a change here is on purpose, `python scripts/config_fixtures.py` records the new result
    assert config_fixtures.snapshot(folder) == expected


def test_there_is_a_fixture_for_each_kind_of_file():
    names = {f.name for f in config_fixtures.fixtures()}
    assert {"fresh-install", "cut-off", "version-1", "version-2", "future-version", "damaged-sections"} <= names
