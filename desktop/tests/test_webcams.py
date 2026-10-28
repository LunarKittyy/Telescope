"""Finding webcams, picking how to capture from them and reading their frames (telescope/webcams.py)."""

import os
import sys
from types import SimpleNamespace

import cv2
import numpy as np
import pytest

from telescope import webcams
from telescope.webcams import Choice, Device, LinuxScanner, Mode, WebcamReader, choose

linux_only = pytest.mark.skipif(sys.platform == "win32", reason="sysfs, /dev and /proc are Linux's")


# ── Choosing a mode ──────────────────────────────────────────────────────────

# A typical USB 2 webcam: raw YUYV is slow at the big sizes, MJPEG does 30 everywhere
C920 = (
    Mode("YUYV", 640, 480, (5.0, 10.0, 15.0, 30.0)),
    Mode("YUYV", 1280, 720, (5.0, 10.0)),
    Mode("YUYV", 1920, 1080, (5.0,)),
    Mode("MJPG", 640, 480, (15.0, 30.0)),
    Mode("MJPG", 1280, 720, (15.0, 30.0, 60.0)),
    Mode("MJPG", 1920, 1080, (15.0, 30.0)),
    Mode("MJPG", 3840, 2160, (15.0, 30.0)),
)


def test_raw_is_used_where_it_keeps_the_frame_rate():
    assert choose(C920, (640, 480), 30) == Choice("YUYV", 640, 480, 30.0)


def test_mjpeg_is_used_where_raw_cant_keep_up():
    assert choose(C920, (1920, 1080), 30) == Choice("MJPG", 1920, 1080, 30.0)
    assert choose(C920, (1280, 720), 60) == Choice("MJPG", 1280, 720, 60.0)


def test_auto_takes_the_largest_size_up_to_1080p_that_keeps_the_rate():
    assert choose(C920, None, 30) == Choice("MJPG", 1920, 1080, 30.0)
    raw_only = [Mode("YUYV", 640, 480, (30.0,)), Mode("YUYV", 1920, 1080, (5.0,))]
    assert choose(raw_only, None, 30) == Choice("YUYV", 640, 480, 30.0)


def test_auto_falls_back_to_the_fastest_size_when_none_keeps_the_rate():
    slow = [Mode("YUYV", 640, 480, (15.0,)), Mode("YUYV", 1280, 720, (10.0,))]
    assert choose(slow, None, 30) == Choice("YUYV", 640, 480, 15.0)


def test_a_size_the_camera_lacks_takes_the_next_bigger_one():
    assert choose(C920, (1024, 576), 30).width == 1280
    assert choose(C920, (7680, 4320), 30).width == 3840  # nothing bigger: the biggest


def test_rates_close_to_the_wanted_one_count_and_the_next_faster_is_asked_for():
    ntsc = [Mode("YUYV", 1280, 720, (29.97, 59.94))]
    assert choose(ntsc, (1280, 720), 30).fps == 29.97
    assert choose(ntsc, (1280, 720), 50).fps == 59.94
    assert choose(ntsc, (1280, 720), 120).fps == 59.94  # can't: the fastest there is


def test_formats_opencv_cant_read_are_ignored():
    assert choose([Mode("H264", 1920, 1080, (30.0,))], None, 30) is None


def test_sizes_and_rates_for_the_card():
    assert webcams.sizes(C920)[:2] == [(3840, 2160), (1920, 1080)]
    assert webcams.rates(C920, (1280, 720)) == [60, 30, 15, 10, 5]
    assert webcams.rates(C920, None)[0] == 60


def test_a_choice_describes_itself():
    assert Choice("MJPG", 1920, 1080, 30.0).describe() == "1920×1080, MJPEG, 30 fps"
    assert Choice("RAW", 1280, 720, 29.97).describe() == "1280×720, 29.97 fps"


# ── Which devices are listed ─────────────────────────────────────────────────

def test_telescopes_own_and_loopback_cameras_are_virtual(monkeypatch):
    monkeypatch.setattr(webcams.vcam, "IS_LINUX", True)
    assert webcams.is_virtual("Phone Camera")
    assert webcams.is_virtual("Phone Camera 3")
    assert webcams.is_virtual("Anything", driver="v4l2 loopback")
    assert not webcams.is_virtual("HD Pro Webcam C920", driver="uvcvideo")


@pytest.mark.parametrize("linux", [True, False])
def test_virtual_cameras_are_told_apart_by_name(monkeypatch, linux):
    monkeypatch.setattr(webcams.vcam, "IS_LINUX", linux)
    names = ("OBS Virtual Camera", "XSplit VCam") + (() if linux else ("Telescope", "Telescope #2", "Unity Video Capture"))
    for name in names:
        assert webcams.is_virtual(name), name
    assert not webcams.is_virtual("Integrated Camera")
    assert not webcams.is_virtual("Cam Link 4K")


def test_a_card_name_with_its_cut_off_repeat_is_tidied():
    assert webcams.tidy_name("HP 320 FHD Webcam: HP 320 FHD W") == "HP 320 FHD Webcam"
    assert webcams.tidy_name("Integrated Camera: Integrated C") == "Integrated Camera"
    assert webcams.tidy_name("HD Pro Webcam C920") == "HD Pro Webcam C920"
    assert webcams.tidy_name("Cam Link 4K: Video") == "Cam Link 4K: Video"


def test_two_cameras_of_one_model_are_numbered():
    devices = [Device("a", "C270", "/dev/video0"), Device("b", "C270", "/dev/video2"), Device("c", "Brio", "/dev/video4")]
    assert [d.name for d in webcams.tell_apart(devices)] == ["C270", "C270 (2)", "Brio"]


def test_v4l2_structs_match_the_kernel():
    assert (webcams._CAP.size, webcams._FMTDESC.size, webcams._FRMSIZE.size, webcams._FRMIVAL.size) == (104, 64, 44, 52)
    assert webcams.VIDIOC_QUERYCAP == 0x80685600
    assert webcams.VIDIOC_ENUM_FMT == 0xC0405602
    assert webcams.VIDIOC_ENUM_FRAMESIZES == 0xC02C564A
    assert webcams.VIDIOC_ENUM_FRAMEINTERVALS == 0xC034564B


def _code(fourcc: str) -> int:
    return int.from_bytes(fourcc.encode(), "little")


def test_modes_are_read_from_the_driver(monkeypatch):
    """A camera with discrete MJPEG sizes, a YUYV size range and an H.264 format that's skipped."""
    formats = ["MJPG", "H264", "YUYV"]
    frame_sizes = {"MJPG": [(1, 1280, 720), (1, 1920, 1080)], "YUYV": [(3, 320, 1920, 240, 1080)]}
    rates = {("MJPG", 1280, 720): [(1, 1, 30), (1, 1, 60)], ("MJPG", 1920, 1080): [(1, 1001, 30000)]}

    def ioctl(_fd, request, packed):
        if request == webcams.VIDIOC_ENUM_FMT:
            index = webcams._FMTDESC.unpack(packed)[0]
            if index >= len(formats):
                return None
            return webcams._FMTDESC.pack(index, 1, 0, b"", _code(formats[index]), 0)
        if request == webcams.VIDIOC_ENUM_FRAMESIZES:
            index, code = webcams._FRMSIZE.unpack(packed)[:2]
            fourcc = code.to_bytes(4, "little").decode()
            got = frame_sizes.get(fourcc, [])
            if index >= len(got):
                return None
            kind, *dims = got[index]
            if kind == 1:
                return webcams._FRMSIZE.pack(index, code, 1, dims[0], dims[1], 0, 0, 0, 0)
            return webcams._FRMSIZE.pack(index, code, 3, dims[0], dims[1], 8, dims[2], dims[3], 8)
        if request == webcams.VIDIOC_ENUM_FRAMEINTERVALS:
            index, code, w, h = webcams._FRMIVAL.unpack(packed)[:4]
            fourcc = code.to_bytes(4, "little").decode()
            got = rates.get((fourcc, w, h))
            if got is None:  # a range from 1/30 s to 1 s
                if index:
                    return None
                return webcams._FRMIVAL.pack(index, code, w, h, 3, 1, 30, 1, 1, 1, 30)
            if index >= len(got):
                return None
            return webcams._FRMIVAL.pack(index, code, w, h, got[index][0], got[index][1], got[index][2], 0, 0, 0, 0)
        return None

    monkeypatch.setattr(webcams, "_ioctl", ioctl)
    modes = webcams.list_modes(3)
    assert Mode("MJPG", 1280, 720, (30.0, 60.0)) in modes
    assert Mode("MJPG", 1920, 1080, (29.97,)) in modes
    assert {(m.width, m.height) for m in modes if m.fourcc == "YUYV"} == {(640, 480), (1280, 720), (1920, 1080)}
    assert next(m for m in modes if m.fourcc == "YUYV").fps == (5, 10, 15, 20, 24, 25, 30)
    assert all(m.fourcc != "H264" for m in modes)


def _node(sysfs, name, label, loopback=False):
    folder = sysfs / name
    folder.mkdir()
    (folder / "name").write_text(label + "\n")
    (folder / "dev").write_text("81:0\n")
    if loopback:
        (folder / "max_openers").write_text("10\n")
    return folder


WEBCAM = (("uvcvideo", "HD Webcam", True), (Mode("MJPG", 1280, 720, (30.0,)),))


@pytest.fixture
def sysfs(tmp_path):
    root = tmp_path / "video4linux"
    root.mkdir()
    return root


def _scanner(sysfs, answers, opened, keys=None, allow_loopback=False):
    def open_node(path):
        opened.append(os.path.basename(path))
        return answers.get(os.path.basename(path))
    return LinuxScanner(sysfs=str(sysfs), dev="/dev", allow_loopback=allow_loopback, open_node=open_node,
                        stable_keys=lambda: keys or {})


@linux_only
def test_scanning_lists_capture_nodes_and_skips_the_rest(sysfs):
    _node(sysfs, "video0", "HD Webcam")
    _node(sysfs, "video1", "HD Webcam")  # its metadata node: no capture
    _node(sysfs, "video2", "Encoder")  # only formats OpenCV can't read
    _node(sysfs, "video10", "OBS Virtual Camera", loopback=True)
    _node(sysfs, "video11", "Phone Camera", loopback=True)
    opened = []
    answers = {"video0": WEBCAM, "video1": (("uvcvideo", "HD Webcam", False), ()),
               "video2": (("encoder", "Encoder", True), ())}
    found = _scanner(sysfs, answers, opened, keys={"/dev/video0": "Vendor_HD_Webcam_123"}).scan()
    assert found == [Device("Vendor_HD_Webcam_123", "HD Webcam", "/dev/video0")]
    assert found[0].modes == WEBCAM[1]
    assert "video10" not in opened and "video11" not in opened  # loopback devices aren't even opened


@linux_only
def test_a_node_is_only_opened_again_once_it_was_replaced(sysfs):
    folder = _node(sysfs, "video0", "HD Webcam")
    opened = []
    scanner = _scanner(sysfs, {"video0": WEBCAM}, opened)
    assert len(scanner.scan()) == 1
    assert len(scanner.scan()) == 1
    assert opened == ["video0"]  # opening wakes a sleeping camera, so not every scan

    for f in folder.iterdir():  # unplugged, and another camera gets the same node
        f.unlink()
    folder.rmdir()
    assert scanner.scan() == []
    _node(sysfs, "video0", "Brio")
    scanner._open = lambda path: (("uvcvideo", "Brio", True), WEBCAM[1])
    assert [d.name for d in scanner.scan()] == ["Brio"]


def _usb(tmp_path, sysfs, node, port, serial="", interface="00"):
    """A node whose sysfs device is interface port:1.<n> of a USB camera."""
    usb = tmp_path / "devices" / "pci0000:00" / port
    iface = usb / f"{port}:1.{int(interface)}"
    iface.mkdir(parents=True)
    (usb / "idVendor").write_text("046d\n")
    (usb / "idProduct").write_text("0892\n")
    if serial:
        (usb / "serial").write_text(serial + "\n")
    (iface / "bInterfaceNumber").write_text(interface + "\n")
    os.symlink(iface, _node(sysfs, node, "HD Pro Webcam C920") / "device")


@linux_only
def test_usb_webcams_are_keyed_from_sysfs_without_waiting_for_udev(tmp_path, sysfs):
    _usb(tmp_path, sysfs, "video0", "8-4", serial="ABC123")
    found = _scanner(sysfs, {"video0": WEBCAM}, []).scan()
    assert found[0].key == "usb-046d:0892-ABC123-if00"


@linux_only
def test_identical_webcams_without_a_serial_are_told_apart_by_their_port(tmp_path, sysfs):
    _usb(tmp_path, sysfs, "video0", "8-4")
    _usb(tmp_path, sysfs, "video2", "8-5")
    keys = [d.key for d in _scanner(sysfs, {"video0": WEBCAM, "video2": WEBCAM}, []).scan()]
    assert len(set(keys)) == 2 and all(k.startswith("usb-port-") and k.endswith(":1.0") for k in keys)


@linux_only
def test_a_key_that_fell_back_to_the_node_is_looked_up_again(sysfs):
    _node(sysfs, "video2", "HD Webcam")
    links = {}
    opened = []
    scanner = LinuxScanner(sysfs=str(sysfs), dev="/dev", open_node=lambda path: opened.append(path) or WEBCAM,
                           stable_keys=lambda: dict(links))
    assert [d.key for d in scanner.scan()] == ["video2"]  # scanned before udev made its link
    links["/dev/video2"] = "Vendor_HD_Webcam"
    assert [d.key for d in scanner.scan()] == ["Vendor_HD_Webcam"]
    assert len(opened) == 1  # without opening the camera again


@linux_only
def test_a_node_that_cant_be_opened_is_tried_again(sysfs):
    _node(sysfs, "video0", "HD Webcam")
    opened = []
    answers = {}
    scanner = _scanner(sysfs, answers, opened)
    assert scanner.scan() == []
    answers["video0"] = WEBCAM
    assert len(scanner.scan()) == 1


@linux_only
def test_other_loopback_cameras_can_be_let_in_for_testing_but_never_virtual_ones(sysfs):
    _node(sysfs, "video10", "Fake Webcam", loopback=True)
    _node(sysfs, "video11", "Phone Camera", loopback=True)
    _node(sysfs, "video12", "OBS Virtual Camera", loopback=True)
    opened = []
    answers = {"video10": (("v4l2 loopback", "Fake Webcam", True), WEBCAM[1]),
               "video12": (("v4l2 loopback", "OBS Virtual Camera", True), WEBCAM[1]),
               "video11": (("v4l2 loopback", "Phone Camera", True), WEBCAM[1])}
    found = _scanner(sysfs, answers, opened, allow_loopback=True).scan()
    assert [d.name for d in found] == ["Fake Webcam"]
    assert opened == ["video10"]
    _scanner(sysfs, answers, opened, allow_loopback=True).scan()
    assert opened.count("video10") == 2  # a loopback's caps change with its writer, so it's asked every time


def test_windows_lists_qt_cameras_at_their_directshow_index(monkeypatch):
    import sys
    from enum import Enum

    class QVideoFrameFormat:  # Qt Multimedia itself needs libpulse on Linux, which CI lacks
        PixelFormat = Enum("PixelFormat", "Format_Jpeg Format_NV12 Format_YUYV")

    def fmt(pixel, w, h, lo, hi):
        return SimpleNamespace(resolution=lambda: SimpleNamespace(width=lambda: w, height=lambda: h),
                               minFrameRate=lambda: lo, maxFrameRate=lambda: hi, pixelFormat=lambda: pixel)

    def camera(name, cid, formats):
        return SimpleNamespace(description=lambda: name, id=lambda: cid, videoFormats=lambda: formats)

    pf = QVideoFrameFormat.PixelFormat
    cameras = [camera("Telescope (Windows Virtual Camera)", b"t", []),
               camera("Integrated Camera", b"\\\\?\\usb#1", [fmt(pf.Format_NV12, 1920, 1080, 30, 30),
                                                         fmt(pf.Format_Jpeg, 1920, 1080, 30, 30),
                                                         fmt(pf.Format_YUYV, 640, 480, 30, 30)]),
               camera("OBS Virtual Camera", b"o", []),
               camera("Cam Link 4K", b"\\\\?\\usb#2", [fmt(pf.Format_NV12, 1920, 1080, 59.94, 59.94)]),
               camera("Phone Link camera", b"\\\\?\\swd#3", [])]  # Media Foundation only: OpenCV can't open it
    monkeypatch.setitem(sys.modules, "PyQt6.QtMultimedia", SimpleNamespace(
        QMediaDevices=SimpleNamespace(videoInputs=lambda: cameras), QVideoFrameFormat=QVideoFrameFormat))
    monkeypatch.setattr(webcams.vcam, "IS_LINUX", False)
    # DirectShow's own order, with a virtual camera's filter first and paths in another case
    found = webcams.scan_windows(paths=["", "\\\\?\\USB#2", "\\\\?\\usb#1"])
    assert [(d.name, d.location) for d in found] == [("Integrated Camera", 2), ("Cam Link 4K", 1)]
    assert found[0].key == "\\\\?\\usb#1"
    assert found[0].modes == (Mode("MJPG", 1920, 1080, (30,)), Mode("YUYV", 640, 480, (30,)))  # no NV12 copy
    assert found[1].modes == (Mode("NV12", 1920, 1080, (59.94,)),)
    assert choose(found[0].modes, (1920, 1080), 30) == Choice("MJPG", 1920, 1080, 30.0)


@linux_only
def test_who_uses_finds_the_program_holding_the_device(tmp_path):
    proc = tmp_path / "proc"
    (proc / "123" / "fd").mkdir(parents=True)
    (proc / "123" / "comm").write_text("firefox-bin\n")
    device = tmp_path / "video0"
    device.write_text("")
    os.symlink(device, proc / "123" / "fd" / "7")
    (proc / "456" / "fd").mkdir(parents=True)
    assert webcams.who_uses(str(device), proc=str(proc)) == "firefox-bin"
    assert webcams.who_uses(str(tmp_path / "video2"), proc=str(proc)) == ""


# ── Reading frames ───────────────────────────────────────────────────────────

class _Capture:
    def __init__(self, frames, opened=True):
        self.frames = list(frames)
        self.opened = opened
        self.released = False

    def isOpened(self):
        return self.opened

    def read(self):
        if not self.frames:
            return False, None
        return True, self.frames.pop(0)

    def release(self):
        self.released = True
        self.opened = False  # like OpenCV's


DEV = Device("cam", "HD Webcam", __file__, C920)  # a path that exists, like a plugged-in camera's node
FRAME = np.zeros((720, 1280, 3), np.uint8)


def _reader(capture, located=DEV, users=lambda _path: ""):
    reports = []
    reader = WebcamReader(lambda: located, lambda dev: choose(dev.modes, (1280, 720), 30),
                          lambda state, detail="": reports.append((state, detail)),
                          opener=lambda dev, choice: capture, users=users)
    return reader, reports


def test_the_reader_hands_out_the_frame_it_checked_first(monkeypatch):
    monkeypatch.setattr(webcams.vcam, "IS_LINUX", True)
    second = FRAME + 1
    reader, reports = _reader(_Capture([FRAME, second]))
    assert reader.open() and reader.isOpened()
    assert reports == [("ok", "1280×720, MJPEG, 30 fps")]
    ok, frame = reader.read()
    assert ok and frame is FRAME
    assert reader.read()[1] is second
    assert reader.last_frame_bytes == 0  # MJPEG that came decoded, as DirectShow does: its size isn't known


def test_a_raw_frame_counts_what_crossed_the_cable_not_the_decoded_size():
    raw = np.zeros((480, 640, 3), np.uint8)
    reader = WebcamReader(lambda: DEV, lambda dev: Choice("YUYV", 640, 480, 30.0), opener=lambda dev, choice: _Capture([raw]))
    assert reader.open() and reader.read()[0]
    assert reader.last_frame_bytes == 640 * 480 * 2


def test_mjpeg_frames_are_decoded_by_the_streams_decoders(monkeypatch):
    monkeypatch.setattr(webcams.vcam, "IS_LINUX", True)
    jpeg = cv2.imencode(".jpg", np.full((720, 1280, 3), 200, np.uint8))[1].reshape(1, -1)
    reader, _ = _reader(_Capture([jpeg]))
    assert reader.open()
    assert reader.parallel_decode  # several JPEGs at once on the decoder threads
    ok, packet = reader.read_packet()
    assert ok and packet.shape[0] == 1
    frame = reader.decode(packet)
    assert frame.shape == (720, 1280, 3) and abs(int(frame[0, 0, 0]) - 200) < 4


def test_an_unplugged_webcam_waits_to_be_plugged_back_in():
    reader, reports = _reader(_Capture([FRAME]), located=None)
    assert not reader.open()
    assert reports == [("gone", "")]
    assert reader.waiting_text == webcams.GONE_TEXT


def test_a_webcam_another_program_holds_says_which(monkeypatch):
    monkeypatch.setattr(webcams.vcam, "IS_LINUX", True)
    capture = _Capture([])
    reader, reports = _reader(capture, users=lambda path: "zoom")
    assert not reader.open()
    assert capture.released
    assert reports == [("busy", "zoom")]
    assert reader.waiting_text == webcams.BUSY_TEXT


def test_a_webcam_that_wont_open_counts_as_busy(monkeypatch):
    monkeypatch.setattr(webcams.vcam, "IS_LINUX", True)
    reader, reports = _reader(_Capture([], opened=False))
    assert not reader.open()
    assert reports == [("busy", "")]


def test_an_open_webcam_that_sends_nothing_says_so(monkeypatch):
    monkeypatch.setattr(webcams.vcam, "IS_LINUX", True)
    reader, reports = _reader(_Capture([]))
    assert not reader.open()
    assert reports == [("silent", "")]
    assert reader.waiting_text == webcams.NO_FRAMES_TEXT


def test_unplugging_mid_stream_drops_the_stream_and_says_so(monkeypatch, tmp_path):
    monkeypatch.setattr(webcams.vcam, "IS_LINUX", True)
    node = tmp_path / "video0"
    node.write_text("")
    located = [Device("cam", "HD Webcam", str(node), C920)]
    reports = []
    reader = WebcamReader(lambda: located[0], lambda dev: choose(dev.modes, None, 30),
                          lambda state, detail="": reports.append(state), opener=lambda dev, choice: _Capture([FRAME]))
    assert reader.open()
    assert reader.read()[0]
    node.unlink()
    located[0] = None
    assert reader.read() == (False, None)
    assert reports[-1] == "gone"


def test_the_control_client_has_no_phone_and_no_mic():
    ctrl = webcams.WebcamControl()
    assert ctrl.get_state() is None and ctrl.audio is False
    ctrl.send(action="resolution", width=1, height=1)
    assert ctrl.auth.headers() == {}
