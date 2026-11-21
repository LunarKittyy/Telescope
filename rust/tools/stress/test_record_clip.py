import unittest
from unittest.mock import Mock, patch

import record_clip


class RecordClipTest(unittest.TestCase):
    def test_session_start_and_stop_include_the_desktop_version(self):
        con = Mock()
        con.getresponse.return_value.read.return_value = b"{}"
        with patch.object(record_clip, "connection", return_value=con):
            for action in ("start", "stop"):
                record_clip.post(7480, "token", "/v1/session", {"action": action})
                method, path, body, headers = con.request.call_args.args
                self.assertEqual((method, path), ("POST", "/v1/session"))
                self.assertEqual(record_clip.json.loads(body), {"action": action})
                self.assertEqual(headers["Authorization"], "Bearer token")
                self.assertEqual(headers["X-Telescope-Version"], record_clip.VERSION)
                self.assertTrue(record_clip.VERSION)
                con.close.assert_called()


if __name__ == "__main__":
    unittest.main()
