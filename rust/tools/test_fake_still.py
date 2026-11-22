#!/usr/bin/env python3
import json
import time
import unittest
from http.server import ThreadingHTTPServer
import threading
import http.client

from fake_phone import Phone, handler_for


class FakeStillTest(unittest.TestCase):
    def setUp(self):
        self.phone = Phone([b"frame"], 30, "secret", "1.2.3")
        self.phone.streaming = True
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), handler_for(self.phone))
        self.server.stream_port = self.server.server_address[1]
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.addCleanup(self.server.server_close)
        self.addCleanup(self.server.shutdown)

    def ask(self, method, target, body=None):
        conn = http.client.HTTPConnection("127.0.0.1", self.server.server_address[1], timeout=5)
        raw = json.dumps(body).encode() if body is not None else None
        conn.request(method, target, raw, {"Authorization": "Bearer secret", "Content-Type": "application/json"})
        answer = conn.getresponse()
        data = answer.read()
        conn.close()
        return answer, data

    def take(self, **options):
        return json.loads(self.ask("POST", "/v1/still", {"action": "take", "format": "jpeg", **options})[1])

    def wait_done(self, id):
        for _ in range(100):
            status = json.loads(self.ask("GET", f"/v1/still?id={id}")[1])
            if status["state"] == "done":
                return status
            time.sleep(0.05)
        self.fail("the photo never ended")

    def test_a_photo_ends_with_its_picture_to_fetch(self):
        started = self.take()
        self.assertTrue(started["ok"])
        status = self.wait_done(started["id"])
        self.assertEqual((status["shots"], status["taken"]), (1, 1))
        self.assertEqual(status["files"][0]["type"], "image/jpeg")
        answer, data = self.ask("GET", f"/v1/still/file?id={started['id']}&n=0")
        self.assertEqual((answer.status, answer.getheader("Content-Type")), (200, "image/jpeg"))
        self.assertEqual(int(answer.getheader("Content-Length")), len(data))
        self.assertEqual(len(data), status["files"][0]["size"])
        self.assertTrue(data.startswith(b"\xff\xd8"))
        self.assertEqual(self.ask("GET", f"/v1/still/file?id={started['id']}&n=1")[0].status, 404)

    def test_a_burst_takes_every_picture_and_the_timer_waits_first(self):
        started = self.take(count=3, timer_s=1)
        status = json.loads(self.ask("GET", f"/v1/still?id={started['id']}")[1])
        self.assertEqual((status["state"], status["taken"], status["files"]), ("waiting", 0, []))
        self.assertTrue(0 < status["countdown_ms"] <= 1000)
        self.assertEqual(len(self.wait_done(started["id"])["files"]), 3)

    def test_only_one_photo_at_a_time_and_a_forgotten_one_is_gone(self):
        started = self.take(timer_s=1)
        self.assertEqual(self.take()["code"], "busy")
        self.assertEqual(json.loads(self.ask("POST", "/v1/still", {"action": "forget", "id": started["id"]})[1]), {"ok": True})
        self.assertEqual(self.ask("GET", f"/v1/still?id={started['id']}")[0].status, 404)
        self.assertEqual(json.loads(self.ask("POST", "/v1/still", {"action": "forget", "id": "nothing"})[1]), {"ok": True})
        self.assertTrue(self.take()["ok"])

    def test_what_the_lens_cannot_do_is_refused_with_the_phones_codes(self):
        for options, code in ((dict(format="heic"), "unsupported"), (dict(raw="1"), "unsupported"), (dict(format="gif"), "bad_option"),
                              (dict(format="none"), "bad_option"), (dict(extension="night"), "unsupported"), (dict(count=0), "bad_option"),
                              (dict(count=61), "bad_option"), (dict(bracket_ev="-1,0,1"), "unsupported")):
            self.assertEqual(self.take(**options)["code"], code, options)

    def test_nothing_is_taken_without_a_running_camera_and_stopping_clears_the_photos(self):
        started = self.take()
        self.phone.state["camera_off"] = True
        self.assertEqual(self.take()["code"], "camera_off")
        self.phone.state["camera_off"] = False
        self.ask("POST", "/v1/session", {"action": "stop"})
        self.assertEqual(self.ask("GET", f"/v1/still?id={started['id']}")[0].status, 404)
        self.phone.streaming = False
        self.assertEqual(self.take()["code"], "camera_off")

    def test_the_lenses_say_they_take_photos_and_a_wrong_token_is_refused(self):
        self.assertEqual(self.phone.state["stills"], 1)
        for cam in self.phone.state["cameras"]:
            self.assertEqual(cam["stills"]["formats"], ["jpeg"])
        conn = http.client.HTTPConnection("127.0.0.1", self.server.server_address[1], timeout=5)
        conn.request("GET", "/v1/still?id=x", headers={"Authorization": "Bearer wrong"})
        self.assertEqual(conn.getresponse().status, 401)


if __name__ == "__main__":
    unittest.main()
