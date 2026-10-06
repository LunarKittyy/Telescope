import logging
import shutil
import socket
import subprocess
import threading
import time
from dataclasses import replace
from typing import Callable, Optional

from PyQt6.QtCore import QPoint, QSize, Qt, QTimer, pyqtSignal
from PyQt6.QtGui import QAction
from PyQt6.QtWidgets import (
    QApplication, QFrame, QHBoxLayout, QLabel,
    QMainWindow, QMenu, QMessageBox, QPushButton, QScrollArea,
    QSizePolicy, QSystemTrayIcon, QVBoxLayout, QWidget,
)

import update_guard
from telescope import dev_profile, diagnostics, theme, vcam
from telescope.config import DEVICE_LOCAL_PLUGINS, load_config, save_config
from telescope.models import PhoneState, PhoneStateError
from telescope.phone_client import PhoneControlClient
from telescope.platform import IS_LINUX
from telescope.platform.linux import CANCELLED
from telescope.plugin import UNCHANGED, EventBus, TelescopePlugin
from telescope.session import StreamSession
from telescope.sources import PhoneSource, PluginSource, Source
from telescope.stream import StreamWorker, guarded_step
from telescope.widgets.banner import BannerAction, BannerArea, Issue, copy_action
from telescope.widgets.tiles import StreamTiles
from telescope.widgets.common import (
    ElidingLabel, create_app_icon, create_tray_icon, create_vector_icon, set_status_kind, ui_px,
)

STATUS_COLORS = theme.STATUS_COLORS
_WIDTH_THREE_COL = 1300
_WIDTH_TWO_COL   = 900
# Both rails share one width so the preview sits on the window's centre line.
_RAIL_WIDTH       = 412
_RAIL_WIDTH_SOLO  = 440  # two-column mode: the one rail holding every card
_CAMERA_LIMITED_TIP = "The phone's camera is making fewer frames than asked for. Dim light slows it down."


# ── Single-instance enforcement ───────────────────────────────────────────────
# The guard checks it before an update's roll-back; a dev profile takes the next one so it runs next to the real app
_INSTANCE_PORT = update_guard.INSTANCE_PORT + (1 if dev_profile.active() else 0)


def acquire_single_instance(wait: float = 0.0) -> Optional[socket.socket]:
    """The single-instance socket, or None after asking the running copy to show itself.

    wait: keep retrying this long first, for a relaunch after an update while the old copy exits.
    """
    deadline = time.monotonic() + wait
    while True:
        srv = update_guard.instance_socket()
        try:
            srv.bind(("127.0.0.1", _INSTANCE_PORT))
            srv.listen(1)
            return srv
        except OSError:
            srv.close()
            if time.monotonic() >= deadline:
                break
            time.sleep(0.25)
    c = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    try:
        c.settimeout(1)
        c.connect(("127.0.0.1", _INSTANCE_PORT))
        c.sendall(b"raise")
    except Exception:
        pass
    finally:
        c.close()
    return None


def listen_for_raise(srv: socket.socket, raise_cb):
    srv.settimeout(1.0)
    while True:
        try:
            conn, _ = srv.accept()
        except socket.timeout:
            continue
        except OSError:
            break  # closed on quit
        try:
            conn.settimeout(1.0)
            if conn.recv(16) == b"raise":
                raise_cb()
        except OSError:
            pass  # a connection that says nothing or resets mustn't stop every later raise
        finally:
            conn.close()


# ── Main window ───────────────────────────────────────────────────────────────
class TelescopeWindow(QMainWindow):
    _sig_raise = pyqtSignal()
    _sig_canvas_reload_done = pyqtSignal(bool, str, bool, str)  # ok, msg, restart_stream, command to run by hand
    _sig_slots_ready = pyqtSignal(bool, str, str)  # ok, msg, command to run by hand
    _sig_extras_removed = pyqtSignal(bool, str)

    def __init__(self):
        super().__init__()
        self.setWindowTitle("Telescope (dev)" if dev_profile.active() else "Telescope")
        self.setMinimumSize(ui_px(560), ui_px(520))
        self.resize(ui_px(1380), ui_px(900))

        self._bus     = EventBus()
        self._bus.resolution_change_requested.connect(self._on_resolution_pending)
        # Both restart the phone's camera, so the stream gets a moment before it counts as behind.
        self._bus.resolution_change_requested.connect(lambda _w, _h: self._settle_stream())
        self._bus.camera_switched.connect(lambda _cam: self._settle_stream())
        # Animated "Stream dropped - reconnecting..." status.
        self._reconnecting_timer: Optional[QTimer] = None
        self._reconnecting_base = ""
        self._reconnecting_dots = 1
        self._plugins: list[TelescopePlugin] = []
        self._plugins_by_name: dict[str, TelescopePlugin] = {}
        # Plugin defaults; lets us reset before applying device profile.
        self._plugin_defaults: dict[str, dict] = {}

        # Each streaming source holds its StreamSession (worker and client); the id guards against stale async results.
        self._next_session_id = 1
        self._save_failure_notified = False

        # Where streams come from: each phone (made when first needed), and the StreamSources plugins offer, by id.
        self._phones: dict = {}
        self._sources: dict[str, Source] = {}
        self._streams: list[Source] = []  # the sources streaming now, in the order they started
        self._focus: Optional[Source] = None  # the stream the panels show (and plugins that follow them hear about)
        self._main: Optional[Source] = None  # the first stream: plugins that don't follow the panels (the mic) hear it
        self._stopping_all = False
        self._slot_memory: dict = {}  # source id: the extra virtual camera it went to last, so it goes there again
        self._camera_on_shown = True  # what camera_on_changed last said, to catch a pick with another camera choice
        self._waking = False
        self._wake_source: Optional[Source] = None  # the source a start is waking, which a stop meanwhile has to reach
        self._preparing = False  # _start is finding the phone; timers still fire meanwhile, so it must not start again
        self._orphans: set = set()  # workers that outlived Stop's wait, kept referenced until they finish
        self._restarting = False  # stopping only to start again (reconnect, virtual camera resize)
        self._start_id: Optional[str] = None  # what the last start was for, so its banner's Try again starts it again

        self._save_timer = QTimer(self)
        self._save_timer.setSingleShot(True)
        self._save_timer.timeout.connect(self.save_now)

        self._tray: Optional[QSystemTrayIcon] = None
        self._keep_in_tray = False

        self.setWindowIcon(create_app_icon(64))
        self._build_ui()
        self._setup_tray()

        self._sig_raise.connect(self._tray_show)
        self._sig_canvas_reload_done.connect(self._on_canvas_reload_done)
        self._sig_slots_ready.connect(self._on_slots_ready)
        self._slots_then = None
        self._sig_extras_removed.connect(self._on_extras_removed)
        self._extras_done = None
        QTimer.singleShot(0, self._announce_idle_outputs)  # once the plugins are in
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
    def _session(self) -> Optional[StreamSession]:
        """The stream the panels show."""
        return self._focus.session if self._focus is not None else None

    @_session.setter
    def _session(self, session: Optional[StreamSession]):
        # Tests and teardown: make session the panels' stream, or with None drop theirs.
        if session is None:
            if self._focus is not None:
                self._forget_stream(self._focus)
                self._focus = self._streams[0] if self._streams else None
            return
        self._set_session(session.source, session)
        self._focus = session.source
        if self._main is None:
            self._main = session.source

    def _set_session(self, source: Source, session: StreamSession):
        source.session = session
        if source not in self._streams:
            self._streams.append(source)

    def _forget_stream(self, source: Source):
        source.session = None
        if source in self._streams:
            self._streams.remove(source)

    @property
    def _phone(self) -> "PhoneSource":
        """The picked phone (or, with a StreamSource picked, the one with no phone)."""
        conn = self._plugin("connection")
        picked = conn.selected_device if conn else None
        return self._phone_source(None if picked in self._sources else picked)

    def _phone_source(self, pid: Optional[str]) -> "PhoneSource":
        if pid not in self._phones:
            self._phones[pid] = PhoneSource(self, pid)
        return self._phones[pid]

    @property
    def _worker(self) -> Optional[StreamWorker]:
        return self._session.worker if self._session else None

    @property
    def _ctrl(self) -> Optional[PhoneControlClient]:
        return self._session.client if self._session else None

    def register_plugin(self, plugin: TelescopePlugin):
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
                action.setParent(menu)
                menu.insertAction(self._tray_quit_sep, action)  # between Show and Quit
        if plugin.name:
            self._plugins_by_name[plugin.name] = plugin
        if plugin.name:
            self._plugin_defaults[plugin.name] = plugin.get_config()

    def _plugin(self, name: str) -> Optional[TelescopePlugin]:
        """Look up a registered plugin by its declared name, or None."""
        return self._plugins_by_name.get(name)

    def apply_saved_config(self):
        """Restore persisted config into all registered plugins. Call after all plugins registered."""
        self._apply_config(load_config())

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

        self._start_btn = QPushButton("Start Streaming")
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
            self._start_btn.setText(verb if self._layout_mode == "one" else f"{verb} Streaming")
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

        divider = QFrame()
        divider.setObjectName("header_divider")
        divider.setFixedWidth(1)
        divider.setFixedHeight(22)
        lay.addWidget(divider)

        fps_cap = QLabel("FPS")
        fps_cap.setObjectName("footer_label")
        lay.addWidget(fps_cap)

        self._fps_lbl = QLabel("—")
        self._fps_lbl.setObjectName("fps_lbl")
        self._fps_lbl.setMinimumWidth(ui_px(72))
        lay.addWidget(self._fps_lbl)

        divider2 = QFrame()
        divider2.setObjectName("header_divider")
        divider2.setFixedWidth(1)
        divider2.setFixedHeight(22)
        lay.addWidget(divider2)

        net_cap = QLabel("Throughput")
        net_cap.setObjectName("footer_label")
        lay.addWidget(net_cap)

        self._net_lbl = QLabel("—")
        self._net_lbl.setObjectName("fps_lbl")
        self._net_lbl.setMinimumWidth(ui_px(80))
        lay.addWidget(self._net_lbl)

        return bar

    def _show_settings_menu(self):
        menu = QMenu(self)
        for p in self._plugins:
            for entry in p.create_menu_actions():
                if isinstance(entry, QMenu):
                    entry.setParent(menu, entry.windowFlags())  # keeps it a popup, owned by this menu
                    menu.addMenu(entry)
                else:
                    entry.setParent(menu)
                    menu.addAction(entry)
        if menu.isEmpty():
            menu.deleteLater()
            return
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

    def schedule_save(self):
        self._save_timer.start(500)

    def _each_plugin(self, hook: str, *args):
        """Call a hook on every plugin; one that raises is logged and the others still hear about it."""
        for p in self._plugins:
            try:
                getattr(p, hook)(*args)
            except Exception:
                logging.exception("Plugin %s failed in %s", p.name, hook)

    @staticmethod
    def _config_of(plugin: TelescopePlugin, saved):
        """plugin.get_config(), or what was saved before if it raises (so one plugin can't stop every save)."""
        try:
            return plugin.get_config()
        except Exception:
            logging.exception("Plugin %s couldn't report its settings; keeping the saved ones", plugin.name)
            return saved

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
            if p.name and p.name not in DEVICE_LOCAL_PLUGINS:
                if (c := self._config_of(p, global_pcfg.get(p.name))) is not None:
                    global_pcfg[p.name] = c
        # Per-device plugin configs: the panels' stream's, and the main stream's for a plugin that stays with it
        for p in self._plugins:
            key = self._profile_key(p)
            if key and p.name and p.name in DEVICE_LOCAL_PLUGINS:
                dev_pcfg = cfg.setdefault("devices", {}).setdefault(key, {}).setdefault("plugin_configs", {})
                if (c := self._config_of(p, dev_pcfg.get(p.name))) is not None:
                    dev_pcfg[p.name] = c
        self._drop_unchanged(cfg)
        if self._slot_memory:
            cfg["output_slots"] = dict(self._slot_memory)
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
        if not plugin.follows_focus and self._main is not None:
            return self._main.id
        conn = self._plugin("connection")
        return conn.selected_device if conn else None

    def _save_profile(self, name: Optional[str], plugins: list):
        """Store plugins' device-local settings as name's."""
        cfg = self._config_to_update()
        if cfg is None or not name:
            return
        pcfg = cfg.setdefault("devices", {}).setdefault(name, {}).setdefault("plugin_configs", {})
        for p in plugins:
            if p.name and p.name in DEVICE_LOCAL_PLUGINS:
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
            if p.name and p.name in DEVICE_LOCAL_PLUGINS:
                p.set_config(self._plugin_defaults.get(p.name, {}))
                if p.name in pcfg:
                    self._set_plugin_config(p, pcfg[p.name])

    def _set_plugin_config(self, plugin: TelescopePlugin, cfg):
        """Load a saved config; one that won't load (hand-edited, another version's) leaves the plugin on its defaults."""
        try:
            plugin.set_config(cfg)
        except Exception:
            logging.exception("Saved settings for %s couldn't be loaded; using the defaults", plugin.name)
            plugin.set_config(self._plugin_defaults.get(plugin.name, {}))

    def switch_device(self, prev_name, new_name: Optional[str]):
        """Switch device profile; save old before applying new."""
        cfg = self._config_to_update()
        if cfg is not None and prev_name:
            prev_pcfg = cfg.setdefault("devices", {}).setdefault(prev_name, {}).setdefault("plugin_configs", {})
            for p in self._plugins:
                if p.name and p.name in DEVICE_LOCAL_PLUGINS:
                    if (c := self._config_of(p, prev_pcfg.get(p.name))) is not None:
                        prev_pcfg[p.name] = c
        if cfg is not None:
            self._drop_unchanged(cfg)
            save_config(cfg)

        was_streaming = self._session is not None or self._waking  # a start still waking would stream the old phone
        if was_streaming:
            self._stop_all()

        if cfg is not None:
            cfg["selected_device"] = new_name
            save_config(cfg)
        self._apply_device_profile(new_name)
        self._bus.device_changed.emit(new_name or "")

        if was_streaming:
            self._start()

    def forget_device_settings(self, name: str):
        """Drop a removed phone's per-device settings from the config."""
        cfg = self._config_to_update()
        self._slot_memory.pop(name, None)
        if cfg is not None and (cfg.get("devices", {}).pop(name, None) is not None
                                or cfg.get("output_slots", {}).pop(name, None) is not None):
            save_config(cfg)

    def _shutdown_plugins(self):
        for p in self._plugins:
            try:
                p.shutdown()
            except Exception:
                logging.exception("Plugin %s failed to shut down", p.name)

    def reconnect_stream(self):
        """Restart the panels' stream to pick up changed connection settings."""
        if self._session is None and not self._waking:  # a start still waking would use the old settings
            return
        source = self._active_source()
        self._stop_for_restart(source)
        self._start(source=source)

    def _stop_for_restart(self, source: Optional[Source] = None):
        """Stop a stream that starts again right after; stream_stopped handlers can tell with is_restarting()."""
        self._restarting = True
        try:
            self._stop(remote_stop=False, source=source, keep_focus=True)
        finally:
            self._restarting = False

    def is_restarting(self) -> bool:
        return self._restarting

    def is_streaming(self) -> bool:
        """Whether a stream is running, the camera off (just the mic) included."""
        return bool(self._streams)

    def is_streaming_from(self, source_id: Optional[str]) -> bool:
        """Whether source_id streams (or a start is waking it), whether or not the panels show it."""
        if self._waking and self._wake_source is not None and self._wake_source.id == source_id:
            return True
        return any(s.id == source_id for s in self._streams)

    def stream_count(self) -> int:
        return len(self._streams)

    def is_starting(self) -> bool:
        return self._waking or self._preparing

    def stop_stream(self, source_id: Optional[str] = None):
        """Stop the panels' stream, or source_id's; safe no-op if idle. Waking counts as active."""
        if source_id is not None:
            source = next((s for s in self._streams if s.id == source_id), None)
            if source is not None:
                self._stop(source=source)
        elif self._streams or self._waking:
            self._stop()

    def update_stream_output(self, width=UNCHANGED, height=UNCHANGED, fps=UNCHANGED):
        """Push output geometry/fps to worker. No-op if not streaming."""
        worker = self._worker
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
        if source is None:
            return
        source.end_recovery()
        self._refresh_tiles()
        source.settling_reports = 1  # the next throughput report still counts the gap
        session = source.session
        if session is None:
            return
        self._tell(source, "on_stream_start", session.url, session.client)

    def _apply_config(self, cfg: dict):
        if not cfg:
            return
        # config.py's load_config() already ran migration; cfg is always current here
        selected    = cfg.get("selected_device")
        global_pcfg = cfg.get("plugin_configs", {})
        slots = cfg.get("output_slots")
        self._slot_memory = {k: v for k, v in slots.items() if isinstance(v, int)} if isinstance(slots, dict) else {}

        conn = self._plugin("connection")
        for p in self._plugins:
            if not p.name or p.name in DEVICE_LOCAL_PLUGINS:
                continue
            if p.name in global_pcfg:
                self._set_plugin_config(p, global_pcfg[p.name])
        self._apply_device_profile(selected)
        # Sync connection plugin profile after device profile applied.
        if conn:
            conn.sync_active_profile()

    def _toggle(self):
        if self._preparing:                return
        if self._streams or self._waking: self._stop_all()
        else:                              self._start()

    def add_stream_source(self, source):
        old = self._sources.get(source.id)
        if old is not None and old._source is not source:
            old._source = source  # the same one offered again (a new name): a stream from it carries on
        elif old is None:
            self._sources[source.id] = PluginSource(self, source)
        self._bus.stream_sources_changed.emit([(s.id, s.name) for s in self._sources.values()])
        if len(self._streams) > 1:
            self._refresh_tiles()  # a new name for one that streams

    def remove_stream_source(self, source_id: str):
        if source_id not in self._sources:
            return
        if self.is_streaming_from(source_id):
            self.stop_stream(source_id)
        conn = self._plugin("connection")
        if conn is not None and conn.selected_device == source_id and not self._streams:
            conn.select(next((sid for sid in self._sources if sid != source_id), None))
        del self._sources[source_id]
        self._bus.stream_sources_changed.emit([(s.id, s.name) for s in self._sources.values()])

    def stream_source(self, source_id: str):
        """Start source_id: picked and streamed when nothing streams yet, else next to the others."""
        if self._waking or self._preparing or self.is_streaming_from(source_id) or source_id not in self._sources:
            return
        if self._streams:
            self.add_stream(source_id)
            return
        conn = self._plugin("connection")
        if conn is not None and conn.selected_device != source_id:
            conn.select(source_id)
        self._start(interactive=False)

    def stream_output(self, source_id: str) -> str:
        source = next((s for s in self._streams if s.id == source_id), None)
        return vcam.slot_label(source.slot) if source is not None else ""

    def _source_by_id(self, source_id: Optional[str]) -> Source:
        return self._sources.get(source_id) or self._phone_source(source_id)

    def _selected_source(self) -> Source:
        conn = self._plugin("connection")
        return self._source_by_id(conn.selected_device) if conn else self._phone_source(None)

    def _active_source(self) -> Source:
        """The panels' stream's source, the one a start is waking, or while idle the one Start would stream from."""
        if self._focus is not None and (self._focus.session is not None or not self._waking):
            return self._focus
        if self._waking and self._wake_source is not None:
            return self._wake_source  # the picker may have moved on already: a switch stops the wake after that
        return self._selected_source()

    def _worker_source(self, worker) -> Optional[Source]:
        """The source whose worker sent a signal (None: one that's been let go); called directly, the active one."""
        if worker is None:
            return self._active_source()
        return next((s for s in self._streams if s.session.worker is worker), None)

    def _tell(self, source: Source, hook: str, *args):
        """Call a stream hook on the plugins that hear about source: those following the panels if they show it, and
        those staying with the first stream (the mic) if it's that one."""
        for p in self._plugins:
            if source is (self._focus if p.follows_focus else self._main):
                try:
                    getattr(p, hook)(*args)
                except Exception:
                    logging.exception("Plugin %s failed in %s", p.name, hook)

    def _followers(self) -> list:
        return [p for p in self._plugins if p.follows_focus]

    def _each_follower(self, hook: str, *args):
        for p in self._plugins:
            if p.follows_focus:
                try:
                    getattr(p, hook)(*args)
                except Exception:
                    logging.exception("Plugin %s failed in %s", p.name, hook)

    def _source_status(self, source: Source, msg: str, kind: str):
        """A source's own status line, shown in the footer while the panels show it."""
        source.status = (msg, kind)
        if source is self._active_source():
            self._set_status(msg, kind)

    def _start(self, interactive: bool = True, source: Optional[Source] = None):
        """Ensure phone camera is running before reading frames; wake happens off-thread to avoid blocking UI."""
        conn = self._plugin("connection")
        if not conn:
            return
        self.clear_issue("start")
        source = source or self._selected_source()
        target = source.redirect() if not self._streams else None
        if target and target in self._sources:  # e.g. Browser camera picked with a browser already there
            if conn.selected_device != target:
                conn.select(target)
            source = self._sources[target]
        self._start_id = source.id
        self.clear_issue(f"stopped:{source.id}")
        self._preparing = True
        try:
            ok = source.prepare(interactive)
        except Exception:
            logging.exception("Getting %s ready to stream failed", source.name)
            self.show_issue("start", Issue("Couldn't start streaming", "Something went wrong. Copy diagnostics has the details.",
                                           [BannerAction("Try again", self.start_again())]))
            ok = False
        finally:
            self._preparing = False
        if not ok:
            self._start_failed(source)
            return
        if not source.wakes:  # nothing to wake: the stream starts right away
            self._begin_stream(source)
            return

        self._waking = True
        self._wake_source = source
        self._set_start_button(streaming=True)
        self._start_btn.setEnabled(False)
        self._source_status(source, "Starting the phone's mic…" if source.camera_off else "Starting the phone's camera…",
                            "dim")
        if self._streams:
            self._refresh_tiles()  # its tile says it's starting
        source.wake()

    def _start_failed(self, source: Source):
        if self._streams and source is self._focus and source.session is None:
            self._move_focus(self._streams[0])  # an added camera that didn't start: back to one that streams
        self._bus.stream_start_failed.emit()

    def _on_wake_done(self, source: Source, ok: bool, reason: str):
        self._waking = False
        self._wake_source = None
        self._start_btn.setEnabled(True)
        if not ok:
            self._set_start_button(streaming=bool(self._streams))
            self._source_status(source, "Not streaming", "dim")
            self.show_issue("start", Issue("Couldn't start the phone's camera", reason,
                                           [BannerAction("Try again", self.start_again())]))
            self._start_failed(source)
            return
        self._begin_stream(source)

    def _begin_stream(self, source: Source):
        url = source.url
        ctrl = source.control_client()
        session_id = self._next_session_id
        self._next_session_id += 1
        source.camera_toggle = None
        first = not self._streams
        if first:
            self._focus = self._main = source
        source.slot = self._free_slot(source)
        worker = None if source.camera_off else self._start_worker(url, source.auth, source)
        self._set_session(source, StreamSession(id=session_id, url=url, client=ctrl, worker=worker, source=source))

        if first:
            self._bus.stream_started.emit(url)
            self._each_plugin("on_stream_start", url, ctrl)
        elif source is self._focus:
            self._each_follower("on_stream_start", url, ctrl)
        if source.camera_off:
            self._tell(source, "on_camera_off")

        source.started(session_id)

        self._set_start_button(streaming=True)
        if source.camera_off:
            self._show_camera_off()
        else:
            self._source_status(source, "Connecting...", "dim")
        self._streams_changed()

    def _free_slot(self, source: Source) -> int:
        """The virtual camera source streams to: the usual one alone, else the one it had before if it's free."""
        used = {s.slot for s in self._streams if s is not source}
        if not used:
            return 0
        remembered = self._slot_memory.get(source.id)
        if isinstance(remembered, int) and 0 < remembered < vcam.MAX_SLOTS and remembered not in used:
            return remembered
        slot = min(set(range(vcam.MAX_SLOTS)) - used, default=0)
        if slot and source.id:
            self._slot_memory[source.id] = slot
            self.schedule_save()
        return slot

    def _start_worker(self, url: str, auth, source: Source) -> StreamWorker:
        """A video worker on url, after whatever holds the virtual camera while idle has let go of it."""
        w, h, fps = source.stream_params()

        setup = self._plugin("setup")
        canvas_w, canvas_h = setup.get_canvas_dims() if setup else (None, None)
        if canvas_w is None and source.auto_canvas:
            canvas_w, canvas_h = source.auto_canvas
        if source.slot and not (canvas_w and canvas_h):
            canvas_w, canvas_h = vcam.DEFAULT_SIZE  # an extra camera keeps one size, whatever streams to it

        if source.slot == 0:
            self._each_plugin("on_stream_starting")
        else:
            self._announce_idle_outputs(opening=source.slot)

        worker = StreamWorker(
            url=url, width=w, height=h, fps=fps,
            frame_pipeline=self._pipeline(live=source is self._focus or not self._streams),
            canvas_width=canvas_w, canvas_height=canvas_h,
            auth=auth,
            open_reader=source.open_reader,
            slot=source.slot,
        )
        worker.status.connect(self._on_worker_status)
        worker.reconnected.connect(self._on_stream_reconnected)
        if source.slot == 0:
            worker.vcam_opened.connect(self._bus.vcam_opened)
        worker.start()
        return worker

    def _pipeline(self, live: bool = True) -> list:
        """The plugins' frame steps: live for the stream the panels show, or frozen at their current settings for one
        that carries on while they show another."""
        steps = []
        for p in self._plugins:
            if type(p).process_frame is TelescopePlugin.process_frame:
                continue
            try:
                fn = p.process_frame if live else p.frame_step()
            except Exception:
                logging.exception("Plugin %s couldn't freeze its frame step; leaving it out", p.name)
                continue
            if fn is not None:
                steps.append(guarded_step(p.name or type(p).__name__, fn))
        return steps

    def _end_worker(self, worker: StreamWorker):
        for signal, slot in ((worker.status, self._on_worker_status),
                             (worker.reconnected, self._on_stream_reconnected),
                             (worker.vcam_opened, self._bus.vcam_opened)):
            try:
                signal.disconnect(slot)
            except (TypeError, RuntimeError, ValueError):
                pass  # already disconnected
        worker.request_stop()
        # Bounded wait so a stalled read can't freeze the UI; if it doesn't finish in time, let it keep unwinding in the background.
        if not worker.wait(5000):
            logging.warning("Stream worker did not stop within 5s; abandoning it in the background")
            # Dropping the last reference to a running QThread aborts the app, so hold it until it ends.
            self._orphans.add(worker)
            worker.finished.connect(lambda w=worker: self._orphans.discard(w))

    # ── Several streams: which one the panels show ────────────────────────

    def focus_stream(self, source_id: str):
        """Point the panels at source_id's stream (a tile was clicked)."""
        source = next((s for s in self._streams if s.id == source_id), None)
        if source is not None and not self._waking:
            self._move_focus(source)

    def pick_source(self, source_id: Optional[str]) -> bool:
        """The picker moved to source_id. True if that's handled here: its stream gets the panels, or with several
        running it takes the place of the one they show."""
        source = next((s for s in self._streams if s.id == source_id), None)
        if source is not None:
            if not self._waking:
                self._move_focus(source)
            return True
        if len(self._streams) > 1 and not self._waking and not self._preparing:
            old = self._focus
            self._stop(source=old, keep_focus=True)
            new = self._source_by_id(source_id)
            self._move_focus(new)
            self._start(source=new)
            return True
        return False

    def add_stream(self, source_id: str, before: Optional[Callable[[], None]] = None):
        """Start streaming source_id next to what streams already, to a virtual camera of its own (before: as in
        _start_source)."""
        if (self._waking or self._preparing or not self._streams or len(self._streams) >= vcam.MAX_SLOTS
                or self.is_streaming_from(source_id)):
            return
        source = self._source_by_id(source_id)
        if not vcam.slot_ready(self._free_slot(source)):
            self._set_up_slots(lambda: self.add_stream(source_id, before))
            return
        if self._focus is not None and self._focus.camera_off:
            self.set_camera_on(True)  # one camera off is for the mic alone; with two it would just be confusing
        self._move_focus(source)  # its own settings first, so it starts with them
        if before is not None:
            before()
        self._start(source=source)

    def _move_focus(self, source: Source):
        old = self._focus
        if source is old:
            return
        if old is not None:
            if old.session is not None:
                self._save_profile(old.id, self._followers())  # as it was, before the stop hooks
                if old.session.worker is not None:
                    old.session.worker.set_pipeline(self._pipeline(live=False))
                self._each_follower("on_stream_stop")
            if old.behind:
                old.behind = False
                self._bus.stream_behind.emit(False)
        self._focus = source
        conn = self._plugin("connection")
        if conn is not None:
            conn.show_selected(source.id)
        self._apply_device_profile(source.id, self._followers())
        self._bus.device_changed.emit(source.id or "")
        self._show_focus()

    def _show_focus(self):
        """Bring the panels and footer up to date with the stream they show now."""
        source, session = self._focus, self._focus.session
        self._clear_pending_resolution(source)
        self._fps_lbl.setText(source.fps_text)
        self._fps_lbl.setToolTip("")
        self._net_lbl.setStyleSheet("")
        self._net_lbl.setText(source.net_text)
        self._set_status(*source.status)
        if session is not None:
            if session.worker is not None:
                session.worker.set_pipeline(self._pipeline())
            self._each_follower("on_stream_start", session.url, session.client)
            if source.last_state is not None:
                self._bus.phone_state_updated.emit(source.last_state)
                self._each_follower("on_phone_state", source.last_state)
            if source.recovering:
                self._bus.stream_lost.emit()
            elif source.connected:
                self._bus.stream_connected.emit()
        self._refresh_tiles()

    def _sync_main(self):
        """The first stream left is the one plugins that don't follow the panels (the mic) hear about."""
        main = self._streams[0] if self._streams else None
        if main is self._main or self._stopping_all:
            return
        stays = [p for p in self._plugins if not p.follows_focus]
        old, self._main = self._main, None
        if old is not None:
            for p in stays:
                try:
                    p.on_stream_stop()
                except Exception:
                    logging.exception("Plugin %s failed in on_stream_stop", p.name)
            self._save_profile(old.id, stays)
        self._main = main
        if main is not None:
            self._apply_device_profile(main.id, stays)
            self._tell(main, "on_stream_start", main.session.url, main.session.client)

    def _streams_changed(self):
        self._announce_idle_outputs()
        self._bus.streams_changed.emit(len(self._streams))
        self._refresh_tiles()

    def _announce_idle_outputs(self, opening: Optional[int] = None):
        """Tell the wait screen which extra cameras nothing streams to; opening is one a stream is about to take."""
        used = {s.slot for s in self._streams} | {opening}
        self._bus.idle_outputs.emit([n for n in range(1, vcam.MAX_SLOTS) if n not in used and vcam.slot_ready(n)])

    def _refresh_tiles(self):
        """The tile row: one per stream while there's more than one."""
        entries = []
        for s in self._streams:
            if s.recovering:
                state, kind = "Reconnecting", "status_warn"
            elif s.connected or s.session.worker is None:
                state, kind = "Live", "status_ok"
            else:
                state, kind = "Connecting", "status_dim"
            entries.append((s.id or "", s.name, vcam.slot_label(s.slot), state, kind, s is self._focus))
        if self._waking and self._wake_source is not None and self._streams:
            s = self._wake_source
            entries.append((s.id or "", s.name, vcam.slot_label(self._free_slot(s)), "Starting", "status_dim", True))
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
        for s in self._streams:
            worker = s.session.worker if s.session is not None else None
            self._tiles.set_frame(s.id or "", worker.latest_frame() if worker is not None else None)

    def _add_candidates(self) -> list:
        """[(id, name)] of what Add camera offers: every phone and StreamSource that isn't streaming."""
        conn = self._plugin("connection")
        phones = [(p.id, p.name) for p in getattr(conn, "phones", [])] if conn is not None else []
        return [(sid, name) for sid, name in phones + [(s.id, s.name) for s in self._sources.values()]
                if not self.is_streaming_from(sid)]

    def _show_add_button(self):
        self._add_btn.setVisible(bool(self._streams) and len(self._streams) < vcam.MAX_SLOTS
                                 and bool(self._add_candidates()))
        self._add_btn.setEnabled(not self._waking)

    def _show_add_menu(self):
        menu = QMenu(self)
        for sid, name in self._add_candidates():
            menu.addAction(name).triggered.connect(lambda _=False, s=sid: self.add_stream(s))
        if not menu.isEmpty():
            menu.exec(self._add_btn.mapToGlobal(self._add_btn.rect().bottomLeft()) + QPoint(0, 6))
        menu.deleteLater()

    def _set_up_slots(self, then):
        """Set up the extra virtual cameras (asking first, as it takes a password), then call then."""
        if self._slots_then is not None or not self._ask_slots():
            return
        self._slots_then = then
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
        if self._extras_done is not None or not extras or any(s.slot for s in self._streams):
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
        self._announce_idle_outputs()
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
        then, self._slots_then = self._slots_then, None
        if self._focus is not None:
            self._set_status(*self._focus.status)
        if ok:
            self.clear_issue("vcam")
            self._announce_idle_outputs()
            if then is not None:
                then()
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
        if len(self._streams) > 1 or (self._streams and source.session is None):
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
            self._set_start_button(streaming=self._waking)
        self._bus.camera_on_changed.emit(on)

    def _on_source_selected(self, _sid: str):
        """Each source keeps its own camera choice, so picking another can change what Start does."""
        if not self._streams and self.is_camera_on() != self._camera_on_shown:
            self._set_start_button(streaming=self._waking)
            self._bus.camera_on_changed.emit(self.is_camera_on())

    def _camera_off_now(self, session: StreamSession):
        self._end_recovery()
        if session.worker is not None:
            self._end_worker(session.worker)
            self._set_session(session.source, replace(session, worker=None))
        self._clear_pending_resolution()
        self._set_behind(False)
        self._tell(session.source, "on_camera_off")
        self._show_camera_off()

    def _camera_on_now(self, session: StreamSession):
        session.source.stop_watching_alive()
        self._end_recovery()
        self._set_session(session.source, replace(
            session, worker=self._start_worker(session.url, session.client.auth, session.source)))
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
        streaming = bool(self._streams)
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

    def _stop_all(self, remote_stop: bool = True):
        """Stop every stream, and a start still waking a phone (the Stop button)."""
        self._stopping_all = True
        try:
            for source in [s for s in self._streams if s is not self._active_source()]:
                self._stop(remote_stop, source)
        finally:
            self._stopping_all = False
        self._stop(remote_stop)

    def _stop(self, remote_stop: bool = True, source: Optional[Source] = None, keep_focus: bool = False):
        """Tear down a stream, the panels' one unless source says; remote_stop=False for reconnects (changed
        address/vcam reload). keep_focus: it starts again right after, so the panels stay on it."""
        source = source or self._active_source()
        source.end_recovery()
        was_waking = self._waking and self._wake_source in (None, source)
        if was_waking:
            self._waking = False
            self._wake_source = None
            self._start_btn.setEnabled(True)
        shown = source is self._focus or self._focus is None

        # Forget the session before the synchronous teardown below, so any in-flight async state fetch sees "no active session" immediately rather than racing the unwind.
        session = source.session
        self._forget_stream(source)
        worker = session.worker if session else None
        ctrl = session.client if session else None

        source.stopped(remote_stop, active=bool(session or was_waking))

        if worker:
            self._end_worker(worker)
        if ctrl:
            ctrl.close()
        camera_was_off = source.camera_off
        if not self._restarting:
            source.camera_off = source.camera_off_auto = False  # each stream starts with the camera on
        self._clear_pending_resolution(source)
        source.settling_reports = 0
        source.arrival_slow = False
        source.forget_stream_status()
        self._set_behind(False, source)

        if self._streams:  # others carry on
            if shown:
                self._save_profile(source.id, self._followers())
                self._each_follower("on_stream_stop")
                self._show_focus_stopped()
                if not keep_focus:
                    self._move_focus(self._streams[0])
            if source is self._main:
                self._sync_main()
            self._streams_changed()
            return

        if shown:
            self.clear_issue("camera")
        self._focus = self._main = None
        self._set_start_button(streaming=self._waking)
        self._show_focus_stopped()
        self._bus.stream_stopped.emit()
        self._each_plugin("on_stream_stop")
        if camera_was_off and not source.camera_off:
            self._bus.camera_on_changed.emit(True)
        self._streams_changed()

    def _show_focus_stopped(self):
        self._fps_lbl.setText("—")
        self._net_lbl.setStyleSheet("")
        self._net_lbl.setText("—")
        self._set_status("Not streaming", "dim")
        self._fps_lbl.setToolTip("")

    def _begin_recovery(self, source: Optional[Source] = None):
        """The stream dropped: the source looks for its way back (a phone may be reachable another way now)."""
        source = source or (self._session.source if self._session is not None else None)
        if source is None or source.session is None or source.recovering:
            return
        source.recovering = True
        self._refresh_tiles()
        self._set_behind(False, source)
        if source is self._focus:
            self._bus.stream_lost.emit()
        source.lost()

    def _end_recovery(self):
        self._active_source().end_recovery()

    def _drain_phone_stops(self, timeout: float = 2.0):
        """Wait for remote stops to complete, but never block quit indefinitely."""
        deadline = time.monotonic() + timeout
        for source in (*self._phones.values(), *self._sources.values()):
            source.drain_stops(deadline)

    def restart_vcam_canvas(self, w, h, on_done=None):
        """Stop stream, optionally reload the vcam driver, restart stream."""
        self._vcam_reload_callback = on_done
        was_streaming = self._session is not None or self._waking  # a start still waking restarts after it too
        old_worker = self._worker  # capture before _stop() clears it
        # Desktop-side driver reload only - the phone keeps streaming through it.
        if was_streaming:
            self._stop_for_restart()
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
        if ok:
            self._set_status(f"Loopback reloaded: {msg}" if IS_LINUX else "Canvas updated", "ok")
            if restart_stream:
                self.start_stream()  # not over a stream started while the reload ran
        else:
            self._set_status("Not streaming" if command else f"Reload failed: {msg}", "dim" if command else "err")
            if command:
                self.show_issue("vcam", Issue("Resize the virtual camera", msg, [copy_action(command)],
                                              kind="warn", details=command))
        cb = getattr(self, "_vcam_reload_callback", None)
        if cb:
            cb(ok, msg)
            self._vcam_reload_callback = None

    def _apply_state(self, session_id: int, state: dict, source: Optional[Source] = None):
        # A device switch or stop between the fetch completing and this slot
        # running (queued Qt signal) means this result belongs to a session
        # that's no longer active - discard it rather than handing a stale
        # phone's state to plugins for the current device.
        source = source or self._active_source()
        if source.session is None or source.session.id != session_id:
            return
        try:
            PhoneState.from_dict(state)
        except PhoneStateError:
            logging.exception("Phone sent a malformed /v1/state response - not applying it")
            self._set_status("The phone sent data this version can't read. Update both apps to the same version.", "err")
            return
        # Decoded successfully - forwarded as the original dict rather than
        # the typed PhoneState so existing plugins keep consuming the shape
        # they already expect; the validation above is the new behavior.
        if state:
            source.last_state = state  # for the panels, when they come back to this stream
        if source is self._focus:
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

    def start_hidden(self):
        """For --minimized: live in the tray, or minimized where there's no tray."""
        if self._tray is None:
            self.showMinimized()

    def quit_app(self):
        self._tray_quit()

    def start_stream(self, interactive: bool = True):
        if not self._streams and not self._waking and not self._preparing:
            self._start(interactive)

    def start_again(self, source_id: Optional[str] = None, before: Optional[Callable[[], None]] = None
                    ) -> Callable[[], None]:
        source_id = self._start_id if source_id is None else source_id
        return lambda: self._start_source(source_id, before)

    def focused_source_id(self) -> Optional[str]:
        return self._active_source().id

    def _start_source(self, source_id: Optional[str], before: Optional[Callable[[], None]] = None):
        """Start source_id by itself, or next to what streams already (a stopped or failed one, from its banner).
        before runs once the panels show it, so a setting it changes is that source's."""
        if not self._streams:
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

    def clear_issue(self, key: Optional[str] = None):
        self._banners.clear_issue(key)

    def diagnostics_report(self) -> str:
        state = {"Streaming": ("yes" if self.is_camera_on() else "yes, camera off") if self.is_streaming() else "no"}
        if len(self._streams) > 1:
            state["Streams"] = str(len(self._streams))
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

    def _flush_save(self):
        if self._save_timer.isActive():  # a setting changed just before quitting
            self._save_timer.stop()
            self.save_now()

    def _shut_down(self):
        """Everything quitting does, each step on its own, so one that fails can't keep the app from closing."""
        for step in (self._stop_all, self._flush_save, self._drain_phone_stops, self._shutdown_plugins):
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
            self._tray.showMessage(title, body, QSystemTrayIcon.MessageIcon.Warning, 0)

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
        timer.timeout.connect(self._on_resolution_pending_timeout)
        timer.start(8000)  # generous: camera reopen + a possible brief reconnect
        source.pending_resolution_timer = timer

    def _on_resolution_pending_timeout(self):
        source = self._active_source()
        source.pending_resolution = None
        source.pending_resolution_timer = None
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
        if kind in ("ok", "waiting", "reconnecting") and len(self._streams) > 1:
            self._refresh_tiles()  # its tile says how it's doing
        if source is not self._active_source():
            self._background_status(source, kind)
            return
        if kind == "fps":
            self._fps_lbl.setText(msg)
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
            self._bus.stream_connected.emit()
        elif kind == "warn":
            self._set_status(msg, "warn")
        elif kind == "waiting":  # no first frame yet: maybe the route died between waking the phone and now
            self._set_status(msg, "warn")
            self._begin_recovery()
        elif kind == "reconnecting":
            self._start_reconnecting_animation(msg)
            self._begin_recovery()
        elif kind == "idle":
            self._clear_pending_resolution()
            self._fps_lbl.setText("—")
            self._net_lbl.setStyleSheet("")
            self._net_lbl.setText("—")
            self._set_status(msg, "dim")
            if self._session:
                # Stop disconnects the worker first, so this is a worker that ended by itself: tidy up like a Stop.
                logging.warning("The stream worker ended by itself")
                self._stop()
                self.show_issue(f"stopped:{source.id}", Issue(
                    "The stream stopped unexpectedly", "Copy diagnostics has the details.",
                    [BannerAction("Start", lambda sid=source.id: self._start_source(sid))], kind="warn"))
        else:
            self._set_status(msg, "dim")

    def _background_status(self, source: Source, kind: str):
        """A stream the panels don't show: its tile says how it's doing, and a dropped one still finds its way back."""
        if kind in ("waiting", "reconnecting"):
            self._begin_recovery(source)
        elif kind == "idle" and source.session is not None:
            logging.warning("The stream worker for %s ended by itself", source.name)
            self._stop(source=source)
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
        self._fps_lbl.setToolTip(_CAMERA_LIMITED_TIP if slow and camera_limited else "")
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
            if source is self._focus or self._focus is None:
                self._bus.stream_behind.emit(behind)

    def _set_status(self, msg: str, kind: str):
        diagnostics.events.note(f"Status: {msg}")
        self._stop_reconnecting_animation()
        obj = {"ok": "status_ok", "warn": "status_warn",
               "err": "status_err", "dim": "status_dim"}.get(kind, "status_dim")
        set_status_kind(self._status_lbl, obj)
        self._status_lbl.setText(msg)

    def closeEvent(self, event):
        if self._tray and (self._streams or self._keep_in_tray):
            event.ignore()
            self.hide()
            if not self._tray_close_notified:
                self._tray_close_notified = True
                self.send_notification(
                    "Telescope is still running",
                    ("Streaming continues in the background." if self._streams else
                     "It starts streaming by itself when it needs to.")
                    + " Right-click the tray icon to quit.",
                    urgent=False,
                )
        else:
            self._shut_down()
            event.accept()
            QApplication.quit()
