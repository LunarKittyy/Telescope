"""In-window problem banners: what went wrong and the button that fixes it, instead of a popup."""

from dataclasses import dataclass, field
from typing import Callable, Optional

from PyQt6.QtCore import QSize, Qt, pyqtSignal
from PyQt6.QtGui import QGuiApplication
from PyQt6.QtWidgets import QFrame, QHBoxLayout, QLabel, QPushButton, QVBoxLayout, QWidget

from telescope import theme
from telescope.widgets.common import WrapLabel, create_vector_icon

KIND_COLORS = {"err": theme.ERR, "warn": theme.WARN}


@dataclass
class BannerAction:
    label: str
    callback: Callable[[], None]
    keeps_banner: bool = False  # most actions retry or move on, so the banner goes; Copy stays


def copy_action(text: str, label: str = "Copy command") -> BannerAction:
    return BannerAction(label, lambda: QGuiApplication.clipboard().setText(text), keeps_banner=True)


@dataclass
class Issue:
    title: str
    text: str = ""
    actions: list = field(default_factory=list)
    kind: str = "err"
    details: str = ""  # shown monospaced under the text, e.g. a command to run


class Banner(QFrame):
    dismissed = pyqtSignal()

    def __init__(self, issue: Issue, parent: Optional[QWidget] = None):
        super().__init__(parent)
        self.issue = issue
        self.setObjectName("banner")
        self.setProperty("kind", issue.kind)
        lay = QHBoxLayout(self)
        lay.setContentsMargins(14, 12, 10, 12)
        lay.setSpacing(12)

        icon = QLabel()
        icon.setPixmap(create_vector_icon("alert", KIND_COLORS.get(issue.kind, theme.ERR)).pixmap(QSize(20, 20)))
        icon.setAlignment(Qt.AlignmentFlag.AlignTop)
        lay.addWidget(icon)

        col = QVBoxLayout()
        col.setSpacing(3)
        self.title_lbl = QLabel(issue.title)
        self.title_lbl.setObjectName("banner_title")
        col.addWidget(self.title_lbl)
        self.text_lbl = WrapLabel(issue.text)
        self.text_lbl.setObjectName("banner_text")
        self.text_lbl.setVisible(bool(issue.text))
        col.addWidget(self.text_lbl)
        self.details_lbl = WrapLabel(issue.details)
        self.details_lbl.setObjectName("banner_details")
        self.details_lbl.setTextInteractionFlags(Qt.TextInteractionFlag.TextSelectableByMouse)
        self.details_lbl.setVisible(bool(issue.details))
        col.addWidget(self.details_lbl)
        lay.addLayout(col, 1)

        self.buttons: list[QPushButton] = []
        for action in issue.actions:
            btn = QPushButton(action.label)
            btn.setObjectName("banner_action")
            btn.clicked.connect(lambda _=False, a=action: self._run(a))
            lay.addWidget(btn, 0, Qt.AlignmentFlag.AlignVCenter)
            self.buttons.append(btn)

        close = QPushButton()
        close.setObjectName("banner_close")
        close.setIcon(create_vector_icon("close", theme.TEXT_DIM))
        close.setIconSize(QSize(14, 14))
        close.setFixedSize(28, 28)
        close.setToolTip("Dismiss")
        close.clicked.connect(self.dismissed.emit)
        lay.addWidget(close, 0, Qt.AlignmentFlag.AlignTop)
        self.close_btn = close

    def _run(self, action: BannerAction):
        if not action.keeps_banner:
            self.dismissed.emit()
        action.callback()


class BannerArea(QWidget):
    """Keyed issues stacked above the window body. Showing a key again replaces its banner."""

    def __init__(self, parent: Optional[QWidget] = None):
        super().__init__(parent)
        self.setObjectName("banner_area")
        self._lay = QVBoxLayout(self)
        self._lay.setContentsMargins(16, 12, 16, 0)
        self._lay.setSpacing(8)
        self._banners: dict[str, Banner] = {}
        self.setVisible(False)

    def show_issue(self, key: str, issue: Issue):
        self.clear_issue(key)
        banner = Banner(issue, self)
        banner.dismissed.connect(lambda k=key, b=banner: self._dismiss(k, b))
        self._banners[key] = banner
        self._lay.addWidget(banner)
        self.setVisible(True)

    def _dismiss(self, key: str, banner: Banner):
        if self._banners.get(key) is banner:
            self.clear_issue(key)

    def clear_issue(self, key: Optional[str] = None):
        keys = list(self._banners) if key is None else [key]
        for k in keys:
            banner = self._banners.pop(k, None)
            if banner is not None:
                banner.hide()
                banner.deleteLater()
        self.setVisible(bool(self._banners))

    def issue(self, key: str) -> Optional[Issue]:
        banner = self._banners.get(key)
        return banner.issue if banner else None

    def banner(self, key: str) -> Optional[Banner]:
        return self._banners.get(key)

    def keys(self) -> list:
        return list(self._banners)
