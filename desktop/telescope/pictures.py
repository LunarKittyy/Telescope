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


def cover(image: np.ndarray, w: int, h: int) -> np.ndarray:
    """image scaled to fill w x h, cut evenly at the edges that don't fit."""
    ih, iw = image.shape[:2]
    scale = max(w / iw, h / ih)
    sw, sh = max(w, round(iw * scale)), max(h, round(ih * scale))
    scaled = cv2.resize(image, (sw, sh), interpolation=cv2.INTER_AREA if scale < 1 else cv2.INTER_LINEAR)
    x0, y0 = (sw - w) // 2, (sh - h) // 2
    return np.ascontiguousarray(scaled[y0:y0 + h, x0:x0 + w])
