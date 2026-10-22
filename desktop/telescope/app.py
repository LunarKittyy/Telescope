import logging
import shutil
import subprocess
import threading
import time
from dataclasses import replace
from typing import Callable, Optional

from PyQt6.QtCore import QEvent, QPoint, QSize, Qt, QTimer, pyqtSignal
from PyQt6.QtGui import QAction
from PyQt6.QtWidgets import (
    QApplication, QFrame, QHBoxLayout, QLabel,
    QMainWindow, QMenu, QMessageBox, QPushButton, QScrollArea,
    QSizePolicy, QSystemTrayIcon, QVBoxLayout, QWidget,
)

from telescope import dev_profile, diagnostics, theme, vcam
from telescope.config import load_config, save_config, saveable, take_reset_notice
from telescope.models import PhoneState, PhoneStateError
from telescope.phone_client import PhoneControlClient
from telescope.platform import IS_LINUX
from telescope.platform.linux import CANCELLED
from telescope import plugin_loader
from telescope.plugin import UNCHANGED, EventBus, TelescopePlugin
from telescope.session import Sessions, StreamSession
from telescope.single_instance import relaunch, restart_argv
from telescope.sources import PhoneSource, PluginSource, Source
from telescope.stream import StreamWorker
from telescope.stream_flow import StreamFlow
from telescope.view_controller import ViewController
from telescope.vision import Vision
from telescope.widgets.banner import BannerAction, BannerArea, Issue, copy_action
from telescope.widgets.tiles import StreamTiles
from telescope.widgets.common import (
    ElidingLabel, create_app_icon, create_tray_icon, create_vector_icon, set_status_kind, tab_in_reading_order, ui_px,
)

STATUS_COLORS = theme.STATUS_COLORS
_WIDTH_THREE_COL = 1300
_WIDTH_TWO_COL   = 900
# Both rails share one width so the preview sits on the window's centre line.
_RAIL_WIDTH       = 412
_RAIL_WIDTH_SOLO  = 440  # two-column mode: the one rail holding every card
_CAMERA_LIMITED_TIP = "The phone's camera is making fewer frames than asked for. Dim light slows it down."
_DYNAMIC_FPS_TIP = "Dynamic lowered the frame rate to {fps} fps to keep up with a slow connection."
_HEAT_FPS_TIP = "The phone got hot and lowered the frame rate to {fps} fps for now."


def _joined(words: list) -> str:
    """["A", "B", "C"] as "A, B and C"."""
    return words[0] if len(words) == 1 else ", ".join(words[:-1]) + " and " + words[-1]


# ── What sources see of the window ────────────────────────────────────────────
class _SourceHost:
    """The window as sources.SourceHost: the few things a source asks of it, and nothing else."""

    def __init__(self, win: "TelescopeWindow"):
        self._win = win

    def connection(self):
        return self._win._plugin("connection")

    def stream_output(self):
        return self._win._plugin("stream_output")

    def show_issue(self, key: str, issue: Issue):
        self._win.show_issue(key, issue)

    def clear_issue(self, key: str):
        self._win.clear_issue(key)

    def is_streaming(self) -> bool:
        return self._win.is_streaming()

    def is_focused(self, source: Source) -> bool:
        return self._win._sessions.focus is source

    def focus_stream(self, source_id: Optional[str]):
        self._win.focus_stream(source_id)

    def is_waking(self) -> bool:
        return self._win._sessions.waking

    def waking_source(self) -> Optional[Source]:
        return self._win._sessions.wake_source

    def wake_progress(self, source: Source, msg: str):
        self._win._source_status(source, msg, "dim")

    def wake_done(self, source: Source, ok: bool, reason: str):
        self._win._flow.wake_done(source, ok, reason)

    def apply_state(self, source: Source, session_id: int, state: dict):
        self._win._apply_state(session_id, state, source)

    def set_session(self, source: Source, session: StreamSession):
        self._win._sessions.add(source, session)

    def stop_source(self, source: Source):
        self._win._flow.stop(remote_stop=False, source=source)

    def start_source(self, source_id: Optional[str]):
        self._win._start_source(source_id)

    def set_camera_on(self, on: bool):
        self._win.set_camera_on(on)

    def camera_stayed_off(self, session: StreamSession):
        self._win._camera_off_now(session)
        self._win._bus.camera_on_changed.emit(False)

    def mic_reconnected(self, source: Source):
        self._win._on_stream_reconnected(source)
        self._win._show_camera_off()

    def stream_moved(self, source: Source):
        self._win._on_stream_reconnected(source)

    def lost_contact(self, source: Source):
        self._win._start_reconnecting_animation("Lost the phone - reconnecting")
        self._win._flow.begin_recovery(source)

    def park(self, source: Source):
        self._win._flow.park(source)

    def show_slow(self, camera_limited: bool = False):
        self._win._show_slow(True, camera_limited=camera_limited)


# ── What the stream flow sees of the window ──────────────────────────────────
class _FlowHost:
    """The window as stream_flow.FlowHost: its widgets, banners, saved settings and plugins, for StreamFlow."""

    def __init__(self, win: "TelescopeWindow"):
        self._win = win
        self.bus = win._bus

    def plugins(self) -> list:
        return [self._win._view] + self._win._plugins  # the view first: it crops before the plugins see a frame

    def plugin(self, name: str):
        return self._win._plugin(name)

    def call(self, plugins: list, hook: str, *args):
        self._win._call(plugins, hook, *args)

    def source_by_id(self, source_id: Optional[str]) -> Source:
        return self._win._source_by_id(source_id)

    def plugin_source(self, source_id: str) -> Optional[Source]:
        return self._win._sources.get(source_id)

    def selected_source(self) -> Source:
        return self._win._selected_source()

    def later(self, fn: Callable[[], None]):
        QTimer.singleShot(0, fn)

    def start_problem(self, title: str, text: str):
        self._win.show_issue("start", Issue(title, text, [BannerAction("Try again", self._win.start_again())]))

    def clear_issue(self, key: str):
        self._win.clear_issue(key)

    def set_start_button(self, streaming: bool):
        self._win._set_start_button(streaming=streaming)

    def enable_start(self, enabled: bool):
        self._win._start_btn.setEnabled(enabled)

    def source_status(self, source: Source, msg: str, kind: str):
        self._win._source_status(source, msg, kind)

    def show_stream(self, source: Source):
        self._win._show_stream(source)

    def show_stopped(self):
        self._win._show_focus_stopped()

    def show_camera_off(self):
        self._win._show_camera_off()

    def refresh_tiles(self):
        self._win._refresh_tiles()

    def clear_pending_resolution(self, source: Source):
        self._win._clear_pending_resolution(source)

    def set_behind(self, behind: bool, source: Source):
        self._win._set_behind(behind, source)

    def save_profile(self, source_id: Optional[str], plugins: list):
        self._win._save_profile(source_id, plugins)

    def apply_device_profile(self, source_id: Optional[str], plugins: list):
        self._win._apply_device_profile(source_id, plugins)

    def schedule_save(self):
        self._win.schedule_save()

    def canvas_size(self) -> tuple:
        return self._win.canvas_size()

    def set_up_slots(self, then: Callable[[], None]):
        self._win._set_up_slots(then)

    def set_camera_on(self, on: bool):
        self._win.set_camera_on(on)

    def apply_state(self, session_id: int, state: dict, source: Source):
        self._win._apply_state(session_id, state, source)

    def new_worker(self, source: Source, url: str, auth, params: tuple, pipeline: list, canvas: tuple):
        return self._win._new_worker(source, url, auth, params, pipeline, canvas)

    def end_worker(self, worker):
        self._win._end_worker(worker)


# ── Main window ───────────────────────────────────────────────────────────────
class TelescopeWindow(QMainWindow):
    _sig_raise = pyqtSignal()
    _sig_action = pyqtSignal(object, object)  # request, {"done": Event} the answer goes in
    _sig_canvas_reload_done = pyqtSignal(bool, str, bool, str)  # ok, msg, restart_stream, command to run by hand
    _sig_slots_ready = pyqtSignal(bool, str, str)  # ok, msg, command to run by hand
    _sig_extras_removed = pyqtSignal(bool, str)
    _sig_people = pyqtSignal(object, list)  # a stream's worker, [Person], from its people thread
    _sig_vision_state = pyqtSignal(str)

    def __init__(self):
        super().__init__()
        self.setWindowTitle("Telescope (dev)" if dev_profile.active() else "Telescope")
        self.setMinimumSize(ui_px(560), ui_px(520))
        self.resize(ui_px(1380), ui_px(900))

        self._bus     = EventBus()
        self._vision  = Vision(on_people=self._sig_people.emit, on_state=self._sig_vision_state.emit)  # (HostVision)
        self._sig_people.connect(self._on_people)
        self._sig_vision_state.connect(self._bus.vision_state_changed)
        self._view    = ViewController()  # the shown stream's zoom, pan, flip and rotation (HostView)
        self._view.setup(self, self._bus, self._vision)
        self._reported_shown: Optional[bool] = None  # what window_shown last said
        self._bus.resolution_change_requested.connect(self._on_resolution_pending)
        # Both restart the phone's camera, so the stream gets a moment before it counts as behind.
        self._bus.resolution_change_requested.connect(lambda _w, _h: self._settle_stream())
        self._bus.camera_switched.connect(lambda _cam: self._settle_stream())
        # Animated "Stream dropped - reconnecting..." status.
        self._reconnecting_timer: Optional[QTimer] = None
        self._reconnecting_base = ""
        self._reconnecting_dots = 1
        self._plugins: list[TelescopePlugin] = []
        self._builtin_plugins: list = []  # every built-in one, turned off or not (load_plugins)
        self._added_plugins: list = []  # the ones the user installed that are running
        self._plugin_problems: dict = {}  # name: why an added plugin that's on didn't start
        self._plugins_by_name: dict[str, TelescopePlugin] = {}
        self._shortcut_actions: list = []
        # Plugin defaults; lets us reset before applying device profile.
        self._plugin_defaults: dict[str, dict] = {}

        self._save_failure_notified = False

        # Where streams come from: each phone (made when first needed), and the StreamSources plugins offer, by id.
        self._source_host = _SourceHost(self)
        self._phones: dict = {}
        self._sources: dict[str, Source] = {}
        self._flow = StreamFlow(_FlowHost(self))  # starting, stopping and focus; its Sessions has the state
        self._camera_on_shown = True  # what camera_on_changed last said, to catch a pick with another camera choice
        self._orphans: set = set()  # workers that outlived Stop's wait, kept referenced until they finish

        self._save_timer = QTimer(self)
        self._save_timer.setSingleShot(True)
        self._save_timer.timeout.connect(self.save_now)

        self._tray: Optional[QSystemTrayIcon] = None
        self._keep_in_tray = False

        self.setWindowIcon(create_app_icon(64))
        self._build_ui()
        self._setup_tray()

        self._sig_raise.connect(self._tray_show)
        self._sig_action.connect(self._on_remote_action)
        self._sig_canvas_reload_done.connect(self._on_canvas_reload_done)
        self._sig_slots_ready.connect(self._on_slots_ready)
        self._slots_then: Optional[list] = None  # while the extra cameras are being set up: what waits for them
        self._sig_extras_removed.connect(self._on_extras_removed)
        self._extras_done = None
        QTimer.singleShot(0, self._flow.announce_idle_outputs)  # once the plugins are in
        self._bus.phones_changed.connect(lambda _n: self._show_add_button())
        self._bus.stream_sources_changed.connect(lambda _s: self._show_add_button())
        self._thumb_timer = QTimer(self)
        self._thumb_timer.setInterval(100)  # tiny pictures, so 10 fps costs next to nothing
        self._thumb_timer.timeout.connect(self._update_thumbnails)
        self._bus.mic_changed.connect(self._on_mic_changed)
        self._bus.source_selected.connect(self._on_source_selected)
        self._bus.camera_on_changed.connect(lambda on: setattr(self, "_camera_on_shown", on))
        self._mic_state = (False, False)  # the microphone card: on, muted
        self._tray_shows = None
        for signal in (self._bus.stream_started, self._bus.stream_stopped, self._bus.camera_on_changed):
            signal.connect(self._show_tray_state)

    @property
    def _sessions(self) -> Sessions:
        """Which sources stream, which one the panels show, and what's starting (kept by the stream flow)."""
        return self._flow.sessions

    @property
    def _session(self) -> Optional[StreamSession]:
        """The stream the panels show."""
        return self._sessions.shown

    @_session.setter
    def _session(self, session: Optional[StreamSession]):
        # Tests and teardown: make session the panels' stream, or with None drop theirs.
        sessions = self._sessions
        if session is None:
            if sessions.focus is not None:
                sessions.forget(sessions.focus)
                sessions.focus = sessions.streams[0] if sessions.streams else None
            return
        sessions.add(session.source, session)
        sessions.focus = session.source
        if sessions.main is None:
            sessions.main = session.source

    @property
    def _phone(self) -> "PhoneSource":
        """The picked phone (or, with a StreamSource picked, the one with no phone)."""
        conn = self._plugin("connection")
        picked = conn.selected_device if conn else None
        return self._phone_source(None if picked in self._sources else picked)

    def _phone_source(self, pid: Optional[str]) -> "PhoneSource":
        if pid not in self._phones:
            self._phones[pid] = PhoneSource(self._source_host, pid)
        return self._phones[pid]

    @property
    def _worker(self) -> Optional[StreamWorker]:
        return self._session.worker if self._session else None

    @property
    def _ctrl(self) -> Optional[PhoneControlClient]:
        return self._session.client if self._session else None

    def register_plugin(self, plugin: TelescopePlugin, added: bool = False):
        """Set plugin up and give it its place in the window. added: one the user installed, so one that raises here
        is taken back out again (and the exception passed on) rather than left half in."""
        panel = header_widget = None
        tray: list = []
        try:
            plugin.setup(self, self._bus)
            panel = plugin.create_panel()
            if panel:
                region = plugin.panel_region if plugin.panel_region in self._panels else "left"
                self._panels[region].append(panel)
                # Forced: the mode hasn't changed, but the panel set has.
                self._refresh_layout(force=True)
            header_widget = plugin.create_header_widget()
            if header_widget:
                slot = self._header_right_slot if plugin.header_side == "right" else self._header_slot
                slot.addWidget(header_widget)
            self._plugins.append(plugin)
            if self._tray is not None:
                menu = self._tray.contextMenu()
                for action in plugin.create_tray_actions():
                    tray.append(action)
                    action.setParent(menu)
                    menu.insertAction(self._tray_quit_sep, action)  # between Show and Quit
            if plugin.name:
                self._plugins_by_name[plugin.name] = plugin
            if plugin.name:
                self._plugin_defaults[plugin.name] = plugin.get_config()
        except BaseException:
            if added:
                self._take_out(plugin, panel, header_widget, tray)
            raise
        try:
            self._shortcut_actions.extend(plugin.create_actions())
        except Exception:
            logging.exception("Plugin %s couldn't list its shortcut actions", plugin.name)

    def _take_out(self, plugin: TelescopePlugin, panel, header_widget, tray: list):
        """Undo what register_plugin did for a plugin that failed part way (what it connected to the bus stays)."""
        if plugin in self._plugins:
            self._plugins.remove(plugin)
        if self._plugins_by_name.get(plugin.name) is plugin:
            del self._plugins_by_name[plugin.name]
            self._plugin_defaults.pop(plugin.name, None)
        for widget in (panel, header_widget):
            if widget is None:
                continue
            for panels in self._panels.values():
                if widget in panels:
                    panels.remove(widget)
            widget.setParent(None)
            widget.deleteLater()
        if panel is not None:
            self._refresh_layout(force=True)
        for action in tray:
            action.setParent(None)
            if self._tray is not None:
                self._tray.contextMenu().removeAction(action)

    def load_plugins(self, builtins: list):
        """Register the built-in plugins (all but those turned off in Plugins, never the required ones), then the
        added plugins turned on there. One of those that can't load is left out, with a banner saying which."""
        self._builtin_plugins = list(builtins)
        off, on = plugin_loader.builtin_off(), plugin_loader.optional_on()
        for plugin in builtins:
            if plugin.required or (plugin.name in on if plugin.optional else plugin.name not in off):
                self.register_plugin(plugin)
        loaded, failed = plugin_loader.load_enabled(reserved={p.name for p in builtins})
        problems = {item.name: (item.title, item.error) for item in failed}
        for plugin in loaded:
            try:
                self.register_plugin(plugin, added=True)
            except (Exception, SystemExit) as exc:
                logging.exception("Plugin %s failed while starting; leaving it out", plugin.name)
                problems[plugin.name] = (plugin.title or plugin.name, f"{type(exc).__name__}: {exc}"[:300])
        self._added_plugins = [p for p in loaded if p in self._plugins]
        self._plugin_problems = {name: error for name, (_title, error) in problems.items()}
        if problems:
            titles = [title for title, _error in problems.values()]
            self.show_issue("plugins", Issue(
                f"{_joined(titles)} didn't start" if len(titles) < 4 else f"{len(titles)} plugins didn't start",
                "Telescope works without it. Plugins in the settings menu says what went wrong." if len(titles) == 1
                else "Telescope works without them. Plugins in the settings menu says what went wrong.",
                [BannerAction("Plugins…", self.show_plugins)], kind="warn"))
        self._remind_plugins_off()

    def _remind_plugins_off(self):
        """Once per change: which built-in plugins are off, so a card that's gone doesn't look like a bug."""
        try:
            names = plugin_loader.unnoticed_off()
        except OSError:
            return
        titles = [p.title or p.name for p in self._builtin_plugins if p.name in names and not p.required]
        if not titles:
            return

        def noticed():
            try:
                plugin_loader.notice_off()
            except OSError:
                logging.exception("Couldn't note that the plugins turned off were seen")

        def open_plugins():
            noticed()
            self.show_plugins()

        self.show_issue("plugins_off", Issue(
            f"{_joined(titles)} {'is' if len(titles) == 1 else 'are'} turned off",
            "You turned this off in Plugins. To get it back, open Plugins in the settings menu and switch it on."
            if len(titles) == 1 else
            "You turned these off in Plugins. To get them back, open Plugins in the settings menu and switch them on.",
            [BannerAction("Plugins…", open_plugins)], kind="warn", on_dismiss=noticed))

    def plugins_off(self) -> list:
        """The built-in plugins turned off this run (not the optional ones that were never switched on)."""
        return [p for p in self._builtin_plugins if p not in self._plugins and not p.optional]

    def show_plugins(self):
        """The Plugins dialog: every plugin, built in or added, with a switch for each one that can be turned off."""
        from telescope.widgets.plugins_dialog import PluginsDialog
        dlg = getattr(self, "_plugins_dlg", None)
        if dlg is None or not dlg.isVisible():
            # Not modal: it would block every other Telescope window, which can still come up over it
            dlg = self._plugins_dlg = PluginsDialog(self, self._builtin_plugins, {p.name for p in self._plugins},
                                                    self._plugin_problems, self.restart_app, self.is_streaming)
            dlg.setAttribute(Qt.WidgetAttribute.WA_DeleteOnClose)
            dlg.destroyed.connect(lambda: setattr(self, "_plugins_dlg", None))
        dlg.show()
        dlg.raise_()
        dlg.activateWindow()

    def restart_app(self) -> bool:
        """Start a new copy of Telescope and quit this one; False if the new one couldn't be started."""
        if dev_profile.supervised():  # dev_desktop.py starts it again, in the same terminal and profile
            self._tray_close_notified = True
            self._shut_down_and_exit(dev_profile.RESTART_STREAMING if self.is_streaming() else dev_profile.RESTART_EXIT)
            return True
        try:
            relaunch(restart_argv())
        except (OSError, ValueError):
            logging.exception("Couldn't start Telescope again")
            return False
        self._tray_quit()
        return True

    def _plugin(self, name: str) -> Optional[TelescopePlugin]:
        """Look up a registered plugin by its declared name, or None."""
        return self._plugins_by_name.get(name)

    def apply_saved_config(self):
        """Restore persisted config into all registered plugins. Call after all plugins registered."""
        self._apply_config(load_config())
        self._each_plugin("on_plugins_ready")  # what was said during setup, for those set up after
        if (backup := take_reset_notice()) is not None:
            where = f" A copy of the old file is at {backup}." if backup else ""
            self.show_issue("config", Issue(
                "Your settings couldn't be read",
                "Telescope started with its defaults, so paired phones need pairing again." + where, kind="warn"))

    # ── UI construction ───────────────────────────────────────────────────────

    def _build_ui(self):
        # Regions allow rearrangement on resize without plugin awareness.
        self._panels: dict[str, list[QWidget]] = {"left": [], "center": [], "right": []}
        self._layout_mode: Optional[str] = None

        root = QWidget()
        root.setObjectName("body_root")
        self.setCentralWidget(root)
        root_lay = QVBoxLayout(root)
        root_lay.setContentsMargins(0, 0, 0, 0)
        root_lay.setSpacing(0)

        root_lay.addWidget(self._build_header())
        self._banners = BannerArea()
        root_lay.addWidget(self._banners)
        self._tiles = StreamTiles()
        self._tiles.focus_requested.connect(self.focus_stream)
        self._tiles.stop_requested.connect(self.stop_stream)
        root_lay.addWidget(self._tiles)
        root_lay.addWidget(self._build_body(), 1)
        root_lay.addWidget(self._build_footer())

    def _build_header(self) -> QWidget:
        bar = QWidget()
        bar.setObjectName("header_bar")
        lay = QHBoxLayout(bar)
        # Same side margin as the body, so header content lines up with the rails below it.
        lay.setContentsMargins(16, 10, 16, 10)
        lay.setSpacing(12)

        self._header_slot = QHBoxLayout()
        self._header_slot.setContentsMargins(0, 0, 0, 0)
        self._header_slot.setSpacing(14)
        lay.addLayout(self._header_slot)

        lay.addStretch()

        self._header_right_slot = QHBoxLayout()
        self._header_right_slot.setContentsMargins(0, 0, 0, 0)
        self._header_right_slot.setSpacing(8)
        lay.addLayout(self._header_right_slot)

        self._menu_btn = QPushButton()
        self._menu_btn.setObjectName("icon_btn")
        self._menu_btn.setFixedSize(36, 36)
        self._menu_btn.setIcon(create_vector_icon("gear", theme.TEXT_DIM))
        self._menu_btn.setIconSize(QSize(19, 19))
        self._menu_btn.setToolTip("Settings")
        self._menu_btn.setAccessibleName("Settings")
        self._menu_btn.clicked.connect(self._show_settings_menu)
        lay.addWidget(self._menu_btn)

        self._add_btn = QPushButton()
        self._add_btn.setObjectName("icon_btn")
        self._add_btn.setFixedSize(36, 36)
        self._add_btn.setIcon(create_vector_icon("plus", theme.TEXT_DIM))
        self._add_btn.setIconSize(QSize(18, 18))
        self._add_btn.setToolTip("Stream another camera at the same time, to a virtual camera of its own")
        self._add_btn.setAccessibleName("Add camera")
        self._add_btn.clicked.connect(self._show_add_menu)
        self._add_btn.setVisible(False)
        lay.addWidget(self._add_btn)

        self._start_btn = QPushButton("Start streaming")
        self._start_btn.setObjectName("start_btn")
        self._start_btn.setProperty("uiRole", "primary")
        self._start_btn.setProperty("streaming", False)
        self._start_btn.setIconSize(QSize(14, 14))
        self._start_btn.clicked.connect(self._toggle)
        self._set_start_button(streaming=False)
        lay.addWidget(self._start_btn)

        return bar

    def _set_start_button(self, streaming: bool):
        """Keep label, icon, and color consistent."""
        self._streaming_label = streaming
        verb = "Stop" if streaming else "Start"
        if not streaming and not self.is_camera_on():
            self._start_btn.setText("Start mic only")
        else:
            self._start_btn.setText(verb if self._layout_mode == "one" else f"{verb} streaming")
        self._start_btn.setIcon(
            create_vector_icon("stop" if streaming else "play", "#ffffff"))
        self._start_btn.setProperty("streaming", streaming)
        self._start_btn.setStyle(self._start_btn.style())

    def _apply_header_density(self):
        """Shorten the start button to its verb once the window narrows, since the fixed-size header row crushes the device picker below 560px otherwise."""
        self._set_start_button(getattr(self, "_streaming_label", False))

    def _build_body(self) -> QWidget:
        body = QWidget()
        body.setObjectName("body_root")
        self._body_lay = QHBoxLayout(body)
        self._body_lay.setContentsMargins(16, 16, 16, 16)
        self._body_lay.setSpacing(14)

        self._columns: list[QScrollArea] = []
        self._column_layouts: list[QVBoxLayout] = []
        for _ in range(3):
            scroll = QScrollArea()
            scroll.setWidgetResizable(True)
            scroll.setHorizontalScrollBarPolicy(Qt.ScrollBarPolicy.ScrollBarAlwaysOff)
            scroll.setFrameShape(QFrame.Shape.NoFrame)
            content = QWidget()
            content.setObjectName("rail_content")
            col_lay = QVBoxLayout(content)
            # Right inset: scrollbar won't cover card border.
            col_lay.setContentsMargins(0, 0, 6, 0)
            col_lay.setSpacing(14)
            scroll.setWidget(content)
            self._body_lay.addWidget(scroll)
            self._columns.append(scroll)
            self._column_layouts.append(col_lay)

        return body

    def _build_footer(self) -> QWidget:
        bar = QWidget()
        bar.setObjectName("footer_bar")
        lay = QHBoxLayout(bar)
        lay.setContentsMargins(16, 8, 16, 8)
        lay.setSpacing(14)

        # No caption (already reads as status); elides long messages.
        self._status_lbl = ElidingLabel("Not streaming")
        set_status_kind(self._status_lbl, "status_dim")
        lay.addWidget(self._status_lbl, 1)

        self._fps_lbl = QLabel("—")
        self._fps_lbl.setObjectName("fps_lbl")
        lay.addWidget(self._footer_pill("FPS", self._fps_lbl))

        self._net_lbl = QLabel("—")
        self._net_lbl.setObjectName("fps_lbl")
        lay.addWidget(self._footer_pill("Throughput", self._net_lbl))

        return bar

    def _footer_pill(self, caption: str, value: QLabel) -> QFrame:
        pill = QFrame()
        pill.setObjectName("footer_pill")
        pill.setFixedHeight(ui_px(24))
        lay = QHBoxLayout(pill)
        lay.setContentsMargins(10, 0, 10, 0)
        lay.setSpacing(6)
        cap = QLabel(caption)
        cap.setObjectName("footer_label")
        lay.addWidget(cap)
        lay.addWidget(value)
        return pill

    def _show_settings_menu(self):
        menu = QMenu(self)
        off = len(self.plugins_off())
        plugins = QAction(f"Plugins… ({off} off)" if off else "Plugins…", menu)
        plugins.triggered.connect(self.show_plugins)
        placed = False
        for p in self._plugins:
            try:
                entries = p.create_menu_actions()
            except Exception:
                logging.exception("Plugin %s couldn't list its menu entries", p.name)
                continue
            for entry in entries:
                if not placed and isinstance(entry, QAction) and (entry.isSeparator() or entry.isCheckable()):
                    menu.addAction(plugins)  # last of the dialogs, before the divider and the toggles
                    placed = True
                if isinstance(entry, QMenu):
                    entry.setParent(menu, entry.windowFlags())  # keeps it a popup, owned by this menu
                    menu.addMenu(entry)
                else:
                    entry.setParent(menu)
                    menu.addAction(entry)
        if not placed:
            menu.addAction(plugins)
        menu.exec(self._menu_btn.mapToGlobal(
            self._menu_btn.rect().bottomLeft()) + QPoint(0, 6))
        menu.deleteLater()  # rebuilt on every click; without this each one stays alive under the window

    def _layout_mode_for(self, width: int) -> str:
        if width >= ui_px(_WIDTH_THREE_COL):
            return "three"
        if width >= ui_px(_WIDTH_TWO_COL):
            return "two"
        return "one"

    def _refresh_layout(self, force: bool = False):
        mode = self._layout_mode_for(self.width())
        if mode == self._layout_mode and not force:
            return
        self._layout_mode = mode
        self._apply_header_density()

        if mode == "three":
            groups = [self._panels["left"], self._panels["center"], self._panels["right"]]
            widths = [ui_px(_RAIL_WIDTH), None, ui_px(_RAIL_WIDTH)]
        elif mode == "two":
            groups = [self._panels["left"] + self._panels["right"], self._panels["center"], []]
            widths = [ui_px(_RAIL_WIDTH_SOLO), None, None]
        else:
            groups = [self._panels["center"] + self._panels["left"] + self._panels["right"], [], []]
            widths = [None, None, None]

        # Panels a plugin hid on purpose stay hidden; only placement changes here.
        hidden = {id(p) for ps in self._panels.values() for p in ps
                  if p.parentWidget() is not None and p.isHidden()}
        for col_lay in self._column_layouts:
            while col_lay.count():
                item = col_lay.takeAt(0)
                if item.widget():
                    item.widget().setParent(None)

        center_panels = set(id(p) for p in self._panels["center"])
        for scroll, col_lay, panels, width in zip(
                self._columns, self._column_layouts, groups, widths):
            scroll.setVisible(bool(panels))
            if not panels:
                continue
            if width is None:
                scroll.setMinimumWidth(0)
                scroll.setMaximumWidth(16777215)
                scroll.setSizePolicy(QSizePolicy.Policy.Expanding,
                                     QSizePolicy.Policy.Expanding)
            else:
                scroll.setFixedWidth(width)
                scroll.setSizePolicy(QSizePolicy.Policy.Fixed,
                                     QSizePolicy.Policy.Expanding)
            self._body_lay.setStretch(self._columns.index(scroll),
                                      1 if width is None else 0)
            has_center = False
            for panel in panels:
                stretch = 1 if id(panel) in center_panels else 0
                has_center = has_center or bool(stretch)
                col_lay.addWidget(panel, stretch)
                panel.setVisible(id(panel) not in hidden)
            if not has_center:
                col_lay.addStretch()

    def resizeEvent(self, event):
        super().resizeEvent(event)
        self._refresh_layout()

    def focusNextPrevChild(self, next: bool) -> bool:
        # Qt tabs in the order widgets were made; cards build theirs out of order, and rows come and go
        return tab_in_reading_order(self, next)

    def schedule_save(self):
        self._save_timer.start(500)

    @staticmethod
    def _call(plugins, hook: str, *args):
        """Call a hook on each of plugins; one that raises is logged and the others still hear about it."""
        for p in plugins:
            try:
                getattr(p, hook)(*args)
            except Exception:
                logging.exception("Plugin %s failed in %s", p.name, hook)

    def _each_plugin(self, hook: str, *args):
        self._call(self._plugins, hook, *args)

    @staticmethod
    def _config_of(plugin: TelescopePlugin, saved):
        """plugin.get_config(), or what was saved before if it raises or gives something the settings file can't hold
        (so one plugin can't stop every save)."""
        try:
            cfg = plugin.get_config()
        except Exception:
            logging.exception("Plugin %s couldn't report its settings; keeping the saved ones", plugin.name)
            return saved
        if not isinstance(cfg, dict) or not saveable(cfg):
            logging.error("Plugin %s gave settings that can't be saved (get_config() has to return a dict of what "
                          "JSON holds); keeping the saved ones", plugin.name)
            return saved
        return cfg

    def _config_to_update(self) -> Optional[dict]:
        # None when the saved file is there but unreadable right now: saving over it would lose every other phone's settings
        try:
            return load_config(strict=True)
        except OSError:
            logging.exception("Couldn't read the settings file; not saving over it")
            return None

    def save_now(self):
        cfg = self._config_to_update()
        if cfg is None:
            self.schedule_save()  # tries again shortly
            return
        global_pcfg = cfg.setdefault("plugin_configs", {})
        conn = self._plugin("connection")
        selected = conn.selected_device if conn else None
        cfg["selected_device"] = selected
        for p in self._plugins:
            if p.name and not p.device_local:
                if (c := self._config_of(p, global_pcfg.get(p.name))) is not None:
                    global_pcfg[p.name] = c
        # Per-device plugin configs: the panels' stream's, and the main stream's for a plugin that stays with it
        for p in self._plugins:
            key = self._profile_key(p)
            if key and p.name and p.device_local:
                dev_pcfg = cfg.setdefault("devices", {}).setdefault(key, {}).setdefault("plugin_configs", {})
                if (c := self._config_of(p, dev_pcfg.get(p.name))) is not None:
                    dev_pcfg[p.name] = c
        self._drop_unchanged(cfg)
        if self._flow.slot_memory:
            cfg["output_slots"] = dict(self._flow.slot_memory)
        if save_config(cfg):
            self._save_failure_notified = False
        elif not self._save_failure_notified:
            # Warn once per failure streak; debounce prevents repeat notifications.
            self._save_failure_notified = True
            logging.error("Failed to save settings")
            self.send_notification(
                "Telescope - Save failed",
                "Could not save settings. Check disk space and permissions.",
            )

    def _profile_key(self, plugin: TelescopePlugin) -> Optional[str]:
        """Whose settings plugin has now: the picked source's, or for one that doesn't follow the panels, the main
        stream's."""
        if not plugin.follows_focus and self._sessions.main is not None:
            return self._sessions.main.id
        conn = self._plugin("connection")
        return conn.selected_device if conn else None

    def _save_profile(self, name: Optional[str], plugins: list):
        """Store plugins' device-local settings as name's."""
        cfg = self._config_to_update()
        if cfg is None or not name:
            return
        pcfg = cfg.setdefault("devices", {}).setdefault(name, {}).setdefault("plugin_configs", {})
        for p in plugins:
            if p.name and p.device_local:
                if (c := self._config_of(p, pcfg.get(p.name))) is not None:
                    pcfg[p.name] = c
        self._drop_unchanged(cfg)
        save_config(cfg)

    def _drop_unchanged(self, cfg: dict):
        """A source that's only remembered once changed (a browser) keeps no settings that are still the defaults."""
        devices = cfg.get("devices", {})
        for name in [n for n in devices if getattr(self._sources.get(n), "remember_changes_only", False)]:
            pcfg = devices[name].get("plugin_configs", {})
            for pname in [k for k, v in pcfg.items() if v == self._plugin_defaults.get(k)]:
                del pcfg[pname]
            if not pcfg:
                del devices[name]

    def has_device_settings(self, name: str) -> bool:
        cfg = load_config()
        return bool(cfg.get("devices", {}).get(name, {}).get("plugin_configs"))

    def _apply_device_profile(self, name: Optional[str], plugins: Optional[list] = None):
        cfg = load_config()
        pcfg = cfg.get("devices", {}).get(name, {}).get("plugin_configs", {}) if name else {}
        for p in self._plugins if plugins is None else plugins:
            if p.name and p.device_local:
                p.set_config(self._plugin_defaults.get(p.name, {}))
                if p.name in pcfg:
                    self._set_plugin_config(p, pcfg[p.name])

    def _set_plugin_config(self, plugin: TelescopePlugin, cfg):
        """Load a saved config; one that won't load (hand-edited, another version's) leaves the plugin on its defaults."""
        try:
            plugin.set_config(cfg)
            return
        except Exception:
            logging.exception("Saved settings for %s couldn't be loaded; keeping what does load", plugin.name)
        # One bad field shouldn't reset the rest: take the fields that load on their own, the defaults for the others
        good = dict(self._plugin_defaults.get(plugin.name, {}))
        for key, value in (cfg.items() if isinstance(cfg, dict) else ()):
            try:
                plugin.set_config({**good, key: value})
                good[key] = value
            except Exception:
                logging.warning("Saved %s setting %r couldn't be loaded; using its default", plugin.name, key)
        plugin.set_config(good)

    def switch_device(self, prev_name, new_name: Optional[str]):
        """Switch device profile; save old before applying new."""
        cfg = self._config_to_update()
        if cfg is not None and prev_name:
            prev_pcfg = cfg.setdefault("devices", {}).setdefault(prev_name, {}).setdefault("plugin_configs", {})
            for p in self._plugins:
                if p.name and p.device_local:
                    if (c := self._config_of(p, prev_pcfg.get(p.name))) is not None:
                        prev_pcfg[p.name] = c
        if cfg is not None:
            self._drop_unchanged(cfg)
            save_config(cfg)

        # A start still waking would stream the old phone
        was_streaming = self._session is not None or self._sessions.waking
        if was_streaming:
            self._flow.stop_all()

        if cfg is not None:
            cfg["selected_device"] = new_name
            save_config(cfg)
        self._apply_device_profile(new_name)
        self._bus.device_changed.emit(new_name or "")

        if was_streaming:
            self._flow.start()

    def forget_device_settings(self, name: str):
        """Drop a removed phone's per-device settings from the config."""
        cfg = self._config_to_update()
        self._flow.slot_memory.pop(name, None)
        if cfg is not None and (cfg.get("devices", {}).pop(name, None) is not None
                                or cfg.get("output_slots", {}).pop(name, None) is not None):
            save_config(cfg)

    def move_device_settings(self, old: str, new: str):
        """A phone back with a new id (Telescope reinstalled on it): its settings and virtual camera go with it."""
        if old in self._flow.slot_memory:
            self._flow.slot_memory[new] = self._flow.slot_memory.pop(old)
        cfg = self._config_to_update()
        if cfg is None:
            return
        devices = cfg.get("devices")
        if isinstance(devices, dict) and old in devices:
            devices[new] = devices.pop(old)
        slots = cfg.get("output_slots")
        if isinstance(slots, dict) and old in slots:
            slots[new] = slots.pop(old)
        if cfg.get("selected_device") == old:
            cfg["selected_device"] = new
        save_config(cfg)

    def _shutdown_plugins(self):
        for p in self._plugins:
            try:
                p.shutdown()
            except Exception:
                logging.exception("Plugin %s failed to shut down", p.name)
        self._vision.shutdown()

    def reconnect_stream(self, source_id: Optional[str] = None):
        """Restart the panels' stream, or source_id's, to pick up changed connection settings."""
        self._flow.reconnect_stream(source_id)

    def is_restarting(self) -> bool:
        return self._sessions.restarting

    def is_streaming(self) -> bool:
        """Whether a stream is running, the camera off (just the mic) included."""
        return bool(self._sessions.streams)

    def is_streaming_from(self, source_id: Optional[str]) -> bool:
        """Whether source_id streams (or a start is waking it), whether or not the panels show it."""
        return self._sessions.is_streaming_from(source_id)

    def stream_count(self) -> int:
        return len(self._sessions.streams)

    def is_starting(self) -> bool:
        return self._sessions.is_starting()

    def stop_stream(self, source_id: Optional[str] = None):
        """Stop the panels' stream, or source_id's; safe no-op if idle."""
        self._flow.stop_stream(source_id)

    def update_stream_output(self, width=UNCHANGED, height=UNCHANGED, fps=UNCHANGED, source_id=None):
        """Push output geometry/fps to worker. No-op if not streaming."""
        if source_id is None:
            worker = self._worker
        else:
            source = self._sessions.find(source_id)
            worker = source.session.worker if source is not None else None
        if worker is None:
            return
        kwargs = {}
        if width is not UNCHANGED:  kwargs["width"] = width
        if height is not UNCHANGED: kwargs["height"] = height
        if fps is not UNCHANGED:    kwargs["fps"] = fps
        if kwargs:
            worker.update_output(**kwargs)

    def _on_stream_reconnected(self, source: Optional[Source] = None):
        source = source or self._worker_source(self.sender())
        if source is not None:
            self._flow.stream_reconnected(source)

    def _move_signals(self, worker) -> list:
        """A worker's hand-over signals and where they go (a stand-in worker in the tests may have none)."""
        pairs = ((getattr(worker, "moved", None), self._on_stream_moved),
                 (getattr(worker, "move_failed", None), self._on_stream_move_failed))
        return [(signal, slot) for signal, slot in pairs if signal is not None]

    def _on_stream_moved(self, url: str):
        source = self._worker_source(self.sender())
        if source is not None and hasattr(source, "on_moved"):
            source.on_moved(url)

    def _on_stream_move_failed(self, url: str):
        source = self._worker_source(self.sender())
        if source is not None and hasattr(source, "on_move_failed"):
            source.on_move_failed(url)

    def hand_over_stream(self, route, source_id: Optional[str] = None) -> bool:
        """Move a running phone stream onto route without a gap; False if it can't try (see PhoneSource.hand_over)."""
        source = self._sessions.find(source_id) if source_id else self._active_source()
        hand_over = getattr(source, "hand_over", None)
        return bool(hand_over and source.session is not None and hand_over(route))

    def _apply_config(self, cfg: dict):
        if not cfg:
            return
        # config.py's load_config() already ran migration; cfg is always current here
        selected    = cfg.get("selected_device")
        global_pcfg = cfg.get("plugin_configs", {})
        slots = cfg.get("output_slots")
        self._flow.slot_memory = {k: v for k, v in slots.items() if isinstance(v, int)} if isinstance(slots, dict) else {}

        conn = self._plugin("connection")
        for p in self._plugins:
            if not p.name or p.device_local:
                continue
            if p.name in global_pcfg:
                self._set_plugin_config(p, global_pcfg[p.name])
        self._apply_device_profile(selected)
        # Sync connection plugin profile after device profile applied.
        if conn:
            conn.sync_active_profile()

    def _toggle(self):
        if self._sessions.preparing:                return
        if self._sessions.streams or self._sessions.waking: self._flow.stop_all()
        else:                              self._flow.start()

    def add_stream_source(self, source):
        old = self._sources.get(source.id)
        if old is not None and old._source is not source:
            old._source = source  # the same one offered again (a new name): a stream from it carries on
        elif old is None:
            self._sources[source.id] = PluginSource(self._source_host, source)
        self._bus.stream_sources_changed.emit([(s.id, s.name) for s in self._sources.values()])
        if len(self._sessions.streams) > 1:
            self._refresh_tiles()  # a new name for one that streams

    def remove_stream_source(self, source_id: str):
        if source_id not in self._sources:
            return
        if self.is_streaming_from(source_id):
            self.stop_stream(source_id)
        conn = self._plugin("connection")
        if conn is not None and conn.selected_device == source_id and not self._sessions.streams:
            conn.select(next((sid for sid in self._sources if sid != source_id), None))
        del self._sources[source_id]
        self._bus.stream_sources_changed.emit([(s.id, s.name) for s in self._sources.values()])

    def stream_source(self, source_id: str):
        """Start source_id: picked and streamed when nothing streams yet, else next to the others."""
        if self._sessions.is_starting() or self.is_streaming_from(source_id) or source_id not in self._sources:
            return
        if self._sessions.streams:
            self.add_stream(source_id)
            return
        conn = self._plugin("connection")
        if conn is not None and conn.selected_device != source_id:
            conn.select(source_id)
        self._flow.start(interactive=False)

    def stream_output(self, source_id: str) -> str:
        source = self._sessions.find(source_id)
        return vcam.slot_label(source.slot) if source is not None else ""

    def _source_by_id(self, source_id: Optional[str]) -> Source:
        return self._sources.get(source_id) or self._phone_source(source_id)

    def _selected_source(self) -> Source:
        conn = self._plugin("connection")
        return self._source_by_id(conn.selected_device) if conn else self._phone_source(None)

    def _active_source(self) -> Source:
        """The panels' stream's source, the one a start is waking, or while idle the one Start would stream from."""
        return self._flow.active_source()

    def _worker_source(self, worker) -> Optional[Source]:
        """The source whose worker sent a signal (None: one that's been let go); called directly, the active one."""
        if worker is None:
            return self._active_source()
        return self._sessions.by_worker(worker)

    def _tell(self, source: Source, hook: str, *args):
        self._flow.tell(source, hook, *args)





    def _source_status(self, source: Source, msg: str, kind: str):
        """A source's own status line, shown in the footer while the panels show it."""
        source.status = (msg, kind)
        if source is self._active_source():
            self._set_status(msg, kind)

    def _new_worker(self, source: Source, url: str, auth, params: tuple, pipeline: list, canvas: tuple
                    ) -> StreamWorker:
        """A started video worker for source, with its signals coming here."""
        w, h, fps = params
        worker = StreamWorker(
            url=url, width=w, height=h, fps=fps,
            frame_pipeline=pipeline,
            canvas_width=canvas[0], canvas_height=canvas[1],
            auth=auth,
            open_reader=source.open_reader,
            slot=source.slot,
        )
        worker.status.connect(self._on_worker_status)
        worker.reconnected.connect(self._on_stream_reconnected)
        for signal, slot in self._move_signals(worker):
            signal.connect(slot)
        if source.slot == 0:
            worker.vcam_opened.connect(self._bus.vcam_opened)
        worker.start()
        return worker

    def _show_stream(self, source: Source):
        """The footer for the stream the panels show now."""
        self._clear_pending_resolution(source)
        self._fps_lbl.setText(source.fps_text)
        self._fps_lbl.setToolTip("")
        self._net_lbl.setStyleSheet("")
        self._net_lbl.setText(source.net_text)
        self._set_status(*source.status)

    def _end_worker(self, worker: StreamWorker):
        for signal, slot in ((worker.status, self._on_worker_status),
                             (worker.reconnected, self._on_stream_reconnected),
                             (worker.vcam_opened, self._bus.vcam_opened), *self._move_signals(worker)):
            try:
                signal.disconnect(slot)
            except (TypeError, RuntimeError, ValueError):
                pass  # already disconnected
        worker.request_stop()
        self._vision.drop_stream(worker)
        # Bounded wait so a stalled read can't freeze the UI; if it doesn't finish in time, let it keep unwinding in the background.
        if not worker.wait(5000):
            logging.warning("Stream worker did not stop within 5s; abandoning it in the background")
            # Dropping the last reference to a running QThread aborts the app, so hold it until it ends.
            self._orphans.add(worker)
            worker.finished.connect(lambda w=worker: self._orphans.discard(w))

    # ── Several streams: which one the panels show ────────────────────────

    def focus_stream(self, source_id: str):
        """Point the panels at source_id's stream (a tile was clicked)."""
        self._flow.focus_stream(source_id)

    def pick_source(self, source_id: Optional[str]) -> bool:
        return self._flow.pick_source(source_id)

    def add_stream(self, source_id: str, before: Optional[Callable[[], None]] = None):
        self._flow.add_stream(source_id, before)

    def make_main(self, source_id: str) -> bool:
        return self._flow.make_main(source_id)

    def main_stream(self) -> Optional[tuple]:
        return self._flow.main_stream()

    def _refresh_tiles(self):
        """The tile row: one per stream while there's more than one."""
        entries = []
        for s in self._sessions.streams:
            if s.recovering:
                state, kind = "Reconnecting", "status_warn"
            elif s.connected or s.session.worker is None:
                state, kind = "Live", "status_ok"
            else:
                state, kind = "Connecting", "status_dim"
            entries.append((s.id or "", s.name, vcam.slot_label(s.slot), state, kind, s is self._sessions.focus))
        if self._sessions.waking and self._sessions.wake_source is not None and self._sessions.streams:
            s = self._sessions.wake_source
            entries.append((s.id or "", s.name, vcam.slot_label(self._flow.free_slot(s)), "Starting", "status_dim",
                            s is self._sessions.focus))
        if self._sessions.pending is not None and self._sessions.streams:
            s = self._sessions.pending
            entries.append((s.id or "", s.name, vcam.slot_label(self._flow.free_slot(s)), "Waiting", "status_dim",
                            s is self._sessions.focus))
        self._tiles.show_streams(entries)
        if len(entries) > 1:
            if not self._thumb_timer.isActive():
                self._thumb_timer.start()
        else:
            self._thumb_timer.stop()
        self._show_add_button()

    def _update_thumbnails(self):
        if not self.isVisible():
            return
        for s in self._sessions.streams:
            worker = s.session.worker if s.session is not None else None
            self._tiles.set_frame(s.id or "", worker.latest_frame() if worker is not None else None)

    def _add_candidates(self) -> list:
        """[(id, name)] of what Add camera offers: every phone and StreamSource that isn't streaming."""
        conn = self._plugin("connection")
        phones = [(p.id, p.name) for p in getattr(conn, "phones", [])] if conn is not None else []
        pending = self._sessions.pending.id if self._sessions.pending is not None else None
        return [(sid, name) for sid, name in phones + [(s.id, s.name) for s in self._sources.values()]
                if not self.is_streaming_from(sid) and sid != pending]

    def _show_add_button(self):
        self._add_btn.setVisible(bool(self._sessions.streams) and len(self._sessions.streams) < vcam.MAX_SLOTS
                                 and bool(self._add_candidates()))
        self._add_btn.setEnabled(not self._sessions.waking)

    def _show_add_menu(self):
        menu = QMenu(self)
        for sid, name in self._add_candidates():
            menu.addAction(name).triggered.connect(lambda _=False, s=sid: self.add_stream(s))
        if not menu.isEmpty():
            menu.exec(self._add_btn.mapToGlobal(self._add_btn.rect().bottomLeft()) + QPoint(0, 6))
        menu.deleteLater()

    def _set_up_slots(self, then):
        """Set up the extra virtual cameras (asking first, as it takes a password), then call then."""
        if self._slots_then is not None:
            self._slots_then.append(then)  # already under way: then goes once they're ready, after the others
            return
        if not self._ask_slots():
            return
        self._slots_then = [then]
        self._set_status("Setting up more virtual cameras…", "dim")

        def work():
            command = ""
            try:
                if IS_LINUX:
                    from telescope.platform.linux import v4l2_add_devices
                    result = v4l2_add_devices([vcam.slot_label(n) for n in range(1, vcam.MAX_SLOTS)
                                               if not vcam.slot_ready(n)])
                    ok, msg, command = result.ok, result.message, result.command or ""
                else:
                    from telescope.platform.windows import register_unitycapture
                    ok, msg = register_unitycapture(vcam.MAX_SLOTS)
            except Exception as exc:  # still report back, so a later Add camera can try again
                logging.exception("Setting up more virtual cameras failed")
                ok, msg = False, str(exc)
            self._sig_slots_ready.emit(ok, msg, command)

        threading.Thread(target=work, daemon=True).start()

    def remove_extra_cameras(self, on_done=None):
        extras = [n for n in range(1, vcam.MAX_SLOTS) if vcam.slot_ready(n)]
        if self._extras_done is not None or not extras or any(s.slot for s in self._sessions.streams):
            if on_done is not None:
                on_done(False, "Stop the streams going to them first" if extras else "There are none to remove")
            return
        self._extras_done = on_done or (lambda ok, msg: None)

        def work():
            try:
                with vcam.device_released():  # the wait screen lets go of them
                    if IS_LINUX:
                        from telescope.platform.linux import v4l2_remove_devices
                        result = v4l2_remove_devices([d for d in map(vcam.slot_device, extras) if d])
                        ok, msg = result.ok, result.message
                    else:
                        from telescope.platform.windows import register_unitycapture
                        ok, msg = register_unitycapture(1, reset=True)
            except Exception as exc:
                logging.exception("Removing the extra virtual cameras failed")
                ok, msg = False, str(exc)
            self._sig_extras_removed.emit(ok, msg)

        threading.Thread(target=work, daemon=True).start()

    def _on_extras_removed(self, ok: bool, msg: str):
        done, self._extras_done = self._extras_done, None
        self._flow.announce_idle_outputs()
        if done is not None:
            done(ok, msg)

    def _ask_slots(self) -> bool:
        names = f"{vcam.slot_label(1)} to {vcam.slot_label(vcam.MAX_SLOTS - 1)}"
        box = QMessageBox(self)
        box.setWindowTitle("Set up more virtual cameras")
        box.setText(f"To stream several cameras at once, Telescope adds more virtual cameras ({names}). "
                    + ("This asks for your password." if IS_LINUX else "Windows asks for permission."))
        box.setStandardButtons(QMessageBox.StandardButton.Ok | QMessageBox.StandardButton.Cancel)
        box.setDefaultButton(QMessageBox.StandardButton.Ok)
        return box.exec() == QMessageBox.StandardButton.Ok

    def _on_slots_ready(self, ok: bool, msg: str, command: str):
        waiting, self._slots_then = self._slots_then or [], None
        if self._sessions.focus is not None:
            self._set_status(*self._sessions.focus.status)
        if ok:
            self.clear_issue("vcam")
            self._flow.announce_idle_outputs()
            self._sessions.start_later.extend(waiting)  # a phone wakes before the next one can start
            self._flow.after_start()
            return
        if msg == CANCELLED:
            return
        self.show_issue("vcam", Issue("Couldn't add more virtual cameras", msg,
                                      [copy_action(command)] if command else [], kind="warn",
                                      details=command))

    # ── Camera off: the mic alone ─────────────────────────────────────────

    def is_camera_on(self) -> bool:
        return not self._active_source().camera_off

    def is_camera_off_auto(self) -> bool:
        source = self._active_source()
        return source.camera_off and source.camera_off_auto

    def can_turn_camera_off(self) -> tuple:
        """(whether the camera can go off now, and why not); it takes the microphone to have anything to stream."""
        session = self._session
        source = self._active_source()
        if not source.mic_alone:
            return False, "Only a phone can turn its camera off and keep streaming its mic."
        if len(self._sessions.streams) > 1 or (self._sessions.streams and source.session is None):
            return False, "Only while one camera streams."
        if not self._mic_enabled():
            return False, "Turn on the phone mic first. With the camera off, the mic is all that streams."
        if session is not None and source.camera_toggle is False:
            return False, "Update Telescope on the phone to turn its camera off while streaming."
        return True, ""

    def set_camera_on(self, on: bool, auto: bool = False):
        """Turn the camera off (the stream carries on with just the mic) or back on; before a start, picks what it
        starts with. auto: Automatic streaming did it, not the user."""
        source = self._active_source()
        if on == (not source.camera_off):
            if on or not auto:
                source.camera_off_auto = False  # a choice by hand isn't the rule's to undo
            return
        if not on and not self.can_turn_camera_off()[0]:
            return
        source.camera_off = not on
        source.camera_off_auto = auto and not on
        logging.info("Camera %s%s", "on" if on else "off", " (automatic)" if auto else "")
        session = self._session
        if session is not None:
            session.client.send(action="camera_on", value=1 if on else 0)
            if on:
                self._camera_on_now(session)
            else:
                self._camera_off_now(session)
        else:
            self._set_start_button(streaming=self._sessions.waking)
        self._bus.camera_on_changed.emit(on)

    def _on_source_selected(self, _sid: str):
        """Each source keeps its own camera choice, so picking another can change what Start does."""
        if not self._sessions.streams and self.is_camera_on() != self._camera_on_shown:
            self._set_start_button(streaming=self._sessions.waking)
            self._bus.camera_on_changed.emit(self.is_camera_on())

    def _camera_off_now(self, session: StreamSession):
        source = session.source  # the panels may have moved on, e.g. a camera that didn't come back once one was added
        source.end_recovery()
        if session.worker is not None:
            self._end_worker(session.worker)
            self._sessions.add(source, replace(session, worker=None))
            self._flow.announce_idle_outputs()  # its virtual camera gets the wait screen
        self._clear_pending_resolution(source)
        self._set_behind(False, source)
        self._tell(source, "on_camera_off")
        if source is self._sessions.focus:
            self._show_camera_off()

    def _camera_on_now(self, session: StreamSession):
        session.source.stop_watching_alive()
        session.source.end_recovery()
        self._sessions.add(session.source, replace(
            session, worker=self._flow.start_worker(session.url, session.client.auth, session.source)))
        self._tell(session.source, "on_camera_on")
        self._set_status("Turning the phone's camera on…", "dim")
        session.source.camera_turned_on(session.id)

    def _show_camera_off(self):
        self._fps_lbl.setText("—")
        self._fps_lbl.setToolTip("")
        self._net_lbl.setStyleSheet("")
        self._net_lbl.setText("—")
        self._source_status(self._active_source(), "Streaming the mic, camera off", "ok")
        self._active_source().watch_alive()

    def _mic_enabled(self) -> bool:
        return bool((self.plugin_config("microphone") or {}).get("enabled"))

    def _on_mic_changed(self, enabled: bool, muted: bool):
        self._mic_state = (enabled, muted)
        if not enabled and not self.is_camera_on():
            self.set_camera_on(True)  # with neither, the stream would carry nothing
        self._show_tray_state()

    def _show_tray_state(self, *_):
        """The tray icon and its tooltip say what's streaming: a red dot for the camera, a mic for the mic."""
        streaming = bool(self._sessions.streams)
        camera = streaming and self.is_camera_on()
        enabled, muted = self._mic_state
        mic = ("muted" if muted else "on") if streaming and enabled else None
        if self._tray is None or (camera, mic) == self._tray_shows:
            return
        self._tray_shows = (camera, mic)
        self._tray.setIcon(create_tray_icon(camera, mic))
        if not streaming:
            tip = "Telescope"
        elif camera:
            tip = "Telescope: streaming the camera" + {None: "", "on": " and mic", "muted": ", mic muted"}[mic]
        else:
            tip = "Telescope: streaming the mic, camera off" + (" (muted)" if mic == "muted" else "")
        self._tray.setToolTip(tip)

    def _show_focus_stopped(self):
        self._fps_lbl.setText("—")
        self._net_lbl.setStyleSheet("")
        self._net_lbl.setText("—")
        self._set_status("Not streaming", "dim")
        self._fps_lbl.setToolTip("")

    def _drain_phone_stops(self, timeout: float = 2.0):
        """Wait for remote stops to complete, but never block quit indefinitely."""
        deadline = time.monotonic() + timeout
        for source in (*self._phones.values(), *self._sources.values()):
            source.drain_stops(deadline)

    def canvas_size(self) -> tuple:
        setup = self._plugin("setup")
        return setup.get_canvas_dims() if setup else (None, None)

    def restart_vcam_canvas(self, w, h, on_done=None):
        """Stop stream, optionally reload the vcam driver, restart stream."""
        if len(self._sessions.streams) > 1:  # the reload needs every virtual camera free, and would drop the extra ones
            if on_done is not None:
                on_done(False, "Stop all but one camera first, then apply again")
            return
        self._vcam_reload_callback = on_done
        was_streaming = self._session is not None or self._sessions.waking  # a start still waking restarts after it too
        self._sessions.canvas_restart = self._sessions.canvas_restart or was_streaming
        old_worker = self._worker  # capture before _stop() clears it
        # Desktop-side driver reload only - the phone keeps streaming through it.
        if was_streaming:
            self._flow.stop_for_restart()
        # Idle: nothing to stop, and a stream_stopped now would tell Startup the user stopped one

        if IS_LINUX:
            self._set_status("Reloading v4l2loopback…", "dim")

            def worker():
                if old_worker:
                    old_worker.wait(5000)
                from telescope.platform.linux import v4l2_reload
                try:
                    with vcam.device_released():
                        result = v4l2_reload()
                except Exception as exc:  # still report back, so the stream restarts and the dialog frees up
                    logging.exception("Loopback reload failed")
                    self._sig_canvas_reload_done.emit(False, str(exc), was_streaming, "")
                    return
                self._sig_canvas_reload_done.emit(result.ok, result.message, was_streaming,
                                                  result.command or "")

            threading.Thread(target=worker, daemon=True).start()
        else:
            self._set_status("Restarting stream…", "dim")

            def worker():
                if old_worker:
                    old_worker.wait(5000)
                self._sig_canvas_reload_done.emit(True, "", was_streaming, "")

            threading.Thread(target=worker, daemon=True).start()

    def _on_canvas_reload_done(self, ok: bool, msg: str, restart_stream: bool, command: str = ""):
        restart = restart_stream and self._sessions.canvas_restart  # not after a Stop since
        if restart_stream:
            self._sessions.canvas_restart = False
        if ok:
            self._set_status(f"Loopback reloaded: {msg}" if IS_LINUX else "Canvas updated", "ok")
            if restart:
                self.start_stream()  # not over a stream started while the reload ran
        else:
            self._set_status("Not streaming" if command else f"Reload failed: {msg}", "dim" if command else "err")
        cb = getattr(self, "_vcam_reload_callback", None)
        self._vcam_reload_callback = None
        if cb:
            cb(ok, msg, command)  # whoever asked shows a command to run by hand
        elif command and not ok:
            self.show_issue("vcam", Issue("Resize the virtual camera", msg, [copy_action(command)],
                                          kind="warn", details=command))

    def _codec_trouble(self, source: Source, state: dict) -> bool:
        """Whether a stream the panels don't show asks its phone for H.264 (Light, the default) and the phone says it
        can't: no encoder at all, or one that failed."""
        if "cameras" in state and "h264" not in (state.get("codecs") or ()):
            trouble = True  # a phone leaves codecs out when it's only MJPEG
        else:
            trouble = bool(state.get("codec_error"))
        if not trouble:
            return False
        cfg = load_config().get("devices", {}).get(source.id, {}).get("plugin_configs", {}).get("stream_output", {})
        return cfg.get("format") != "mjpeg"

    def _apply_state(self, session_id: int, state: dict, source: Optional[Source] = None):
        source = source or self._active_source()
        if source.session is None or source.session.id != session_id:
            return  # a session stopped or switched since the fetch began: its state isn't this stream's
        try:
            PhoneState.from_dict(state)
        except PhoneStateError:
            logging.exception("Phone sent a malformed /v1/state response - not applying it")
            self._set_status("The phone sent data this version can't read. Update both apps to the same version.", "err")
            return
        if source is not self._sessions.focus and self._codec_trouble(source, state):
            if self._sessions.is_starting():
                # The panels belong to that start for now
                self._sessions.held_states[source.id] = (source, session_id, state)
            else:
                self._flow.move_focus(source)  # Stream output falls back, or stops and says why, for the panels' stream
        # Plugins get the dict as sent; PhoneState above only checks it
        if source is self._sessions.focus:
            self._bus.phone_state_updated.emit(state)
        if source.session is None or source.session.id != session_id:
            return  # something listening (Monitoring, too hot) stopped the stream just now
        self._tell(source, "on_phone_state", state)
        if state:
            source.on_phone_state(state)

    def _setup_tray(self):
        self._tray_close_notified = False
        if not QSystemTrayIcon.isSystemTrayAvailable():
            self._tray = None
            return

        self._tray = QSystemTrayIcon(create_app_icon(22), self)
        self._tray.setToolTip("Telescope")

        menu = QMenu()
        show_action = QAction("Show", self)
        quit_action = QAction("Quit", self)
        show_action.triggered.connect(self._tray_show)
        quit_action.triggered.connect(self._tray_quit)
        menu.addAction(show_action)
        self._tray_quit_sep = menu.addSeparator()
        menu.addAction(quit_action)
        self._tray.setContextMenu(menu)

        self._tray.activated.connect(self._on_tray_activated)
        self._tray.show()

    def _tray_show(self):
        self.showNormal()
        self.raise_()
        self.activateWindow()

    def toggle_window(self) -> bool:
        if self.isVisible() and not self.isMinimized() and self.isActiveWindow():
            if self._tray is not None:
                self.hide()
            else:
                self.showMinimized()
            return False
        self._tray_show()
        return True

    def stop_all_streams(self):
        if self._sessions.streams or self._sessions.waking:
            self._flow.stop_all()

    def shortcut_actions(self) -> list:
        return list(self._shortcut_actions)

    def run_remote_action(self, request: dict) -> dict:
        """From the single-instance listener's thread: run a `telescope --action` request on this one and wait for
        its answer."""
        box = {"done": threading.Event()}
        self._sig_action.emit(request, box)
        if not box["done"].wait(5):
            return {"ok": False, "text": "Telescope didn't answer in time."}
        return box["reply"]

    def _on_remote_action(self, request, box):
        try:
            plugin = self._plugin("shortcuts")
            box["reply"] = plugin.handle_remote(request) if plugin is not None else {
                "ok": False, "text": "This Telescope has no shortcuts."}
        except Exception:
            logging.exception("A --action request failed")
            box["reply"] = {"ok": False, "text": "That failed; Copy diagnostics has the details."}
        finally:
            box["done"].set()

    def start_hidden(self):
        """For --minimized: live in the tray, or minimized where there's no tray."""
        if self._tray is None:
            self.showMinimized()
        self._report_shown()  # never shown, so no event says so

    def quit_app(self):
        self._tray_quit()

    def start_stream(self, interactive: bool = True):
        if not self._sessions.streams and not self._sessions.waking and not self._sessions.preparing:
            self._flow.start(interactive)

    def start_again(self, source_id: Optional[str] = None, before: Optional[Callable[[], None]] = None
                    ) -> Callable[[], None]:
        source_id = self._flow.start_id if source_id is None else source_id
        return lambda: self._start_source(source_id, before)

    def focused_source_id(self) -> Optional[str]:
        return self._active_source().id

    def background_streams(self) -> list:
        return [(s.id, s.name, s.session.client) for s in self._sessions.background()]

    def view(self):
        return self._view.view

    def set_view(self, **changes):
        self._view.set_view(**changes)

    def take_view(self, owner: str) -> bool:
        return self._view.take_view(owner)

    def steer_view(self, owner: str, zoom: float, pan_x: float, pan_y: float):
        self._view.steer_view(owner, zoom, pan_x, pan_y)

    def release_view(self, owner: str):
        self._view.release_view(owner)

    def vision_state(self) -> str:
        return self._vision.state()

    def person_mask(self, frame):
        return self._vision.person_mask(frame)

    def want_people(self, owner: str, on: bool):
        self._vision.want_people(owner, on)

    def _on_people(self, worker, people: list):
        source = self._sessions.by_worker(worker)
        if source is not None:
            self._bus.people_seen.emit(source.id or "", people)

    def device_config(self, source_id: Optional[str], plugin_name: str) -> dict:
        cfg = load_config()
        for key in ("devices", source_id, "plugin_configs", plugin_name):
            cfg = cfg.get(key) if isinstance(cfg, dict) and key else None
        return cfg if isinstance(cfg, dict) else {}

    def _start_source(self, source_id: Optional[str], before: Optional[Callable[[], None]] = None):
        """Start source_id by itself, or next to what streams already (a stopped or failed one, from its banner).
        before runs once the panels show it, so a setting it changes is that source's."""
        if not self._sessions.streams:
            conn = self._plugin("connection")
            if conn is not None and source_id is not None and conn.selected_device != source_id:
                conn.select(source_id)
            if before is not None:
                before()
            self.start_stream()
        else:
            self.add_stream(source_id, before)

    def set_keep_in_tray(self, keep: bool):
        self._keep_in_tray = keep

    def show_issue(self, key: str, issue: Issue):
        diagnostics.events.note(f"Banner: {issue.title}. {issue.text}" if issue.text else f"Banner: {issue.title}")
        self._banners.show_issue(key, issue)

    def clear_issue(self, key: Optional[str] = None, issue: Optional[Issue] = None):
        self._banners.clear_issue(key, issue)

    def diagnostics_report(self) -> str:
        state = {"Streaming": ("yes" if self.is_camera_on() else "yes, camera off") if self.is_streaming() else "no"}
        if len(self._sessions.streams) > 1:
            state["Streams"] = str(len(self._sessions.streams))
        if off := self.plugins_off():
            state["Plugins off"] = ", ".join(p.name for p in off)
        if self._added_plugins or self._plugin_problems:
            added = [f"{p.name} (running)" for p in self._added_plugins]
            added += [f"{name} (didn't start)" for name in self._plugin_problems]
            state["Added plugins"] = ", ".join(added)
        state.update(self._vision.diagnostics())
        for plugin in self._plugins:
            try:
                state.update(plugin.diagnostics())
            except Exception as exc:
                state[plugin.name] = f"couldn't read ({type(exc).__name__})"
        return diagnostics.report(state, QApplication.platformName())

    def plugin_config(self, name: str) -> Optional[dict]:
        plugin = self._plugin(name)
        return plugin.get_config() if plugin else None

    def apply_preset(self, name: str, cfg: dict):
        plugin = self._plugin(name)
        if plugin:
            plugin.apply_preset(cfg)

    def _tray_quit(self):
        self._tray_close_notified = True
        self._shut_down()
        QApplication.quit()

    def _shut_down_and_exit(self, code: int):
        self._shut_down()
        QApplication.exit(code)

    def _flush_save(self):
        if self._save_timer.isActive():  # a setting changed just before quitting
            self._save_timer.stop()
            self.save_now()

    def _shut_down(self):
        """Everything quitting does, each step on its own, so one that fails can't keep the app from closing."""
        for step in (self._flow.stop_all, self._flush_save, self._drain_phone_stops, self._shutdown_plugins):
            try:
                step()
            except Exception:
                logging.exception("Quitting: %s failed", step.__name__)

    def _on_tray_activated(self, reason):
        if reason == QSystemTrayIcon.ActivationReason.Trigger:
            if self.isVisible():
                self.hide()
            else:
                self._tray_show()

    def send_notification(self, title: str, body: str, urgent: bool = True):
        if IS_LINUX and shutil.which("notify-send"):
            subprocess.Popen(
                ["notify-send", "-a", "Telescope", "-u", "critical" if urgent else "normal", "--", title, body],
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            )
        elif self._tray:
            icon = QSystemTrayIcon.MessageIcon.Warning if urgent else QSystemTrayIcon.MessageIcon.Information
            self._tray.showMessage(title, body, icon, 0)

    def _settle_stream(self):
        if (worker := self._worker) is not None:
            worker.settle()

    def _on_resolution_pending(self, w: int, h: int):
        """Resolution change sent; phone must reopen camera, so show pending state."""
        source = self._active_source()
        if source.pending_resolution_timer:
            source.pending_resolution_timer.stop()
        source.pending_resolution = (w, h)
        self._fps_lbl.setStyleSheet(f"color: {theme.WARN};")
        timer = QTimer(self)
        timer.setSingleShot(True)
        timer.timeout.connect(lambda: self._on_resolution_pending_timeout(source, timer))
        timer.start(8000)  # generous: camera reopen + a possible brief reconnect
        source.pending_resolution_timer = timer

    def _on_resolution_pending_timeout(self, source: Source, timer: QTimer):
        if source.pending_resolution_timer is not timer:
            return  # a newer change, or one that already landed
        source.pending_resolution = None
        source.pending_resolution_timer = None
        if source is self._active_source():
            self._fps_lbl.setStyleSheet(f"color: {theme.ERR};")
            QTimer.singleShot(4000, lambda: self._fps_lbl.setStyleSheet(""))

    def _clear_pending_resolution(self, source: Optional[Source] = None):
        source = source or self._active_source()
        if source.pending_resolution_timer:
            source.pending_resolution_timer.stop()
            source.pending_resolution_timer = None
        source.pending_resolution = None
        if source is self._active_source():
            self._fps_lbl.setStyleSheet("")

    def _start_reconnecting_animation(self, base_msg: str):
        """Show animated reconnecting status with 1/second dot progression."""
        diagnostics.events.note(f"Status: {base_msg}")
        self._reconnecting_base = base_msg
        self._reconnecting_dots = 1
        self._render_reconnecting_frame()
        if self._reconnecting_timer is None:
            timer = QTimer(self)
            timer.timeout.connect(self._tick_reconnecting_animation)
            self._reconnecting_timer = timer
        self._reconnecting_timer.start(1000)

    def _tick_reconnecting_animation(self):
        self._reconnecting_dots = (self._reconnecting_dots % 3) + 1
        self._render_reconnecting_frame()

    def _render_reconnecting_frame(self):
        # Deliberately bypasses _set_status(), which stops this animation's own timer as its first step.
        set_status_kind(self._status_lbl, "status_warn")
        self._status_lbl.setText(f"{self._reconnecting_base}{'.' * self._reconnecting_dots}")

    def _stop_reconnecting_animation(self):
        if self._reconnecting_timer:
            self._reconnecting_timer.stop()

    def _on_worker_status(self, kind: str, msg: str, source: Optional[Source] = None):
        source = source or self._worker_source(self.sender())
        if source is None:
            return  # a worker that's been let go
        source.note_status(kind, msg)
        if kind in ("ok", "waiting", "reconnecting") and len(self._sessions.streams) > 1:
            self._refresh_tiles()  # its tile says how it's doing
        if source is not self._active_source():
            self._background_status(source, kind)
            return
        if kind == "fps":
            self._fps_lbl.setText(msg.split("  ", 1)[0].removesuffix(" fps"))  # the status line already gives the size
            if (pending := self._active_source().pending_resolution) is not None:
                try:
                    size_text = msg.rsplit(" ", 1)[-1]
                    w_str, h_str = size_text.split("x")
                    if (int(w_str), int(h_str)) == pending:
                        self._clear_pending_resolution()
                except ValueError:
                    pass
        elif kind == "net":
            self._active_source().arrival_slow = False
            self._net_lbl.setText(msg)
            self._show_slow(False)
        elif kind == "net_warn":
            self._active_source().arrival_slow = True
            self._net_lbl.setText(msg)
            self._on_slow_arrival()
        elif kind == "ok":
            self._set_status(msg, "ok")
            self._set_behind(False)  # its banner goes with the rest below; a stream still behind says so again
            for key in self._banners.keys():  # whatever stopped the last Start is fixed now
                # h264 says why this very stream is MJPEG, and comes just before its reconnect; another stream's stop
                # still needs its Start
                if key != "h264" and (not key.startswith("stopped:") or key == f"stopped:{source.id}"):
                    self._banners.clear_issue(key)
            self._bus.stream_connected.emit(source.id or "")
        elif kind == "warn":
            self._set_status(msg, "warn")
        elif kind == "waiting":  # no first frame yet: maybe the route died between waking the phone and now
            self._set_status(msg, "warn")
            self._flow.begin_recovery()
        elif kind == "reconnecting":
            self._start_reconnecting_animation(msg)
            self._flow.begin_recovery()
        elif kind == "idle":
            self._clear_pending_resolution()
            self._fps_lbl.setText("—")
            self._net_lbl.setStyleSheet("")
            self._net_lbl.setText("—")
            self._set_status(msg, "dim")
            if self._session:
                # Stop disconnects the worker first, so this is a worker that ended by itself: tidy up like a Stop.
                logging.warning("The stream worker ended by itself")
                self._flow.stop()
                self.show_issue(f"stopped:{source.id}", Issue(
                    "The stream stopped unexpectedly", "Copy diagnostics has the details.",
                    [BannerAction("Start", lambda sid=source.id: self._start_source(sid))], kind="warn"))
        else:
            self._set_status(msg, "dim")

    def _background_status(self, source: Source, kind: str):
        """A stream the panels don't show: its tile says how it's doing, and a dropped one still finds its way back."""
        if kind in ("waiting", "reconnecting"):
            self._flow.begin_recovery(source)
        elif kind == "ok":
            self._bus.stream_connected.emit(source.id or "")
        elif kind == "idle" and source.session is not None:
            logging.warning("The stream worker for %s ended by itself", source.name)
            self._flow.stop(source=source)
            self.show_issue(f"stopped:{source.id}", Issue(
                f"The stream from {source.name} stopped unexpectedly", "Copy diagnostics has the details.",
                [BannerAction("Start", lambda sid=source.id: self._start_source(sid))], kind="warn"))

    def _on_slow_arrival(self):
        """Frames arrive under the rate asked for. If the phone's camera makes about that many, it's not the link."""
        session = self._session
        source = self._active_source()
        arrival = getattr(session.worker, "last_arrival_fps", None) if session else None
        if source.recovering or source.settling_reports or not arrival:
            self._show_slow(True)  # nothing to ask, or nothing to say yet: as before
        else:
            source.check_camera_rate(session, arrival)

    def _show_slow(self, slow: bool, camera_limited: bool = False):
        behind = slow and not camera_limited
        self._net_lbl.setStyleSheet(f"color: {theme.WARN};" if behind else "")
        tip = ""
        if slow and camera_limited:
            source = self._active_source()
            stepped, heat = getattr(source, "dynamic_fps", 0), getattr(source, "heat_fps", 0)
            if heat and (not stepped or heat <= stepped):
                tip = _HEAT_FPS_TIP.format(fps=heat)
            else:
                tip = _DYNAMIC_FPS_TIP.format(fps=stepped) if stepped else _CAMERA_LIMITED_TIP
        self._fps_lbl.setToolTip(tip)
        self._note_throughput(behind)

    def _note_throughput(self, behind: bool):
        """A dropped stream isn't slow, and the first report after it's back still counts the gap, so neither says."""
        source = self._active_source()
        if source.recovering:
            return
        if source.settling_reports:
            source.settling_reports -= 1
            return
        self._set_behind(behind)

    def _set_behind(self, behind: bool, source: Optional[Source] = None):
        source = source or self._active_source()
        if behind != source.behind:
            source.behind = behind
            self._bus.stream_behind.emit(source.id or "", behind)

    def _set_status(self, msg: str, kind: str):
        diagnostics.events.note(f"Status: {msg}")
        self._stop_reconnecting_animation()
        obj = {"ok": "status_ok", "warn": "status_warn",
               "err": "status_err", "dim": "status_dim"}.get(kind, "status_dim")
        set_status_kind(self._status_lbl, "status_dim" if obj == "status_ok" else obj)  # only trouble gets a colour
        self._status_lbl.setText(msg)

    def showEvent(self, event):
        super().showEvent(event)
        self._report_shown()

    def hideEvent(self, event):
        super().hideEvent(event)
        self._report_shown()

    def changeEvent(self, event):
        super().changeEvent(event)
        if event.type() == QEvent.Type.WindowStateChange:
            self._report_shown()

    def _report_shown(self):
        """Tells the plugins whether the window is on screen; the tray and minimized both count as not."""
        shown = self.isVisible() and not self.isMinimized()
        if shown != self._reported_shown:
            self._reported_shown = shown
            self._bus.window_shown.emit(shown)

    def closeEvent(self, event):
        if self._tray and (self._sessions.streams or self._keep_in_tray):
            event.ignore()
            self.hide()
            if not self._tray_close_notified:
                self._tray_close_notified = True
                self.send_notification(
                    "Telescope is still running",
                    ("Streaming continues in the background." if self._sessions.streams else
                     "It starts streaming by itself when it needs to.")
                    + " Right-click the tray icon to quit.",
                    urgent=False,
                )
        else:
            self._shut_down()
            event.accept()
            QApplication.quit()
