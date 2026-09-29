"""Tooltips: shown sooner, wrapped to a readable width, and kept up while the mouse is on their control."""

import pytest
from PyQt6.QtCore import QCoreApplication, QEvent, QPoint, QTimerEvent
from PyQt6.QtWidgets import QApplication, QLabel, QStyle, QToolTip, QWidget

import telescope.theme as theme


@pytest.fixture
def themed(qapp):
    theme.apply_theme(qapp)
    return qapp


def _dispose(*widgets):
    """Delete now: a window left alive can trip up Qt in a later test."""
    for w in widgets:
        w.close()
        w.deleteLater()
    QCoreApplication.sendPostedEvents(None, QEvent.Type.DeferredDelete)


def _tip_label():
    return next((w for w in QApplication.topLevelWidgets() if w.inherits("QTipLabel") and w.isVisible()), None)


def test_tooltips_show_sooner(themed):
    assert themed.style().styleHint(QStyle.StyleHint.SH_ToolTip_WakeUpDelay) == theme.TIP_WAKE_MS < 700


def test_a_long_tooltip_wraps_and_a_short_one_stays_on_one_line(themed):
    owner = QWidget()
    owner.show()
    QApplication.processEvents()  # showing a window hides any tooltip: let that happen first
    try:
        long_text = "All the way right is Dynamic: as much as the connection carries, lowered by itself " * 3
        QToolTip.showText(QPoint(50, 50), long_text, owner)
        QApplication.processEvents()
        tip = _tip_label()
        assert tip is not None
        limit = tip.fontMetrics().averageCharWidth() * theme.TIP_LINE_CHARS
        margins = tip.contentsMargins()
        assert tip.wordWrap() and tip.width() <= limit + margins.left() + margins.right()
        wrapped_height = tip.height()

        QToolTip.showText(QPoint(50, 50), "Settings", owner)  # the same window, reused
        QApplication.processEvents()
        assert tip.width() < limit and not tip.wordWrap()
        assert wrapped_height >= tip.height() + tip.fontMetrics().height() * 2  # several lines, none cut off
    finally:
        QToolTip.hideText()
        _dispose(owner)


def test_a_tip_stays_while_the_mouse_is_on_its_control(themed, monkeypatch):
    tip_filter = theme._style._tip_filter
    owner, elsewhere = QLabel("owner"), QLabel("elsewhere")
    owner.setToolTip("About the owner")
    tip = QLabel()
    tip.show()
    try:
        under = [owner]
        monkeypatch.setattr(theme.QApplication, "widgetAt", lambda _pos: under[0])
        tip_filter.eventFilter(tip, QEvent(QEvent.Type.Show))
        assert tip_filter.eventFilter(tip, QTimerEvent(1)) is True  # Qt's time limit: kept
        under[0] = elsewhere
        assert tip_filter.eventFilter(tip, QTimerEvent(1)) is False  # moved off: Qt hides it as usual
    finally:
        _dispose(tip, owner, elsewhere)
