"""Scenes for camera, cinema tools, framing, transforms, preview, stream output, background, presets, wait screen, browser camera: see translation_scenes.py for how scenes are registered."""

from scripts.translation_scenes import *  # noqa: F401,F403
from scripts.translation_scenes import Joined, _Held, parts_of  # noqa: F401

PHONE_URL = "http://192.168.1.20:8080/video.mjpeg"
CARD_WIDTH = 444  # a card's column in the main window, plus the picture's margins
BANNER_WIDTH = 1000  # the banners run across the whole window, so a picture with them is wider


# ── Building blocks ──────────────────────────────────────────────────────────

class Ctrl(_Held):
    """Stands in for the phone's control connection: takes every command and does nothing."""

    auth = None


def ready_window(stack, phone=True):
    """The main window with the first-run checklist done, so the video stage is there."""
    from telescope import vision
    win = main_window(stack, phone=phone)
    patch(stack, win, "vision_state", lambda: vision.READY)  # the download there is on this computer would change the cards
    patch(stack, win, "want_people", lambda *_a, **_k: None)  # nothing loads MediaPipe in a picture
    onboarding = next(p for p in win._plugins if p.name == "onboarding")
    onboarding._vcam_checked = onboarding._vcam_ready = onboarding._streamed = True
    onboarding._render()
    return win


def plugin_of(win, name):
    return next(p for p in win._plugins if p.name == name)


def patch(stack, target, name, value):
    from unittest import mock
    stack.enter_context(mock.patch.object(target, name, value))


def h264_decoding(stack, available=True):
    """This computer can (or can't) decode H.264, whatever is installed here."""
    from telescope import h264_reader
    patch(stack, h264_reader, "available", lambda: available)


def lens(id="0", current=True, **fields):
    """A camera as the phone's /v1/state lists it."""
    sizes = [(3840, 2160), (1920, 1080), (1280, 720), (854, 480), (640, 480), (1440, 1080)]
    cam = {"id": id, "current": current, "label": "Back ~24mm OIS", "facing": "back", "focalMm": 24, "hasOis": True,
           "hwLevel": "FULL", "supportsManualSensor": True, "supportsManualWB": True, "supportsManualFocus": True,
           "supportsFocusPoint": True, "supportsFlash": True, "minFocusDistance": 10.0, "isoMin": 50, "isoMax": 3200,
           "shutterMinNs": 100_000, "shutterMaxNs": 500_000_000, "aeCompMin": -8, "aeCompMax": 8, "aeCompStep": 0.167,
           "maxFps": 60, "supportedSizes": [{"width": w, "height": h} for w, h in sizes],
           "h264Sizes": [{"width": w, "height": h} for w, h in sizes[:-1]], "zoomRatioMax": 1.0}
    cam.update(fields)
    return cam


def phone_state(*cams, **fields):
    """A /v1/state answer: the lenses (one full-featured lens by default) and what the phone is doing."""
    state = {"cameras": list(cams) or [lens()], "auto": True, "wb_manual": False, "ois": True, "focus_mode": "continuous",
             "focus_distance": 0.0, "ae_comp": 0, "nr_mode": 1, "edge_mode": 1, "black_level_lock": False,
             "torch": False, "battery": 64, "charging": False, "battery_temp_c": 30.0, "codecs": ["h264", "mjpeg"],
             "codec": "mjpeg", "dynamic_bitrate": True, "stream_width": 1920, "stream_height": 1080}
    state.update(fields)
    return state


def streaming_window(stack, state=None, camera_on=True):
    """The window with the phone streaming (no frames come), and the plugins told so as the real start does."""
    from PyQt6.QtCore import QTimer
    from telescope.session import StreamSession
    win = ready_window(stack)
    patch(stack, win, "reconnect_stream", lambda *_a, **_k: None)  # nothing really streams that could restart
    patch(stack, QTimer, "singleShot", lambda *_a, **_k: None)  # a stop that follows a banner comes later
    source = win._active_source()
    session = StreamSession(1, PHONE_URL, Ctrl(), None, source)
    win._sessions.add(source, session)
    win._sessions.focus = win._sessions.main = source
    source.camera_off = not camera_on
    win._tell(source, "on_stream_start", PHONE_URL, session.client)
    if state is not None:
        win._apply_state(session.id, state, source)
    return win, source


CARD_WIDGETS = {"camera_control": "_lens_panel", "framing": "_framing_slider", "transforms": "_zoom_slider",
                "preview": "_preview_lbl", "stream_output": "_res_combo", "background": "_blur_slider",
                "browser_camera": "_card", "cinema": "_scope_combo", "face_exposure": "_switch",
                "privacy_blur": "_now_btn"}


def card_of(win, widget):
    """The card of the window that holds widget."""
    panels = [p for group in win._panels.values() for p in group]
    while widget is not None and widget not in panels:
        widget = widget.parentWidget()
    return widget


def cards(stack, win, *names, banners=False, width=CARD_WIDTH, column=CARD_WIDTH - 32):
    """The cards of these plugins in one column, as they sit in the window. With banners, the window's banners run
    across the top of a wider picture, as they do above the columns."""
    from PyQt6.QtWidgets import QApplication, QHBoxLayout, QVBoxLayout, QWidget
    top = QWidget()
    top.setObjectName("translation_preview")
    outer = QVBoxLayout(top)
    outer.setContentsMargins(16, 16, 16, 16)
    outer.setSpacing(0)
    if banners:
        outer.addWidget(win._banners)
        win._banners.layout().setContentsMargins(0, 0, 0, 12)
    holder = QWidget()
    holder.setFixedWidth(column)
    layout = QVBoxLayout(holder)
    layout.setContentsMargins(0, 0, 0, 0)
    layout.setSpacing(12)
    row = QHBoxLayout()
    row.addWidget(holder, 0)
    row.addStretch(1)
    outer.addLayout(row)
    for name in names:
        card = card_of(win, getattr(plugin_of(win, name), CARD_WIDGETS[name]))
        layout.addWidget(card)
        card.setVisible(True)
    width = max(width, BANNER_WIDTH) if banners else width
    top.setFixedWidth(width)
    top.show()
    QApplication.processEvents()
    top.setFixedHeight(top.sizeHint().height())
    QApplication.processEvents()
    stack.callback(top.close)
    return painted(top)


def painted(top):
    """Say which texts tiles and cards draw by hand (the caption of a tile, the note under a format), which no label holds."""
    from PyQt6.QtWidgets import QWidget
    for widget in top.findChildren(QWidget):
        for attr, slot in (("_caption", "caption"), ("_note", "note")):
            text = getattr(widget, attr, None)
            if isinstance(text, str) and parts_of(text):
                remember(widget, slot, text)
    return top


def recording(stack, module):
    """The translated texts module makes from now on, in the order it makes them."""
    seen = []
    original = module.tr

    def tr(*args, **kwargs):
        text = original(*args, **kwargs)
        seen.append(text)
        return text

    patch(stack, module, "tr", tr)
    return seen


def rejoin(widget, seen):
    """A tooltip made with "\\n".join keeps its words but not which texts they are: set it again from those in seen."""
    text = widget.toolTip()
    parts = {}
    for item in seen:
        if parts_of(item) and str(item) and str(item) in text:
            parts.setdefault(item.identity, item)
    widget.setToolTip(Joined(text, list(parts.values())))


# ── Camera card ──────────────────────────────────────────────────────────────

@scene("camera-loading", "states", "Camera · loading lenses", width=CARD_WIDTH)
def camera_loading(stack):
    win, _source = streaming_window(stack)
    return cards(stack, win, "camera_control")


@scene("camera-unavailable", "states", "Camera · phone sent nothing", width=CARD_WIDTH)
def camera_unavailable(stack):
    win, _source = streaming_window(stack)
    plugin_of(win, "camera_control").on_phone_state({})
    return cards(stack, win, "camera_control")


def camera_state(stack, state):
    from telescope.plugins import camera_control
    seen = recording(stack, camera_control)
    win, _source = streaming_window(stack, state)
    rejoin(plugin_of(win, "camera_control")._cam_info_lbl, seen)
    return cards(stack, win, "camera_control")


@scene("camera-manual", "main", "Camera · manual exposure, white balance and focus", width=CARD_WIDTH)
def camera_manual(stack):
    return camera_state(stack, phone_state(lens(), auto=False, iso=400, shutter_ns=8_000_000, wb_manual=True,
                                           focus_mode="manual", focus_distance=2.0, low_light_toggle=True,
                                           low_light=True))


NONE = {"supportsManualSensor": False, "supportsManualWB": False, "supportsManualFocus": False,
        "supportsFocusPoint": False, "supportsFlash": False, "hasOis": False}
LENS_LEVELS = (("LEGACY", "Legacy", NONE), ("LIMITED", "Limited", {"supportsManualWB": False, "supportsFlash": False}),
               ("FULL", "Full", {}), ("LEVEL_3", "Level 3", {}), ("EXTERNAL", "External", {"hasOis": False, "supportsFocusPoint": False}))


def _lens_level_scene(code, name, fields):
    scene(f"camera-level-{code.lower().replace('_', '-')}", "states", f"Camera · {name} hardware level", width=CARD_WIDTH)(
        lambda stack: camera_state(stack, phone_state(lens(hwLevel=code, **fields))))


for _code, _name, _fields in LENS_LEVELS:
    _lens_level_scene(_code, _name, _fields)


# ── Stream output ────────────────────────────────────────────────────────────

def stream_output(stack, state, config=None, decoding=True, then=None, banners=True):
    """The Stream output card (with the window's banners above it) after the phone's state came in."""
    h264_decoding(stack, decoding)
    win, source = streaming_window(stack)
    plugin = plugin_of(win, "stream_output")
    if config is not None:
        plugin.set_config(config)
    win._apply_state(1, state, source)
    if then is not None:
        then(win, source, plugin)
    return cards(stack, win, "stream_output", banners=banners)


def phone_codec_error(code, size, **fields):
    """What the phone sends when it dropped from H.264 to MJPEG: its English text, a code and the size it failed at."""
    text = {"encoder_stopped": "The H.264 encoder stopped", "h264_unavailable": "H.264 isn't available",
            "camera_cant_feed": "The camera can't feed H.264", "camera_stopped": "H.264 stopped the camera"}[code]
    return {"codec_error": text, "codec_error_code": code, "codec_error_width": size[0], "codec_error_height": size[1],
            **fields}


ENCODER_CODES = (("encoder_stopped", "the encoder stopped"), ("h264_unavailable", "H.264 isn't available at that size"),
                 ("camera_cant_feed", "the camera can't feed it"), ("camera_stopped", "it stopped the camera"))


def _encoder_scene(code, what):
    def build(stack):
        def too_much(win, source, plugin):
            win._apply_state(1, phone_state(codec="mjpeg", stream_width=3840, stream_height=2160, codec_unsupported=True,
                                            **phone_codec_error(code, (3840, 2160))), source)
        return stream_output(stack, phone_state(codec="h264"), then=too_much)
    scene(f"stream-encoder-{code.replace('_', '-')}", "states", f"Stream output · too much for the encoder ({what})",
          width=BANNER_WIDTH)(build)


for _code, _what in ENCODER_CODES:
    _encoder_scene(_code, _what)


@scene("stream-encoder-no-fallback", "states", "Stream output · too much for the encoder, no size to go back to",
       width=BANNER_WIDTH)
def stream_encoder_no_fallback(stack):
    return stream_output(stack, phone_state(codec="mjpeg", stream_width=3840, stream_height=2160, codec_unsupported=True,
                                            **phone_codec_error("h264_unavailable", (3840, 2160))))


@scene("stream-switched-to-heavy", "states", "Stream output · phone went back to Heavy", width=BANNER_WIDTH)
def stream_switched_to_heavy(stack):
    return stream_output(stack, phone_state(codec="mjpeg", **phone_codec_error("camera_stopped", (1920, 1080))))


def falling_behind(stack, state, config=None, decoding=True):
    def behind(win, source, plugin):
        win._bus.stream_behind.emit(source.id or "", True)
    return stream_output(stack, state, config, decoding, then=behind)


BEHIND = (
    ("heavy", "on Heavy, weak Wi-Fi", {"wifi_rssi": -80, "wifi_band": "5"}, {"format": "mjpeg"}),
    ("dynamic", "on Light with Dynamic, 2.4 GHz Wi-Fi", {"wifi_band": "2.4", "wifi_rssi": -50}, {"bitrate_mbps": -1}),
    ("light", "on Light", {}, {"bitrate_mbps": 8}),
    ("old-phone", "on Light, phone without Dynamic", {"dynamic_bitrate": False}, {"bitrate_mbps": 8}),
)


def _behind_scene(id, what, fields, config):
    scene(f"stream-behind-{id}", "states", f"Stream output · can't keep up ({what})", width=BANNER_WIDTH)(
        lambda stack: falling_behind(stack, phone_state(codec="h264" if config.get("format") != "mjpeg" else "mjpeg",
                                                        **fields), config))


for _id, _what, _fields, _config in BEHIND:
    _behind_scene(_id, _what, _fields, _config)


@scene("stream-no-encoder", "states", "Stream output · phone has no H.264 encoder", width=CARD_WIDTH)
def stream_no_encoder(stack):
    return stream_output(stack, phone_state(codecs=["mjpeg"]), banners=False)


@scene("stream-no-pyav", "states", "Stream output · this computer can't decode H.264", width=CARD_WIDTH)
def stream_no_pyav(stack):
    return stream_output(stack, phone_state(), decoding=False, banners=False)


@scene("stream-dynamic-old-phone", "states", "Stream output · Dynamic bitrate on a phone that can't", width=CARD_WIDTH)
def stream_dynamic_old_phone(stack):
    return stream_output(stack, phone_state(dynamic_bitrate=False, codec="h264"), {"bitrate_mbps": -1}, banners=False)


# ── Transforms and Auto framing ──────────────────────────────────────────────

def two_lenses():
    """A phone with a main camera and a telephoto, which the main camera's zoom hands over to at 3x."""
    return phone_state(lens("0", True, zoomRatioMax=8.0, lensZooms=[3.0]),
                       lens("2", False, label="Back ~85mm", focalMm=85, hasOis=False))


def zoom_view(stack, zoom, steps, lens_zooms=(3.0,), taken=None):
    """Transforms (with Auto framing under it) zoomed to zoom, after the window's view told it how the zoom is shared
    with the phone, step by step: (the ratio the phone was sent, what this computer crops, the lens the phone is on)."""
    from telescope.plugins import transforms
    from telescope.view import PhoneZoom, PhoneZoomCaps, ViewStatus
    seen = recording(stack, transforms)
    win, source = streaming_window(stack)
    plugin = plugin_of(win, "transforms")
    plugin._settle_timer.stop()
    plugin._zoom_slider.setValue(round(zoom * 100))
    caps = PhoneZoomCaps(8.0, 1.0, False, tuple(lens_zooms)) if lens_zooms is not None else None
    for sent, desktop, active in steps:
        state = {**two_lenses(), "active_lens": active} if active else two_lenses()
        win._apply_state(1, state, source)
        win._bus.view_status.emit(ViewStatus(True, caps, PhoneZoom(sent) if caps else None, 0.0, desktop))
        plugin.on_phone_state(state)  # the window's own view status came with the state above: this one says what the picture needs
        plugin._settle_timer.stop()
    if taken:
        taken(win)
    rejoin(plugin._lens_dot, seen)
    return cards(stack, win, "transforms", "framing")


@scene("transforms-cropped-here", "states", "Transforms · this lens can't zoom itself", width=CARD_WIDTH)
def transforms_cropped_here(stack):
    return zoom_view(stack, 2.0, [(1.0, 2.0, None)], lens_zooms=None)


@scene("transforms-switched-lens", "states", "Transforms · phone switched to the telephoto", width=CARD_WIDTH)
def transforms_switched_lens(stack):
    return zoom_view(stack, 3.0, [(1.0, 1.0, "0"), (3.0, 1.0, "2")])


@scene("transforms-panned-past", "states", "Transforms · panned past the telephoto's view", width=CARD_WIDTH)
def transforms_panned_past(stack):
    return zoom_view(stack, 4.0, [(1.0, 1.0, "0"), (3.0, 1.0, "2"), (1.0, 4.0, "0")])


@scene("transforms-stayed-on-main", "states", "Transforms · phone stayed on the main camera", width=CARD_WIDTH)
def transforms_stayed_on_main(stack):
    return zoom_view(stack, 3.0, [(1.0, 1.0, "0"), (3.0, 1.0, "0")])


@scene("transforms-panned-past-unseen", "states", "Transforms · panned past a telephoto not seen yet", width=CARD_WIDTH)
def transforms_panned_past_unseen(stack):
    return zoom_view(stack, 4.0, [(1.0, 1.0, None)])


@scene("transforms-framing-moves-zoom", "states", "Transforms · Auto framing moves the zoom", width=CARD_WIDTH)
def transforms_framing_moves_zoom(stack):
    return zoom_view(stack, 1.0, [], taken=lambda win: plugin_of(win, "framing")._switch.setChecked(True))


@scene("framing-zoom-taken", "states", "Auto framing · something else moves the zoom", width=CARD_WIDTH)
def framing_zoom_taken(stack):
    def taken(win):
        win.take_view("Example plugin")
        plugin_of(win, "framing")._switch.setChecked(True)
    return zoom_view(stack, 1.0, [], taken=taken)


FRAMING_READOUTS = 10  # the most values any of its three sliders has: Speed's ten words


def _framing_scene(n):
    def build(stack):
        win = ready_window(stack)
        plugin_of(win, "framing").set_config({"framing": n % 4, "speed": n + 1, "room": n % 6})
        return cards(stack, win, "framing")
    scene(f"framing-readouts-{n + 1}", "main", f"Auto framing · readouts {n + 1} of {FRAMING_READOUTS}", width=CARD_WIDTH)(build)


for _n in range(FRAMING_READOUTS):
    _framing_scene(_n)


# ── Background ───────────────────────────────────────────────────────────────

def empty_folder(stack):
    """A folder with nothing in it, for a file dialog to open in instead of the real home folder."""
    import tempfile
    import os
    # A fixed path so the picture is the same every time; a random one only if something was put in it
    folder = os.path.join(tempfile.gettempdir(), "telescope-preview", "Pictures")
    os.makedirs(folder, exist_ok=True)
    if os.listdir(folder):
        folder = os.path.join(stack.enter_context(tempfile.TemporaryDirectory(prefix="telescope-translation-files-")), "Pictures")
        os.makedirs(folder)
    return folder


def hold_file_dialogs(stack):
    """File pickers open at once as real dialogs (in an empty folder) and are kept for the picture; returns the list."""
    from PyQt6.QtWidgets import QFileDialog
    held, folder = [], empty_folder(stack)

    def get_open(parent=None, caption="", _directory="", filter="", *_args, **_kwargs):
        dialog = QFileDialog(parent, caption, folder, filter)
        dialog.setOption(QFileDialog.Option.DontUseNativeDialog)
        dialog.setSidebarUrls([])  # the places on this computer are not part of the picture
        dialog.setMinimumWidth(760)  # wide enough for the longest list of file types
        remember(dialog, "window title", caption)
        remember(dialog, "file types", filter)
        held.append(dialog)
        return "", ""

    patch(stack, QFileDialog, "getOpenFileName", staticmethod(get_open))
    return held


def background_card(stack, setup):
    win = ready_window(stack)
    setup(plugin_of(win, "background"), empty_folder(stack))
    return cards(stack, win, "background")


@scene("background-blur", "main", "Background · blur", width=CARD_WIDTH)
def background_blur(stack):
    return background_card(stack, lambda plugin, _folder: plugin._set_mode("blur"))


@scene("background-no-picture", "states", "Background · no picture chosen yet", width=CARD_WIDTH)
def background_no_picture(stack):
    return background_card(stack, lambda plugin, _folder: plugin._set_mode("picture"))


@scene("background-unreadable-picture", "states", "Background · picture that can't be opened", width=CARD_WIDTH)
def background_unreadable_picture(stack):
    def broken(plugin, folder):
        import os
        path = os.path.join(folder, "holiday.png")
        with open(path, "wb") as file:
            file.write(b"not a picture")
        plugin.set_config({"mode": "picture", "picture": path})
    return background_card(stack, broken)


@scene("dialog-background-picture", "dialogs", "Background · choose a picture")
def background_dialog(stack):
    held = hold_file_dialogs(stack)
    win = ready_window(stack)
    plugin_of(win, "background")._choose_picture()
    return shown(stack, held[-1])


# ── Expose for my face ───────────────────────────────────────────────────────

@scene("face-exposure-picked-by-hand", "states", "Expose for my face · a point picked by hand", width=CARD_WIDTH)
def face_exposure_picked_by_hand(stack):
    win = ready_window(stack)
    plugin = plugin_of(win, "face_exposure")
    plugin._switch.setChecked(True)
    win._bus.focus_point_picked.emit(0.3, 0.4)
    return cards(stack, win, "face_exposure")


@scene("face-exposure-manual-focus", "states", "Expose for my face · Manual focus on the Camera card", width=CARD_WIDTH)
def face_exposure_manual_focus(stack):
    win = ready_window(stack)
    win._bus.focus_mode_changed.emit("manual")
    plugin_of(win, "face_exposure")._switch.setChecked(True)
    return cards(stack, win, "face_exposure")


@scene("face-exposure-no-point", "states", "Expose for my face · a lens that can't meter on a point", width=CARD_WIDTH)
def face_exposure_no_point(stack):
    win, _source = streaming_window(stack, phone_state(lens(supportsFocusPoint=False)))
    plugin_of(win, "face_exposure")._switch.setChecked(True)
    return cards(stack, win, "face_exposure")


# ── Privacy blur ─────────────────────────────────────────────────────────────

@scene("privacy-blur", "main", "Privacy blur", width=CARD_WIDTH)
def privacy_blur(stack):
    win = ready_window(stack)
    plugin_of(win, "privacy_blur")._switch.setChecked(True)
    return cards(stack, win, "privacy_blur")


@scene("privacy-blur-no-wait-screen", "states", "Privacy blur · Wait screen switched off", width=CARD_WIDTH)
def privacy_blur_no_wait_screen(stack):
    win = ready_window(stack)
    win._bus.cover_requested.disconnect()  # nothing answers, as with Wait screen switched off
    plugin = plugin_of(win, "privacy_blur")
    plugin._set_mode("wait")
    plugin._now_btn.setChecked(True)
    return cards(stack, win, "privacy_blur")


# ── Preview ──────────────────────────────────────────────────────────────────

PREVIEW_WIDTH = 760


def preview_stage(stack, win, banners=False):
    plugin_of(win, "preview")._show_skeleton_btn()
    return cards(stack, win, "preview", banners=banners, width=PREVIEW_WIDTH, column=PREVIEW_WIDTH - 32)


def mic_on(win):
    """The phone's mic switched on, which is what lets the camera go off."""
    plugin = plugin_of(win, "microphone")
    plugin.set_config({**plugin.get_config(), "enabled": True})


@scene("preview-idle", "states", "Preview · not streaming", width=PREVIEW_WIDTH)
def preview_idle(stack):
    win = ready_window(stack)
    mic_on(win)
    return preview_stage(stack, win)


@scene("preview-mic-only", "states", "Preview · start with the camera off", width=PREVIEW_WIDTH)
def preview_mic_only(stack):
    win = ready_window(stack)
    mic_on(win)
    win.set_camera_on(False)
    return preview_stage(stack, win)


@scene("preview-waiting", "states", "Preview · waiting for the first frame", width=PREVIEW_WIDTH)
def preview_waiting(stack):
    win, _source = streaming_window(stack)
    patch(stack, win, "can_turn_camera_off", lambda: (True, ""))  # the mic is on: switching it on here would start a real one
    plugin_of(win, "preview")._show_camera_btn()
    return preview_stage(stack, win)


def camera_off_while_streaming(stack, auto):
    win, source = streaming_window(stack, camera_on=False)
    source.camera_off_auto = auto
    win._tell(source, "on_camera_off")
    return preview_stage(stack, win)


@scene("preview-camera-off", "states", "Preview · camera off, mic streaming", width=PREVIEW_WIDTH)
def preview_camera_off(stack):
    return camera_off_while_streaming(stack, False)


@scene("preview-camera-off-auto", "states", "Preview · camera turned off by itself", width=PREVIEW_WIDTH)
def preview_camera_off_auto(stack):
    return camera_off_while_streaming(stack, True)


@scene("preview-hidden", "states", "Preview · hidden", width=PREVIEW_WIDTH)
def preview_hidden(stack):
    win = ready_window(stack)
    plugin_of(win, "preview")._toggle()
    return preview_stage(stack, win)


@scene("preview-popout", "main", "Preview · popped out")
def preview_popout(stack):
    win = ready_window(stack)
    plugin = plugin_of(win, "preview")
    plugin._open_popout()
    stack.callback(plugin._popout.close)
    return shown(stack, plugin._popout)


def picture(win):
    """A plain picture on the stage, for what's drawn over one."""
    from PyQt6.QtGui import QColor, QPixmap
    pixmap = QPixmap(416, 234)  # inside the stage's least height, so the whole picture shows
    pixmap.fill(QColor(78, 86, 96))
    plugin_of(win, "preview")._preview_lbl.setPixmap(pixmap)


def picking(stack, kind):
    """The preview while a plugin asks for a point, an area or four corners."""
    win, _source = streaming_window(stack)
    picture(win)
    overlays = win.preview_overlays()
    win.pick_on_preview("Scene", kind, lambda _pick: None)
    remember(plugin_of(win, "preview")._preview_lbl, "hint", overlays._hint(overlays._pick))
    return preview_stage(stack, win)


@scene("preview-pick-point", "states", "Preview · picking a point", width=PREVIEW_WIDTH)
def preview_pick_point(stack):
    return picking(stack, "point")


@scene("preview-pick-area", "states", "Preview · marking an area", width=PREVIEW_WIDTH)
def preview_pick_area(stack):
    return picking(stack, "rect")


@scene("preview-pick-corners", "main", "Preview · dragging four corners", width=PREVIEW_WIDTH)
def preview_pick_corners(stack):
    return picking(stack, "quad")


# ── Cinema tools ─────────────────────────────────────────────────────────────

@scene("cinema-tools", "main", "Cinema tools · focus peaking and a waveform", width=CARD_WIDTH)
def cinema_tools(stack):
    win, _source = streaming_window(stack)
    plugin_of(win, "cinema").set_config({"peaking": True, "scope": "waveform"})
    return cards(stack, win, "cinema")


@scene("preview-false-colour", "main", "Preview · false colour and its legend", width=PREVIEW_WIDTH)
def preview_false_colour(stack):
    from telescope.plugins import cinema
    win, _source = streaming_window(stack)
    picture(win)
    plugin_of(win, "cinema").set_config({"false_colour": True})
    names = [cinema.tr(name) for _upper, colour, name in reversed(cinema.FALSE_COLOURS) if colour is not None]
    remember(plugin_of(win, "preview")._preview_lbl, "legend", Joined("\n".join(names), names))
    return preview_stage(stack, win)


# ── Browser camera ───────────────────────────────────────────────────────────

class ServerStats:
    """How far browsers got, as the server counts it."""

    def __init__(self, port=8443, **counts):
        import time
        self.port, self.wanted_port, self.fell_back = port, port, False
        self.started = time.monotonic() - 5
        self.counts = {"connections": 0, "tls_failed": 0, "pages": 0, "refused": 0, "browsers": 0, **counts}

    def __getitem__(self, step):
        return self.counts[step]

    def nothing_arrived(self):
        return self.counts["connections"] == 0


class WebServer:
    """The browser camera's server without a network behind it: it is up, with a link, and nobody has come."""

    stats = None
    fail = None

    def __init__(self, hub, _cert, _key, on_change=None, **_kwargs):
        self.hub, self.token, self.port = hub, "example-token", 8443
        self.stats = ServerStats()

    def start(self):
        if self.fail is not None:
            raise self.fail

    def stop(self):
        pass

    def url_for(self, host):
        return f"https://{host}:{self.port}/#{self.token}"

    def new_token(self):
        pass


def browser_window(stack, available=True, server=WebServer, picked="browser"):
    """The window with Browser camera picked: its server up (or not able to start) and its card on show."""
    import os
    from pathlib import Path
    from types import SimpleNamespace
    from telescope import browser_rtc
    from telescope.plugins import browser_camera
    win = ready_window(stack)
    plugin = plugin_of(win, "browser_camera")
    patch(stack, browser_camera, "certificate_available", lambda: available != "cryptography")
    patch(stack, browser_rtc, "available", lambda: available != "aiortc")
    patch(stack, browser_camera, "ensure_certificate", lambda *_a, **_k: (Path(os.devnull), Path(os.devnull)))
    plugin._server_cls = server
    plugin._addresses = lambda: [SimpleNamespace(ip="192.168.1.20"), SimpleNamespace(ip="10.0.0.5")]
    plugin._join_waiting = lambda: None  # a browser that joins would start streaming for real, even as the window closes
    if picked:
        plugin_of(win, "connection").select(picked)
    return win, plugin


def join_browser(plugin, bid="b1", device="Chrome on Android", **feed):
    """A browser the server has let in, saying what it runs on and what it sends."""
    hub_feed = plugin.hub.join(bid)
    gen = hub_feed.connect()
    hub_feed.note_hello(gen, device, feed.get("mic_error", ""))
    hub_feed.note_rtc(gen, feed.get("codec", "H264"))
    hub_feed.note_sent(gen, feed.get("sent", {"width": 1280, "height": 720, "fps": 30, "limit": ""}))
    plugin._tidy_browsers()
    return hub_feed


def browser_card(stack, **kwargs):
    win, plugin = browser_window(stack, **kwargs)
    plugin._render()
    return win, plugin


@scene("browser-waiting", "main", "Browser camera · code to scan", width=CARD_WIDTH)
def browser_waiting(stack):
    win, _plugin = browser_card(stack)
    return cards(stack, win, "browser_camera")


@scene("browser-hint-certificate", "states", "Browser camera · certificate not accepted yet", width=CARD_WIDTH)
def browser_hint_certificate(stack):
    win, plugin = browser_card(stack)
    plugin._server.stats = ServerStats(connections=1, tls_failed=1)
    plugin._render()
    return cards(stack, win, "browser_camera")


@scene("browser-hint-nothing-arrived", "states", "Browser camera · nothing has reached this computer", width=CARD_WIDTH)
def browser_hint_nothing_arrived(stack):
    import time
    win, plugin = browser_card(stack)
    plugin._server.stats.started = time.monotonic() - 60
    plugin._render()
    return cards(stack, win, "browser_camera")


def one_browser(stack, device="Safari", **feed):
    win, plugin = browser_card(stack, picked=None)
    join_browser(plugin, device=device, **feed)
    plugin_of(win, "connection").select("browser:b1")
    plugin._render()
    return win, plugin


@scene("browser-connected", "states", "Browser camera · browser connected", width=CARD_WIDTH)
def browser_connected(stack):
    win, _plugin = one_browser(stack)
    return cards(stack, win, "browser_camera")


@scene("browser-slow-processor", "states", "Browser camera · device's processor can't keep up", width=CARD_WIDTH)
def browser_slow_processor(stack):
    win, _plugin = one_browser(stack, sent={"width": 1280, "height": 720, "fps": 18, "limit": "cpu"},
                               mic_error="the page wasn't allowed to use it")
    return cards(stack, win, "browser_camera")


@scene("browser-slow-connection", "states", "Browser camera · browser lowering the picture", width=CARD_WIDTH)
def browser_slow_connection(stack):
    win, _plugin = one_browser(stack, sent={"width": 1280, "height": 720, "fps": 30, "limit": "bandwidth"})
    return cards(stack, win, "browser_camera")


@scene("browser-two-connected", "states", "Browser camera · two browsers connected", width=CARD_WIDTH)
def browser_two_connected(stack):
    win, plugin = browser_card(stack)
    join_browser(plugin, "b1", "Safari")
    join_browser(plugin, "b2", "Chrome")
    plugin._render()
    return cards(stack, win, "browser_camera")


@scene("browser-video-failed", "states", "Browser camera · video connection couldn't be set up", width=CARD_WIDTH)
def browser_video_failed(stack):
    import os
    from pathlib import Path
    from telescope.browser_server import BrowserServer

    class Failing:
        def answer(self, _offer, _kbps):
            raise RuntimeError("no common codec")

    win, plugin = one_browser(stack)
    feed = plugin.hub.feed("b1")
    server = BrowserServer(plugin.hub, Path(os.devnull), Path(os.devnull), rtc_session=lambda *_a: Failing())
    server._answer(feed, feed.generation, [None], "offer")
    plugin._render()
    return cards(stack, win, "browser_camera")


class BusyServer(WebServer):
    fail = OSError("Address already in use")


BROWSER_PROBLEMS = (("cryptography", WebServer, "needs a package"), ("aiortc", WebServer, "needs another package"),
                    (True, BusyServer, "server won't start"))


def _problem_scene(index, available, server, what):
    def build(stack):
        win, _plugin = browser_card(stack, available=available, server=server)
        return cards(stack, win, "browser_camera")
    scene(f"browser-problem-{index}", "states", f"Browser camera · not available ({what})", width=CARD_WIDTH)(build)


for _index, (_available, _server, _what) in enumerate(BROWSER_PROBLEMS, 1):
    _problem_scene(_index, _available, _server, _what)


def full_window(stack, win, width=None):
    import telescope.app as app_module
    grow(win, width or app_module._WIDTH_THREE_COL, 900)
    return win


@scene("browser-unavailable-banner", "states", "Browser camera · Start with it not available", width=1300)
def browser_unavailable_banner(stack):
    win, plugin = browser_card(stack, server=BusyServer)
    plugin.prepare()
    return full_window(stack, win)


@scene("browser-waiting-banner", "states", "Browser camera · Start while waiting for a browser", width=1300)
def browser_waiting_banner(stack):
    from telescope.browser_server import BrowserReader
    win, plugin = browser_card(stack)
    plugin.prepare()
    win._set_status(i18n.tr(BrowserReader.waiting_text), "warn")  # what the stream worker says while no browser has come
    return full_window(stack, win)


def browser_behind(stack, config):
    win, plugin = browser_card(stack)
    plugin.set_config({**plugin.get_config(), **config})
    win._bus.stream_behind.emit(win.focused_source_id() or "", True)
    return win


@scene("browser-behind-lower", "states", "Browser camera · can't keep up, picture can be lowered", width=BANNER_WIDTH)
def browser_behind_lower(stack):
    win = browser_behind(stack, {})
    return cards(stack, win, "browser_camera", banners=True)


@scene("browser-behind-lowest", "states", "Browser camera · can't keep up, already the lowest", width=BANNER_WIDTH)
def browser_behind_lowest(stack):
    win = browser_behind(stack, {"size": "480p", "fps": 15})
    return cards(stack, win, "browser_camera", banners=True)


@scene("dialog-phones-browser", "dialogs", "Your phones · a browser kept for later")
def phones_browser(stack):
    import telescope.plugins.connection as connection
    win, plugin = browser_card(stack)
    plugin._known = {"b1": {"name": "Chrome on Android", "seen": 1_768_478_400.0}}  # midday on 15 January 2026
    patch(stack, win, "has_device_settings", lambda _sid: True)
    phones = FakePhones()
    phones.remembered = plugin.remembered()
    return shown(stack, connection.PhonesDialog(phones))


# ── Wait screen ──────────────────────────────────────────────────────────────

SETTINGS_PAGE = 700  # about the width a Settings page gives a card


def on_its_own(stack, widget, width=SETTINGS_PAGE):
    """widget (a part of a Settings page) at the width a page gives it, picked out of the window it is in."""
    from PyQt6.QtWidgets import QApplication, QVBoxLayout, QWidget
    top = QWidget()
    top.setObjectName("translation_preview")
    layout = QVBoxLayout(top)
    layout.setContentsMargins(16, 16, 16, 16)
    layout.addWidget(widget)
    top.setFixedWidth(width)
    top.show()
    QApplication.processEvents()
    top.setFixedHeight(top.sizeHint().height())
    QApplication.processEvents()
    stack.callback(top.close)
    return top


def wait_section(stack, setup=None):
    """The Wait screen section of the Streaming page, on its own, after setup(plugin, section, folder)."""
    win = ready_window(stack)
    win.open_settings()
    plugin = plugin_of(win, "wait_screen")
    section = plugin._section
    if setup is not None:
        setup(plugin, section, empty_folder(stack))
    return on_its_own(stack, section)


def spare_file(folder, name, content=b"not a picture"):
    import os
    path = os.path.join(folder, name)
    with open(path, "wb") as file:
        file.write(content)
    return path


@scene("wait-screen-image-not-kept", "states", "Wait screen · image that couldn't be copied", width=SETTINGS_PAGE)
def wait_screen_image_not_kept(stack):
    from telescope.plugins.wait_screen import WaitScreenPlugin

    def full_disk(_path):
        raise OSError(28, "No space left on device")

    def pick(_plugin, section, folder):
        patch(stack, WaitScreenPlugin, "_keep_copy", staticmethod(full_disk))
        section._set(spare_file(folder, "logo.png"))
    return wait_section(stack, pick)


@scene("wait-screen-image-unreadable", "states", "Wait screen · image that can't be read", width=SETTINGS_PAGE)
def wait_screen_image_unreadable(stack):
    def broken(plugin, section, folder):
        plugin.set_config({"image": spare_file(folder, "logo.png")})
        section.refresh()
    return wait_section(stack, broken)


@scene("dialog-wait-screen-image", "dialogs", "Wait screen · choose an image")
def wait_screen_dialog(stack):
    held = hold_file_dialogs(stack)
    wait_section(stack, lambda _plugin, section, _folder: section._choose())
    return shown(stack, held[-1])


# ── Presets and shortcuts that apply them ────────────────────────────────────

PRESET_NAMES = [{"name": "Desk"}, {"name": "Stream"}]


@scene("menu-presets-submenus", "dialogs", "Presets menu · Rename and Delete")
def presets_submenus(stack):
    from PyQt6.QtCore import QPoint
    win = ready_window(stack)
    plugin = plugin_of(win, "presets")
    plugin.set_config({"presets": PRESET_NAMES})
    menu = plugin.build_menu(win)
    kept = menu.actions()  # the entries for the submenus are only named while something holds them
    for action, text in zip([a for a in kept if a.menu() is not None], (i18n.tr("Rename"), i18n.tr("Delete"))):
        remember(action, "text", text)
    menu._kept = kept
    menu.popup(QPoint(20, 20))
    stack.callback(menu.close)
    return shown(stack, menu)


def preset_bindings():
    return [{"id": "b1", "keys": "Ctrl+1", "action": "presets.apply", "params": {"name": "Desk"}, "global": False},
            {"id": "b2", "keys": "Ctrl+Shift+Right", "action": "presets.cycle", "params": {"op": "next"}, "global": False},
            {"id": "b3", "keys": "Ctrl+Shift+Left", "action": "presets.cycle", "params": {"op": "previous"}, "global": False}]


def presets_and_shortcuts(stack):
    win = ready_window(stack)
    plugin_of(win, "presets").set_config({"presets": PRESET_NAMES})
    shortcuts = plugin_of(win, "shortcuts")
    shortcuts.set_bindings(preset_bindings())
    return win, shortcuts


@scene("settings-shortcuts-presets", "settings", "Settings · Keyboard shortcuts that apply presets")
def settings_shortcuts_presets(stack):
    from PyQt6.QtWidgets import QApplication
    from telescope.widgets.settings_window import _SIZE
    win, _shortcuts = presets_and_shortcuts(stack)
    win.resize(1380, 900)
    win.open_settings()
    settings = win._settings_win
    settings.show_tab("shortcuts")
    QApplication.processEvents()
    grow(settings, _SIZE[0], _SIZE[1])
    return settings


@scene("dialog-shortcut-preset-apply", "dialogs", "Keyboard shortcut editor · apply a preset")
def shortcut_apply_preset(stack):
    from telescope.plugins.shortcuts import BindingEditor
    _win, shortcuts = presets_and_shortcuts(stack)
    return shown(stack, BindingEditor(shortcuts, preset_bindings()[0]))


@scene("dialog-shortcut-preset-cycle", "dialogs", "Keyboard shortcut editor · next or previous preset")
def shortcut_cycle_preset(stack):
    from telescope.plugins.shortcuts import BindingEditor
    _win, shortcuts = presets_and_shortcuts(stack)
    return shown(stack, BindingEditor(shortcuts, preset_bindings()[1]))


@scene("notify-preset-applied", "notifications", "Notification · preset applied by a shortcut")
def notify_preset_applied(stack):
    sent = capture_notifications(stack)
    _win, shortcuts = presets_and_shortcuts(stack)
    shortcuts.set_notify(True)
    shortcuts.press("b1", "global")
    shortcuts.press("b2", "global")
    return bubble(stack, sent)


@scene("settings-plugins-off", "settings", "Settings · Plugins that are switched off", width=SETTINGS_PAGE)
def plugins_switched_off(stack):
    from telescope import plugin_loader
    from telescope.widgets.plugins_panel import PluginsPanel
    win = ready_window(stack)
    builtins = list(win._plugins)
    for plugin in builtins:
        if not plugin.required and not plugin.optional:
            plugin_loader.set_builtin_off(plugin.name, True)
    return on_its_own(stack, PluginsPanel(builtins, set(), {}, lambda: False, lambda: False))
