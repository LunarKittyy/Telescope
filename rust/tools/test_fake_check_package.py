#!/usr/bin/env python3
"""check_package.py reports a package that can't be inspected instead of passing it."""
import sys
import tempfile
import unittest
from pathlib import Path

import check_package


@unittest.skipUnless(sys.platform == "linux", "uses ldd")
class CheckPackageTest(unittest.TestCase):
    def setUp(self):
        check_package.problems.clear()
        self.addCleanup(check_package.problems.clear)

    def test_a_library_ldd_cant_read_is_a_problem(self):
        with tempfile.TemporaryDirectory() as folder:
            gp = Path(folder) / "lib" / "gphoto2"
            gp.mkdir(parents=True)
            (gp / "camlibs").mkdir()
            (gp / "iolibs").mkdir()
            for lib in ("libgphoto2.so.6", "libgphoto2_port.so.12", "camlibs/ptp2.so", "iolibs/usb1.so"):
                (gp / lib).write_bytes(b"not an ELF file")
            check_package.check_gphoto2("pkg", Path(folder))
        self.assertEqual(len(check_package.problems), 4, check_package.problems)

    def test_a_missing_library_is_a_problem_not_a_crash(self):
        with tempfile.TemporaryDirectory() as folder:
            self.assertIsNone(check_package.first_match("pkg", Path(folder), "libavutil.so.*"))
        self.assertEqual(len(check_package.problems), 1)


if __name__ == "__main__":
    unittest.main()
