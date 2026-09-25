"""Which build this is, and where its release lives."""

import importlib
import sys
import types

from telescope import version


def _reload_with(monkeypatch, build):
    if build is None:
        monkeypatch.setitem(sys.modules, "telescope._build", None)  # import fails: a source checkout
    else:
        monkeypatch.setitem(sys.modules, "telescope._build", types.SimpleNamespace(**build))
    return importlib.reload(version)


def test_a_source_checkout_is_dev_and_reads_the_version_file(monkeypatch):
    v = _reload_with(monkeypatch, None)
    assert v.CHANNEL == "dev" and v.BUILD == 0
    assert v.VERSION.count(".") == 2
    assert v.display_version() == f"{v.VERSION} dev"
    assert v.release_asset_url("Telescope.apk").endswith("/releases/download/nightly/Telescope.apk")


def test_a_stamped_build_reports_itself(monkeypatch):
    v = _reload_with(monkeypatch, {"VERSION": "1.2.3", "BUILD": 99, "CHANNEL": "stable", "COMMIT": "abc"})
    assert v.display_version() == "1.2.3"
    assert v.release_asset_url("Telescope.apk").endswith("/releases/download/v1.2.3/Telescope.apk")
    v = _reload_with(monkeypatch, {"VERSION": "1.2.3", "BUILD": 99, "CHANNEL": "nightly", "COMMIT": "abc"})
    assert v.display_version() == "1.2.3 nightly 99"


def teardown_module():
    sys.modules.pop("telescope._build", None)
    importlib.reload(version)
