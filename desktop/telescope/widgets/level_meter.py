"""A DAW-style level meter: peak and RMS bars, a held peak marker, and a clip light that latches for a while."""

import math
import time
from typing import Callable, Optional

from PyQt6.QtCore import QRectF, Qt
from PyQt6.QtGui import QColor, QPainter, QPainterPath
from PyQt6.QtWidgets import QSizePolicy, QWidget

from telescope import theme
from telescope.widgets.common import ROW_HEIGHT, ui_px

FLOOR_DB = -60.0
WARN_DB = -18.0  # green below, yellow from here
HOT_DB = -6.0    # red from here
TICKS_DB = (-40, -30, -20, -12, -6)
KNEE_DB = -20.0  # the scale stretches above this, like a DAW's, so the loud end isn't a sliver
KNEE_AT = 0.5
FALL_DB_PER_S = 24.0  # how fast the bars sink once the sound stops
HOLD_S = 1.5          # the peak marker stays put this long, then sinks
CLIP_HOLD_S = 3.0
CLIP_AT = 0.998       # within a couple of steps of full scale counts as clipped

BAR_H = 8
CLIP_DOT = 8
CLIP_GAP = 6
TRACK_INSET = 10  # px, where a slider's groove starts, so a meter under a slider lines up with it


def to_db(level: float) -> float:
    return max(FLOOR_DB, 20 * math.log10(level)) if level > 0 else FLOOR_DB


class LevelMeter(QWidget):
    """Feed it set_level() about 30 times a second. Clicking it clears the held peak and the clip light."""

    def __init__(self, clock: Callable = time.monotonic, parent=None):
        super().__init__(parent)
        self._clock = clock
        self._peak_db = self._rms_db = self._hold_db = FLOOR_DB
        self._hold_until = 0.0
        self._clip_until = 0.0
        self._limit_until = 0.0
        self._last: Optional[float] = None
        self._muted = False
        self.setMinimumWidth(ui_px(84))
        self.setFixedHeight(ROW_HEIGHT)
        self.setSizePolicy(QSizePolicy.Policy.Expanding, QSizePolicy.Policy.Fixed)
        self.setCursor(Qt.CursorShape.PointingHandCursor)
        self.setToolTip("The phone mic's level. The line holds the latest peak. The dot turns yellow while the "
                        "limiter is holding loud peaks down, and red when the sound clips (too loud to record "
                        "cleanly). Click to clear them.")

    # ── State ─────────────────────────────────────────────────────────────────

    def set_level(self, peak: float, rms: float, limited: bool = False):
        now = self._clock()
        dt = 0.0 if self._last is None else min(now - self._last, 0.5)
        self._last = now
        fall = FALL_DB_PER_S * dt
        self._peak_db = max(to_db(peak), self._peak_db - fall)
        self._rms_db = max(to_db(rms), self._rms_db - fall)
        if self._peak_db >= self._hold_db:
            self._hold_db, self._hold_until = self._peak_db, now + HOLD_S
        elif now >= self._hold_until:
            self._hold_db = max(self._peak_db, self._hold_db - fall)
        if peak >= CLIP_AT:
            self._clip_until = now + CLIP_HOLD_S
        if limited:
            self._limit_until = now + CLIP_HOLD_S
        self.update()

    def clear(self):
        """Back to silence, as when the audio stops."""
        self._peak_db = self._rms_db = self._hold_db = FLOOR_DB
        self._hold_until = self._clip_until = self._limit_until = 0.0
        self._last = None
        self.update()

    def set_muted(self, muted: bool):
        self._muted = muted
        self.update()

    def held_db(self) -> float:
        return self._hold_db

    def clipping(self) -> bool:
        return self._clock() < self._clip_until

    def limiting(self) -> bool:
        return self._clock() < self._limit_until

    def mousePressEvent(self, event):
        if event.button() == Qt.MouseButton.LeftButton:
            self._hold_db, self._hold_until, self._clip_until, self._limit_until = self._peak_db, 0.0, 0.0, 0.0
            self.update()
        super().mousePressEvent(event)

    # ── Painting ──────────────────────────────────────────────────────────────

    def _x(self, track: QRectF, db: float) -> float:
        if db <= KNEE_DB:
            frac = KNEE_AT * (db - FLOOR_DB) / (KNEE_DB - FLOOR_DB)
        else:
            frac = KNEE_AT + (1 - KNEE_AT) * (db - KNEE_DB) / -KNEE_DB
        return track.left() + track.width() * frac

    def _zone_color(self, db: float) -> QColor:
        if self._muted:
            return QColor(theme.TEXT_FAINT)
        return QColor(theme.ERR if db >= HOT_DB else theme.WARN if db >= WARN_DB else theme.OK)

    def _fill(self, p: QPainter, track: QRectF, db: float, alpha: float):
        # Each zone keeps its own colour along the bar, the way hardware meters light up
        if db <= FLOOR_DB:
            return
        edges = [FLOOR_DB, WARN_DB, HOT_DB, 0.0]
        for lo, hi in zip(edges, edges[1:]):
            if db <= lo:
                break
            color = self._zone_color(lo)
            color.setAlphaF(alpha)
            left, right = round(self._x(track, lo)), round(self._x(track, min(db, hi)))  # whole pixels, so zones meet without a seam
            p.fillRect(QRectF(left, track.top(), right - left, track.height()), color)

    def paintEvent(self, event):
        p = QPainter(self)
        p.setRenderHint(QPainter.RenderHint.Antialiasing)
        dot = ui_px(CLIP_DOT)
        bar_h = ui_px(BAR_H)
        top = (self.height() - bar_h) / 2 - 2  # a little above centre, so the ticks below fit
        track = QRectF(TRACK_INSET + 0.5, top, self.width() - TRACK_INSET - dot - ui_px(CLIP_GAP) - 1, bar_h)

        shape = QPainterPath()
        shape.addRoundedRect(track, bar_h / 2, bar_h / 2)
        p.fillPath(shape, QColor(theme.SURFACE_SUNK))
        p.save()
        p.setClipPath(shape)
        self._fill(p, track, self._peak_db, 0.55)
        self._fill(p, track, self._rms_db, 1.0)
        if self._hold_db > FLOOR_DB:
            x = self._x(track, self._hold_db)
            x = min(max(x, track.left() + 3), track.right() - bar_h / 2)  # clear of the rounded ends
            p.fillRect(QRectF(x - 1, track.top(), 2, track.height()),
                       QColor(theme.TEXT) if not self._muted else QColor(theme.TEXT_DIM))
        p.restore()
        p.setPen(QColor(theme.BORDER_STRONG))
        p.setBrush(Qt.BrushStyle.NoBrush)
        p.drawPath(shape)

        tick_top = track.bottom() + 3
        for db in TICKS_DB:
            x = round(self._x(track, db)) + 0.5
            p.drawLine(QRectF(x, tick_top, 0, 3).topLeft(), QRectF(x, tick_top, 0, 3).bottomLeft())

        lit = None if self._muted else theme.ERR if self.clipping() else theme.WARN if self.limiting() else None
        clipped = lit is not None
        p.setPen(Qt.PenStyle.NoPen)
        p.setBrush(QColor(lit or theme.SURFACE_SUNK))
        dot_rect = QRectF(self.width() - dot - 0.5, track.center().y() - dot / 2, dot, dot)
        p.drawEllipse(dot_rect)
        if not clipped:
            p.setPen(QColor(theme.BORDER_STRONG))
            p.setBrush(Qt.BrushStyle.NoBrush)
            p.drawEllipse(dot_rect)
        p.end()
