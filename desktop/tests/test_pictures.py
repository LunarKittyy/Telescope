import cv2
import numpy as np
import pytest

from telescope.pictures import shrink


def _scene(w=1920, h=1080):
    yy, xx = np.mgrid[0:h, 0:w].astype(np.float32)
    frame = np.dstack([xx / w * 255, yy / h * 255, (xx + yy) / (w + h) * 255]).astype(np.uint8)
    cv2.circle(frame, (w // 2, h // 2), h // 4, (40, 200, 90), -1)
    return frame


@pytest.mark.parametrize("size", [(120, 67), (240, 135), (256, 144), (128, 72), (1500, 900), (1920, 1080)])
def test_shrink_gives_the_size_asked_for_and_looks_like_one_area_step(size):
    frame = _scene()
    out = shrink(frame, *size)
    assert out.shape == (size[1], size[0], 3) and out.dtype == np.uint8
    one_step = cv2.resize(frame, size, interpolation=cv2.INTER_AREA)
    assert np.abs(out.astype(int) - one_step).mean() < 1.5


def test_shrink_by_powers_of_two_matches_one_area_step_to_a_level():
    frame = _scene()
    assert np.abs(shrink(frame, 240, 135).astype(int) - cv2.resize(frame, (240, 135), interpolation=cv2.INTER_AREA)).max() <= 1


def test_shrink_takes_one_channel_and_odd_sizes():
    mask = np.zeros((1081, 1923), np.uint8)
    mask[300:800, 600:1300] = 255
    assert shrink(mask, 120, 67).shape == (67, 120)
