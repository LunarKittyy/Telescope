"""The redesign's line icons: a 16-unit grid drawn with a 1.4 stroke, tinted per use."""

from PyQt6.QtCore import QByteArray, Qt
from PyQt6.QtGui import QIcon, QPainter, QPixmap
from PyQt6.QtSvg import QSvgRenderer

LINE_ICONS = {
    "monitor": '<rect x="1.5" y="2.5" width="13" height="8.5" rx="1.5"/><path d="M5.5 14h5M8 11v3"/>',
    "layout": '<rect x="1.5" y="2.5" width="13" height="11" rx="1.5"/><rect x="8.5" y="8" width="4" height="3.5" rx=".8"/>',
    "crop": '<path d="M4 1.5v9.5a1 1 0 0 0 1 1h9.5M1.5 4H11a1 1 0 0 1 1 1v9.5"/>',
    "mic": '<rect x="5.5" y="1.5" width="5" height="8.5" rx="2.5"/><path d="M3 7.5a5 5 0 0 0 10 0M8 12.5v2"/>',
    "camera": '<rect x="1.5" y="4.5" width="13" height="9" rx="2"/><circle cx="8" cy="9" r="2.5"/><path d="M5.5 4.5l1-2h3l1 2"/>',
    "phone": '<rect x="4" y="1.5" width="8" height="13" rx="2"/><path d="M7 12h2"/>',
    "usb": '<path d="M8 1.5v9M5.5 4.5L8 2l2.5 2.5M4.5 7.5v2.5h7V7.5M8 10.5v4"/>',
    "wifi": '<path d="M2 6.5a9 9 0 0 1 12 0M4.3 9a5.5 5.5 0 0 1 7.4 0M6.6 11.4a2 2 0 0 1 2.8 0"/>',
    "sun": '<circle cx="8" cy="8" r="3"/><path d="M8 1.5v2M8 12.5v2M1.5 8h2M12.5 8h2M3.4 3.4l1.4 1.4M11.2 11.2l1.4 1.4M3.4 12.6l1.4-1.4M11.2 4.8l1.4-1.4"/>',
    "focus": '<path d="M1.5 5V2.5a1 1 0 0 1 1-1H5M11 1.5h2.5a1 1 0 0 1 1 1V5M14.5 11v2.5a1 1 0 0 1-1 1H11M5 14.5H2.5a1 1 0 0 1-1-1V11"/><circle cx="8" cy="8" r="1.2"/>',
    "framing": '<path d="M1.5 5V2.5a1 1 0 0 1 1-1H5M11 1.5h2.5a1 1 0 0 1 1 1V5M14.5 11v2.5a1 1 0 0 1-1 1H11M5 14.5H2.5a1 1 0 0 1-1-1V11"/><circle cx="8" cy="6.3" r="1.9"/><path d="M4.6 12.3c.6-1.7 1.9-2.6 3.4-2.6s2.8.9 3.4 2.6"/>',
    "skeleton": '<circle cx="8" cy="3" r="1.6"/><path d="M8 4.6v5.2M3.5 6.8L8 6l4.5.8M8 9.8l-2.8 4.7M8 9.8l2.8 4.7"/>',
    "image": '<rect x="1.5" y="2.5" width="13" height="11" rx="2"/><path d="M1.5 11l4-3.5 3 2.5 2-1.5 4 3"/>',
    "stabilize": '<rect x="4" y="3" width="8" height="10" rx="1.5"/><path d="M1.5 6v4M14.5 6v4"/>',
    "moon": '<path d="M13 9.5A5.5 5.5 0 0 1 6.5 3a5.5 5.5 0 1 0 6.5 6.5z"/>',
    "lock": '<rect x="3" y="7" width="10" height="7.5" rx="1.5"/><path d="M5.5 7V5a2.5 2.5 0 0 1 5 0v2"/>',
    "torch": '<path d="M5 1.5h6v3L9.5 7v7.5h-3V7L5 4.5z"/>',
    "bolt": '<path d="M9 1.5L3.5 9H8l-1 5.5L12.5 7H8z"/>',
    "heavy": '<rect x="2" y="2" width="12" height="12" rx="2"/><path d="M5 8h6M8 5v6"/>',
    "mirror": '<path d="M8 1.5v13M6 4L2 12h4zM10 4l4 8h-4z"/>',
    "upside_down": '<path d="M1.5 8h13M4 6l8-4v4zM4 10l8 4v-4z"/>',
    "eye": '<path d="M1.5 8s2.5-4.5 6.5-4.5S14.5 8 14.5 8 12 12.5 8 12.5 1.5 8 1.5 8z"/><circle cx="8" cy="8" r="2"/>',
    "eye_off": '<path d="M1.5 8s2.5-4.5 6.5-4.5S14.5 8 14.5 8 12 12.5 8 12.5 1.5 8 1.5 8zM2.5 13.5l11-11"/>',
    "pop_out": '<path d="M9.5 2h4.5v4.5M14 2L8 8M12 9.5V13a1 1 0 0 1-1 1H3a1 1 0 0 1-1-1V5a1 1 0 0 1 1-1h3.5"/>',
    "snapshot": '<rect x="2" y="4.5" width="12" height="8.5" rx="1.5"/><circle cx="8" cy="8.7" r="2.2"/><path d="M5.5 4.5l1-2h3l1 2"/>',
    "bell": '<path d="M4 11V7a4 4 0 0 1 8 0v4l1.5 1.5h-11zM6.5 14h3"/>',
    "battery": '<rect x="1.5" y="5" width="11.5" height="6.5" rx="1.6"/><path d="M14.5 7.2v2.2"/>',
    "battery_charging": '<rect x="1.5" y="5" width="11.5" height="6.5" rx="1.6"/><path d="M14.5 7.2v2.2M7.6 6L6 8.3h2.6L7 10.6"/>',
    "thermometer": '<path d="M6.3 2.8a1.7 1.7 0 0 1 3.4 0v5.9a3.1 3.1 0 1 1-3.4 0z"/><path d="M8 6.2v4.3"/>',
    "chevron": '<path d="M4.5 6.5l3.5 3.5 3.5-3.5"/>',
    "search": '<circle cx="7" cy="7" r="4.5"/><path d="M10.5 10.5L14 14"/>',
    "plus": '<path d="M8 3v10M3 8h10"/>',
    "x": '<path d="M4.5 4.5l7 7M11.5 4.5l-7 7"/>',
    "check": '<path d="M3.5 8.5l3 3 6-7"/>',
    "pencil": '<path d="M10.5 2.5l3 3L6 13H3v-3zM9 4l3 3"/>',
    "trash": '<path d="M2.5 4.5h11M6 4.5v-2h4v2M4 4.5l.7 9a1 1 0 0 0 1 1h4.6a1 1 0 0 0 1-1l.7-9M6.8 7.5v4M9.2 7.5v4"/>',
    "keyboard": '<rect x="1.5" y="3.5" width="13" height="9" rx="1.5"/><path d="M4.5 6.5h.01M7 6.5h.01M9.5 6.5h.01M12 6.5h.01M5 9.5h6"/>',
    "plug": '<path d="M6 1.5v3M10 1.5v3M3.5 4.5h9v2.5a4.5 4.5 0 0 1-9 0zM8 11.5v3"/>',
    "reset": '<path d="M3 8a5 5 0 1 0 1.6-3.7M3 2v3h3"/>',
    "notes": '<rect x="2.5" y="1.5" width="11" height="13" rx="1.5"/><path d="M5 5h6M5 8h6M5 11h3.5"/>',
    "bookmark": '<path d="M4 2h8v12l-4-3-4 3z"/>',
    "colour": '<circle cx="8" cy="8" r="6.5"/><path d="M8 1.5V8l5.6 3.2M8 8l-5.6 3.2"/>',
    "sliders": '<path d="M2 4.5h7M12 4.5h2M2 11.5h2M7 11.5h7"/><circle cx="10.5" cy="4.5" r="1.6"/><circle cx="5.5" cy="11.5" r="1.6"/>',
    "copy": '<rect x="5" y="5" width="9" height="9" rx="1.5"/><path d="M11 5V3a1 1 0 0 0-1-1H3a1 1 0 0 0-1 1v7a1 1 0 0 0 1 1h2"/>',
    "download": '<path d="M8 1.5v9M4.5 7L8 10.5 11.5 7M2.5 13.5h11"/>',
    "chip": '<rect x="3.5" y="3.5" width="9" height="9" rx="1.5"/><path d="M6 1.5v2M10 1.5v2M6 12.5v2M10 12.5v2M1.5 6h2M1.5 10h2M12.5 6h2M12.5 10h2"/>',
    "video": '<rect x="1.5" y="4" width="9.5" height="8" rx="1.5"/><path d="M11 7l3.5-2v6L11 9"/>',
    "qr": '<rect x="2" y="2" width="5" height="5" rx="1"/><rect x="9" y="2" width="5" height="5" rx="1"/><rect x="2" y="9" width="5" height="5" rx="1"/><path d="M9.5 9.5h1.5v1.5M13.5 9.5v4h-4"/>',
    "help": '<circle cx="8" cy="8" r="6.5"/><path d="M6.2 6.2a1.9 1.9 0 0 1 3.6.8c0 1.3-1.8 1.6-1.8 2.7M8 11.6v.1"/>',
    "stop": '<rect x="3" y="3" width="10" height="10" rx="2" fill="currentColor"/>',
}

_RENDER_PX = 64


def line_icon(name: str, color: str) -> QIcon:
    """One of LINE_ICONS in color; an unknown name gives a blank icon."""
    body = LINE_ICONS.get(name, "").replace("currentColor", color)
    svg = (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16" fill="none" stroke="{color}" '
           f'stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round">{body}</svg>')
    pixmap = QPixmap(_RENDER_PX, _RENDER_PX)
    pixmap.fill(Qt.GlobalColor.transparent)
    painter = QPainter(pixmap)
    painter.setRenderHint(QPainter.RenderHint.Antialiasing)
    QSvgRenderer(QByteArray(svg.encode())).render(painter)
    painter.end()
    return QIcon(pixmap)


