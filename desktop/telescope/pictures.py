"""Pictures a plugin puts behind the picture (Background, Chroma key, the combined picture): reading one and
scaling it to cover the frame. No Qt."""
from typing import Optional

import cv2
import numpy as np


def read_picture(path: str) -> Optional[np.ndarray]:
    """A picture file as BGR, or None; read as bytes since cv2.imread fails on non-ASCII paths on Windows."""
    try:
        data = np.fromfile(path, np.uint8)
    except (OSError, ValueError):
        return None
    image = cv2.imdecode(data, cv2.IMREAD_COLOR) if data.size else None
    return image if image is not None and image.size else None


def shrink(image: np.ndarray, w: int, h: int) -> np.ndarray:
    """image shrunk to w x h with INTER_AREA, in exact halves first: one INTER_AREA step is slow at big or uneven ratios
    (1080p to a sixteenth took 3.4 ms, in halves 0.35) and halves average the same pixels."""
    while image.shape[1] >= 2 * w and image.shape[0] >= 2 * h:
        ih, iw = image.shape[:2]
        image = cv2.resize(image[:ih - ih % 2, :iw - iw % 2], (iw // 2, ih // 2), interpolation=cv2.INTER_AREA)
    if image.shape[:2] == (h, w):
        return image
    return cv2.resize(image, (w, h), interpolation=cv2.INTER_AREA)


def cover(image: np.ndarray, w: int, h: int) -> np.ndarray:
    """image scaled to fill w x h, cut evenly at the edges that don't fit."""
    ih, iw = image.shape[:2]
    scale = max(w / iw, h / ih)
    sw, sh = max(w, round(iw * scale)), max(h, round(ih * scale))
    scaled = cv2.resize(image, (sw, sh), interpolation=cv2.INTER_AREA if scale < 1 else cv2.INTER_LINEAR)
    x0, y0 = (sw - w) // 2, (sh - h) // 2
    return np.ascontiguousarray(scaled[y0:y0 + h, x0:x0 + w])
