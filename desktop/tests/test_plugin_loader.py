"""Installing, switching on and loading plugins that don't come with Telescope."""

import json
import os
import shutil
import sys
import zipfile
from pathlib import Path

import numpy as np
import pytest

from telescope import plugin_loader

EXAMPLE = Path(__file__).resolve().parent.parent / "examples" / "plugins" / "black_and_white"


@pytest.fixture
def plugins(config_home, tmp_path, monkeypatch):
    folder = tmp_path / "plugins"
    folder.mkdir(mode=0o755)
    monkeypatch.setattr(plugin_loader, "plugins_dir", lambda: folder)
    yield folder
    for name in [m for m in sys.modules if m.startswith("telescope_ext.")]:
        del sys.modules[name]


def _example(folder: Path, name="black_and_white") -> Path:
    target = folder / name
    shutil.copytree(EXAMPLE, target, ignore=shutil.ignore_patterns("__pycache__"))
    return target


def _plugin(folder: Path, folder_name: str, code: str, **manifest) -> Path:
    target = folder / folder_name
    target.mkdir()
    (target / "plugin.json").write_text(json.dumps({"name": folder_name, "api": 1, "class": "P", **manifest}))
    (target / "__init__.py").write_text(code)
    return target


_SIMPLE = "from telescope.plugin import TelescopePlugin\n\nclass P(TelescopePlugin):\n    name = {name!r}\n"


def _one(folder):
    (found,) = plugin_loader.find_installed(folder=folder)
    return found


def test_a_plugin_is_off_until_switched_on_then_loads(plugins):
    _example(plugins)
    found = _one(plugins)
    assert found.problem == "" and not found.enabled and found.title == "Black and white"
    assert plugin_loader.load_enabled() == ([], [])  # nothing runs that wasn't switched on
    plugin_loader.set_enabled(found, True)
    loaded, failed = plugin_loader.load_enabled()
    assert failed == [] and [p.name for p in loaded] == ["black_and_white"]
    plugin = loaded[0]
    assert plugin.title == "Black and white" and plugin.about.startswith("Turns the picture")
    plugin.setup(_Host(), None)
    plugin.set_config({"on": True})
    frame = np.zeros((4, 4, 3), np.uint8)
    frame[..., 2] = 200  # red
    out = plugin.process_frame(frame)
    assert out.shape == frame.shape and (out[..., 0] == out[..., 2]).all()


class _Host:
    def schedule_save(self):
        pass


def test_a_plugin_that_changed_after_it_was_switched_on_stays_off(plugins):
    folder = _example(plugins)
    plugin_loader.set_enabled(_one(plugins), True)
    (folder / "__init__.py").write_text((folder / "__init__.py").read_text() + "\n# changed\n")
    found = _one(plugins)
    assert not found.enabled and found.changed
    loaded, failed = plugin_loader.load_enabled()
    assert loaded == [] and [f.name for f in failed] == ["black_and_white"] and "changed" in failed[0].error


def test_pycache_doesnt_count_as_a_change(plugins):
    folder = _example(plugins)
    plugin_loader.set_enabled(_one(plugins), True)
    (folder / "__pycache__").mkdir()
    (folder / "__pycache__" / "x.cpython-314.pyc").write_bytes(b"\0")
    assert _one(plugins).enabled


@pytest.mark.parametrize("manifest, expect", [
    ({"name": "other"}, "folder's name"),
    ({"api": 99}, "newer Telescope"),
    ({"api": 0}, "older Telescope"),
    ({"class": "not a class"}, "plugin class"),
])
def test_a_manifest_that_doesnt_fit_says_why(plugins, manifest, expect):
    _plugin(plugins, "thing", _SIMPLE.format(name="thing"), **manifest)
    assert expect in _one(plugins).problem


def test_a_built_in_name_cant_be_taken(plugins):
    _plugin(plugins, "camera_control", _SIMPLE.format(name="camera_control"))
    (found,) = plugin_loader.find_installed(reserved={"camera_control"}, folder=plugins)
    assert "comes with Telescope" in found.problem


def test_a_folder_without_a_manifest_or_with_a_bad_name_is_listed_with_why(plugins):
    (plugins / "no_manifest").mkdir()
    (plugins / "Bad-Name").mkdir()
    problems = {p.name: p.problem for p in plugin_loader.find_installed(folder=plugins)}
    assert "plugin.json" in problems["no_manifest"] and "lowercase" in problems["Bad-Name"]


@pytest.mark.skipif(os.name != "posix", reason="links and permissions as on Linux")
def test_a_link_inside_a_plugin_is_refused(plugins, tmp_path):
    folder = _plugin(plugins, "thing", _SIMPLE.format(name="thing"))
    (tmp_path / "elsewhere.py").write_text("")
    (folder / "helper.py").symlink_to(tmp_path / "elsewhere.py")
    assert "link" in _one(plugins).problem


@pytest.mark.skipif(os.name != "posix", reason="permissions as on Linux")
def test_a_plugin_other_users_can_change_isnt_loaded(plugins):
    folder = _plugin(plugins, "thing", _SIMPLE.format(name="thing"))
    folder.chmod(0o777)
    assert "Other users" in _one(plugins).problem


@pytest.mark.skipif(os.name != "posix", reason="permissions as on Linux")
def test_group_write_is_fine_when_the_group_is_the_users_own(plugins, monkeypatch):
    folder = _plugin(plugins, "thing", _SIMPLE.format(name="thing"))
    folder.chmod(0o775)
    monkeypatch.setattr(plugin_loader, "_private_group", lambda gid: True)
    assert _one(plugins).problem == ""
    monkeypatch.setattr(plugin_loader, "_private_group", lambda gid: False)  # a group others are in
    assert "Other users" in _one(plugins).problem


def test_compiled_code_left_in_a_plugin_never_runs(plugins, tmp_path):
    import importlib.util
    import py_compile
    folder = _plugin(plugins, "thing", _SIMPLE.format(name="thing"))
    plugin_loader.set_enabled(_one(plugins), True)
    evil = tmp_path / "evil.py"
    marker = tmp_path / "ran"
    evil.write_text(f"open({str(marker)!r}, 'w').close()\n" + _SIMPLE.format(name="thing"))
    cached = folder / "__pycache__" / Path(importlib.util.cache_from_source(str(folder / "__init__.py"))).name
    cached.parent.mkdir()
    py_compile.compile(str(evil), cfile=str(cached), invalidation_mode=py_compile.PycInvalidationMode.UNCHECKED_HASH)
    assert _one(plugins).enabled  # a cache isn't a change...
    loaded, failed = plugin_loader.load_enabled()
    assert failed == [] and [p.name for p in loaded] == ["thing"]
    assert not marker.exists()  # ...because it's cleared before loading, and the source is what runs


def test_a_pyc_next_to_the_source_is_refused(plugins):
    folder = _plugin(plugins, "thing", _SIMPLE.format(name="thing"))
    (folder / "helper.pyc").write_bytes(b"\0")
    assert "compiled code" in _one(plugins).problem


def test_one_that_was_on_and_got_a_bad_file_says_why_it_didnt_start(plugins):
    folder = _plugin(plugins, "thing", _SIMPLE.format(name="thing"))
    plugin_loader.set_enabled(_one(plugins), True)
    (folder / "native.so").write_bytes(b"\0")
    loaded, failed = plugin_loader.load_enabled()
    assert loaded == [] and "compiled code" in failed[0].error  # so the window shows a banner


@pytest.mark.skipif(os.name != "posix", reason="permissions as on Linux")
def test_a_file_inside_other_users_can_change_is_caught(plugins):
    folder = _plugin(plugins, "thing", _SIMPLE.format(name="thing"))
    (folder / "sub").mkdir()
    (folder / "sub" / "x.py").write_text("")
    (folder / "sub" / "x.py").chmod(0o666)
    assert "Other users" in _one(plugins).problem


def test_a_plugin_that_exits_on_import_doesnt_end_telescope(plugins):
    _plugin(plugins, "thing", "import sys\nsys.exit(3)\n")
    plugin_loader.set_enabled(_one(plugins), True)
    loaded, failed = plugin_loader.load_enabled()
    assert loaded == [] and "SystemExit" in failed[0].error


def test_a_plugins_own_modules_dont_shadow_anything(plugins):
    folder = _plugin(plugins, "thing", "import json\nfrom telescope.plugin import TelescopePlugin\n"
                                       "from .helper import VALUE\n\nclass P(TelescopePlugin):\n"
                                       "    name = 'thing'\n    value = VALUE\n    has_dumps = hasattr(json, 'dumps')\n")
    (folder / "helper.py").write_text("VALUE = 42\n")
    (folder / "json.py").write_text("raise RuntimeError('a planted json module ran')\n")
    found = _one(plugins)
    plugin_loader.set_enabled(found, True)
    plugin = plugin_loader.load(_one(plugins))
    assert plugin.value == 42 and plugin.has_dumps
    assert str(folder) not in sys.path


def test_a_plugin_that_raises_on_import_is_reported_not_raised(plugins):
    _plugin(plugins, "thing", "raise RuntimeError('broken on purpose')\n")
    plugin_loader.set_enabled(_one(plugins), True)
    loaded, failed = plugin_loader.load_enabled()
    assert loaded == [] and "broken on purpose" in failed[0].error
    assert "telescope_ext.thing" not in sys.modules


def test_a_class_with_another_name_is_refused(plugins):
    _plugin(plugins, "thing", _SIMPLE.format(name="something_else"))
    plugin_loader.set_enabled(_one(plugins), True)
    _loaded, failed = plugin_loader.load_enabled()
    assert "has to be 'thing'" in failed[0].error


def test_built_in_plugins_switch_off_and_back_on(plugins):
    plugin_loader.set_builtin_off("presets", True)
    plugin_loader.set_builtin_off("preview", True)
    assert plugin_loader.builtin_off() == {"presets", "preview"}
    assert plugin_loader.unnoticed_off() == {"presets", "preview"}
    plugin_loader.notice_off()
    assert plugin_loader.unnoticed_off() == set()
    plugin_loader.set_builtin_off("monitoring", True)
    assert plugin_loader.unnoticed_off() == {"monitoring"}  # a new one is told about again
    plugin_loader.all_builtin_on()
    assert plugin_loader.builtin_off() == set()


def test_the_switches_survive_a_settings_save(plugins, config_home):
    plugin_loader.set_builtin_off("presets", True)
    cfg = config_home.load_config()
    cfg["selected_device"] = "x"
    config_home.save_config(cfg)
    assert plugin_loader.builtin_off() == {"presets"}


# ── Installing from a .zip ──────────────────────────────────────────────────

def _zip(path: Path, files: dict, links=()) -> Path:
    with zipfile.ZipFile(path, "w") as zf:
        for name, data in files.items():
            zf.writestr(name, data)
        for name in links:
            info = zipfile.ZipInfo(name)
            info.external_attr = (0o120777 << 16)
            zf.writestr(info, "/etc/passwd")
    return path


def _example_files(prefix="black_and_white/") -> dict:
    return {prefix + p.name: p.read_bytes() for p in EXAMPLE.iterdir() if p.is_file()}


def test_a_zip_installs_switched_off(plugins, tmp_path):
    archive = _zip(tmp_path / "bw.zip", _example_files())
    found = plugin_loader.install_zip(archive, folder=plugins)
    assert found.problem == "" and not found.enabled and (plugins / "black_and_white" / "plugin.json").is_file()
    assert [p.name for p in plugins.iterdir()] == ["black_and_white"]  # nothing left from unpacking
    if os.name == "posix":
        assert all(p.stat().st_mode & 0o022 == 0 for p in (plugins / "black_and_white").rglob("*"))


@pytest.mark.parametrize("files, links, expect", [
    ({"black_and_white/../../evil.py": b""}, (), "outside"),
    ({"/abs/evil.py": b""}, (), "outside"),
    ({"black_and_white\\..\\evil.py": b""}, (), "outside"),
    ({"a/x.py": b"", "b/y.py": b""}, (), "one folder"),
    ({"Black And White/x.py": b""}, (), "lowercase"),
    ({"black_and_white/x.py": b""}, ("black_and_white/link",), "link"),
])
def test_a_zip_that_could_write_elsewhere_is_refused(plugins, tmp_path, files, links, expect):
    archive = _zip(tmp_path / "bad.zip", files, links)
    with pytest.raises(ValueError, match=expect):
        plugin_loader.install_zip(archive, folder=plugins)
    assert list(plugins.iterdir()) == [] and not (tmp_path / "evil.py").exists()


def test_a_zip_too_big_is_refused(plugins, tmp_path, monkeypatch):
    monkeypatch.setattr(plugin_loader, "_MAX_BYTES", 10)
    with pytest.raises(ValueError, match="too big"):
        plugin_loader.install_zip(_zip(tmp_path / "big.zip", _example_files()), folder=plugins)


def test_a_zip_thats_not_a_zip_is_refused(plugins, tmp_path):
    (tmp_path / "x.zip").write_bytes(b"nope")
    with pytest.raises(ValueError, match="isn't a .zip"):
        plugin_loader.install_zip(tmp_path / "x.zip", folder=plugins)


def test_installing_over_one_already_there_is_refused(plugins, tmp_path):
    _example(plugins)
    with pytest.raises(FileExistsError):
        plugin_loader.install_zip(_zip(tmp_path / "bw.zip", _example_files()), folder=plugins)


def test_a_built_in_name_cant_be_installed(plugins, tmp_path):
    with pytest.raises(ValueError, match="comes with Telescope"):
        plugin_loader.install_zip(_zip(tmp_path / "p.zip", _example_files("presets/")), reserved={"presets"},
                                  folder=plugins)


def test_removing_deletes_the_folder_and_its_switch(plugins):
    _example(plugins)
    plugin_loader.set_enabled(_one(plugins), True)
    plugin_loader.remove(_one(plugins), folder=plugins)
    assert list(plugins.iterdir()) == [] and plugin_loader.load_enabled() == ([], [])


def test_removing_refuses_a_folder_outside_the_plugins_folder(plugins, tmp_path):
    outside = tmp_path / "keep_me"
    outside.mkdir()
    fake = plugin_loader.Installed(folder=outside, name="keep_me")
    with pytest.raises(ValueError):
        plugin_loader.remove(fake, folder=plugins)
    assert outside.is_dir()
