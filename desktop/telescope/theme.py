"""Telescope's visual theme: palette tokens, QSS stylesheet, and theme application."""

from PyQt6.QtCore import QEvent, QObject, Qt
from PyQt6.QtGui import QColor, QCursor, QPalette
from PyQt6.QtWidgets import QApplication, QMenu, QProxyStyle, QStyle, QWidget

# ── Palette ───────────────────────────────────────────────────────────────────
# Surfaces run darkest-to-lightest: the window canvas sits *behind* the
# panels, so panels read as raised rather than cut out of the background.

BG            = "#0f1216"   # window canvas, gutters between panels
SURFACE       = "#161a20"   # panel/card fill
SURFACE_RAISE = "#1d222a"   # inputs, subsections, inset wells
SURFACE_HOVER = "#242a33"   # hover fill for raised controls
SURFACE_SUNK  = "#0a0c0f"   # preview letterbox, anything that reads as a hole
CHROME        = "#121519"   # header bar, footer bar

BORDER        = "#232932"
BORDER_STRONG = "#2f3641"
BORDER_HOVER  = "#4a5462"

TEXT          = "#e7eaef"
TEXT_DIM      = "#a1abb8"
TEXT_FAINT    = "#7a8491"
TEXT_DISABLED = "#525b67"

ACCENT        = "#aa9cf5"   # icons, slider fill, focus rings (pastel lavender)
ACCENT_SOFT   = "#cdc4fa"   # value readouts
FILL          = "#6a58cf"   # filled/primary buttons, selected segments; white text stays >4.5:1
FILL_HOVER    = "#7867dc"
FILL_PRESS    = "#5b4bb8"

OK            = "#5fc98c"
WARN          = "#f0b65c"
ERR           = "#ef6f6b"
DIM           = "#8792a0"

# Kept as a dict because app.py maps a status "kind" onto an object name and
# monitoring.py reaches for the raw hex to colour its readouts inline.
STATUS_COLORS = {
    "status_ok":   OK,
    "status_warn": WARN,
    "status_err":  ERR,
    "status_dim":  DIM,
}

FONT_STACK = (
    "'Inter', 'Segoe UI', -apple-system, BlinkMacSystemFont, 'Ubuntu', "
    "'Cantarell', 'Helvetica Neue', 'Arial', sans-serif"
)


def _palette() -> QPalette:
    """Dark QPalette for native chrome (menus, message-box icons, text selection)."""
    p = QPalette()
    c = QColor
    p.setColor(QPalette.ColorRole.Window,          c(BG))
    p.setColor(QPalette.ColorRole.WindowText,      c(TEXT))
    p.setColor(QPalette.ColorRole.Base,            c(SURFACE_RAISE))
    p.setColor(QPalette.ColorRole.AlternateBase,   c(SURFACE))
    p.setColor(QPalette.ColorRole.ToolTipBase,     c(SURFACE_RAISE))
    p.setColor(QPalette.ColorRole.ToolTipText,     c(TEXT))
    p.setColor(QPalette.ColorRole.Text,            c(TEXT))
    p.setColor(QPalette.ColorRole.Button,          c(SURFACE_RAISE))
    p.setColor(QPalette.ColorRole.ButtonText,      c(TEXT))
    p.setColor(QPalette.ColorRole.BrightText,      c("#ffffff"))
    p.setColor(QPalette.ColorRole.Link,            c(ACCENT))
    p.setColor(QPalette.ColorRole.Highlight,       c(FILL))
    p.setColor(QPalette.ColorRole.HighlightedText, c("#ffffff"))
    p.setColor(QPalette.ColorRole.PlaceholderText, c(TEXT_FAINT))
    for role in (QPalette.ColorRole.WindowText, QPalette.ColorRole.Text,
                 QPalette.ColorRole.ButtonText):
        p.setColor(QPalette.ColorGroup.Disabled, role, c(TEXT_DISABLED))
    return p


QSS = f"""
* {{
    font-family: {FONT_STACK};
    font-size: 9.5pt;
}}

/* ── Shell ──────────────────────────────────────────────────────────────── */
QMainWindow, QDialog {{
    background-color: {BG};
    color: {TEXT};
}}
QWidget#rail_content, QWidget#center_column, QWidget#body_root {{
    background-color: {BG};
}}
QScrollArea, QScrollArea > QWidget, QScrollArea > QWidget > QWidget {{
    background-color: {BG};
    border: none;
}}

QWidget#header_bar {{
    background-color: {CHROME};
    border-bottom: 1px solid {BORDER};
}}
QWidget#footer_bar {{
    background-color: {CHROME};
    border-top: 1px solid {BORDER};
}}
QLabel#header_label {{
    color: {TEXT_FAINT};
    font-size: 8pt;
    font-weight: 600;
}}
QLabel#header_value {{
    color: {TEXT};
    font-size: 10pt;
    font-weight: 600;
}}
QFrame#header_divider {{
    background-color: {BORDER};
    max-width: 1px;
    border: none;
}}
QLabel#footer_label {{
    color: {TEXT_FAINT};
}}
/* ── Panels ─────────────────────────────────────────────────────────────── */
QFrame#card {{
    background-color: {SURFACE};
    border: 1px solid {BORDER};
    border-radius: 14px;
}}
QFrame#separator {{
    background-color: {BORDER};
    max-height: 1px;
    border: none;
}}
QLabel#card_title {{
    font-size: 11pt;
    font-weight: 600;
    color: {TEXT};
}}
QLabel#card_subtitle {{
    color: {TEXT_FAINT};
    font-size: 9pt;
}}
QLabel#section_title {{
    color: {TEXT_FAINT};
    font-size: 7.5pt;
    font-weight: 700;
    letter-spacing: 1px;
    margin-top: 10px;
    margin-bottom: 0px;
}}
QFrame#subsection {{
    background-color: {SURFACE_RAISE};
    border: 1px solid {BORDER};
    border-radius: 8px;
}}
QWidget#ip_row_container, QWidget#battery_row, QWidget#form_row,
QWidget#form_row_content, QWidget#inline_control, QWidget#lens_panel,
QWidget#card_body {{
    background-color: transparent;
    border: none;
}}

/* ── Text roles ─────────────────────────────────────────────────────────── */
QLabel {{
    color: {TEXT};
}}
QLabel#form_label, QLabel#dim {{
    color: {TEXT_DIM};
}}
QLabel#val {{
    color: {ACCENT_SOFT};
    font-weight: 600;
}}
QLabel#status_ok   {{ color: {OK}; }}
QLabel#status_warn {{ color: {WARN}; }}
QLabel#status_err  {{ color: {ERR}; }}
QLabel#status_dim  {{ color: {DIM}; }}
QLabel#fps_lbl {{
    color: {TEXT};
    font-weight: 600;
}}
QLabel#key_chip {{
    color: {ACCENT_SOFT};
    font-weight: 600;
}}
QLabel#dialog_title {{
    color: {TEXT};
    font-size: 14pt;
    font-weight: 600;
}}
QLabel#dialog_subtitle {{
    color: {TEXT_DIM};
}}
QLabel#step_badge, QLabel#step_badge_done {{
    min-width: 26px; max-width: 26px; min-height: 26px; max-height: 26px;
    border-radius: 13px;
    font-weight: 700;
    qproperty-alignment: AlignCenter;
}}
QLabel#step_badge {{
    background-color: {SURFACE_RAISE};
    border: 1px solid {BORDER_STRONG};
    color: {ACCENT_SOFT};
}}
QLabel#step_badge_done {{
    background-color: {FILL};
    border: 1px solid {FILL};
    color: white;
}}
QLabel#step_title {{
    font-weight: 600;
}}
QLabel:disabled {{
    color: {TEXT_DISABLED};
}}

/* ── Inputs ─────────────────────────────────────────────────────────────── */
QComboBox {{
    min-height: 30px;
    padding: 0 26px 0 10px;
    border: 1px solid {BORDER_STRONG};
    border-radius: 7px;
    background-color: {SURFACE_RAISE};
    color: {TEXT};
}}
QComboBox::drop-down {{
    width: 22px;
    border: none;
}}
/* No ::down-arrow rule: styling it without an image asset only ever
   produces a rotated-looking box, so Fusion draws its own arrow, which
   picks up the palette. */
QComboBox QAbstractItemView {{
    background-color: {SURFACE_RAISE};
    border: 1px solid {BORDER_HOVER};
    border-radius: 7px;
    padding: 4px;
    outline: none;
    selection-background-color: {FILL};
    selection-color: #ffffff;
}}
QLineEdit, QTextEdit, QSpinBox, QDoubleSpinBox {{
    min-height: 30px;
    padding: 0 10px;
    border: 1px solid {BORDER_STRONG};
    border-radius: 7px;
    background-color: {SURFACE_RAISE};
    color: {TEXT};
    selection-background-color: {FILL};
    selection-color: #ffffff;
}}
QSpinBox::up-button, QSpinBox::down-button,
QDoubleSpinBox::up-button, QDoubleSpinBox::down-button {{
    width: 0;
    border: none;
}}
QComboBox:hover, QLineEdit:hover, QSpinBox:hover, QDoubleSpinBox:hover {{
    border-color: {BORDER_HOVER};
}}
QLineEdit:focus, QTextEdit:focus, QComboBox:focus,
QSpinBox:focus, QDoubleSpinBox:focus {{
    color: #ffffff;
    border: 1px solid {ACCENT};
}}
QComboBox:disabled, QLineEdit:disabled, QSpinBox:disabled, QDoubleSpinBox:disabled {{
    background-color: {BG};
    color: {TEXT_DISABLED};
    border-color: {BORDER};
}}

/* ── Check / radio ──────────────────────────────────────────────────────── */
QRadioButton, QCheckBox {{
    spacing: 8px;
    color: {TEXT};
    min-height: 28px;
    background: transparent;
}}
QRadioButton:disabled, QCheckBox:disabled {{
    color: {TEXT_DISABLED};
}}
QCheckBox::indicator, QRadioButton::indicator {{
    width: 16px;
    height: 16px;
    border: 1px solid {BORDER_HOVER};
    background-color: {SURFACE_RAISE};
}}
QCheckBox::indicator {{
    border-radius: 4px;
}}
QRadioButton::indicator {{
    border-radius: 9px;
}}
QCheckBox::indicator:hover, QRadioButton::indicator:hover {{
    border-color: {ACCENT};
}}
QCheckBox::indicator:checked {{
    background-color: {FILL};
    border-color: {FILL};
    /* A tick would need an image asset; the fill plus an inset ring reads
       clearly enough at 16px and keeps the app asset-free. */
}}
QRadioButton::indicator:checked {{
    background-color: {FILL};
    border: 4px solid {SURFACE_RAISE};
    outline: 1px solid {FILL};
}}
QCheckBox::indicator:disabled, QRadioButton::indicator:disabled {{
    border-color: {BORDER};
    background-color: {BG};
}}
QCheckBox::indicator:checked:disabled, QRadioButton::indicator:checked:disabled {{
    background-color: {BORDER_HOVER};
}}

/* Segmented toggles: checkable SegmentButtons in a zero-gap row. Segment
   widths come from the layout (equal shares of the row), never from their text. */
QPushButton[segmented="true"] {{
    background-color: {SURFACE_RAISE};
    border: 1px solid {BORDER_STRONG};
    border-radius: 0;
    color: {TEXT_DIM};
    font-weight: 600;
    min-height: 30px;
    padding: 0 8px;
    margin: 0;
}}
QPushButton[segPos="first"] {{
    border-top-left-radius: 7px;
    border-bottom-left-radius: 7px;
}}
QPushButton[segPos="last"] {{
    border-top-right-radius: 7px;
    border-bottom-right-radius: 7px;
    border-left: none;
}}
QPushButton[segPos="mid"] {{
    border-left: none;
}}
QPushButton[segPos="only"] {{
    border-radius: 7px;
}}
QPushButton[segmented="true"]:hover {{
    background-color: {SURFACE_HOVER};
    border-color: {BORDER_STRONG};
    color: {TEXT};
}}
QPushButton[segmented="true"]:checked {{
    background-color: {FILL};
    border-color: {FILL};
    color: #ffffff;
}}
QPushButton[segmented="true"]:checked:hover {{
    background-color: {FILL_HOVER};
}}
QPushButton[segmented="true"]:disabled {{
    background-color: {BG};
    border-color: {BORDER};
    color: {TEXT_DISABLED};
}}

/* ── Sliders ────────────────────────────────────────────────────────────── */
QSlider {{
    background: transparent;
    height: 20px;
    padding-left: 3px;
    padding-right: 3px;
}}
QSlider::groove:horizontal {{
    border: none;
    height: 4px;
    background: {BORDER_STRONG};
    border-radius: 2px;
    margin-left: 7px;
    margin-right: 7px;
}}
QSlider::sub-page:horizontal {{
    background: {ACCENT};
    border-radius: 2px;
    margin-left: 7px;  /* Qt doesn't apply the groove's margin to the fill, which then stuck out left of the handle at 0 */
}}
QSlider::handle:horizontal {{
    background: {ACCENT};
    width: 14px;
    height: 14px;
    margin-top: -5px;
    margin-bottom: -5px;
    border-radius: 7px;
}}
QSlider::handle:horizontal:hover {{
    background: {ACCENT_SOFT};
}}
QSlider::handle:horizontal:disabled {{
    background: {TEXT_DISABLED};
}}
QSlider::groove:horizontal:disabled {{
    background: {SURFACE_RAISE};
}}
QSlider::sub-page:horizontal:disabled {{
    background: {BORDER_HOVER};
}}

/* ── Buttons ────────────────────────────────────────────────────────────── */
QPushButton {{
    min-height: 30px;
    background-color: {SURFACE_HOVER};
    border: 1px solid {BORDER_STRONG};
    border-radius: 7px;
    padding: 0 13px;
    color: {TEXT};
    font-weight: 600;
}}
QPushButton:hover {{
    background-color: #2c333e;
    border-color: {BORDER_HOVER};
}}
QPushButton:pressed {{
    background-color: {SURFACE_RAISE};
}}
QPushButton:disabled {{
    background-color: {BG};
    border-color: {BORDER};
    color: {TEXT_DISABLED};
}}
QPushButton:checked {{
    background-color: {FILL};
    border-color: {FILL};
    color: #ffffff;
}}
QPushButton:checked:hover {{
    background-color: {FILL_HOVER};
}}
QPushButton[uiRole="primary"] {{
    background-color: {FILL};
    border-color: {FILL};
    color: #ffffff;
}}
QPushButton[uiRole="primary"]:hover {{
    background-color: {FILL_HOVER};
    border-color: {FILL_HOVER};
}}
QPushButton[uiRole="primary"]:pressed {{
    background-color: {FILL_PRESS};
}}
QPushButton[uiRole="primary"]:disabled {{
    background-color: {BG};
    border-color: {BORDER};
    color: {TEXT_DISABLED};
}}
QPushButton[uiRole="success"] {{
    background-color: #2c6b4a;
    border-color: #377e59;
    color: #ffffff;
}}
QPushButton[uiRole="success"]:hover {{
    background-color: #358259;
}}
QPushButton[uiRole="danger"] {{
    background-color: #6b3638;
    border-color: #874648;
    color: #ffffff;
}}
QPushButton[uiRole="danger"]:hover {{
    background-color: #824244;
}}
QPushButton[uiRole="quiet"] {{
    background-color: transparent;
    border-color: {BORDER};
    color: {TEXT_DIM};
}}
QPushButton[uiRole="quiet"]:hover {{
    background-color: {SURFACE_RAISE};
    border-color: {BORDER_HOVER};
    color: {TEXT};
}}
QPushButton[uiRole="quiet"]:disabled {{
    background-color: transparent;
    border-color: {BORDER};
    color: {TEXT_DISABLED};
}}
QPushButton[uiRole="quiet"]:checked {{
    background-color: {SURFACE_RAISE};
    border-color: {ACCENT};
    color: {ACCENT_SOFT};
}}
QPushButton[uiRole="quiet"][camera_off=true] {{
    background-color: {SURFACE_RAISE};
    border-color: {ACCENT};
    color: {ACCENT_SOFT};
}}
QPushButton#lens_button {{
    background-color: {SURFACE_RAISE};
    border: 1px solid {BORDER_STRONG};
    color: {TEXT};
    text-align: center;
}}
QPushButton#lens_button:hover {{
    background-color: {SURFACE_HOVER};
    border-color: {BORDER_HOVER};
}}
QPushButton#lens_button:checked {{
    background-color: {FILL};
    border-color: {ACCENT};
    color: #ffffff;
}}
QPushButton#start_btn {{
    font-size: 10pt;
    font-weight: 700;
    min-height: 38px;
    padding: 0 22px;
    border-radius: 8px;
    background-color: {FILL};
    border: 1px solid {FILL};
    color: #ffffff;
}}
QPushButton#start_btn:hover {{
    background-color: {FILL_HOVER};
    border-color: {FILL_HOVER};
}}
QPushButton#start_btn[streaming=true] {{
    background-color: #b04a46;
    border-color: #c45a55;
}}
QPushButton#start_btn[streaming=true]:hover {{
    background-color: #c45a55;
}}
QPushButton#card_action {{
    min-height: 28px;
    padding: 0 10px;
    border-radius: 7px;
    background-color: transparent;
    border: 1px solid {BORDER};
    color: {TEXT_DIM};
}}
QPushButton#card_action:hover {{
    background-color: {SURFACE_RAISE};
    border-color: {BORDER_HOVER};
    color: {TEXT};
}}
QPushButton#card_action[muted=true] {{
    border-color: #5a3134;
    color: {ERR};
}}
QPushButton#card_action[muted=true]:hover {{
    background-color: #2a1d20;
    border-color: {ERR};
}}
QFrame#focus_marker {{
    background: transparent;
    border: 2px solid {ACCENT_SOFT};
    border-radius: 6px;
}}
QFrame#banner {{
    background-color: #2a1d20;
    border: 1px solid #5a3134;
    border-radius: 12px;
}}
QFrame#banner[kind="warn"] {{
    background-color: #2a2419;
    border-color: #5c4a2a;
}}
QLabel#banner_title {{
    font-weight: 600;
    color: {TEXT};
}}
QLabel#banner_text {{
    color: {TEXT_DIM};
}}
QLabel#banner_details {{
    color: {TEXT};
    font-family: 'JetBrains Mono', 'Cascadia Mono', 'DejaVu Sans Mono', monospace;
    background-color: rgba(0, 0, 0, 0.25);
    border-radius: 6px;
    padding: 6px 8px;
}}
QPushButton#banner_action {{
    min-height: 30px;
    padding: 0 12px;
    border-radius: 8px;
    background-color: rgba(255, 255, 255, 0.06);
    border: 1px solid rgba(255, 255, 255, 0.14);
    color: {TEXT};
}}
QPushButton#banner_action:hover {{
    background-color: rgba(255, 255, 255, 0.12);
}}
QPushButton#banner_close {{
    background-color: transparent;
    border: none;
    border-radius: 6px;
}}
QPushButton#banner_close:hover {{
    background-color: rgba(255, 255, 255, 0.08);
}}
QPushButton#icon_btn {{
    background-color: transparent;
    border: 1px solid {BORDER};
    border-radius: 8px;
}}
QPushButton#icon_btn:hover {{
    background-color: {SURFACE_RAISE};
    border-color: {BORDER_HOVER};
}}
QToolButton#section_toggle {{
    min-height: 32px;
    padding: 0 4px;
    border: none;
    background-color: transparent;
    color: {TEXT_DIM};
    font-size: 10pt;
    font-weight: 600;
    text-align: left;
}}
QToolButton#section_toggle:hover {{
    color: {ACCENT};
}}

/* The lens capability summary: supporting text, deliberately quiet. */
QLabel#caps_line {{
    color: {TEXT_FAINT};
    font-size: 8pt;
}}

/* ── Stream tiles (several streams at once) ─────────────────────────────── */
QFrame#stream_tile {{
    background-color: {SURFACE};
    border: 1px solid {BORDER};
    border-radius: 10px;
}}
QFrame#stream_tile:hover {{
    border-color: {BORDER_HOVER};
}}
QFrame#stream_tile[focused=true] {{
    border-color: {ACCENT};
}}
QLabel#stream_tile_thumb {{
    background-color: {SURFACE_SUNK};
    border-radius: 6px;
    color: {TEXT_FAINT};
}}
QLabel#stream_tile_name {{
    color: {TEXT};
    font-weight: 600;
}}
QLabel#stream_tile_out {{
    color: {TEXT_FAINT};
    font-size: 8pt;
}}

/* ── Preview stage ──────────────────────────────────────────────────────── */
QFrame#preview_stage {{
    background-color: {SURFACE_SUNK};
    border: 1px solid {BORDER};
    border-radius: 12px;
}}
QLabel#preview_surface {{
    background-color: {SURFACE_SUNK};
    border-radius: 11px;
    color: {TEXT_FAINT};
    font-size: 10pt;
}}
QLabel#preview_surface[uiRole="camera_off"] {{
    color: {ACCENT_SOFT};
    font-size: 11pt;
}}
QWidget#preview_toolbar {{
    background-color: {SURFACE};
    border-top: 1px solid {BORDER};
    border-bottom-left-radius: 11px;
    border-bottom-right-radius: 11px;
}}

/* ── Containers Qt draws itself ─────────────────────────────────────────── */
QGroupBox {{
    margin-top: 12px;
    padding: 20px 14px 14px 14px;
    border: 1px solid {BORDER_STRONG};
    border-radius: 10px;
    color: {TEXT};
    font-size: 9pt;
    font-weight: 600;
}}
QGroupBox::title {{
    subcontrol-origin: margin;
    left: 12px;
    padding: 0 6px;
    color: {TEXT_DIM};
    background-color: {BG};
}}
QTextBrowser#guide_body {{
    background-color: transparent;
    border: none;
    padding: 0;
}}
QListWidget, QTextBrowser {{
    background-color: {SURFACE_RAISE};
    border: 1px solid {BORDER};
    border-radius: 8px;
    color: {TEXT};
    padding: 4px;
}}
QListWidget::item {{
    padding: 5px 6px;
    border-radius: 5px;
}}
QListWidget::item:selected {{
    background-color: {FILL};
    color: #ffffff;
}}
QListWidget::item:hover:!selected {{
    background-color: {SURFACE_HOVER};
}}
QMenu {{
    background-color: {SURFACE_RAISE};
    border: 1px solid {BORDER_HOVER};
    border-radius: 8px;
    padding: 5px;
    color: {TEXT};
}}
QMenu::item {{
    padding: 7px 22px 7px 14px;
    border-radius: 5px;
}}
QMenu::item:disabled {{
    color: {TEXT_DISABLED};
}}
QMenu::item:selected {{
    background-color: {FILL};
    color: #ffffff;
}}
QLabel#menu_section {{
    color: {TEXT_FAINT};
    font-size: 7.5pt;
    font-weight: 700;
    letter-spacing: 1px;
    padding: 8px 22px 3px 25px;
}}
QMenu::separator {{
    height: 1px;
    background-color: {BORDER};
    margin: 5px 8px;
}}
QToolTip {{
    background-color: {SURFACE_RAISE};
    border: 1px solid {BORDER_HOVER};
    border-radius: 6px;
    padding: 5px 8px;
    color: {TEXT};
}}
QScrollBar:vertical {{
    background: transparent;
    width: 11px;
    margin: 0;
}}
QScrollBar::handle:vertical {{
    background: {BORDER_STRONG};
    border-radius: 5px;
    min-height: 32px;
}}
QScrollBar::handle:vertical:hover {{
    background: {BORDER_HOVER};
}}
QScrollBar:horizontal {{
    background: transparent;
    height: 11px;
    margin: 0;
}}
QScrollBar::handle:horizontal {{
    background: {BORDER_STRONG};
    border-radius: 5px;
    min-width: 32px;
}}
QScrollBar::handle:horizontal:hover {{
    background: {BORDER_HOVER};
}}
QScrollBar::add-line, QScrollBar::sub-line {{
    width: 0;
    height: 0;
    border: none;
    background: none;
}}
QScrollBar::add-page, QScrollBar::sub-page {{
    background: none;
}}
"""


TIP_WAKE_MS = 250     # hover this long before a tooltip shows (Qt's own is 700)
TIP_LINE_CHARS = 64   # a longer tooltip wraps at about this many characters


def _tip_owner(widget):
    """The widget whose tooltip shows over `widget`: itself, or the nearest parent with one."""
    while widget is not None and not widget.toolTip():
        widget = widget.parentWidget()
    return widget


class _TipFilter(QObject):
    """On Qt's one tooltip window only: wraps long text instead of one line across the screen, and keeps the tip up
    while the mouse is still on what it's about (Qt hides it after a few seconds regardless)."""

    def __init__(self, parent):
        super().__init__(parent)
        self._owner = None
        self._wrapping = False

    def eventFilter(self, tip, event):
        kind = event.type()
        if kind == QEvent.Type.Show or (kind == QEvent.Type.Resize and tip.isVisible() and not self._wrapping):
            # Resize too: moving to another control reuses the window without showing it again
            self._owner = _tip_owner(QApplication.widgetAt(QCursor.pos()))
            self._wrap(tip)
        elif kind == QEvent.Type.Resize and not self._wrapping:
            self._wrap(tip)
        elif kind == QEvent.Type.Timer and self._owner is not None and tip.isVisible():
            # Qt's timers only hide the tip: one while the mouse is still on its control is Qt's time limit
            if _tip_owner(QApplication.widgetAt(QCursor.pos())) is self._owner:
                return True
        return False

    def _wrap(self, tip):
        limit = tip.fontMetrics().averageCharWidth() * TIP_LINE_CHARS
        margins = tip.contentsMargins()
        width = limit + margins.left() + margins.right()
        # Already wrapping means it's ours (Qt turns it off for every plain tip), and Qt may have sized it narrower
        if tip.width() <= width and not (tip.wordWrap() and tip.width() != width):
            return
        self._wrapping = True
        try:
            tip.setWordWrap(True)
            height = tip.heightForWidth(width)
            if height > 0:
                tip.resize(width, height)
        finally:
            self._wrapping = False


class _PopupStyle(QProxyStyle):
    """Fusion, but menus and tooltips get a see-through window, so the stylesheet's rounded border isn't drawn over
    square corners. Tooltips also show sooner, wrap, and stay while hovered (_TipFilter)."""

    def __init__(self, base):
        super().__init__(base)
        self._tip_filter = _TipFilter(self)

    def polish(self, arg):
        # Runs once per widget as it's set up, unlike an app-wide event filter that Python sees every event through
        if (isinstance(arg, QWidget) and (isinstance(arg, QMenu) or arg.inherits("QTipLabel"))
                and not arg.testAttribute(Qt.WidgetAttribute.WA_TranslucentBackground)):
            arg.setWindowFlags(arg.windowFlags() | Qt.WindowType.FramelessWindowHint
                               | Qt.WindowType.NoDropShadowWindowHint)
            arg.setAttribute(Qt.WidgetAttribute.WA_TranslucentBackground)
            if arg.inherits("QTipLabel"):
                arg.installEventFilter(self._tip_filter)  # the one tooltip window Qt reuses for every tip
        return super().polish(arg)

    def styleHint(self, hint, option=None, widget=None, returnData=None):
        if hint == QStyle.StyleHint.SH_ToolTip_WakeUpDelay:
            return TIP_WAKE_MS
        return super().styleHint(hint, option, widget, returnData)


_style = None


def apply_theme(app):
    """Install theme onto QApplication. A no-op once installed: re-applying repolishes every live widget."""
    if app.styleSheet() == QSS:
        return
    global _style
    _style = _PopupStyle("Fusion")  # kept here too: app.style() hands back a plain QStyle wrapper
    app.setStyle(_style)
    app.setPalette(_palette())
    app.setStyleSheet(QSS)
