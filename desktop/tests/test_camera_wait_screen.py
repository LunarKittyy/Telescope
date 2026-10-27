"""The Windows 11 camera's built-in wait screen: what write_camera_wait_screen.py writes, and wait_screen.cpp laying
it out like vcam._default_screen (built with whatever C++ compiler is here)."""

import re
import shutil
import struct
import subprocess
import sys
from pathlib import Path
from unittest import mock

import numpy as np
import pytest

from telescope import vcam

DESKTOP = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(DESKTOP / "scripts"))
import write_camera_wait_screen as writer  # noqa: E402

CAMERA = DESKTOP / "virtualcamera"
HARNESS = r"""
#include "wait_screen.h"
#include <cstdio>
#include <cstdlib>
int main(int, char** argv) {
    std::vector<uint8_t> data, out;
    FILE* f = fopen(argv[1], "rb");
    for (int c; (c = fgetc(f)) != EOF;) data.push_back(static_cast<uint8_t>(c));
    fclose(f);
    if (!telescope::ComposeWaitScreen(data.data(), data.size(), atoi(argv[3]), atoi(argv[4]), out)) return 3;
    f = fopen(argv[2], "wb");
    fwrite(out.data(), 1, out.size(), f);
    fclose(f);
}
"""


@pytest.fixture(scope="module")
def resource(qapp):
    try:
        return writer.build()
    except SystemExit as e:  # Qt's offscreen platform on Windows, which the tests run on, has no fonts
        pytest.skip(str(e))


def header_fields() -> list:
    struct_text = re.search(r"struct WaitScreenHeader \{(.*?)\};", (CAMERA / "wait_screen.h").read_text(), re.S).group(1)
    return [name.strip() for line in re.findall(r"uint32_t ([^;]+);", struct_text) for name in line.split(",")]


def test_the_header_is_the_one_the_dll_reads(resource):
    fields = header_fields()
    values = dict(zip(fields, struct.unpack_from(f"<{len(fields)}I", resource)))
    assert values["magic"] == writer.MAGIC and values["version"] == writer.VERSION
    assert f"kWaitScreenVersion = {writer.VERSION};" in (CAMERA / "wait_screen.h").read_text()
    assert (values["screen_width"], values["screen_with_name"], values["name_top"]) == (
        vcam.SCREEN_WIDTH, vcam.SCREEN_WITH_NAME, vcam.SCREEN_NAME_TOP)
    lens, mask = values["lens_size"] ** 2, values["name_width"] * values["name_height"]
    assert len(resource) == 4 * len(fields) + lens + lens // 2 + mask


def test_the_cmake_resource_id_is_the_one_the_dll_loads():
    assert "kWaitScreenResource = 101;" in (CAMERA / "wait_screen.h").read_text()
    assert '"101 RCDATA' in (CAMERA / "CMakeLists.txt").read_text()


def compiler():
    return next((c for c in ("c++", "clang++", "g++") if shutil.which(c)), None)


@pytest.mark.skipif(compiler() is None, reason="no C++ compiler")
@pytest.mark.parametrize("size", [(1280, 720), (640, 480), (1080, 1920)])
def test_the_dll_lays_it_out_like_the_app(resource, tmp_path, size):
    import cv2
    exe = tmp_path / "harness"
    (tmp_path / "harness.cpp").write_text(HARNESS)
    subprocess.run([compiler(), "-std=c++17", "-O2", f"-I{CAMERA}", str(tmp_path / "harness.cpp"),
                    str(CAMERA / "wait_screen.cpp"), "-o", str(exe)], check=True)
    (tmp_path / "screen.bin").write_bytes(resource)
    w, h = size
    subprocess.run([str(exe), str(tmp_path / "screen.bin"), str(tmp_path / "out.nv12"), str(w), str(h)], check=True)
    dll = cv2.cvtColor(np.fromfile(tmp_path / "out.nv12", np.uint8).reshape(h * 3 // 2, w), cv2.COLOR_YUV2RGB_NV12)
    with mock.patch.object(vcam, "tr", lambda _text: ""):  # the camera has no "Waiting for camera" line
        app = vcam._default_screen(w, h)
    app = cv2.cvtColor(cv2.cvtColor(app, cv2.COLOR_RGB2YUV_I420), cv2.COLOR_YUV2RGB_I420)
    diff = np.abs(dll.astype(int) - app.astype(int))
    k, (_x, cy), _with_name = vcam.screen_layout(w, h)
    name_top = int(cy + vcam.SCREEN_NAME_TOP * k)
    assert diff[:name_top].mean() < 1.0 and np.percentile(diff[:name_top], 99) < 8
    assert diff[name_top:].mean() < 5.0  # the name resized from its mask, which hints differently in some fonts


@pytest.mark.skipif(compiler() is None, reason="no C++ compiler")
def test_the_dll_turns_down_a_cut_short_resource(resource, tmp_path):
    exe = tmp_path / "harness"
    (tmp_path / "harness.cpp").write_text(HARNESS)
    subprocess.run([compiler(), "-std=c++17", f"-I{CAMERA}", str(tmp_path / "harness.cpp"),
                    str(CAMERA / "wait_screen.cpp"), "-o", str(exe)], check=True)
    (tmp_path / "screen.bin").write_bytes(resource[:-1])
    done = subprocess.run([str(exe), str(tmp_path / "screen.bin"), str(tmp_path / "out.nv12"), "640", "480"])
    assert done.returncode == 3
