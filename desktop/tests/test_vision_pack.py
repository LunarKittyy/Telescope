import io
import hashlib
import json
import runpy
import subprocess
import sys
import zipfile
from pathlib import Path

import pytest

from telescope import vision_pack
from telescope.vision_pack import PackError


def _zip(files: dict) -> bytes:
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, "w") as z:
        for name, data in files.items():
            z.writestr(name, data)
    return buf.getvalue()


def _tree_of(files: dict, tmp_path) -> str:
    folder = tmp_path / "expected"
    for name, data in files.items():
        (folder / name).parent.mkdir(parents=True, exist_ok=True)
        (folder / name).write_bytes(data)
    return vision_pack.tree_hash(folder)


_FILES = {"site/mediapipe/__init__.py": b"", "models/a.task": b"model", "NOTICES.txt": b"licences"}


def test_pack_hash_uses_case_sensitive_path_order_on_every_platform(tmp_path):
    files = {"site/pkg/METADATA": b"upper", "site/pkg/entry.py": b"lower",
             "site/pkg-info/RECORD": b"sibling"}
    manifest = "".join(f"{name}\0{hashlib.sha256(files[name]).hexdigest()}\n"
                       for name in sorted(files, key=lambda name: tuple(name.split("/"))))
    assert _tree_of(files, tmp_path) == hashlib.sha256(manifest.encode()).hexdigest()


@pytest.fixture
def pack(tmp_path, monkeypatch):
    """A pack in vision_pack.json for this machine, and an opener that serves the given bytes."""
    tree = _tree_of(_FILES, tmp_path)
    data = _zip(_FILES)
    monkeypatch.setattr(vision_pack, "platform_key", lambda: "linux")
    info = tmp_path / "vision_pack.json"
    info.write_text(json.dumps({"linux": {"id": tree, "size": len(data), "files": 3}}))
    monkeypatch.setattr(vision_pack, "PACK_INFO", info)
    served = {"data": data}
    opener = lambda _url, _timeout: io.BytesIO(served["data"])  # noqa: E731
    return tmp_path / "vision", tree, served, opener


def test_a_good_download_is_unpacked_under_its_id_and_older_packs_go(pack):
    folder, tree, _served, opener = pack
    (folder / "0123456789abcdef").mkdir(parents=True)  # an older pack
    (folder / ".old.zip.part").write_bytes(b"x")  # a download that didn't finish
    seen = []
    final = vision_pack.download(folder, progress=lambda done, size: seen.append(done), opener=opener)
    assert final == folder / tree[:16]
    assert (final / "models" / "a.task").read_bytes() == b"model"
    assert vision_pack.installed(folder)
    assert sorted(p.name for p in folder.iterdir()) == [tree[:16]]
    assert seen and seen[-1] == len(_zip(_FILES))


def test_a_download_with_other_files_is_refused_and_leaves_nothing(pack):
    folder, _tree, served, opener = pack
    served["data"] = _zip({**_FILES, "models/a.task": b"something else"})
    with pytest.raises(PackError, match="didn't match"):
        vision_pack.download(folder, opener=opener)
    assert not vision_pack.installed(folder)
    assert list(folder.iterdir()) == []


def test_a_download_that_escapes_its_folder_is_refused(pack):
    folder, _tree, served, opener = pack
    served["data"] = _zip({"../evil.py": b"x"})
    with pytest.raises(PackError, match="unexpected paths"):
        vision_pack.download(folder, opener=opener)
    assert not (folder.parent / "evil.py").exists()


def test_a_download_much_bigger_than_expected_is_stopped(pack):
    folder, _tree, served, opener = pack
    served["data"] = _zip(_FILES) + b"\0" * 10_000
    with pytest.raises(PackError, match="bigger"):
        vision_pack.download(folder, opener=opener)


def test_a_dropped_connection_says_to_check_the_internet(pack):
    folder, _tree, _served, _opener = pack

    def offline(_url, _timeout):
        raise OSError("no route")
    with pytest.raises(PackError, match="internet connection"):
        vision_pack.download(folder, opener=offline)


def test_a_computer_without_a_pack_gets_no_download(pack, monkeypatch):
    folder, _tree, _served, opener = pack
    monkeypatch.setattr(vision_pack, "platform_key", lambda: None)
    assert vision_pack.asset_name() is None
    with pytest.raises(PackError, match="no download for this computer"):
        vision_pack.download(folder, opener=opener)


def test_the_asset_is_named_by_platform_and_pack_id(pack):
    _folder, tree, _served, _opener = pack
    assert vision_pack.asset_name() == f"Telescope-vision-linux-{tree[:16]}.zip"


def test_the_committed_packs_are_for_both_platforms():
    info = json.loads(vision_pack.PACK_INFO.read_text())
    assert set(info) == {"linux", "windows"}
    assert all(len(entry["id"]) == 64 and entry["size"] > 1_000_000 for entry in info.values())


def _finish(qapp, dl):
    import time
    end = time.monotonic() + 2
    while dl.running() or dl.waiting:
        assert time.monotonic() < end, "timed out"
        qapp.processEvents()
        time.sleep(0.005)


def test_the_plugin_waiting_for_the_download_is_switched_on_with_no_dialog_open(qapp, config_home, monkeypatch):
    from telescope import plugin_loader
    from telescope.widgets.vision_download import VisionDownload
    monkeypatch.setattr(vision_pack, "download", lambda *_a, **_kw: None)
    dl = VisionDownload()
    dl.start(switch_on="framing")
    _finish(qapp, dl)
    assert plugin_loader.optional_on() == {"framing"} and dl.error == ""


def test_a_failed_download_remembers_why_for_the_plugin_that_waited(qapp, config_home, monkeypatch):
    from telescope import plugin_loader
    from telescope.widgets.vision_download import VisionDownload

    def fail(*_a, **_kw):
        raise PackError("The download failed. Check the internet connection and try again.")
    monkeypatch.setattr(vision_pack, "download", fail)
    dl = VisionDownload()
    dl.start(switch_on="framing")
    _finish(qapp, dl)
    assert plugin_loader.optional_on() == set()
    assert dl.failed_for == "framing" and "internet connection" in dl.error


def test_the_pack_script_runs_without_opencv_or_the_apps_other_packages():
    # The release's vision job builds the pack with a bare Python; -S leaves out every installed package
    desktop = Path(__file__).resolve().parent.parent
    code = ("import runpy, sys; sys.path.insert(0, sys.argv[1]); "
            "runpy.run_path(sys.argv[2]); print('cv2' in sys.modules)")
    done = subprocess.run([sys.executable, "-S", "-c", code, str(desktop), str(desktop / "scripts" / "build_vision_pack.py")],
                          capture_output=True, text=True, timeout=30)
    assert done.returncode == 0, done.stderr
    assert done.stdout.strip() == "False"


def test_pack_notices_have_the_same_bytes_on_every_build_platform(tmp_path, monkeypatch):
    script = Path(__file__).resolve().parents[1] / "scripts" / "build_vision_pack.py"
    build = runpy.run_path(str(script))["build"]
    monkeypatch.setattr(subprocess, "run", lambda *_a, **_kw: None)
    monkeypatch.setitem(build.__globals__, "pins", lambda: [])
    monkeypatch.setitem(build.__globals__, "fetch_models", lambda *_a: None)
    folder = build("windows", tmp_path / "work", tmp_path / "cache")
    assert (folder / "NOTICES.txt").read_bytes() == build.__globals__["NOTICES"].encode("utf-8")
