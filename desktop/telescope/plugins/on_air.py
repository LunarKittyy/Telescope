"""On Air sign: lets the people you live with know you're on a call (see telescope/on_air.py).

What counts as on air is picked in Settings: an app reading the virtual camera (camera_watched, the default), that or
Telescope's mic being live, the mic alone, or just streaming. The answer is debounced and then goes to a sign page
another device keeps open and to a webhook, either or both. camera_watched comes from Wait screen; without it this
plugin watches the camera itself.
"""

import logging
import re
import time
from typing import Optional

from PyQt6.QtCore import QObject, QSignalBlocker, QTimer, pyqtSignal
from PyQt6.QtGui import QGuiApplication
from PyQt6.QtWidgets import QButtonGroup, QHBoxLayout, QLineEdit, QPushButton, QVBoxLayout, QWidget

from telescope import ip_utils, on_air, vcam
from telescope.browser_server import certificate_available, ensure_certificate
from telescope.config import config_path
from telescope.plugin import SettingsSection, TelescopePlugin
from telescope.widgets.banner import BannerAction, Issue
from telescope.widgets.common import (
    ElidingLabel, NoScrollComboBox, SegmentButton, Switch, add_card_header, button_row, card_action, card_layout,
    control_row, control_row_widget, create_card, segmented_row, set_status_kind, set_ui_role, wrapped_note,
)
from telescope.widgets.qr import QRCodeWidget
from telescope.widgets.settings_search import keywords
from telescope.i18n import N_, tr

logger = logging.getLogger(__name__)

TAB = "on_air"
ISSUE = "on_air_webhook"
TRIGGER_TEXTS = {
    "camera": N_("An app is using the camera"),
    "camera_mic": N_("An app is using the camera or the mic is live"),
    "mic": N_("Telescope's mic is live"),
    "streaming": N_("Telescope is streaming"),
}
_TOKEN = re.compile(r"[A-Za-z0-9_-]{16,64}")
# Translators: Show Details, visit this website, Advanced and Proceed are what Safari and Chrome call their buttons:
# use the names those browsers show in your language
_CERT_NOTE = N_("The browser warns that the connection isn't private: it's this computer's own certificate. "
                "On iPhone tap Show Details, then visit this website. In Chrome tap Advanced, then Proceed.")


class _Signals(QObject):
    watched = pyqtSignal(bool)
    webhook_done = pyqtSignal(str, str)


class OnAirPlugin(TelescopePlugin):
    name = "on_air"
    title = N_("On Air sign")
    about = N_("Shows the people around you that you're on a call, on another device's screen or through a smart "
               "home light.")
    when_off = N_("Without it there's no On Air tab in Settings and no sign or webhook.")
    optional = True
    header_side = "right"

    def __init__(self, server_cls=on_air.OnAirServer, watch_cls=vcam.CameraWatch, sender_cls=on_air.WebhookSender,
                 cert_folder=None, addresses=None, clock=time.monotonic):
        self._server_cls, self._watch_cls, self._sender_cls = server_cls, watch_cls, sender_cls
        self._cert_folder = cert_folder  # tests pass a temporary folder
        self._addresses = addresses or ip_utils.get_pairing_addresses
        self._clock = clock

    def setup(self, host, bus):
        self._host, self._bus = host, bus
        self.trigger = on_air.DEFAULT_TRIGGER
        self.page = True
        self.token = on_air.new_token()
        self.address = ""
        self.webhook_url, self.webhook_method, self.webhook_on, self.webhook_off = "", "POST", "", ""
        self._watched = False
        self._mic = (False, False)
        self._debounce = on_air.Debounce()
        self._server: Optional[on_air.OnAirServer] = None
        self._problem = ""  # why the sign page isn't up
        self._watch = None  # our own camera watch, only without Wait screen
        self._webhook_failing = False
        self.test_result = ("", "")  # (text, status kind) of the last Send test
        self._section: Optional[OnAirSection] = None
        self._header_btn: Optional[QPushButton] = None
        self._signals = _Signals()
        self._signals.watched.connect(self._on_watched)
        self._signals.webhook_done.connect(self._on_webhook_done)
        self._sender = self._sender_cls(self._signals.webhook_done.emit)
        self._timer = QTimer()
        self._timer.setSingleShot(True)
        self._timer.timeout.connect(self._settle)
        bus.camera_watched.connect(self._on_watched)
        bus.mic_changed.connect(self._on_mic_changed)
        bus.stream_started.connect(lambda _url: self._evaluate())
        bus.stream_stopped.connect(self._evaluate)
        bus.streams_changed.connect(lambda _n: self._evaluate())

    def on_plugins_ready(self):
        if self._host.plugin_config("wait_screen") is None and self._watch is None:
            self._watch = self._watch_cls(self._signals.watched.emit)  # nobody else tells us about the camera
            self._watch.start()
        self._sync_server()
        self._evaluate()

    # ── Whether we're on air ──────────────────────────────────────────────

    @property
    def on(self) -> bool:
        return self._debounce.state

    def _on_watched(self, watched: bool):
        self._watched = watched
        self._evaluate()

    def _on_mic_changed(self, enabled: bool, muted: bool):
        self._mic = (enabled, muted)
        self._evaluate()

    def _evaluate(self):
        raw = on_air.is_on_air(self.trigger, self._watched, self._host.is_streaming(), *self._mic)
        self._debounce.feed(raw, self._clock())
        self._settle()

    def _settle(self):
        now = self._clock()
        if self._debounce.settle(now):
            self._changed()
        left = self._debounce.due_in(now)
        if left is None:
            self._timer.stop()
        else:
            self._timer.start(int(left * 1000) + 20)

    def _changed(self):
        logger.info("On Air: %s", "on" if self.on else "off")
        self._publish()
        self._send_state()
        self._refresh()

    def _publish(self):
        if self._server is not None:
            self._server.publish({"on": self.on, "text": tr("On air") if self.on else tr("Off air"),
                                  "title": tr("On Air")})

    def _refresh(self):
        if self._header_btn is not None:
            self._header_btn.setVisible(self.on)
        if self._section is not None:
            self._section.refresh()

    # ── Webhook ───────────────────────────────────────────────────────────

    def webhook_ready(self) -> bool:
        return bool(self.webhook_url) and not on_air.webhook_problem(self.webhook_url)

    def _request(self, on: bool):
        payload = self.webhook_on if on else self.webhook_off
        return on_air.webhook_request(self.webhook_url, self.webhook_method, payload, on)

    def _send_state(self):
        if self.webhook_ready():
            self._sender.send(self._request(self.on), "state")

    def send_test(self, on: bool):
        if not self.webhook_ready():
            self.test_result = (tr("Enter an address starting with http:// or https:// first."), "status_err")
            self._refresh()
            return
        self.test_result = (tr("Sending…"), "dim")
        self._refresh()
        self._sender.send(self._request(on), "test")

    def _on_webhook_done(self, tag: str, error: str):
        if not error:
            self._webhook_failing = False
            self._host.clear_issue(ISSUE)
        elif tag == "state" and not self._webhook_failing:
            self._webhook_failing = True  # one quiet banner per run of failures, however many changes follow
            logger.warning("On Air webhook didn't go through: %s", error)
            self._host.show_issue(ISSUE, Issue(
                tr("The On Air webhook didn't go through"), tr("It answered: {error}", error=error),
                [BannerAction(tr("Settings"), lambda: self._host.open_settings(TAB))], kind="warn"))
        if tag == "test":
            self.test_result = ((tr("It went through."), "status_ok") if not error
                                 else (tr("It didn't go through: {error}", error=error), "status_err"))
        self._refresh()

    # ── Sign page ─────────────────────────────────────────────────────────

    def _sync_server(self):
        if self.page and self._server is None:
            self._start_server()
        elif not self.page and self._server is not None:
            self._stop_server()
        self._refresh()

    def _start_server(self):
        self._problem = ""
        self._fill_address()
        if not certificate_available():
            self._problem = tr("Needs the cryptography package: pip install cryptography")
            return
        folder = self._cert_folder or config_path().parent
        try:
            cert, key = ensure_certificate(folder, [a.ip for a in self._addresses()])
            server = self._server_cls(self.token, cert, key)
            server.start()
        except Exception as exc:
            logger.exception("On Air sign server didn't start")
            self._problem = tr("Couldn't start: {error}", error=exc)
            return
        self._server = server
        self._publish()

    def _stop_server(self):
        server, self._server = self._server, None
        if server is not None:
            server.stop()

    def _fill_address(self):
        candidates = self.addresses()
        if self.address not in candidates:
            self.address = candidates[0] if candidates else ""

    def addresses(self) -> list:
        return [a.ip for a in self._addresses()]

    def url(self) -> str:
        server = self._server
        return server.url_for(self.address) if server is not None and self.address else ""

    def server_problem(self) -> str:
        return self._problem

    def new_link(self):
        """The old link stops working: a sign still open on it says so."""
        self.token = on_air.new_token()
        if self._server is not None:
            self._server.set_token(self.token)
        self._host.schedule_save()
        self._refresh()

    # ── Settings from the section ─────────────────────────────────────────

    def set_trigger(self, trigger: str):
        if trigger in on_air.TRIGGERS and trigger != self.trigger:
            self.trigger = trigger
            self._host.schedule_save()
            self._evaluate()

    def set_page(self, on: bool):
        if on != self.page:
            self.page = on
            self._host.schedule_save()
            self._sync_server()

    def set_address(self, ip: str):
        if ip and ip != self.address:
            self.address = ip
            self._host.schedule_save()
            self._refresh()

    def set_webhook(self, url=None, method=None, on=None, off=None):
        if url is not None:
            self.webhook_url = url.strip()
        if method in on_air.METHODS:
            self.webhook_method = method
        if on is not None:
            self.webhook_on = on
        if off is not None:
            self.webhook_off = off
        self._host.schedule_save()

    # ── UI ────────────────────────────────────────────────────────────────

    def create_header_widget(self) -> QWidget:
        self._header_btn = QPushButton(tr("On air"))
        self._header_btn.setFixedHeight(36)
        self._header_btn.setToolTip(tr("Open the On Air settings"))
        set_ui_role(self._header_btn, "danger")
        self._header_btn.clicked.connect(lambda: self._host.open_settings(TAB))
        self._header_btn.setVisible(self.on)
        return self._header_btn

    def create_settings_sections(self) -> list:
        self._section = OnAirSection(self)
        return [SettingsSection(TAB, self._section, on_show=self._section.refresh, tab_title=tr("On Air"),
                                keywords=tr("on air sign busy light call meeting do not disturb webhook home "
                                            "assistant smart home", ctx="search"))]

    # ── Config ────────────────────────────────────────────────────────────

    def get_config(self) -> dict:
        return {"trigger": self.trigger, "page": self.page, "token": self.token, "address": self.address,
                "webhook_url": self.webhook_url, "webhook_method": self.webhook_method,
                "webhook_on": self.webhook_on, "webhook_off": self.webhook_off}

    def set_config(self, cfg: dict):
        self.trigger = cfg.get("trigger") if cfg.get("trigger") in on_air.TRIGGERS else on_air.DEFAULT_TRIGGER
        self.page = cfg.get("page") is not False
        token = cfg.get("token")
        if isinstance(token, str) and _TOKEN.fullmatch(token):
            self.token = token
        else:
            self._host.schedule_save()  # the link made in setup has to survive a restart
        if self._server is not None:
            self._server.set_token(self.token)
        self.address = cfg.get("address") if isinstance(cfg.get("address"), str) else ""
        self.webhook_url = cfg.get("webhook_url") if isinstance(cfg.get("webhook_url"), str) else ""
        method = cfg.get("webhook_method")
        self.webhook_method = method if method in on_air.METHODS else "POST"
        self.webhook_on = cfg.get("webhook_on") if isinstance(cfg.get("webhook_on"), str) else ""
        self.webhook_off = cfg.get("webhook_off") if isinstance(cfg.get("webhook_off"), str) else ""

    def diagnostics(self) -> dict:
        page = f"on, port {self._server.port}" if self._server is not None else ("off" if not self.page else "not running")
        webhook = "not set"
        if self.webhook_url:
            webhook = ("failing" if self._webhook_failing else "set") + f" ({self.webhook_method})"
        return {"On Air": "on" if self.on else "off", "On Air when": self.trigger, "On Air sign page": page,
                "On Air webhook": webhook}

    def shutdown(self):
        self._timer.stop()
        if self.on and self.webhook_ready():
            on_air.send_webhook(self._request(False), timeout=2.0)  # don't leave the light on after quitting
        self._stop_server()
        if self._watch is not None:
            self._watch.stop()


class OnAirSection(QWidget):
    def __init__(self, plugin: OnAirPlugin):
        super().__init__()
        self._plugin = plugin
        outer = QVBoxLayout(self)
        outer.setContentsMargins(0, 0, 0, 0)
        outer.setSpacing(12)

        card = create_card()
        lay = card_layout(card)
        add_card_header(lay, tr("On Air sign"), "bell")
        lay.addWidget(wrapped_note(tr("Lets the people around you know you're on a call. Show it on a tablet by "
                                      "the door, switch a smart light with the webhook, or both.")))
        self._state_lbl = ElidingLabel("")
        lay.addLayout(control_row(tr("Now"), self._state_lbl, stretch=True))
        self._trigger_combo = NoScrollComboBox()
        for key in on_air.TRIGGERS:
            self._trigger_combo.addItem(tr(TRIGGER_TEXTS[key]), key)
        self._trigger_combo.setSizeAdjustPolicy(NoScrollComboBox.SizeAdjustPolicy.AdjustToMinimumContentsLengthWithIcon)
        self._trigger_combo.setMinimumContentsLength(12)  # a long choice can't squeeze the label
        self._trigger_combo.setToolTip(tr("The mic counts while it streams and isn't muted"))
        self._trigger_combo.currentIndexChanged.connect(
            lambda i: plugin.set_trigger(self._trigger_combo.itemData(i)))
        lay.addLayout(control_row(tr("On air when"), self._trigger_combo, stretch=True))
        outer.addWidget(card)

        card = create_card()
        lay = card_layout(card)
        self._new_link_btn = card_action(tr("New link"), "reset", tr("Make a new link; the old one stops working"))
        self._new_link_btn.clicked.connect(plugin.new_link)
        add_card_header(lay, tr("Sign on another device"), "qr", action=self._new_link_btn)
        keywords(card, tr("tablet phone screen page link qr code", ctx="search"))
        self._page_switch = Switch(tr("Show the sign on a web page"))
        self._page_switch.toggled.connect(plugin.set_page)
        lay.addWidget(self._page_switch)
        self._problem_lbl = wrapped_note("", "status_err")
        lay.addWidget(self._problem_lbl)
        self._qr_box = QWidget()
        self._qr_lay = QVBoxLayout(self._qr_box)
        self._qr_lay.setContentsMargins(0, 4, 0, 4)
        self._qr: Optional[QRCodeWidget] = None
        self._qr_url = ""
        self._qr_row = control_row_widget(tr("Scan"), self._qr_box)
        lay.addWidget(self._qr_row)
        self._addr_combo = NoScrollComboBox()
        self._addr_combo.setToolTip(tr("Which of this computer's addresses the link points to"))
        self._addr_combo.currentIndexChanged.connect(lambda i: plugin.set_address(self._addr_combo.itemData(i)))
        self._addr_row = control_row_widget(tr("Address"), self._addr_combo, stretch=True)
        lay.addWidget(self._addr_row)
        link = QHBoxLayout()
        link.setContentsMargins(0, 0, 0, 0)
        link.setSpacing(8)
        self._url_lbl = ElidingLabel("")
        copy_btn = QPushButton(tr("Copy"))
        copy_btn.setToolTip(tr("Copy the link, to open it on a device that can't scan the code"))
        copy_btn.clicked.connect(lambda: QGuiApplication.clipboard().setText(plugin.url()))
        link.addWidget(self._url_lbl, 1)
        link.addWidget(copy_btn)
        self._link_row = control_row_widget(tr("Link"), link, stretch=True)
        lay.addWidget(self._link_row)
        self._page_note = wrapped_note(tr("Open the link in a browser on any device on the same network and tap the "
                                          "page to make it full screen. The link stays the same after Telescope "
                                          "restarts.") + " " + tr(_CERT_NOTE))
        lay.addWidget(self._page_note)
        outer.addWidget(card)

        card = create_card()
        lay = card_layout(card)
        add_card_header(lay, tr("Webhook"), "plug")
        keywords(card, tr("smart home light home assistant url http request automation", ctx="search"))
        lay.addWidget(wrapped_note(tr("Sends a request each time you go on or off air, for a smart home hub or "
                                      "anything else that takes one.")))
        self._url_edit = QLineEdit()
        self._url_edit.setPlaceholderText("http://homeassistant.local:8123/api/webhook/…")
        self._url_edit.textChanged.connect(lambda text: (plugin.set_webhook(url=text), self._show_url_problem()))
        lay.addLayout(control_row(tr("Address"), self._url_edit, stretch=True))
        self._url_problem = wrapped_note("", "status_err")
        lay.addWidget(self._url_problem)
        self._method_btns = {}
        group = QButtonGroup(card)
        for method in on_air.METHODS:
            btn = SegmentButton(method, value=method)
            btn.clicked.connect(lambda _c=False, m=method: (plugin.set_webhook(method=m), self._show_placeholders()))
            group.addButton(btn)
            self._method_btns[method] = btn
        lay.addLayout(control_row(tr("Method"), segmented_row(*self._method_btns.values()), stretch=True))
        self._on_edit = QLineEdit()
        self._on_edit.textChanged.connect(lambda text: plugin.set_webhook(on=text))
        self._on_edit.setToolTip(tr("POST sends it as the body, GET adds it to the address"))
        lay.addLayout(control_row(tr("Going on air"), self._on_edit, stretch=True))
        self._off_edit = QLineEdit()
        self._off_edit.textChanged.connect(lambda text: plugin.set_webhook(off=text))
        self._off_edit.setToolTip(tr("POST sends it as the body, GET adds it to the address"))
        lay.addLayout(control_row(tr("Going off air"), self._off_edit, stretch=True))
        test_on, test_off = QPushButton(tr("Test on")), QPushButton(tr("Test off"))
        test_on.clicked.connect(lambda: plugin.send_test(True))
        test_off.clicked.connect(lambda: plugin.send_test(False))
        lay.addLayout(button_row(test_on, test_off))
        self._test_lbl = wrapped_note("")
        lay.addWidget(self._test_lbl)
        outer.addWidget(card)
        outer.addStretch(1)

    def _show_placeholders(self):
        method = self._plugin.webhook_method
        self._on_edit.setPlaceholderText(on_air.default_payload(method, True))
        self._off_edit.setPlaceholderText(on_air.default_payload(method, False))

    def _show_url_problem(self):
        url = self._plugin.webhook_url
        bad = bool(url) and bool(on_air.webhook_problem(url))
        self._url_problem.setText(tr("The address has to start with http:// or https://") if bad else "")
        self._url_problem.setVisible(bad)

    def refresh(self):
        p = self._plugin
        set_status_kind(self._state_lbl, "status_err" if p.on else "status_dim")
        self._state_lbl.setText(tr("On air") if p.on else tr("Off air"))
        with QSignalBlocker(self._trigger_combo):
            self._trigger_combo.setCurrentIndex(max(0, self._trigger_combo.findData(p.trigger)))
        with QSignalBlocker(self._page_switch):
            self._page_switch.setChecked(p.page)

        url = p.url()
        problem = p.server_problem() if p.page else ""
        self._problem_lbl.setText(problem)
        self._problem_lbl.setVisible(bool(problem))
        if url != self._qr_url:
            self._qr_url = url
            if self._qr is not None:
                self._qr_lay.removeWidget(self._qr)
                self._qr.deleteLater()
                self._qr = None
            if url:
                self._qr = QRCodeWidget(url, module_px=4)
                self._qr_lay.addWidget(self._qr)
        addresses = p.addresses() if url else []
        with QSignalBlocker(self._addr_combo):
            self._addr_combo.clear()
            for ip in addresses:
                self._addr_combo.addItem(ip, ip)
            self._addr_combo.setCurrentIndex(self._addr_combo.findData(p.address))
        self._url_lbl.setText(url)
        for row in (self._qr_row, self._link_row, self._page_note):
            row.setVisible(bool(url))
        self._addr_row.setVisible(len(addresses) > 1)
        self._new_link_btn.setEnabled(p.page)

        for edit, value in ((self._url_edit, p.webhook_url), (self._on_edit, p.webhook_on),
                            (self._off_edit, p.webhook_off)):
            if edit.text() != value and not edit.hasFocus():
                with QSignalBlocker(edit):
                    edit.setText(value)
        self._method_btns[p.webhook_method].setChecked(True)
        self._show_placeholders()
        self._show_url_problem()
        text, kind = p.test_result
        self._test_lbl.setText(text)
        set_status_kind(self._test_lbl, kind or "dim")
        self._test_lbl.setVisible(bool(text))
