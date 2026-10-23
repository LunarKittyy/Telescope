"""Cards and sections you can fold away by clicking their title, remembered between runs.

Nothing here knows which cards exist. `Collapsing.install()` looks at a finished panel and finds what the shared card
helpers in `common.py` made: a card (`create_card` + `add_card_header`) and the section headings inside it
(`add_section_heading`). A plugin that builds its card with those helpers is collapsible with no extra code, and so is
one added later.
"""

import logging
from typing import Callable, Iterable, Optional

from PyQt6 import sip
from PyQt6.QtCore import QEvent, QObject, Qt
from PyQt6.QtGui import QPainter
from PyQt6.QtWidgets import QApplication, QBoxLayout, QFrame, QLabel, QLayoutItem, QToolButton, QVBoxLayout, QWidget

from telescope.widgets.common import paint_focus
from telescope.i18n import tr

# The object names common.py gives the pieces, which is how they're told apart from the rows around them.
CARD = "card"
CARD_TITLE = "card_title"
SECTION_HEAD = "section_head"
SECTION_TITLE = "section_title"
BODY = "card_body"
"""What gets hidden: a card's rows below its header, or a section's rows below its heading."""

_INSTALLED = "collapsibleCard"


def _title_of(item: QLayoutItem) -> Optional[QLabel]:
    """The card title if item is a card's header row (the layout add_card_header makes)."""
    lay = item.layout()
    if lay is None:
        return None
    for i in range(lay.count()):
        widget = lay.itemAt(i).widget()
        if isinstance(widget, QLabel) and widget.objectName() == CARD_TITLE:
            return widget
    return None


def _is_section_head(item: QLayoutItem) -> bool:
    widget = item.widget()
    return widget is not None and widget.objectName() == SECTION_HEAD


def _set_hot(label: QLabel, on: bool):
    """Brighten (or dim again) a title through its `hot` property, which theme.py's stylesheet reads."""
    if bool(label.property("hot")) == on:
        return
    label.setProperty("hot", on)
    label.style().unpolish(label)
    label.style().polish(label)


class _Toggle(QObject):
    """Makes some widgets of a header toggle a fold when clicked, and shades the title while the pointer is on them."""

    def __init__(self, title: QLabel, hit: Iterable[QWidget], on_click: Callable[[], None]):
        super().__init__(title)  # lives and dies with the title
        self._title = title
        self._on_click = on_click
        for widget in hit:
            widget.installEventFilter(self)

    def eventFilter(self, obj, event):
        kind = event.type()
        if kind == QEvent.Type.Enter:
            _set_hot(self._title, True)
        elif kind in (QEvent.Type.Leave, QEvent.Type.Hide, QEvent.Type.EnabledChange):
            _set_hot(self._title, False)
        elif (kind == QEvent.Type.MouseButtonRelease and event.button() == Qt.MouseButton.LeftButton
              and obj.rect().contains(event.position().toPoint())):
            button = obj if isinstance(obj, _FoldButton) else obj.parentWidget()
            if isinstance(button, _FoldButton) and button.isEnabled():
                return False  # let the native button release its pressed state and emit clicked
            self._on_click()
            return True
        return False


class _FoldButton(QToolButton):
    """A native, named toggle for keyboard and screen-reader users, painted as the existing title label."""

    def __init__(self, title: QLabel):
        super().__init__()
        self.setObjectName("fold_heading")
        self.setCheckable(True)
        self.setFocusPolicy(Qt.FocusPolicy.StrongFocus)
        self.setText(title.text())
        self.setAccessibleName(title.text())
        lay = QVBoxLayout(self)
        lay.setContentsMargins(6, 3, 6, 3)  # keep the focus ring clear of the text

    def sizeHint(self):
        return self.layout().sizeHint()

    def minimumSizeHint(self):
        return self.sizeHint()

    def paintEvent(self, _event):
        painter = QPainter(self)
        paint_focus(painter, self)

    def keyPressEvent(self, event):
        if event.key() in (Qt.Key.Key_Return, Qt.Key.Key_Enter):
            if not event.isAutoRepeat():
                self.click()
            event.accept()
        else:
            super().keyPressEvent(event)


def _body(parent_layout: QBoxLayout) -> tuple:
    """A transparent container with the spacing its rows had in parent_layout, and its layout."""
    body = QWidget()
    body.setObjectName(BODY)
    lay = QVBoxLayout(body)
    lay.setContentsMargins(0, 0, 0, 0)
    lay.setSpacing(parent_layout.spacing())
    return body, lay


class Collapsing:
    """Which cards and sections are folded, and the folding.

    A fold hides one container holding the rows under a title, never the rows themselves. Plugins show and hide rows on
    their own (manual exposure, a mode toggle) and those flags are left alone, so unfolding brings back exactly what
    was visible before. State is a set of keys; what isn't in it is open. Keys of cards that aren't there right now
    (a plugin switched off) stay in the set, so switching it back on finds its folds as they were.
    """

    def __init__(self, on_change: Optional[Callable[[], None]] = None):
        self._on_change = on_change
        self._folded: set = set()
        self._bodies: dict = {}  # key -> the container that key folds
        self._buttons: dict = {}  # key -> the title's accessible toggle

    def keys(self) -> list:
        """The folded keys, for saving."""
        return sorted(self._folded)

    def load(self, keys) -> None:
        """Take what keys() saved earlier (anything else is ignored) and apply it to the cards already installed."""
        self._folded = {k for k in keys if isinstance(k, str)} if isinstance(keys, (list, tuple, set)) else set()
        for key in list(self._bodies):
            self._apply(key)

    def is_folded(self, key: str) -> bool:
        return key in self._folded

    def set_folded(self, key: str, folded: bool) -> None:
        if folded == (key in self._folded):
            return
        (self._folded.add if folded else self._folded.discard)(key)
        self._apply(key)
        if self._on_change is not None:
            self._on_change()

    def toggle(self, key: str) -> None:
        self.set_folded(key, key not in self._folded)

    def _apply(self, key: str) -> None:
        body = self._bodies.get(key)
        if body is None or sip.isdeleted(body):
            self._bodies.pop(key, None)
            self._buttons.pop(key, None)
            return
        folded, button = key in self._folded, self._buttons[key]
        focus = QApplication.focusWidget()
        if folded and focus is not None and body.isAncestorOf(focus):
            button.setFocus(Qt.FocusReason.OtherFocusReason)
        body.setVisible(not folded)
        button.setChecked(not folded)
        description = (tr("Collapsed. Press Space or Enter to expand.") if folded else
                       tr("Expanded. Press Space or Enter to collapse."))
        button.setAccessibleDescription(description)
        button.setToolTip(description)

    def install(self, panel: QWidget, scope: str) -> None:
        """Make every card in panel (the panel itself, or cards inside it) foldable, and each section in them.

        scope names the owner (the plugin) so two plugins' "Light" sections don't share a key. Call it once the panel
        is fully built: rows added to a card's layout afterwards land outside the fold. A card without the standard
        header is left as it is.
        """
        cards = ([panel] if panel.objectName() == CARD else []) + panel.findChildren(QFrame, CARD)
        for card in cards:
            try:
                self._install_card(card, scope)
            except Exception:
                logging.exception("Couldn't make a card in %s collapsible", scope)

    def _install_card(self, card: QWidget, scope: str) -> None:
        lay = card.layout()
        if card.property(_INSTALLED) or not isinstance(lay, QBoxLayout):
            return
        head = next((i for i in range(lay.count()) if _title_of(lay.itemAt(i))), None)
        if head is None:
            return
        title = _title_of(lay.itemAt(head))
        header = lay.itemAt(head).layout()
        card.setProperty(_INSTALLED, True)

        # Take out everything under the header, remembering how each was laid out.
        stretches = [lay.stretch(i) for i in range(head + 1, lay.count())]
        rows = [(lay.takeAt(head + 1), stretch) for stretch in stretches]

        used: dict = {}

        def unique(base: str) -> str:
            used[base] = used.get(base, 0) + 1
            return base if used[base] == 1 else f"{base}#{used[base]}"

        card_key = unique(f"{scope}/{title.text()}")
        card_body, card_lay = _body(lay)
        target = card_lay
        for item, stretch in rows:
            if _is_section_head(item):
                heading = item.widget()
                card_lay.addWidget(heading)
                label = heading.findChild(QLabel, SECTION_TITLE)
                section_key = unique(f"{card_key}/{label.text() if label else heading.objectName()}")
                section_body, target = _body(lay)
                card_lay.addWidget(section_body)
                if label is not None:
                    self._register(section_key, section_body, label, [heading, label], heading.layout())
                continue
            if item.widget() is not None:
                target.addWidget(item.widget(), stretch, item.alignment())
            elif item.layout() is not None:
                target.addLayout(item.layout(), stretch)
            else:
                target.addItem(item)
        lay.addWidget(card_body)
        # A card that stretched to fill spare height keeps doing so
        lay.setStretchFactor(card_body, max((s for _i, s in rows), default=0))

        hit = [header.itemAt(i).widget() for i in range(header.count())
               if isinstance(header.itemAt(i).widget(), QLabel)]
        self._register(card_key, card_body, title, hit, header)

    def _register(self, key: str, body: QWidget, title: QLabel, hit: list, header: QBoxLayout) -> None:
        self._bodies[key] = body
        button = self._buttons[key] = _FoldButton(title)
        header.replaceWidget(title, button)
        button.layout().addWidget(title)  # keep its appearance and the QLabel plugins already hold
        button.lower()  # Qt's accessible child order follows stacking order: the heading comes before its rows
        button.clicked.connect(lambda _checked=False, k=key: self.toggle(k))
        _Toggle(title, [*hit, button], lambda k=key: self.toggle(k))
        self._apply(key)
