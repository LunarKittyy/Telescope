"""Black and white: an example plugin. docs/writing-plugins.md walks through it line by line."""

import cv2
from PyQt6.QtCore import QSignalBlocker
from PyQt6.QtWidgets import QCheckBox

from telescope.plugin import TelescopePlugin
from telescope.widgets.common import add_card_header, card_layout, create_card


def _grey(frame):
    """A BGR frame with its colours taken out (still three channels, as every frame has to be)."""
    return cv2.cvtColor(cv2.cvtColor(frame, cv2.COLOR_BGR2GRAY), cv2.COLOR_GRAY2BGR)


class BlackAndWhitePlugin(TelescopePlugin):
    name = "black_and_white"  # the same as "name" in plugin.json and the folder's name
    when_off = "Without it there's no Black and white card and the picture keeps its colours."
    panel_region = "right"  # with the cards about the phone's picture
    device_local = True  # each phone remembers its own choice

    def setup(self, host, bus):
        self._host = host
        self._on = False
        self._box = None

    def create_panel(self):
        card = create_card()
        lay = card_layout(card)
        add_card_header(lay, "Black and white", "camera")
        self._box = QCheckBox("Black and white picture")
        self._box.setChecked(self._on)
        self._box.toggled.connect(self._switch)
        lay.addWidget(self._box)
        return card

    def _switch(self, on: bool):
        self._on = on
        self._host.schedule_save()  # saved a moment later, with every other plugin's settings

    def process_frame(self, frame):
        return _grey(frame) if self._on else frame

    def frame_step(self):
        # For a stream behind the panels: what process_frame does now, frozen
        return _grey if self._on else None

    def get_config(self) -> dict:
        return {"on": self._on}

    def set_config(self, cfg: dict):
        self._on = cfg.get("on") is True
        if self._box is not None:
            with QSignalBlocker(self._box):
                self._box.setChecked(self._on)
