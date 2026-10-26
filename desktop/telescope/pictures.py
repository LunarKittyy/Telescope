"""Pictures a plugin puts behind the picture (Background, Chroma key, Fun pack, the combined picture): reading one,
scaling it to cover the frame and laying the person over it by a mask. No Qt."""
from typing import Optional

import cv2
import numpy as np

_MASK_SMALL = 4  # mask edges are feathered at a quarter of the size, then scaled back up


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


def soften(mask: np.ndarray, edges: int) -> np.ndarray:
    """mask with its edge feathered (edges 0..10; 0 leaves it as it was drawn), blurred at a quarter of the size."""
    if edges <= 0:
        return mask
    h, w = mask.shape[:2]
    small = cv2.resize(mask, (max(1, w // _MASK_SMALL), max(1, h // _MASK_SMALL)), interpolation=cv2.INTER_AREA)
    return cv2.resize(cv2.GaussianBlur(small, (0, 0), edges * 0.6), (w, h), interpolation=cv2.INTER_LINEAR)


def compose(top: np.ndarray, mask: np.ndarray, under: np.ndarray) -> np.ndarray:
    """top where mask is 255 over under, all uint8: two multiplies and an add."""
    weight = cv2.merge((mask, mask, mask))
    return cv2.add(cv2.multiply(top, weight, scale=1 / 255), cv2.multiply(under, cv2.bitwise_not(weight), scale=1 / 255))
