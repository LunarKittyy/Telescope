#!/usr/bin/env python3
import http.client
import json
import socket
import threading
import unittest
from http.server import ThreadingHTTPServer

from fake_phone import Phone, PresenceResponder, handler_for, presence_packet, read_presence


class FakePresenceTest(unittest.TestCase):
    def setUp(self):
        self.phone = Phone([b"frame"], 30, "pairing-secret", "1.2.3")

    def test_mac_identity_version_addresses_and_bounds(self):
        packet = presence_packet(self.phone.token, "q", "desktop", self.phone.id, "ab" * 16, "1.2.3", ["192.168.1.2"])
        self.assertIsNotNone(read_presence(self.phone.token, packet))
        self.assertNotIn(self.phone.token.encode(), packet)
        for field in (b"desktop", b"1.2.3", b"192.168.1.2", b"TP1"):
            self.assertIsNone(read_presence(self.phone.token, packet.replace(field, b"changed", 1)))
        self.assertIsNone(read_presence("revoked", packet))
        self.assertIsNone(read_presence(self.phone.token, packet + b"x" * 512))
        bad = presence_packet(self.phone.token, "q", "desktop", self.phone.id, "ab" * 16, "1.2.3", ["224.0.0.1"])
        self.assertIsNone(read_presence(self.phone.token, bad))

    def test_exchange_replay_and_udp_only_block(self):
        responder = PresenceResponder(self.phone, "127.0.0.1", 0)
        client = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        client.bind(("127.0.0.1", 0))
        client.settimeout(0.3)
        target = responder.socket.getsockname()
        try:
            request = presence_packet(self.phone.token, "q", "desktop", self.phone.id, "ab" * 16, "1.2.4", [])
            client.sendto(request, target)
            answer, _ = client.recvfrom(513)
            challenge, _ = client.recvfrom(513)
            self.assertEqual("a", read_presence(self.phone.token, answer)[1])
            fields = read_presence(self.phone.token, challenge)
            self.assertEqual("q", fields[1])
            self.assertFalse(self.phone.streaming)
            self.assertNotIn("desktop", self.phone.desktop_versions)
            client.sendto(presence_packet(self.phone.token, "a", "desktop", self.phone.id, fields[4], "1.2.4", []), target)
            client.sendto(request, target)
            with self.assertRaises(socket.timeout):
                client.recvfrom(513)
            self.assertEqual("1.2.4", self.phone.desktop_versions["desktop"])
            self.phone.presence_blocked = True
            client.sendto(presence_packet(self.phone.token, "q", "desktop", self.phone.id, "cd" * 16, "1.2.4", []), target)
            with self.assertRaises(socket.timeout):
                client.recvfrom(513)
        finally:
            client.close()
            responder.stop()
        self.assertFalse(responder.thread.is_alive())

    def test_legacy_capability_is_optional(self):
        self.assertIsNone(self.phone.desktop_version)
        server = ThreadingHTTPServer(("127.0.0.1", 0), handler_for(self.phone))
        thread = threading.Thread(target=server.serve_forever)
        thread.start()
        try:
            for capable in (True, False):
                self.phone.presence = capable
                for path in ("/v1/hello", "/v1/ping"):
                    client = http.client.HTTPConnection(*server.server_address, timeout=1)
                    client.request("GET", path, headers={"Authorization": "Bearer " + self.phone.token})
                    body = json.loads(client.getresponse().read())
                    client.close()
                    self.assertEqual(capable, body.get("presence") == 1)
            self.assertEqual("", self.phone.desktop_version)
            client = http.client.HTTPConnection(*server.server_address, timeout=1)
            client.request("GET", "/v1/ping", headers={"Authorization": "Bearer " + self.phone.token,
                           "X-Telescope-Version": "1.2.4", "X-Telescope-Addresses": "127.0.0.1,192.168.1.3"})
            client.getresponse().read()
            client.close()
            self.assertEqual("1.2.4", self.phone.desktop_version)
            self.assertEqual(["192.168.1.3"], self.phone.desktop_candidates)
        finally:
            server.shutdown()
            server.server_close()
            thread.join()


if __name__ == "__main__":
    unittest.main()
