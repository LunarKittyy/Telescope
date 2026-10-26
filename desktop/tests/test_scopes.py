"""The scope maths: histogram, waveform, RGB parade and vectorscope counts, and the pictures made of them."""

import numpy as np
import pytest

from telescope import scopes


def _frame(bgr, w=64, h=36):
    return np.full((h, w, 3), bgr, np.uint8)


def test_shrink_keeps_the_shape_and_never_grows():
    big = np.zeros((1080, 1920, 3), np.uint8)
    assert scopes.shrink(big).shape == (144, 256, 3)
    small = np.zeros((36, 64, 3), np.uint8)
    assert scopes.shrink(small) is small


def test_histogram_counts_every_pixel_at_its_level():
    frame = _frame((10, 20, 30))
    counts = scopes.histogram(frame)
    assert counts.shape == (4, 256)
    assert counts[0, 10] == counts[1, 20] == counts[2, 30] == frame.shape[0] * frame.shape[1]
    assert counts.sum(axis=1).tolist() == [frame.shape[0] * frame.shape[1]] * 4
    assert counts[3, round(0.0722 * 10 + 0.7152 * 20 + 0.2126 * 30)] == frame.shape[0] * frame.shape[1]


def test_waveform_puts_bright_columns_at_the_top():
    channel = np.zeros((36, 64), np.uint8)
    channel[:, 32:] = 255
    counts = scopes.waveform(channel)
    assert counts.shape == (256, 64)
    assert (counts[255, :32] == 36).all() and (counts[0, 32:] == 36).all()  # level 0 at the bottom, 255 at the top
    assert counts.sum() == channel.size


def test_parade_has_red_green_and_blue_in_that_order():
    counts = scopes.parade(_frame((0, 0, 255), w=10))  # pure red
    red, green, blue = counts[:, :10], counts[:, 10:20], counts[:, 20:]
    assert (red[0] == 36).all() and (green[255] == 36).all() and (blue[255] == 36).all()


def test_grey_sits_in_the_middle_of_the_vectorscope_and_red_up_and_left_of_it():
    grey = scopes.vectorscope(_frame((128, 128, 128)))
    assert grey[128, 128] == 36 * 64
    red = scopes.vectorscope(_frame((0, 0, 255)))
    y, x = np.unravel_index(red.argmax(), red.shape)
    assert (x, y) == scopes.scope_point((0, 0, 255))
    assert x < 128 and y < 128  # little blue, a lot of red


@pytest.mark.parametrize("name", list(scopes.SCOPES))
def test_every_scope_makes_a_bgra_picture(name):
    frame = (np.random.default_rng(1).random((540, 960, 3)) * 255).astype(np.uint8)
    image = scopes.scope_image(name, frame)
    assert image.dtype == np.uint8 and image.ndim == 3 and image.shape[2] == 4
    assert image[..., 3].min() > 0  # the backdrop shows everywhere, so the scope reads over any picture


def test_a_brighter_spot_glows_more():
    glow = scopes.glow(np.array([0, 1, 10, 1000]), per_cell=10)
    assert glow[0] == 0 and glow[1] < glow[2] < glow[3] <= 1
