"""Telescope's visual theme: palette tokens, QSS stylesheet, and theme application."""

from PyQt6.QtGui import QColor, QPalette

# ── Palette ───────────────────────────────────────────────────────────────────
# Surfaces run darkest-to-lightest: the window canvas sits *behind* the
# panels, so panels read as raised rather than cut out of the background.

BG            = "#0e1319"   # window canvas, gutters between panels
SURFACE       = "#151b23"   # panel/card fill
SURFACE_RAISE = "#1a212a"   # inputs, subsections, inset wells
SURFACE_SUNK  = "#0a0e13"   # preview letterbox, anything that reads as a hole
CHROME        = "#11161d"   # header bar, footer bar

BORDER        = "#28323e"
BORDER_STRONG = "#3a4654"
BORDER_HOVER  = "#52657a"

TEXT          = "#e8edf4"
TEXT_DIM      = "#94a4b6"
TEXT_FAINT    = "#7f8d9e"
TEXT_DISABLED = "#5e6b79"

ACCENT        = "#6aa9ed"   # icons, headings, slider fill, focus rings
ACCENT_SOFT   = "#8bbcf2"   # value readouts
FILL          = "#2f6fd0"   # filled/primary buttons, selected segments
FILL_HOVER    = "#3d82ea"
FILL_PRESS    = "#2860b4"

OK            = "#66bb6a"
WARN          = "#ffa726"
ERR           = "#ef5350"
DIM           = "#78909c"

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
    font-weight: 700;
    letter-spacing: 0.7px;
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
    font-size: 8pt;
    font-weight: 700;
    letter-spacing: 0.7px;
}}

/* ── Panels ─────────────────────────────────────────────────────────────── */
QFrame#card {{
    background-color: {SURFACE};
    border: 1px solid {BORDER};
    border-radius: 12px;
}}
QFrame#separator {{
    background-color: {BORDER};
    max-height: 1px;
    border: none;
}}
QLabel#card_title {{
    font-size: 9pt;
    font-weight: 700;
    color: {TEXT};
    letter-spacing: 1.1px;
}}
QLabel#card_subtitle {{
    color: {TEXT_FAINT};
    font-size: 9pt;
}}
QLabel#section_title {{
    color: {ACCENT};
    font-size: 8pt;
    font-weight: 700;
    letter-spacing: 0.7px;
    margin-top: 4px;
    margin-bottom: 1px;
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
    font-size: 9pt;
    font-weight: 500;
}}
QLabel#val {{
    color: {ACCENT_SOFT};
    font-family: monospace;
    font-size: 9pt;
}}
QLabel#status_ok   {{ color: {OK}; }}
QLabel#status_warn {{ color: {WARN}; }}
QLabel#status_err  {{ color: {ERR}; }}
QLabel#status_dim  {{ color: {DIM}; }}
QLabel#fps_lbl {{
    color: {OK};
    font-family: monospace;
    font-size: 13pt;
    font-weight: 700;
}}
QLabel#dialog_title {{
    color: {TEXT};
    font-size: 15pt;
    font-weight: 600;
}}
QLabel#dialog_subtitle {{
    color: {TEXT_FAINT};
    font-size: 9pt;
    margin-bottom: 3px;
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
    background-color: #37474f;
}}

/* Segmented toggles: a radio/checkbox row styled as one joined pill strip.
   Purely visual - the widgets stay ordinary radios in a QButtonGroup, so
   exclusivity and existing signal wiring are untouched. */
QRadioButton[segmented="true"], QCheckBox[segmented="true"] {{
    background-color: {SURFACE_RAISE};
    border: 1px solid {BORDER_STRONG};
    color: {TEXT_DIM};
    font-weight: 600;
    font-size: 9pt;
    min-height: 28px;
    padding: 0 14px;
    margin: 0;
    spacing: 0;
}}
QRadioButton[segmented="true"]::indicator, QCheckBox[segmented="true"]::indicator {{
    width: 0;
    height: 0;
    border: none;
    margin: 0;
}}
QRadioButton[segPos="first"], QCheckBox[segPos="first"] {{
    border-top-left-radius: 7px;
    border-bottom-left-radius: 7px;
}}
QRadioButton[segPos="last"], QCheckBox[segPos="last"] {{
    border-top-right-radius: 7px;
    border-bottom-right-radius: 7px;
    border-left: none;
}}
QRadioButton[segPos="mid"], QCheckBox[segPos="mid"] {{
    border-left: none;
}}
QRadioButton[segPos="only"], QCheckBox[segPos="only"] {{
    border-radius: 7px;
}}
QRadioButton[segmented="true"]:hover, QCheckBox[segmented="true"]:hover {{
    background-color: #212a35;
    color: {TEXT};
}}
QRadioButton[segmented="true"]:checked, QCheckBox[segmented="true"]:checked {{
    background-color: {FILL};
    border-color: {FILL};
    color: #ffffff;
}}
QRadioButton[segmented="true"]:disabled, QCheckBox[segmented="true"]:disabled {{
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
    background: #2b3540;
    border-radius: 2px;
    margin-left: 7px;
    margin-right: 7px;
}}
QSlider::sub-page:horizontal {{
    background: {ACCENT};
    border-radius: 2px;
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
    background: #9ccbff;
}}
QSlider::handle:horizontal:disabled {{
    background: #4c5c6b;
}}
QSlider::groove:horizontal:disabled {{
    background: #18202a;
}}
QSlider::sub-page:horizontal:disabled {{
    background: #37474f;
}}

/* ── Buttons ────────────────────────────────────────────────────────────── */
QPushButton {{
    min-height: 30px;
    background-color: #2a3542;
    border: 1px solid {BORDER_STRONG};
    border-radius: 7px;
    padding: 0 13px;
    color: {TEXT};
    font-weight: 600;
}}
QPushButton:hover {{
    background-color: #35424f;
    border-color: {BORDER_HOVER};
}}
QPushButton:pressed {{
    background-color: #222c37;
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
QPushButton[uiRole="success"] {{
    background-color: #2f6a4c;
    border-color: #3b7f5c;
    color: #ffffff;
}}
QPushButton[uiRole="success"]:hover {{
    background-color: #3b8460;
}}
QPushButton[uiRole="danger"] {{
    background-color: #6d3a3e;
    border-color: #8a4b50;
    color: #ffffff;
}}
QPushButton[uiRole="danger"]:hover {{
    background-color: #87484d;
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
QPushButton#lens_button {{
    background-color: {SURFACE_RAISE};
    border: 1px solid {BORDER_STRONG};
    color: #c8d5e3;
    text-align: center;
}}
QPushButton#lens_button:hover {{
    background-color: #212c38;
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
    background-color: #a94742;
    border-color: #c75c54;
}}
QPushButton#start_btn[streaming=true]:hover {{
    background-color: #c75c54;
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
    color: #b8c5d3;
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
    background-color: #212a35;
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
QMenu::item:selected {{
    background-color: {FILL};
    color: #ffffff;
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
    background: #2d3845;
    border-radius: 5px;
    min-height: 32px;
}}
QScrollBar::handle:vertical:hover {{
    background: #3c4a59;
}}
QScrollBar:horizontal {{
    background: transparent;
    height: 11px;
    margin: 0;
}}
QScrollBar::handle:horizontal {{
    background: #2d3845;
    border-radius: 5px;
    min-width: 32px;
}}
QScrollBar::handle:horizontal:hover {{
    background: #3c4a59;
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


def apply_theme(app):
    """Install theme onto QApplication."""
    app.setStyle("Fusion")
    app.setPalette(_palette())
    app.setStyleSheet(QSS)
