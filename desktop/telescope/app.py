import logging
import shutil
import socket
import subprocess
import threading
import time
from typing import Optional

from PyQt6.QtCore import QPoint, QSize, Qt, QTimer, pyqtSignal
from PyQt6.QtGui import QAction
from PyQt6.QtWidgets import (
    QApplication, QFrame, QHBoxLayout, QLabel,
    QMainWindow, QMenu, QMessageBox, QPushButton, QScrollArea,
    QSizePolicy, QSystemTrayIcon, QVBoxLayout, QWidget,
)

from telescope import theme
from telescope.config import DEVICE_LOCAL_PLUGINS, load_config, save_config
from telescope.models import PhoneState, PhoneStateError
from telescope.phone_client import PhoneControlClient
from telescope.platform import IS_LINUX, IS_WINDOWS
from telescope.plugin import UNCHANGED, EventBus, TelescopePlugin
from telescope.session import StreamSession
from telescope.stream import StreamWorker
from telescope.widgets.common import ElidingLabel, create_app_icon, create_vector_icon

# ── Theme ─────────────────────────────────────────────────────────────────────
# Re-exported here so existing callers keep one import site.
STATUS_COLORS = theme.STATUS_COLORS

# Breakpoints where layout folds from 3 to 2 to 1 column.
_WIDTH_THREE_COL = 1300
_WIDTH_TWO_COL   = 900

_RAIL_WIDTH_LEFT  = 364
_RAIL_WIDTH_RIGHT = 434


# ── Single-instance enforcement ───────────────────────────────────────────────
_INSTANCE_PORT = 47823


def acquire_single_instance() -> Optional[socket.socket]:
    srv = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    srv.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 0)
    try:
        srv.bind(("127.0.0.1", _INSTANCE_PORT))
        srv.listen(1)
        return srv
    except OSError:
        c = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        try:
            c.settimeout(1)
            c.connect(("127.0.0.1", _INSTANCE_PORT))
            c.sendall(b"raise")
        except Exception:
            pass
        finally:
            c.close()
        srv.close()
        return None


def listen_for_raise(srv: socket.socket, raise_cb):
    srv.settimeout(1.0)
    while True:
        try:
            conn, _ = srv.accept()
            try:
                if conn.recv(16) == b"raise":
                    raise_cb()
            finally:
                conn.close()
        except socket.timeout:
            continue
        except Exception:
            break


# ── Main window ───────────────────────────────────────────────────────────────
class TelescopeWindow(QMainWindow):
    _sig_state = pyqtSignal(int, dict)
    _sig_raise = pyqtSignal()
    _sig_canvas_reload_done = pyqtSignal(bool, str, bool)  # ok, msg, restart_stream
    _sig_wake_done = pyqtSignal(int, bool, str, str, str)  # wake_id, ok, reason, url, token
    _sig_wake_progress = pyqtSignal(int, str)  # wake_id, status text

    def __init__(self):
        super().__init__()
        self.setWindowTitle("Telescope")
        self.setMinimumSize(560, 520)
        self.resize(1380, 900)

        self._bus     = EventBus()
        self._bus.resolution_change_requested.connect(self._on_resolution_pending)
        # In-flight resolution change; cleared on confirm or timeout.
        self._pending_resolution: Optional[tuple[int, int]] = None
        self._pending_resolution_timer: Optional[QTimer] = None
        # Animated "Stream dropped - reconnecting..." status.
        self._reconnecting_timer: Optional[QTimer] = None
        self._reconnecting_base = ""
        self._reconnecting_dots = 1
        self._plugins: list[TelescopePlugin] = []
        self._plugins_by_name: dict[str, TelescopePlugin] = {}
        # Plugin defaults; lets us reset before applying device profile.
        self._plugin_defaults: dict[str, dict] = {}

        # StreamSession owns current worker/client; id guards against stale async results.
        self._session: Optional[StreamSession] = None
        self._next_session_id = 1
        self._save_failure_notified = False

        # Generation counter for phone-wake; guards against stale async results.
        self._wake_id = 0
        self._waking = False
        # Remote-stop requests; quit path waits for these to complete.
        self._stop_threads: list[threading.Thread] = []

        self._save_timer = QTimer(self)
        self._save_timer.setSingleShot(True)
        self._save_timer.timeout.connect(self.save_now)

        self._tray: Optional[QSystemTrayIcon] = None

        self.setWindowIcon(create_app_icon(64))
        self._build_ui()
        self._setup_tray()

        self._sig_state.connect(self._apply_state)
        self._sig_raise.connect(self._tray_show)
        self._sig_canvas_reload_done.connect(self._on_canvas_reload_done)
        self._sig_wake_done.connect(self._on_wake_done)
        self._sig_wake_progress.connect(self._on_wake_progress)

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
            self._header_slot.addWidget(header_widget)
        self._plugins.append(plugin)
        if plugin.name:
            self._plugins_by_name[plugin.name] = plugin
        if plugin.name in DEVICE_LOCAL_PLUGINS:
            self._plugin_defaults[plugin.name] = plugin.get_config()

    def _plugin(self, name: str) -> Optional[TelescopePlugin]:
        """Look up a registered plugin by its declared name, or None. Central
        accessor so the host isn't peppered with inline `next(p for p ...)`
        name scans."""
        return self._plugins_by_name.get(name)

    def apply_saved_config(self):
        """Restore persisted config into all registered plugins. Call after all plugins registered."""
        self._apply_config(load_config())

    # ── UI construction ───────────────────────────────────────────────────────

    def _build_ui(self):
        # Panels are routed by the region a plugin asks for rather than being
        # appended to one stack, so the window can rearrange them as it
        # resizes without any plugin knowing.
        self._panels: dict[str, list[QWidget]] = {"left": [], "center": [], "right": []}
        self._layout_mode: Optional[str] = None

        root = QWidget()
        root.setObjectName("body_root")
        self.setCentralWidget(root)
        root_lay = QVBoxLayout(root)
        root_lay.setContentsMargins(0, 0, 0, 0)
        root_lay.setSpacing(0)

        root_lay.addWidget(self._build_header())
        root_lay.addWidget(self._build_body(), 1)
        root_lay.addWidget(self._build_footer())

    def _build_header(self) -> QWidget:
        bar = QWidget()
        bar.setObjectName("header_bar")
        lay = QHBoxLayout(bar)
        lay.setContentsMargins(18, 10, 18, 10)
        lay.setSpacing(12)

        # Plugins land header controls here.
        self._header_slot = QHBoxLayout()
        self._header_slot.setContentsMargins(0, 0, 0, 0)
        self._header_slot.setSpacing(14)
        lay.addLayout(self._header_slot)

        lay.addStretch()

        self._menu_btn = QPushButton()
        self._menu_btn.setObjectName("icon_btn")
        self._menu_btn.setFixedSize(36, 36)
        self._menu_btn.setIcon(create_vector_icon("gear", theme.TEXT_DIM))
        self._menu_btn.setIconSize(QSize(19, 19))
        self._menu_btn.setToolTip("Setup and tools")
        self._menu_btn.clicked.connect(self._show_settings_menu)
        lay.addWidget(self._menu_btn)

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
        self._start_btn.setText(verb if self._layout_mode == "one" else f"{verb} Streaming")
        self._start_btn.setIcon(
            create_vector_icon("stop" if streaming else "play", "#ffffff"))
        self._start_btn.setProperty("streaming", streaming)
        self._start_btn.setStyle(self._start_btn.style())

    def _apply_header_density(self):
        """Shorten the start button to its verb once the window narrows.

        Everything in the header is fixed-size, so at 560px the row simply
        ran out of room and Qt crushed the device picker under the button.
        The picker and the primary action survive at any width.
        """
        self._set_start_button(getattr(self, "_streaming_label", False))

    def _build_body(self) -> QWidget:
        body = QWidget()
        body.setObjectName("body_root")
        self._body_lay = QHBoxLayout(body)
        self._body_lay.setContentsMargins(16, 16, 16, 16)
        self._body_lay.setSpacing(14)

        # Three columns; narrower modes hide trailing ones.
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
        lay.setContentsMargins(18, 8, 18, 8)
        lay.setSpacing(14)

        # No caption (already reads as status); elides long messages.
        self._status_lbl = ElidingLabel("Idle - press Start Streaming")
        self._status_lbl.setObjectName("status_dim")
        lay.addWidget(self._status_lbl, 1)

        divider = QFrame()
        divider.setObjectName("header_divider")
        divider.setFixedWidth(1)
        divider.setFixedHeight(22)
        lay.addWidget(divider)

        fps_cap = QLabel("LIVE FPS")
        fps_cap.setObjectName("footer_label")
        lay.addWidget(fps_cap)

        self._fps_lbl = QLabel("—")
        self._fps_lbl.setObjectName("fps_lbl")
        self._fps_lbl.setMinimumWidth(72)
        lay.addWidget(self._fps_lbl)

        divider2 = QFrame()
        divider2.setObjectName("header_divider")
        divider2.setFixedWidth(1)
        divider2.setFixedHeight(22)
        lay.addWidget(divider2)

        net_cap = QLabel("LIVE THROUGHPUT")
        net_cap.setObjectName("footer_label")
        lay.addWidget(net_cap)

        self._net_lbl = QLabel("—")
        self._net_lbl.setObjectName("fps_lbl")
        self._net_lbl.setMinimumWidth(80)
        lay.addWidget(self._net_lbl)

        return bar

    def _show_settings_menu(self):
        """Built fresh to reflect current plugins."""
        menu = QMenu(self)
        for p in self._plugins:
            for action in p.create_menu_actions():
                action.setParent(menu)
                menu.addAction(action)
        if menu.isEmpty():
            return
        menu.exec(self._menu_btn.mapToGlobal(
            self._menu_btn.rect().bottomLeft()) + QPoint(0, 6))

    # ── Responsive layout ─────────────────────────────────────────────────────

    def _layout_mode_for(self, width: int) -> str:
        if width >= _WIDTH_THREE_COL:
            return "three"
        if width >= _WIDTH_TWO_COL:
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
            widths = [_RAIL_WIDTH_LEFT, None, _RAIL_WIDTH_RIGHT]
        elif mode == "two":
            # Rails fold together on the left; the video stage keeps a column
            # of its own because it's the thing that actually needs the width.
            groups = [self._panels["left"] + self._panels["right"], self._panels["center"], []]
            widths = [_RAIL_WIDTH_RIGHT, None, None]
        else:
            groups = [self._panels["center"] + self._panels["left"] + self._panels["right"], [], []]
            widths = [None, None, None]

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
            # Only flexible column absorbs slack.
            self._body_lay.setStretch(self._columns.index(scroll),
                                      1 if width is None else 0)
            has_center = False
            for panel in panels:
                stretch = 1 if id(panel) in center_panels else 0
                has_center = has_center or bool(stretch)
                col_lay.addWidget(panel, stretch)
                panel.setVisible(True)
            # Column with center panel absorbs slack.
            if not has_center:
                col_lay.addStretch()

    def resizeEvent(self, event):
        super().resizeEvent(event)
        self._refresh_layout()

    # ── Config persistence ────────────────────────────────────────────────────

    def schedule_save(self):
        self._save_timer.start(500)

    def save_now(self):
        cfg = load_config()
        # Global plugin configs (connection, setup, etc.)
        global_pcfg = cfg.setdefault("plugin_configs", {})
        conn = self._plugin("connection")
        selected = conn.selected_device if conn else None
        cfg["selected_device"] = selected
        for p in self._plugins:
            if p.name and p.name not in DEVICE_LOCAL_PLUGINS:
                global_pcfg[p.name] = p.get_config()
        # Per-device plugin configs
        if selected:
            dev = cfg.setdefault("devices", {}).setdefault(selected, {})
            dev_pcfg = dev.setdefault("plugin_configs", {})
            for p in self._plugins:
                if p.name and p.name in DEVICE_LOCAL_PLUGINS:
                    dev_pcfg[p.name] = p.get_config()
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

    def _apply_device_profile(self, name: Optional[str]):
        """Reset device-local plugins to defaults, then apply device profile."""
        cfg = load_config()
        pcfg = cfg.get("devices", {}).get(name, {}).get("plugin_configs", {}) if name else {}
        for p in self._plugins:
            if p.name and p.name in DEVICE_LOCAL_PLUGINS:
                p.set_config(self._plugin_defaults.get(p.name, {}))
                if p.name in pcfg:
                    p.set_config(pcfg[p.name])

    def switch_device(self, prev_name, new_name: Optional[str]):
        """Switch device profile; save old before applying new."""
        cfg = load_config()
        if prev_name:
            prev_pcfg = cfg.setdefault("devices", {}).setdefault(prev_name, {}).setdefault("plugin_configs", {})
            for p in self._plugins:
                if p.name and p.name in DEVICE_LOCAL_PLUGINS:
                    prev_pcfg[p.name] = p.get_config()
        save_config(cfg)

        was_streaming = self._worker is not None
        if was_streaming:
            self._stop()

        cfg["selected_device"] = new_name
        save_config(cfg)
        self._apply_device_profile(new_name)

        if was_streaming:
            self._start()

    def reconnect_stream(self):
        """Restart stream to pick up changed connection settings."""
        if self._worker is None:
            return
        self._stop(remote_stop=False)
        self._start()

    # ── Public stream controls (HostServices contract for plugins) ─────────────

    def is_streaming(self) -> bool:
        """Whether a stream worker is currently active."""
        return self._worker is not None

    def stop_stream(self):
        """Stop stream; safe no-op if idle. Waking counts as active."""
        if self._worker is not None or self._waking:
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

    def _on_stream_reconnected(self):
        """Stream reconnected; plugins resend settings to phone."""
        session = self._session
        if session is None:
            return
        for p in self._plugins:
            p.on_stream_start(session.url, session.client)

    def _apply_config(self, cfg: dict):
        if not cfg:
            return
        # config.py's load_config() already ran migration; cfg is always v2 here
        selected    = cfg.get("selected_device")
        global_pcfg = cfg.get("plugin_configs", {})

        conn = self._plugin("connection")
        for p in self._plugins:
            if not p.name or p.name in DEVICE_LOCAL_PLUGINS:
                continue
            if p.name in global_pcfg:
                p.set_config(global_pcfg[p.name])
        self._apply_device_profile(selected)
        # Sync connection plugin profile after device profile applied.
        if conn:
            conn.sync_active_profile()

    # ── Start / Stop ──────────────────────────────────────────────────────────

    def _toggle(self):
        if self._worker or self._waking: self._stop()
        else:                            self._start()

    def _start(self):
        """Phase one of starting: make sure the phone's camera is actually
        running before we try to read frames off it.

        The phone used to have to be started by hand, which meant two taps on
        two devices for one session. Now the desktop asks it over the session
        port first (ConnectionPlugin.ensure_phone_streaming), which is a
        network round trip plus however long the camera takes to open - far
        too long to block the UI thread on, hence the split into
        _on_wake_done/_begin_stream.
        """
        conn = self._plugin("connection")
        if not conn:
            return
        url, token, ok = conn.get_stream_info()
        if not ok:
            return

        self._wake_id += 1
        wake_id = self._wake_id
        self._waking = True
        self._set_start_button(streaming=True)
        self._start_btn.setEnabled(False)
        self._set_status("Waking phone camera...", "dim")

        self._spawn_wake(wake_id, conn, url, token)

    def _spawn_wake(self, wake_id: int, conn, url: str, token: str):
        """Split out from _start() so tests can make this synchronous - same
        reason ConnectionPlugin._spawn_pair_probe is split out, and with the
        same hazard: a real background thread outliving its QObject is a hard
        PyQt abort when the queued signal is finally delivered."""
        threading.Thread(
            target=self._wake_phone, args=(wake_id, conn, url, token), daemon=True,
        ).start()

    def _wake_phone(self, wake_id: int, conn, url: str, token: str):
        def on_progress(msg: str):
            try:
                self._sig_wake_progress.emit(wake_id, msg)
            except RuntimeError:
                pass  # window went away mid-wake

        try:
            ok, reason = conn.ensure_phone_streaming(on_progress=on_progress)
        except Exception:
            logging.exception("Phone wake failed")
            ok, reason = False, "Couldn't reach the phone."
        try:
            self._sig_wake_done.emit(wake_id, ok, reason, url, token)
        except RuntimeError:
            # The window went away mid-wake; nothing left to tell.
            pass

    def _on_wake_progress(self, wake_id: int, msg: str):
        # Same staleness guard as _on_wake_done: a progress tick for a wake
        # the user has already cancelled (stop/device switch/quit) has
        # nothing left to report to.
        if wake_id != self._wake_id or not self._waking:
            return
        self._set_status(msg, "dim")

    def _on_wake_done(self, wake_id: int, ok: bool, reason: str, url: str, token: str):
        # Stop/device switch/quit while the wake was in flight: whatever this
        # says is about a session the user has already walked away from.
        if wake_id != self._wake_id or not self._waking:
            return
        self._waking = False
        self._start_btn.setEnabled(True)
        if not ok:
            self._set_start_button(streaming=False)
            self._set_status("Idle - press Start Streaming", "dim")
            QMessageBox.warning(self, "Couldn't start the phone's camera", reason)
            return
        self._begin_stream(url, token)

    def _begin_stream(self, url: str, token: str):
        so = self._plugin("stream_output")
        w, h, fps = so.get_stream_params() if so else (None, None, 30)

        setup = self._plugin("setup")
        canvas_w, canvas_h = setup.get_canvas_dims() if setup else (None, None)

        pipeline = [p.process_frame for p in self._plugins]

        ctrl = PhoneControlClient(url, token)
        worker = StreamWorker(
            url=url, width=w, height=h, fps=fps,
            frame_pipeline=pipeline,
            canvas_width=canvas_w, canvas_height=canvas_h,
            token=token,
        )
        session_id = self._next_session_id
        self._next_session_id += 1
        self._session = StreamSession(id=session_id, url=url, client=ctrl, worker=worker)

        worker.status.connect(self._on_worker_status)
        worker.reconnected.connect(self._on_stream_reconnected)
        worker.start()

        self._bus.stream_started.emit(url)
        for p in self._plugins:
            p.on_stream_start(url, ctrl)

        threading.Thread(target=self._fetch_state_async, args=(session_id,), daemon=True).start()

        self._set_start_button(streaming=True)
        self._set_status("Connecting...", "dim")

    def _stop(self, remote_stop: bool = True):
        """Tear down this side of the stream, and by default the phone's side
        with it.

        ``remote_stop=False`` is for the internal stop/start pairs that are
        really a reconnect (a changed address, a vcam canvas reload): the
        phone's camera is fine and bouncing it would cost seconds and a
        needless lens re-open. The _start() that follows pings, sees a stream
        already up, and connects straight through.
        """
        # Invalidate any wake still in flight before touching anything else,
        # so a phone that finishes starting a moment from now can't have its
        # result build a stream the user has just cancelled.
        self._wake_id += 1
        was_waking = self._waking
        self._waking = False
        self._start_btn.setEnabled(True)

        # Captured before clearing self._session, which must happen first so
        # any in-flight async completion (_fetch_state_async/_apply_state)
        # sees "no active session" immediately, even while the teardown below
        # is still unwinding the actual worker/client synchronously.
        session = self._session
        self._session = None
        worker = session.worker if session else None
        ctrl = session.client if session else None

        # Stop is symmetric with start: the desktop brought the phone's camera
        # up, so it takes it back down rather than leaving it burning battery
        # until someone walks over to the phone. Also covers a cancelled wake,
        # where the phone may already be mid-start.
        if remote_stop and (session or was_waking):
            self._stop_phone_async()

        if worker:
            worker.status.disconnect(self._on_worker_status)
            worker.reconnected.disconnect(self._on_stream_reconnected)
            worker.request_stop()
            # Bounded wait so a stalled read can't freeze the GUI. With the
            # OpenCV open/read timeouts in _open_cap() this should normally
            # finish well within this window; if it doesn't, let the worker
            # keep unwinding in the background rather than force-killing it.
            if not worker.wait(5000):
                logging.warning("Stream worker did not stop within 5s; abandoning it in the background")
        if ctrl:
            ctrl.close()
        self._set_start_button(streaming=False)
        self._clear_pending_resolution()
        self._fps_lbl.setText("—")
        self._net_lbl.setStyleSheet("")
        self._net_lbl.setText("—")
        self._set_status("Stopped.", "dim")

        self._bus.stream_stopped.emit()
        for p in self._plugins:
            p.on_stream_stop()

    def _stop_phone_async(self):
        """Tell the phone to shut its camera down, off the UI thread.

        Kept as a tracked thread rather than a fire-and-forget daemon so
        _drain_phone_stops() can give it a moment on the way out - a daemon
        thread dies with the interpreter, which on the quit path is usually
        before the request has left the machine.
        """
        conn = self._plugin("connection")
        if not conn:
            return

        def stop():
            try:
                conn.stop_phone_streaming()
            except Exception:
                logging.debug("Remote stop failed", exc_info=True)

        t = threading.Thread(target=stop, daemon=True)
        self._stop_threads = [x for x in self._stop_threads if x.is_alive()]
        self._stop_threads.append(t)
        t.start()

    def _drain_phone_stops(self, timeout: float = 2.0):
        """Bounded wait for outstanding remote stops before the process exits.
        Bounded because a phone that has already gone away must not be able to
        hold the app open."""
        deadline = time.monotonic() + timeout
        for t in self._stop_threads:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                break
            t.join(remaining)
        self._stop_threads.clear()

    def restart_vcam_canvas(self, w, h, on_done=None):
        """Stop stream, optionally reload the vcam driver, restart stream."""
        self._vcam_reload_callback = on_done
        was_streaming = self._worker is not None
        old_worker = self._worker  # capture before _stop() clears it
        # Desktop-side driver reload only - the phone keeps streaming through it.
        self._stop(remote_stop=False)

        if IS_LINUX:
            self._set_status("Reloading v4l2loopback…", "dim")

            def worker():
                if old_worker:
                    old_worker.wait(5000)
                from telescope.platform.linux import v4l2_reload
                ok, msg = v4l2_reload()
                self._sig_canvas_reload_done.emit(ok, msg, was_streaming)

            threading.Thread(target=worker, daemon=True).start()
        else:
            self._set_status("Restarting stream…", "dim")

            def worker():
                if old_worker:
                    old_worker.wait(5000)
                self._sig_canvas_reload_done.emit(True, "canvas updated", was_streaming)

            threading.Thread(target=worker, daemon=True).start()

    def _on_canvas_reload_done(self, ok: bool, msg: str, restart_stream: bool):
        if ok:
            self._set_status(f"Loopback reloaded: {msg}", "ok")
            if restart_stream:
                self._start()
        else:
            self._set_status(f"Reload failed: {msg}", "err")
        cb = getattr(self, "_vcam_reload_callback", None)
        if cb:
            cb(ok, msg)
            self._vcam_reload_callback = None

    def _fetch_state_async(self, session_id: int):
        time.sleep(1.5)
        for _ in range(3):
            if self._session is None or self._session.id != session_id or not self._ctrl:
                return
            state = self._ctrl.get_state()
            if state:
                self._sig_state.emit(session_id, state)
                return
            time.sleep(2)
        if self._session is not None and self._session.id == session_id:
            self._sig_state.emit(session_id, {})

    def _apply_state(self, session_id: int, state: dict):
        # A device switch or stop between the fetch completing and this slot
        # running (queued Qt signal) means this result belongs to a session
        # that's no longer active - discard it rather than handing a stale
        # phone's state to plugins for the current device.
        if self._session is None or self._session.id != session_id:
            return
        try:
            PhoneState.from_dict(state)
        except PhoneStateError:
            logging.exception("Phone sent a malformed /v1/state response - not applying it")
            self._set_status("Protocol error: phone sent malformed state", "err")
            return
        # Decoded successfully - forwarded as the original dict rather than
        # the typed PhoneState so existing plugins keep consuming the shape
        # they already expect; the validation above is the new behavior.
        self._bus.phone_state_updated.emit(state)
        for p in self._plugins:
            p.on_phone_state(state)

    # ── Tray ──────────────────────────────────────────────────────────────────

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
        menu.addSeparator()
        menu.addAction(quit_action)
        self._tray.setContextMenu(menu)

        self._tray.activated.connect(self._on_tray_activated)
        self._tray.show()

    def _tray_show(self):
        self.showNormal()
        self.raise_()
        self.activateWindow()

    def _tray_quit(self):
        self._tray_close_notified = True
        self._stop()
        self._drain_phone_stops()
        QApplication.quit()

    def _on_tray_activated(self, reason):
        if reason == QSystemTrayIcon.ActivationReason.Trigger:
            if self.isVisible():
                self.hide()
            else:
                self._tray_show()

    def send_notification(self, title: str, body: str):
        if IS_LINUX and shutil.which("notify-send"):
            subprocess.Popen(
                ["notify-send", "-a", "Telescope", "-u", "critical", title, body],
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            )
        elif self._tray:
            self._tray.showMessage(title, body, QSystemTrayIcon.MessageIcon.Warning, 0)

    # ── Worker status ─────────────────────────────────────────────────────────

    def _on_resolution_pending(self, w: int, h: int):
        """A resolution change was just sent to the phone - it has to close
        and reopen its camera to honour it, so the live readout won't reflect
        it for a second or two. Shown as pending (amber) rather than just
        leaving the old value up, which reads as "did that even work?"."""
        if self._pending_resolution_timer:
            self._pending_resolution_timer.stop()
        self._pending_resolution = (w, h)
        self._fps_lbl.setStyleSheet(f"color: {theme.WARN};")
        timer = QTimer(self)
        timer.setSingleShot(True)
        timer.timeout.connect(self._on_resolution_pending_timeout)
        timer.start(8000)  # generous: camera reopen + a possible brief reconnect
        self._pending_resolution_timer = timer

    def _on_resolution_pending_timeout(self):
        self._pending_resolution = None
        self._pending_resolution_timer = None
        self._fps_lbl.setStyleSheet(f"color: {theme.ERR};")
        QTimer.singleShot(4000, lambda: self._fps_lbl.setStyleSheet(""))

    def _clear_pending_resolution(self):
        if self._pending_resolution_timer:
            self._pending_resolution_timer.stop()
            self._pending_resolution_timer = None
        self._pending_resolution = None
        self._fps_lbl.setStyleSheet("")

    def _start_reconnecting_animation(self, base_msg: str):
        """"Stream dropped - reconnecting" gets its own colour (warn/yellow,
        distinct from the plain grey the other status text uses) and an
        animated "." -> ".." -> "..." -> "." loop, 1 frame/second, so it
        reads as actively retrying rather than a static, possibly-stuck
        message. Any other status arriving (via _set_status) cancels it."""
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
        # Deliberately bypasses _set_status - that stops this very timer as
        # its first step, which would cancel the animation from inside its
        # own tick.
        self._status_lbl.setObjectName("status_warn")
        self._status_lbl.setText(f"{self._reconnecting_base}{'.' * self._reconnecting_dots}")
        self._status_lbl.setStyleSheet(f"color: {theme.WARN};")

    def _stop_reconnecting_animation(self):
        if self._reconnecting_timer:
            self._reconnecting_timer.stop()

    def _on_worker_status(self, kind: str, msg: str):
        if kind == "fps":
            self._fps_lbl.setText(msg)
            if self._pending_resolution is not None:
                try:
                    size_text = msg.rsplit(" ", 1)[-1]
                    w_str, h_str = size_text.split("x")
                    if (int(w_str), int(h_str)) == self._pending_resolution:
                        self._clear_pending_resolution()
                except ValueError:
                    pass
        elif kind == "net":
            self._net_lbl.setStyleSheet("")
            self._net_lbl.setText(msg)
        elif kind == "net_warn":
            self._net_lbl.setStyleSheet(f"color: {theme.WARN};")
            self._net_lbl.setText(msg)
        elif kind == "ok":
            self._set_status(msg, "ok")
            self._bus.stream_connected.emit()
        elif kind == "warn":
            self._set_status(msg, "warn")
        elif kind == "reconnecting":
            self._start_reconnecting_animation(msg)
        elif kind == "idle":
            self._clear_pending_resolution()
            self._fps_lbl.setText("—")
            self._net_lbl.setStyleSheet("")
            self._net_lbl.setText("—")
            self._set_status(msg, "dim")
            if self._session:
                self._session = None
                self._set_start_button(streaming=False)
        else:
            self._set_status(msg, "dim")

    def _set_status(self, msg: str, kind: str):
        self._stop_reconnecting_animation()
        obj = {"ok": "status_ok", "warn": "status_warn",
               "err": "status_err", "dim": "status_dim"}.get(kind, "status_dim")
        self._status_lbl.setObjectName(obj)
        self._status_lbl.setText(msg)
        self._status_lbl.setStyleSheet("")

    def closeEvent(self, event):
        if self._tray and self._worker is not None:
            event.ignore()
            self.hide()
            if not self._tray_close_notified:
                self._tray_close_notified = True
                self.send_notification(
                    "Telescope is still running",
                    "Streaming continues in the background. Right-click the tray icon to quit.",
                )
        else:
            self._stop()
            self._drain_phone_stops()
            event.accept()
            QApplication.quit()
