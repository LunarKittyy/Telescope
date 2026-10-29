#!/usr/bin/env python3
"""Starts the Python desktop (desktop/main.py, same arguments) with two things a scripted session needs and the
app has no way to do from outside: an action that opens Add phone (connection.add_phone), and the QR code's
content written to the file TELESCOPE_PARITY_OFFER names, so the fake phone can "scan" it. The traffic to the
phone is the app's own; only these two hooks are added. The Rust desktop needs the same two for the pair script.
"""

import os
import runpy
import sys
from pathlib import Path

DESKTOP = Path(__file__).resolve().parents[2] / "desktop"


def add_hooks():
    from telescope import pairing
    from telescope.plugins import connection
    from telescope.shortcuts import ShortcutAction

    start = pairing.PairingServer.start

    def start_and_tell(self, *args, **kwargs):
        offer = start(self, *args, **kwargs)
        target = os.environ.get("TELESCOPE_PARITY_OFFER")
        if target:
            Path(target).write_text(offer.payload, encoding="utf-8")
        return offer
    pairing.PairingServer.start = start_and_tell

    create_actions = connection.ConnectionPlugin.create_actions

    def with_add_phone(self):
        def run(_params):
            self.open_add_phone()
            return "Add phone"
        return create_actions(self) + [ShortcutAction("connection.add_phone", "Add phone", "Telescope", run)]
    connection.ConnectionPlugin.create_actions = with_add_phone


def main():
    sys.path.insert(0, str(DESKTOP))
    if "--action" not in sys.argv:  # a one-off --action only talks to the running copy
        add_hooks()
    sys.argv = [str(DESKTOP / "main.py")] + sys.argv[1:]
    runpy.run_path(str(DESKTOP / "main.py"), run_name="__main__")


if __name__ == "__main__":
    main()
