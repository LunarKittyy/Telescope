import pytest

from telescope import phones
from telescope.phones import (
    DESKTOP_OUTDATED, LOCAL_ONLY, NOT_PAIRED, PHONE_OUTDATED, READY, ROUTE_USB, ROUTE_WIFI, UNREACHABLE, USB_APP_CLOSED,
    USB_NEEDS_ATTENTION, USB_NO_ADB, USB_NO_CABLE, USB_OTHER_PHONE, USB_UNAUTHORIZED,
    Phone, RouteResolver, UsbTunnels,
)
from telescope.session_client import (
    HELLO_MISSING, HELLO_NONE, HELLO_OK, SESSION_PROTOCOL, Hello, PingResult,
)

PHONE = Phone(id="ph-1", name="Pixel", token="tok", ips=["192.168.1.40"])


OLD_APP = "old app"  # answers on the session port, but from before /v1/hello


class FakeNet:
    """Who answers at which base URL: {base: (phone_id, ping_status, local_only[, protocol])} or OLD_APP."""

    def __init__(self, answers):
        self.answers = answers
        self.pinged = []

    def factory(self, base, token):
        net = self

        class Client:
            def __init__(self):
                self.base = base

            def hello(self, timeout=None):
                a = net.answers.get(base)
                if a == OLD_APP:
                    return Hello(HELLO_MISSING)
                if not a:
                    return Hello(HELLO_NONE)
                protocol = a[3] if len(a) > 3 else SESSION_PROTOCOL
                return Hello(HELLO_OK, a[0], "Phone", protocol, "1.0", 7)

            def ping(self):
                net.pinged.append(base)
                a = net.answers.get(base)
                if not a or a == OLD_APP:
                    return PingResult("unreachable")
                return PingResult(a[1], streaming=False, busy=False, local_only=a[2], phone_id=a[0])
        return Client()


def _resolver(net, states=(), adb=True, discovered=()):
    ports = {}

    def forward(serial, remote):
        ports.setdefault(serial, 40000 + len(ports))
        return ports[serial]
    tunnels = UsbTunnels(forward, lambda serial, local: None)
    return RouteResolver(
        adb_available=lambda: adb, adb_device_states=lambda: list(states), tunnels=tunnels,
        discover=lambda pid: list(discovered), client_factory=net.factory, wifi_timeout=0.1,
    ), ports


def test_usb_wins_whenever_this_phone_answers_over_the_cable():
    net = FakeNet({"http://127.0.0.1:40000": ("ph-1", "paired", False),
                   "http://192.168.1.40:8766": ("ph-1", "paired", False)})
    resolver, _ = _resolver(net, states=[("SER", "device")])
    res = resolver.resolve(PHONE)
    assert res.status == READY and res.route.kind == "usb" and res.route.serial == "SER"
    assert res.usb_note is None


def test_falls_back_to_wifi_and_says_why():
    net = FakeNet({"http://192.168.1.40:8766": ("ph-1", "paired", False)})
    for states, adb, note in (
        ([], True, USB_NO_CABLE),
        ([("SER", "unauthorized")], True, USB_UNAUTHORIZED),
        ([("SER", "device")], True, USB_APP_CLOSED),
        ([], False, USB_NO_ADB),
    ):
        resolver, _ = _resolver(net, states=states, adb=adb)
        res = resolver.resolve(PHONE)
        assert (res.status, res.route.kind, res.usb_note) == (READY, "wifi", note), note


def test_a_different_phone_on_usb_is_not_mistaken_for_ours():
    net = FakeNet({"http://127.0.0.1:40000": ("someone-else", "paired", False),
                   "http://192.168.1.40:8766": ("ph-1", "paired", False)})
    resolver, _ = _resolver(net, states=[("OTHER", "device")])
    res = resolver.resolve(PHONE)
    assert res.route.kind == "wifi" and res.usb_note == USB_OTHER_PHONE


def test_picks_our_phone_out_of_several_plugged_in():
    net = FakeNet({"http://127.0.0.1:40000": ("someone-else", "paired", False),
                   "http://127.0.0.1:40001": ("ph-1", "paired", False)})
    resolver, _ = _resolver(net, states=[("A", "device"), ("B", "device")])
    res = resolver.resolve(PHONE)
    assert res.route.kind == "usb" and res.route.serial == "B"


def test_forced_usb_never_silently_uses_wifi():
    net = FakeNet({"http://192.168.1.40:8766": ("ph-1", "paired", False)})
    resolver, _ = _resolver(net, states=[("SER", "unauthorized")])
    res = resolver.resolve(PHONE, ROUTE_USB)
    assert res.status == USB_NEEDS_ATTENTION and res.route is None and res.usb_note == USB_UNAUTHORIZED


def test_forced_wifi_skips_usb():
    net = FakeNet({"http://127.0.0.1:40000": ("ph-1", "paired", False),
                   "http://192.168.1.40:8766": ("ph-1", "paired", False)})
    resolver, ports = _resolver(net, states=[("SER", "device")])
    res = resolver.resolve(PHONE, ROUTE_WIFI)
    assert res.route.kind == "wifi" and ports == {}


def test_discovered_address_is_tried_so_a_new_dhcp_lease_still_works():
    net = FakeNet({"http://192.168.1.77:8766": ("ph-1", "paired", False)})
    resolver, _ = _resolver(net, discovered=["192.168.1.77"])
    res = resolver.resolve(PHONE)
    assert res.route == phones.Route("wifi", "192.168.1.77")


def test_phone_that_forgot_this_computer_says_so():
    net = FakeNet({"http://192.168.1.40:8766": ("ph-1", "not_paired", False)})
    resolver, _ = _resolver(net)
    assert resolver.resolve(PHONE).status == NOT_PAIRED


def test_local_only_phone_reachable_on_wifi_asks_for_usb():
    net = FakeNet({"http://192.168.1.40:8766": ("ph-1", "paired", True)})
    resolver, _ = _resolver(net)
    assert resolver.resolve(PHONE).status == LOCAL_ONLY


def test_nothing_answering_is_unreachable():
    resolver, _ = _resolver(FakeNet({}))
    assert resolver.resolve(PHONE).status == UNREACHABLE


def test_an_older_phone_app_is_named_instead_of_unreachable():
    net = FakeNet({"http://192.168.1.40:8766": ("ph-1", "paired", False, SESSION_PROTOCOL - 1)})
    resolver, _ = _resolver(net)
    res = resolver.resolve(PHONE)
    assert res.status == PHONE_OUTDATED and res.phone_version == "1.0"
    assert net.pinged == []  # an older protocol's ping may not even parse


def test_a_newer_phone_app_asks_for_a_desktop_update():
    net = FakeNet({"http://192.168.1.40:8766": ("ph-1", "paired", False, SESSION_PROTOCOL + 1)})
    resolver, _ = _resolver(net)
    assert resolver.resolve(PHONE).status == DESKTOP_OUTDATED


def test_an_app_from_before_hello_counts_as_outdated_and_keeps_its_usb_serial():
    net = FakeNet({"http://127.0.0.1:40000": OLD_APP})
    resolver, _ = _resolver(net, states=[("SER", "device")])
    res = resolver.resolve(PHONE)
    assert res.status == PHONE_OUTDATED and res.route.serial == "SER"
    net = FakeNet({"http://192.168.1.40:8766": OLD_APP})
    resolver, _ = _resolver(net)
    assert resolver.resolve(PHONE).status == PHONE_OUTDATED


def test_a_working_address_beats_an_outdated_answer_elsewhere():
    net = FakeNet({"http://192.168.1.40:8766": OLD_APP, "http://192.168.1.77:8766": ("ph-1", "paired", False)})
    resolver, _ = _resolver(net, discovered=["192.168.1.77"])
    res = resolver.resolve(PHONE)
    assert res.status == READY and res.phone_version == "1.0"


def test_usb_tunnels_are_shared_and_released_last():
    calls = []
    tunnels = UsbTunnels(lambda s, r: calls.append(("fwd", s)) or 5000,
                         lambda s, l: calls.append(("unfwd", s, l)))
    assert tunnels.acquire("A", 8766) == 5000
    assert tunnels.acquire("A", 8766) == 5000
    tunnels.release("A", 8766)
    assert calls == [("fwd", "A")]
    tunnels.release("A", 8766)
    assert calls == [("fwd", "A"), ("unfwd", "A", 5000)]


def test_phone_round_trips_and_rejects_incomplete_entries():
    assert Phone.from_dict(PHONE.to_dict()) == PHONE
    with pytest.raises(ValueError):
        Phone.from_dict({"id": "x", "name": "y"})
