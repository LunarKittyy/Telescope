"""The name tag: a lower third with a name, pronouns and an optional second line, drawn with QPainter.

layout() works out where everything goes on a canvas of a given size, render() paints it once into a small premultiplied
tile, and TagCache keeps that tile until the text, the look or the canvas changes, so a frame only costs one blend of
the tile's rectangle (apply()). QPainter on a QImage is fine off the GUI thread, and its text falls back to another font
for scripts the first one lacks, so names in any script come out right.
"""

import unicodedata
from dataclasses import dataclass
from typing import Optional

import cv2
import numpy as np
from PyQt6.QtCore import QPointF, QRectF, Qt
from PyQt6.QtGui import QColor, QFont, QFontMetricsF, QImage, QPainter, QPainterPath

STYLES = ("bar", "pill", "light", "shadow")
POSITIONS = ("bottom_left", "bottom_centre", "bottom_right", "top_left", "top_right")
SIZES = {"small": 0.032, "medium": 0.042, "large": 0.056}  # the name's text height, of the canvas height
DEFAULT_STYLE, DEFAULT_POSITION, DEFAULT_SIZE = "bar", "bottom_left", "medium"
MAX_CHARS = 80

_PRONOUNS = 0.72  # pronouns and the second line, relative to the name
_SECOND = 0.66
_ACCENT = "#aa9cf5"
_INKS = {  # style: (fill, name, pronouns, second line)
    "bar": ("#d90f1216", "#ffffff", "#c9cfd8", "#b4bcc8"),
    "pill": ("#a60f1216", "#ffffff", "#c9cfd8", "#b4bcc8"),
    "light": ("#ebfafafc", "#15181d", "#4a5260", "#58606e"),
    "shadow": ("", "#ffffff", "#e6e9ee", "#dde1e7"),
}


@dataclass(frozen=True)
class Spec:
    """What the tag says and how it looks; frozen, so it can key the cache."""
    name: str = ""
    pronouns: str = ""
    second: str = ""
    style: str = DEFAULT_STYLE
    position: str = DEFAULT_POSITION
    size: str = DEFAULT_SIZE
    families: tuple = ()  # fonts to try in order, before Qt's own fallbacks

    @property
    def empty(self) -> bool:
        return not (self.name.strip() or self.pronouns.strip() or self.second.strip())


@dataclass
class Layout:
    """Where the tag goes on a w x h canvas: the box, and each piece of text with its baseline, inside the box."""
    box: QRectF
    name: tuple  # (text, QFont, QPointF baseline), or None
    pronouns: tuple
    second: tuple
    stripe: Optional[QRectF]  # the accent bar's rectangle (bar style)
    radius: float
    em: float  # the name's pixel size, what the rest is measured in


@dataclass
class Tag:
    """A rendered tag: where its tile goes, and the tile as premultiplied colour and what's left of the frame under it."""
    x: int
    y: int
    ink: np.ndarray  # h x w x 3 uint8, colour times alpha
    keep: np.ndarray  # h x w x 3 uint8, 255 minus alpha


def right_to_left(text: str) -> bool:
    """Whether text starts out right to left (its first strongly directional character is Arabic, Hebrew and the like)."""
    for ch in text:
        kind = unicodedata.bidirectional(ch)
        if kind in ("R", "AL"):
            return True
        if kind == "L":
            return False
    return False


def app_families() -> tuple:
    """The fonts the app's stylesheet asks for (Inter first), as QFont families."""
    from telescope.theme import FONT_STACK
    names = (name.strip().strip("'\"") for name in FONT_STACK.split(","))
    return tuple(n for n in names if n and not n.startswith("-") and n not in ("BlinkMacSystemFont", "sans-serif"))


def _font(families: tuple, px: float, weight: QFont.Weight) -> QFont:
    font = QFont()
    if families:
        font.setFamilies(list(families))
    font.setStyleHint(QFont.StyleHint.SansSerif)
    font.setPixelSize(max(6, round(px)))
    font.setWeight(weight)
    font.setHintingPreference(QFont.HintingPreference.PreferNoHinting)  # smooth at video sizes
    font.setStyleStrategy(QFont.StyleStrategy.PreferAntialias)
    return font


def layout(spec: Spec, w: int, h: int) -> Optional[Layout]:
    """Where everything goes on a w x h canvas, or None when there's nothing to show."""
    if spec.empty or w < 16 or h < 16:
        return None
    em = max(8.0, h * SIZES.get(spec.size, SIZES[DEFAULT_SIZE]))
    pad_x, pad_y, gap = em * 0.7, em * 0.42, em * 0.16
    stripe_w = em * 0.16 if spec.style == "bar" else 0.0
    margin_x, margin_y = max(4.0, w * 0.035), max(4.0, h * (0.075 if spec.position.startswith("bottom") else 0.05))
    room = max(em, w - 2 * margin_x - 2 * pad_x - stripe_w)  # the widest a line may get
    name_font = _font(spec.families, em, QFont.Weight.DemiBold)
    pron_font = _font(spec.families, em * _PRONOUNS, QFont.Weight.Normal)
    second_font = _font(spec.families, em * _SECOND, QFont.Weight.Normal)
    fm_name, fm_pron, fm_second = QFontMetricsF(name_font), QFontMetricsF(pron_font), QFontMetricsF(second_font)

    name, pronouns, second = spec.name.strip(), spec.pronouns.strip(), spec.second.strip()
    pron_gap = em * 0.45 if name and pronouns else 0.0
    pron_w = fm_pron.horizontalAdvance(pronouns) if pronouns else 0.0
    if pron_w > room * 0.5 and name:
        pronouns = fm_pron.elidedText(pronouns, Qt.TextElideMode.ElideRight, room * 0.5)
        pron_w = fm_pron.horizontalAdvance(pronouns)
    if name:
        name = fm_name.elidedText(name, Qt.TextElideMode.ElideRight, room - pron_w - pron_gap)
    if not name and pronouns:
        pronouns = fm_pron.elidedText(pronouns, Qt.TextElideMode.ElideRight, room)
        pron_w = fm_pron.horizontalAdvance(pronouns)
    if second:
        second = fm_second.elidedText(second, Qt.TextElideMode.ElideRight, room)
    name_w = fm_name.horizontalAdvance(name) if name else 0.0
    first_w = name_w + pron_gap + pron_w
    second_w = fm_second.horizontalAdvance(second) if second else 0.0
    first_h = max(fm_name.height() if name else 0.0, fm_pron.height() if pronouns else 0.0)
    inner_w = max(first_w, second_w)
    inner_h = first_h + (gap + fm_second.height() if second else 0.0)
    box_w, box_h = inner_w + 2 * pad_x + stripe_w, inner_h + 2 * pad_y

    if spec.position.endswith("left"):
        x = margin_x
    elif spec.position.endswith("right"):
        x = w - margin_x - box_w
    else:
        x = (w - box_w) / 2
    y = h - margin_y - box_h if spec.position.startswith("bottom") else margin_y
    box = QRectF(round(x), round(y), round(box_w), round(box_h))

    rtl = right_to_left(spec.name or spec.pronouns or spec.second)
    left, right = box.left() + pad_x, box.right() - pad_x
    if rtl:
        right -= stripe_w  # the accent goes where the text starts
    else:
        left += stripe_w
    centred = spec.position == "bottom_centre"

    def start(width: float) -> float:
        if centred:
            return left + (inner_w - width) / 2
        return right - width if rtl else left

    base = box.top() + pad_y + max(fm_name.ascent() if name else 0.0, fm_pron.ascent() if pronouns else 0.0)
    line = start(first_w)
    name_x, pron_x = (line + pron_w + pron_gap, line) if rtl else (line, line + name_w + pron_gap)
    stripe = QRectF(box.right() - stripe_w if rtl else box.left(), box.top(), stripe_w, box.height()) if stripe_w else None
    radius = {"pill": box_h / 2 if not second else em * 0.5, "light": em * 0.28}.get(spec.style, 0.0)
    return Layout(
        box=box,
        name=(name, name_font, QPointF(name_x, base)) if name else None,
        pronouns=(pronouns, pron_font, QPointF(pron_x, base)) if pronouns else None,
        second=(second, second_font, QPointF(start(second_w), base + max(fm_name.descent(), fm_pron.descent())
                                             + gap + fm_second.ascent())) if second else None,
        stripe=stripe, radius=radius, em=em)


def _color(hex_argb: str) -> QColor:
    return QColor(hex_argb) if hex_argb else QColor(0, 0, 0, 0)


def render(spec: Spec, w: int, h: int) -> Optional[Tag]:
    """The tag for a w x h canvas, painted once; None when there's nothing to show."""
    lay = layout(spec, w, h)
    if lay is None:
        return None
    blur = lay.em * 0.14 if spec.style == "shadow" else 0.0
    spread = int(blur * 3 + 2) if blur else 0  # room for the shadow around the box
    x0, y0 = max(0, int(lay.box.left()) - spread), max(0, int(lay.box.top()) - spread)
    x1, y1 = min(w, int(lay.box.right()) + spread + 1), min(h, int(lay.box.bottom()) + spread + 1)
    if x1 <= x0 or y1 <= y0:
        return None
    image = QImage(x1 - x0, y1 - y0, QImage.Format.Format_ARGB32_Premultiplied)
    image.fill(0)
    p = QPainter(image)
    p.setRenderHint(QPainter.RenderHint.Antialiasing)
    p.setRenderHint(QPainter.RenderHint.TextAntialiasing)
    p.translate(-x0, -y0)
    fill, ink_name, ink_pron, ink_second = _INKS.get(spec.style, _INKS[DEFAULT_STYLE])
    if fill:
        path = QPainterPath()
        path.addRoundedRect(lay.box, lay.radius, lay.radius)
        p.fillPath(path, _color(fill))
    if lay.stripe is not None:
        p.fillRect(lay.stripe, QColor(_ACCENT))
    for piece, ink in ((lay.name, ink_name), (lay.pronouns, ink_pron), (lay.second, ink_second)):
        if piece is not None:
            text, font, at = piece
            p.setFont(font)
            p.setPen(QColor(ink))
            p.drawText(at, text)
    p.end()
    bgra = _array(image)
    ink, alpha = bgra[..., :3], bgra[..., 3]
    if blur:
        shadow = cv2.GaussianBlur(alpha, (0, 0), blur)
        shift = max(1, round(lay.em * 0.05))
        shadow = np.roll(shadow, shift, axis=0)
        shadow[:shift] = 0
        shadow = (shadow.astype(np.uint16) * 200 // 255).astype(np.uint8)  # a soft dark halo under the text
        alpha = cv2.add(alpha, cv2.multiply(shadow, cv2.bitwise_not(alpha), scale=1 / 255))
    keep = cv2.bitwise_not(alpha)
    return Tag(x0, y0, np.ascontiguousarray(ink), cv2.merge((keep, keep, keep)))


def _array(image: QImage) -> np.ndarray:
    """A premultiplied ARGB32 QImage as an h x w x 4 BGRA array (its byte order on little-endian machines)."""
    w, h = image.width(), image.height()
    ptr = image.constBits()
    ptr.setsize(image.sizeInBytes())
    rows = np.frombuffer(ptr, np.uint8).reshape(h, image.bytesPerLine())
    return rows[:, :w * 4].reshape(h, w, 4).copy()


def apply(frame: np.ndarray, tag: Tag) -> np.ndarray:
    """The tag blended onto frame, in place where frame allows it: one multiply and an add over the tile."""
    if not frame.flags.writeable:
        frame = frame.copy()
    th, tw = tag.ink.shape[:2]
    roi = frame[tag.y:tag.y + th, tag.x:tag.x + tw]
    if roi.shape[:2] != (th, tw):
        return frame
    roi[:] = cv2.add(cv2.multiply(roi, tag.keep, scale=1 / 255), tag.ink)
    return frame


class TagCache:
    """The last rendered tag, kept until the spec or the canvas size changes."""

    def __init__(self):
        self._entry: tuple = (None, None)  # (key, tag), swapped whole so a stream behind the panels can share it
        self.renders = 0  # how many times it painted, for tests

    def get(self, spec: Spec, w: int, h: int) -> Optional[Tag]:
        key = (spec, w, h)
        entry = self._entry
        if entry[0] != key:
            entry = self._entry = (key, render(spec, w, h))
            self.renders += 1
        return entry[1]
