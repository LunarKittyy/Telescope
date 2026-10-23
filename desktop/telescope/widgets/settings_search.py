"""Finding settings: what's on the tabs is read off the widgets, then ranked against a typed query with some tolerance."""

from dataclasses import dataclass, field

from PyQt6.QtWidgets import QAbstractButton, QCheckBox, QComboBox, QFrame, QLabel, QWidget

from telescope import fuzzy
from telescope.i18n import tr

KEYWORDS = "keywords"
"""The dynamic property that holds extra words a setting should be found by."""

_FIELD_WEIGHT = {"name": 1.0, "keywords": 0.9, "card": 0.62, "tab": 0.5, "detail": 0.55, "extra": 0.42}


def keywords(widget: QWidget, text: str) -> QWidget:
    """Words (synonyms, what it's called elsewhere) a setting is found by on top of what it says; returns the widget."""
    widget.setProperty(KEYWORDS, text)
    return widget


@dataclass
class Entry:
    tab: str
    tab_name: str
    card: str
    name: str
    target: QWidget
    detail: str = ""
    keywords: str = ""
    extra: str = ""
    card_keywords: str = ""
    tab_keywords: str = ""
    card_entry: bool = False
    tab_entry: bool = False
    _doc: object = field(default=None, repr=False)

    @property
    def path(self) -> str:
        if self.tab_entry:
            return tr("Tab")
        same = {self.name, self.tab_name}  # a card named like its tab, or like the setting, adds nothing
        return f"{self.tab_name}  ›  {self.card}" if self.card and self.card not in same else self.tab_name


def _doc(entry: Entry) -> fuzzy.Doc:
    if entry._doc is None:
        entry._doc = fuzzy.Doc(name=entry.name, keywords=entry.keywords, card=f"{entry.card} {entry.card_keywords}",
                               tab=f"{entry.tab_name} {entry.tab_keywords}", detail=entry.detail, extra=entry.extra)
    return entry._doc


def score(entry: Entry, query: str, partial: bool = False) -> float:
    """How well the entry fits the query, 0 for not at all."""
    result = _doc(entry).score(query, _FIELD_WEIGHT, partial=partial)
    return result * 0.92 if entry.card_entry else result  # a setting outranks the card it's in


def _rank(entries: list, query: str, limit: int, partial: bool) -> list:
    ranked = []
    for order, entry in enumerate(entries):
        s = score(entry, query, partial)
        if s > 0:
            ranked.append((-s, len(entry.name), order, entry))
    ranked.sort(key=lambda r: r[:3])
    return [r[3] for r in ranked[:limit]]


def search_ex(entries: list, query: str, limit: int = 12) -> tuple:
    """(entries that fit every word of the query, best first; False) or, if none do, (ones that fit some; True)."""
    hits = _rank(entries, query, limit, partial=False)
    if hits:
        return hits, False
    return _rank(entries, query, limit, partial=True), True


def search(entries: list, query: str, limit: int = 12) -> list:
    """The entries that fit the query, best first."""
    return search_ex(entries, query, limit)[0]


# ── Reading the tabs ──────────────────────────────────────────────────────────

def _hidden(widget: QWidget, top: QWidget) -> bool:
    node = widget
    while node is not None:
        if node.isHidden():
            return True
        if node is top:
            return False
        node = node.parentWidget()
    return False


def _card_of(widget: QWidget, top: QWidget):
    node = widget.parentWidget()
    while node is not None:
        if isinstance(node, QFrame) and node.objectName() == "card":
            return node
        if node is top:
            return None
        node = node.parentWidget()
    return None


def _card_title(card) -> str:
    if card is None:
        return ""
    label = card.findChild(QLabel, "card_title")
    return label.text() if label is not None else ""


def _row_texts(row: QWidget) -> tuple:
    """(detail, extra): the small print under the name, and what the controls and tooltips in the row say."""
    detail, extra = [], []
    for label in row.findChildren(QLabel):
        if label.objectName() == "setting_name" or not label.text().strip():
            continue
        detail.append(label.text())
    seen = {row.toolTip()} if row.toolTip() else set()
    for widget in [row, *row.findChildren(QWidget)]:
        tip = widget.toolTip()
        if tip and tip not in seen:
            seen.add(tip)
            extra.append(tip)
        if isinstance(widget, QAbstractButton) and widget.text().strip():
            extra.append(widget.text())
        elif isinstance(widget, QComboBox):
            extra.extend(widget.itemText(i) for i in range(min(widget.count(), 40)))
    return " ".join(detail), " ".join(extra)


def collect(tabs: list) -> list:
    """Entries for every setting shown: tabs is [(tab id, tab name, [SettingsSection])]."""
    entries = []
    for tab, tab_name, sections in tabs:
        entries.append(Entry(tab, tab_name, "", tab_name, sections[0].widget, card_entry=True, tab_entry=True))
        for section in sections:
            top = section.widget
            tab_keywords = getattr(section, "keywords", "")
            made = []

            def add(target, name, detail="", extra="", kw=None, card_entry=False):
                if not name.strip() or _hidden(target, top):
                    return
                card = _card_of(target, top)
                own = kw if kw is not None else (target.property(KEYWORDS) or "")
                made.append(Entry(tab, tab_name, _card_title(card), name.strip(), target, detail, own, extra,
                                  (card.property(KEYWORDS) or "") if card is not None else "", tab_keywords,
                                  card_entry))

            for frame in ([top] if isinstance(top, QFrame) else []) + top.findChildren(QFrame):
                if frame.objectName() == "setting_row":
                    name = frame.findChild(QLabel, "setting_name")
                    if name is not None:
                        detail, extra = _row_texts(frame)
                        add(frame, name.text(), detail, extra)
                elif frame.objectName() == "card":
                    title = frame.findChild(QLabel, "card_title")
                    if title is not None:
                        add(frame, title.text(), kw=frame.property(KEYWORDS) or "", card_entry=True)
            for label in top.findChildren(QLabel, "form_label"):
                if label.text().strip():
                    add(label, label.text())
            for box in top.findChildren(QCheckBox):
                if box.text().strip() and not _inside_row(box, top):
                    add(box, box.text(), extra=box.toolTip())
            entries.extend(made)
    return entries


def _inside_row(widget: QWidget, top: QWidget) -> bool:
    node = widget.parentWidget()
    while node is not None:
        if isinstance(node, QFrame) and node.objectName() == "setting_row":
            return True
        if node is top:
            return False
        node = node.parentWidget()
    return False
