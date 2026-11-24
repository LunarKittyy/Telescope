#!/usr/bin/env python3
import glob
import tempfile
import unittest
from pathlib import Path

import fake_phone


class ServeTest(unittest.TestCase):
    def test_stopping_the_phone_removes_its_temporary_folder(self):
        with tempfile.TemporaryDirectory() as folder:
            clip = Path(folder) / "clip.h264"
            clip.write_bytes(b"\x00\x00\x00\x01\x09\xf0frame")
            before = set(glob.glob(str(Path(tempfile.gettempdir()) / "fake-phone-*")))
            _, _, stop = fake_phone.serve(clip, host="127.0.0.1", presence=False, ports=(0, 0))
            made = set(glob.glob(str(Path(tempfile.gettempdir()) / "fake-phone-*"))) - before
            stop()
            left = [p for p in made if Path(p).exists()]
        self.assertEqual(len(made), 1)
        self.assertEqual(left, [])


if __name__ == "__main__":
    unittest.main()
