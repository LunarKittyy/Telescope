#!/usr/bin/env python3
import tempfile
import unittest
from pathlib import Path

import i18n


class WritePoTest(unittest.TestCase):
    def test_an_obsolete_multiline_entry_is_commented_on_every_line(self):
        entry = i18n.Entry("One\nTwo\n")
        entry.msgstr = ["Eins\nZwei\n"]
        entry.obsolete = True
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / "x.po"
            i18n.write_po(path, "Language: x\n", [entry])
            lines = [l for l in path.read_text(encoding="utf-8").splitlines() if l.startswith('"') or l.startswith("#~")]
        self.assertTrue(lines and all(l.startswith("#~") for l in lines), lines)

    def test_trn_open_texts_are_found(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / "a.rs"
            path.write_text('fn f() { trn_open("One late", "{n} late", 2); }\n')
            found = self._find(path)
        self.assertEqual([(e.msgid, e.plural) for e in found], [("One late", "{n} late")])

    @staticmethod
    def _find(path):
        original = Path.relative_to
        Path.relative_to = lambda self, *a: self
        try:
            return i18n.find_texts(path)[0]
        finally:
            Path.relative_to = original


if __name__ == "__main__":
    unittest.main()
