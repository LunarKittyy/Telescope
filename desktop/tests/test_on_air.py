import http.client
import json
import ssl
import threading
import time
import urllib.error
from types import SimpleNamespace

import pytest

from telescope import browser_server as bs
from telescope import on_air
from telescope.plugin import EventBus
from telescope.plugins.on_air import ISSUE, OnAirPlugin


# ── What counts as on air ────────────────────────────────────────────────────

@pytest.mark.parametrize("trigger, watched, streaming, mic, expected", [
    ("camera", True, False, (False, False), True),  # a call watching the wait screen counts
    ("camera", False, True, (True, False), False),
    ("camera_mic", False, True, (True, False), True),
    ("camera_mic", False, True, (True, True), False),  # muted
    ("camera_mic", False, False, (True, False), False),  # the mic only counts while it streams
    ("mic", True, True, (False, False), False),
    ("mic", False, True, (True, False), True),
    ("streaming", False, True, (False, False), True),
    ("streaming", True, False, (True, False), False),
])
def test_each_trigger_reads_what_it_says(trigger, watched, streaming, mic, expected):
    assert on_air.is_on_air(trigger, watched, streaming, *mic) is expected


def test_going_on_waits_a_moment_and_a_blip_never_counts():
    d = on_air.Debounce(on_after=1.0, off_after=6.0)
    d.feed(True, 0.0)
    assert d.due_in(0.0) == 1.0 and not d.settle(0.5)
    d.feed(False, 0.6)  # gone before it counted
    assert d.due_in(0.7) is None and not d.settle(5.0) and d.state is False
    d.feed(True, 10.0)
    assert d.settle(11.0) and d.state is True
    assert d.due_in(11.0) is None


def test_going_off_waits_longer_so_an_app_reopening_the_camera_doesnt_flap():
    d = on_air.Debounce(on_after=1.0, off_after=6.0)
    d.feed(True, 0.0)
    d.settle(1.0)
    d.feed(False, 2.0)
    assert not d.settle(7.9) and d.due_in(7.0) == pytest.approx(1.0)
    d.feed(True, 7.5)  # back before the off counted: nothing changed
    assert not d.settle(20.0) and d.state is True
    d.feed(False, 30.0)
    assert d.settle(36.0) and d.state is False


# ── Webhook requests ─────────────────────────────────────────────────────────

def test_post_sends_json_by_default_and_plain_text_otherwise():
    req = on_air.webhook_request("http://hub.local/hook", "POST", "", True)
    assert req.get_method() == "POST" and req.full_url == "http://hub.local/hook"
    assert json.loads(req.data) == {"on_air": True} and req.get_header("Content-type") == "application/json"
    req = on_air.webhook_request("http://hub.local/hook", "POST", "busy", False)
    assert req.data == b"busy" and req.get_header("Content-type").startswith("text/plain")
    assert req.get_header("User-agent").startswith("Telescope/")


def test_get_adds_the_payload_to_the_query():
    req = on_air.webhook_request("http://10.0.0.9/relay/0?x=1#frag", "GET", "turn=on", True)
    assert req.get_method() == "GET" and req.data is None
    assert req.full_url == "http://10.0.0.9/relay/0?x=1&turn=on"
    assert on_air.webhook_request("http://10.0.0.9/r", "GET", "", False).full_url == "http://10.0.0.9/r?on_air=false"


def test_a_get_payload_is_encoded_so_it_cant_break_the_request():
    req = on_air.webhook_request("http://hub.local/x?a=1", "GET", 'state=on air&note="hi"', True)
    assert req.full_url == "http://hub.local/x?a=1&state=on%20air&note=%22hi%22"
    assert on_air.webhook_request("http://hub.local/x", "GET", "light=100%25", True).full_url.endswith("light=100%25")


@pytest.mark.parametrize("url, ok", [
    ("http://hub.local/x", True), ("https://1.2.3.4:8123/api", True),
    ("", False), ("hub.local/x", False), ("ftp://hub/x", False), ("http://", False), ("http://hub/a b", False),
])
def test_only_http_addresses_are_used(url, ok):
    assert (on_air.webhook_problem(url) == "") is ok


class _Response:
    def __enter__(self):
        return self

    def __exit__(self, *a):
        return False

    def read(self, n):
        return b"ok"


def test_send_reports_what_went_wrong_without_the_address():
    seen = []

    def ok(req, timeout):
        seen.append((req.full_url, timeout))
        return _Response()
    req = on_air.webhook_request("http://hub.local/hook", "POST", "", True)
    assert on_air.send_webhook(req, opener=ok) == "" and seen == [("http://hub.local/hook", on_air.WEBHOOK_TIMEOUT_S)]

    def refused(req, timeout):
        raise urllib.error.URLError(ConnectionRefusedError(111, "Connection refused"))
    assert on_air.send_webhook(req, opener=refused) == "Connection refused"

    def not_found(req, timeout):
        raise urllib.error.HTTPError(req.full_url, 404, "Not Found", {}, None)
    assert on_air.send_webhook(req, opener=not_found) == "HTTP 404"


def test_errors_never_carry_the_address(caplog):
    import http.client
    req = on_air.webhook_request("http://hub.local/api/webhook/SECRET", "POST", "", True)

    def bad_url(req, timeout):
        raise http.client.InvalidURL("URL can't contain control characters. '/api/webhook/SECRET'")
    assert on_air.send_webhook(req, opener=bad_url) == "InvalidURL"

    def unknown(req, timeout):
        raise ValueError("unknown url type: '/api/webhook/SECRET'")
    assert "SECRET" not in on_air.send_webhook(req, opener=unknown)
    done = threading.Event()
    results = []

    def explode(_req):
        raise RuntimeError("boom at /api/webhook/SECRET")
    sender = on_air.WebhookSender(lambda tag, err: (results.append(err), done.set()), send=explode)
    sender.send(req, "state")
    assert done.wait(2) and "SECRET" not in results[0] and "SECRET" not in caplog.text


def test_a_test_never_takes_the_place_of_a_state():
    release, sent = threading.Event(), []

    def send(req):
        sent.append(req.data)
        release.wait(2)
        return ""
    sender = on_air.WebhookSender(lambda tag, err: None, send=send)
    sender.send(on_air.webhook_request("http://h/x", "POST", "first", True), "state:1", slot="state")
    time.sleep(0.05)
    sender.send(on_air.webhook_request("http://h/x", "POST", "on", True), "state:2", slot="state")
    sender.send(on_air.webhook_request("http://h/x", "POST", "test", True), "test")
    release.set()
    assert sender.wait_idle(2)
    assert sent == [b"first", b"on", b"test"]


def test_waiting_for_the_sender_gives_up_at_the_deadline():
    release = threading.Event()
    sender = on_air.WebhookSender(lambda tag, err: None, send=lambda req: release.wait(2) and "")
    sender.send(on_air.webhook_request("http://h/x", "POST", "", True), "quit")
    assert not sender.wait_idle(0.1)
    release.set()
    assert sender.wait_idle(2)


def test_the_sender_only_sends_whats_true_at_the_end():
    release, results, sent = threading.Event(), [], []

    def send(req):
        sent.append(req.data)
        release.wait(2)
        return ""
    done = threading.Event()
    sender = on_air.WebhookSender(lambda tag, err: (results.append((tag, err)), done.set() if len(results) == 2 else 0),
                                  send=send)
    sender.send(on_air.webhook_request("http://h/x", "POST", "1", True), "state")
    time.sleep(0.05)
    sender.send(on_air.webhook_request("http://h/x", "POST", "2", True), "state")  # waits behind the first...
    sender.send(on_air.webhook_request("http://h/x", "POST", "3", True), "state")  # ...and this replaces it
    release.set()
    assert done.wait(2)
    assert sent == [b"1", b"3"] and results == [("state", ""), ("state", "")]


# ── The sign page ────────────────────────────────────────────────────────────

@pytest.fixture(scope="module")
def cert(tmp_path_factory):
    return bs.ensure_certificate(tmp_path_factory.mktemp("cert"), ["127.0.0.1"])


@pytest.fixture
def server(cert):
    srv = on_air.OnAirServer("a" * 24, *cert, port=0, host="127.0.0.1")
    srv.start()
    yield srv
    srv.stop()


def _get(port: int, path: str, token: str = "", timeout: float = 5) -> tuple:
    ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
    ctx.check_hostname = False
    ctx.verify_mode = ssl.CERT_NONE
    conn = http.client.HTTPSConnection("127.0.0.1", port, context=ctx, timeout=timeout)
    conn.request("GET", path, headers={"Authorization": f"Bearer {token}"} if token else {})
    resp = conn.getresponse()
    body = resp.read()
    conn.close()
    return resp.status, resp.getheader("Content-Type"), body, resp.getheader("Content-Security-Policy")


def test_serves_the_sign_and_its_script_and_nothing_else(server):
    status, ctype, body, csp = _get(server.port, "/")
    assert status == 200 and ctype.startswith("text/html") and b"/on-air.js" in body and "default-src 'self'" in csp
    assert _get(server.port, "/on-air.js")[0] == 200
    for path in ("/app.js", "/ws", "/../on_air.py", "/index.html"):  # none of the Browser camera's
        assert _get(server.port, path)[0] == 404


def test_the_state_needs_the_token(server):
    server.publish({"on": True, "text": "On air"})
    assert _get(server.port, "/state?after=-1")[0] == 403
    assert _get(server.port, "/state?after=-1", token="b" * 24)[0] == 403
    status, ctype, body, _ = _get(server.port, "/state?after=-1", token=server.token)
    assert status == 200 and ctype == "application/json"
    assert json.loads(body) == {"on": True, "text": "On air", "seq": 1}
    assert server.stats["refused"] == 2


def test_a_page_waits_for_the_next_change(server):
    server.publish({"on": False})
    got = []
    t = threading.Thread(target=lambda: got.append(_get(server.port, "/state?after=1", token=server.token)))
    t.start()
    time.sleep(0.3)
    assert not got  # nothing new yet: the request is held
    server.publish({"on": True})
    t.join(3)
    assert json.loads(got[0][2]) == {"on": True, "seq": 2}


def test_a_new_link_ends_the_wait_and_refuses_the_old_one(server):
    server.publish({"on": False})
    old = server.token
    got = []
    t = threading.Thread(target=lambda: got.append(_get(server.port, "/state?after=1", token=old)))
    t.start()
    time.sleep(0.3)
    server.set_token("c" * 24)
    t.join(3)
    assert got and _get(server.port, "/state?after=1", token=old)[0] == 403


def test_url_carries_the_token_after_the_hash(server):
    assert server.url_for("10.0.0.2") == f"https://10.0.0.2:{server.port}/#{server.token}"


# ── The plugin ───────────────────────────────────────────────────────────────

class _Host:
    def __init__(self, wait_screen=True):
        self.streaming, self.saves, self.issues, self.opened = False, 0, {}, []
        self._wait_screen = wait_screen

    def is_streaming(self):
        return self.streaming

    def schedule_save(self):
        self.saves += 1

    def plugin_config(self, name):
        return {} if name == "wait_screen" and self._wait_screen else None

    def show_issue(self, key, issue):
        self.issues[key] = issue

    def clear_issue(self, key=None, issue=None):
        self.issues.pop(key, None)

    def open_settings(self, tab=""):
        self.opened.append(tab)


class _Server:
    made = []

    def __init__(self, token, cert, key):
        self.token, self.states, self.stopped = token, [], False
        _Server.made.append(self)

    def start(self):
        pass

    def stop(self):
        self.stopped = True

    def publish(self, state):
        self.states.append(state)

    def set_token(self, token):
        self.token = token

    def url_for(self, host):
        return f"https://{host}:8768/#{self.token}"

    port = 8768


class _Sender:
    def __init__(self, on_result):
        self.on_result, self.sent, self.waited = on_result, [], []

    def send(self, req, tag="", slot=None):
        self.sent.append((req, tag))

    def wait_idle(self, timeout):
        self.waited.append(timeout)
        return True

    def states(self):
        return [(req.data, tag.partition(":")[0]) for req, tag in self.sent]


class _Watch:
    made = []

    def __init__(self, on_change):
        self.on_change, self.started, self.stopped = on_change, False, False
        _Watch.made.append(self)

    def start(self):
        self.started = True

    def stop(self):
        self.stopped = True


@pytest.fixture
def make(qapp, tmp_path, monkeypatch):
    monkeypatch.setattr("telescope.plugins.on_air.certificate_available", lambda: True)
    monkeypatch.setattr("telescope.plugins.on_air.ensure_certificate", lambda folder, hosts: ("c", "k"))
    _Server.made.clear()
    _Watch.made.clear()

    def build(cfg=None, wait_screen=True):
        now = [100.0]
        host, bus = _Host(wait_screen), EventBus()
        plugin = OnAirPlugin(server_cls=_Server, watch_cls=_Watch, sender_cls=_Sender, cert_folder=tmp_path,
                             addresses=lambda: [SimpleNamespace(ip="192.168.1.20")], clock=lambda: now[0])
        plugin.setup(host, bus)
        plugin.set_config(cfg or {})
        plugin.on_plugins_ready()
        plugin.now = now
        return plugin, host, bus
    return build


def _later(plugin, seconds):
    plugin.now[0] += seconds
    plugin._settle()


def test_an_app_watching_the_camera_puts_the_sign_on_after_a_moment(make):
    plugin, host, bus = make()
    server = _Server.made[0]
    bus.camera_watched.emit(True)
    assert not plugin.on
    _later(plugin, on_air.ON_AFTER_S)
    assert plugin.on and server.states[-1]["on"] is True
    bus.camera_watched.emit(False)
    _later(plugin, 2)
    assert plugin.on  # still on: an app closing and reopening the camera doesn't flap it
    _later(plugin, on_air.OFF_AFTER_S)
    assert not plugin.on and server.states[-1]["on"] is False


def test_the_mic_trigger_follows_the_mic_while_streaming(make):
    plugin, host, bus = make({"trigger": "camera_mic"})
    bus.mic_changed.emit(True, False)
    _later(plugin, 2)
    assert not plugin.on  # not streaming yet
    host.streaming = True
    bus.stream_started.emit("url")
    _later(plugin, 2)
    assert plugin.on
    bus.mic_changed.emit(True, True)
    _later(plugin, on_air.OFF_AFTER_S + 1)
    assert not plugin.on


def test_without_wait_screen_it_watches_the_camera_itself(make):
    plugin, host, bus = make(wait_screen=False)
    watch = _Watch.made[0]
    assert watch.started
    watch.on_change(True)  # from the watch's thread, through a queued signal
    qapp_process()
    _later(plugin, 2)
    assert plugin.on
    plugin.shutdown()
    assert watch.stopped


def test_with_wait_screen_there_is_no_second_watch(make):
    make()
    assert not _Watch.made


def qapp_process():
    from PyQt6.QtWidgets import QApplication
    QApplication.processEvents()


def test_each_change_goes_to_the_webhook(make):
    plugin, host, bus = make({"webhook_url": "http://hub.local/hook", "webhook_on": "on", "webhook_off": "off"})
    bus.camera_watched.emit(True)
    _later(plugin, 2)
    bus.camera_watched.emit(False)
    _later(plugin, on_air.OFF_AFTER_S + 1)
    assert plugin._sender.states() == [(b"off", "state"), (b"on", "state"), (b"off", "state")]  # startup first


def test_no_webhook_without_a_usable_address(make):
    plugin, host, bus = make({"webhook_url": "hub.local/hook"})
    bus.camera_watched.emit(True)
    _later(plugin, 2)
    assert plugin.on and not plugin._sender.sent


def _state_tag(plugin):
    return plugin._sender.sent[-1][1]


def test_webhook_failures_make_one_quiet_banner_until_one_works(make):
    plugin, host, bus = make({"webhook_url": "http://hub.local/hook"})
    plugin._on_webhook_done(_state_tag(plugin), "Connection refused")
    first = host.issues[ISSUE]
    assert first.kind == "warn" and "Connection refused" in first.text
    host.issues.clear()  # dismissed
    plugin._on_webhook_done(_state_tag(plugin), "Connection refused")
    assert ISSUE not in host.issues  # not again for the same run of failures
    plugin._on_webhook_done(_state_tag(plugin), "")
    plugin._on_webhook_done(_state_tag(plugin), "HTTP 500")
    assert ISSUE in host.issues
    plugin._on_webhook_done(_state_tag(plugin), "")
    assert ISSUE not in host.issues
    first.actions[0].callback()
    assert host.opened == ["on_air"]


def test_a_test_send_shows_its_result_but_never_a_banner(make):
    plugin, host, bus = make({"webhook_url": "http://hub.local/hook"})
    plugin.send_test(True)
    assert plugin._sender.sent[-1][1] == "test"
    plugin._on_webhook_done("test", "HTTP 404")
    assert "HTTP 404" in plugin.test_result[0] and plugin.test_result[1] == "status_err" and not host.issues


def test_the_link_survives_a_restart_and_a_new_link_replaces_it(make):
    plugin, host, bus = make()
    token = plugin.token
    assert host.saves  # a link made fresh is saved at once
    cfg = plugin.get_config()
    again, _host, _bus = make(cfg)
    assert again.token == token and _Server.made[-1].token == token
    assert again.url() == f"https://192.168.1.20:8768/#{token}"
    again.new_link()
    assert again.token != token and _Server.made[-1].token == again.token


def test_switching_the_page_off_stops_the_server(make):
    plugin, host, bus = make()
    server = _Server.made[0]
    plugin.set_page(False)
    assert server.stopped and plugin.url() == ""
    assert make({"page": False})[0].url() == "" and len(_Server.made) == 1


def test_bad_config_falls_back_to_the_defaults(make):
    plugin = make({"trigger": "moon", "token": "x", "webhook_method": "PUT", "webhook_url": 5})[0]
    assert plugin.trigger == "camera" and plugin.webhook_method == "POST" and plugin.webhook_url == ""
    assert len(plugin.token) >= 16


def test_settings_section_and_header_follow_the_state(make, qt_widgets):
    plugin, host, bus = make({"webhook_url": "http://hub.local/hook"})
    header = qt_widgets(plugin.create_header_widget())
    section = plugin.create_settings_sections()[0]
    qt_widgets(section.widget)
    section.on_show()
    assert section.tab == "on_air" and header.isHidden()
    bus.camera_watched.emit(True)
    _later(plugin, 2)
    assert not header.isHidden()
    assert section.widget._state_lbl.fullText() == "On air" and section.widget._url_lbl.fullText() == plugin.url()
    section.widget._url_edit.setText("ftp://nope")
    assert plugin.webhook_url == "ftp://nope" and not section.widget._url_problem.isHidden()


def test_quitting_while_on_air_turns_the_light_off(make, monkeypatch):
    plugin, host, bus = make({"webhook_url": "http://hub.local/hook"})
    bus.camera_watched.emit(True)
    _later(plugin, 2)
    plugin._sender.sent.clear()
    plugin.shutdown()
    assert plugin._sender.states() == [(b'{"on_air": false}', "quit")] and plugin._sender.waited == [3.0]
    assert _Server.made[0].stopped


def test_quitting_off_air_sends_nothing(make):
    plugin, host, bus = make({"webhook_url": "http://hub.local/hook"})
    plugin._sender.sent.clear()
    plugin.shutdown()
    assert not plugin._sender.sent and not plugin._sender.waited


def test_a_failed_state_is_sent_again_with_backoff_until_it_works(make):
    plugin, host, bus = make({"webhook_url": "http://hub.local/hook"})
    timer = plugin._retry_timer
    delays = []
    for _ in range(6):
        plugin._on_webhook_done(_state_tag(plugin), "Connection refused")
        delays.append(timer.interval() // 1000)
        assert timer.isActive()
        timer.timeout.emit()  # the retry goes out
    assert delays == [5, 15, 60, 300, 300, 300]
    assert len({tag for _req, tag in plugin._sender.sent}) == 1  # always the same change, sent again
    plugin._on_webhook_done(_state_tag(plugin), "")
    assert not timer.isActive() and ISSUE not in host.issues


def test_clearing_the_address_ends_the_retries_and_the_banner(make):
    plugin, host, bus = make({"webhook_url": "http://hub.local/hook"})
    plugin._on_webhook_done(_state_tag(plugin), "Connection refused")
    assert ISSUE in host.issues and plugin._retry_timer.isActive()
    plugin.set_webhook(url="")
    assert ISSUE not in host.issues and not plugin._retry_timer.isActive()
    assert plugin.diagnostics()["On Air webhook"] == "not set"


def test_a_newer_change_replaces_the_retries(make):
    plugin, host, bus = make({"webhook_url": "http://hub.local/hook"})
    old = _state_tag(plugin)
    plugin._on_webhook_done(old, "Connection refused")
    assert plugin._retry_timer.isActive()
    bus.camera_watched.emit(True)
    _later(plugin, 2)
    assert not plugin._retry_timer.isActive() and _state_tag(plugin) != old
    plugin._on_webhook_done(old, "Connection refused")  # a late answer about the old one changes nothing
    assert not plugin._retry_timer.isActive()


def test_the_link_follows_a_network_change(make):
    plugin, host, bus = make()
    plugin._addresses = lambda: [SimpleNamespace(ip="10.0.0.7")]
    saves = host.saves
    section = plugin.create_settings_sections()[0]
    section.on_show()
    assert plugin.url().startswith("https://10.0.0.7:") and host.saves > saves
