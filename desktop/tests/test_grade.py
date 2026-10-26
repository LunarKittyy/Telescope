"""Each control of the Colour grading card against the formula it's meant to be, written out again here plainly."""
import numpy as np
import pytest

from telescope import grade as g
from telescope.colour import CubeLut, lattice
from telescope.grade import Grade


def _colours():
    rng = np.random.default_rng(7)
    return np.concatenate([lattice(5), rng.random((200, 3), dtype=np.float32)]).astype(np.float32)


def _luma(x):
    return 0.2126 * x[:, 0] + 0.7152 * x[:, 1] + 0.0722 * x[:, 2]


def _close(a, b, tol=1e-5):
    assert np.abs(np.asarray(a, np.float64) - np.asarray(b, np.float64)).max() <= tol


def test_the_default_grade_changes_nothing():
    x = _colours()
    assert Grade().is_identity() and Grade().per_channel()
    _close(Grade()(x), x, 0)


def test_temperature_and_tint_are_gains_in_linear_light():
    x = _colours()
    t, n = 40, -25
    gr, gg, gb = 2 ** (0.25 * t / 100), 2 ** (-0.2 * n / 100), 2 ** (-0.25 * t / 100)
    norm = 0.2126 * gr + 0.7152 * gg + 0.0722 * gb

    def to_lin(v):
        return np.where(v <= 0.04045, v / 12.92, ((v + 0.055) / 1.055) ** 2.4)

    def to_srgb(v):
        return np.where(v <= 0.0031308, v * 12.92, 1.055 * np.maximum(v, 0) ** (1 / 2.4) - 0.055)
    expected = np.clip(to_srgb(to_lin(x) * np.array([gr, gg, gb]) / norm), 0, 1)
    _close(Grade(temperature=t, tint=n)(x), expected)
    white = Grade(temperature=t, tint=n)(np.ones((1, 3), np.float32))
    assert white[0, 0] > white[0, 2]  # warmer: more red than blue
    assert Grade(temperature=t).per_channel()


@pytest.mark.parametrize("lift, gamma, gain, offset", [(0.1, 0, 1, 0), (0, 0.5, 1, 0), (0, 0, 1.4, 0), (0, 0, 1, -0.1),
                                                       (-0.05, -0.3, 0.8, 0.05)])
def test_lift_gamma_gain_and_offset_masters(lift, gamma, gain, offset):
    x = _colours()
    y = x + offset
    y = gain * y + lift * (1 - y)
    y = np.maximum(y, 0) ** (2.0 ** -gamma)
    expected = np.clip(y, 0, 1)
    grade = Grade(masters={"lift": lift, "gamma": gamma, "gain": gain, "offset": offset})
    _close(grade(x), expected)
    assert grade.per_channel()


def test_lift_pins_white_gain_pins_black_and_gamma_pins_both():
    ends = np.array([[0, 0, 0], [1, 1, 1]], np.float32)
    lifted = Grade(masters={**g.MASTER_DEFAULT, "lift": 0.2})(ends)
    _close(lifted, [[0.2] * 3, [1] * 3])
    gained = Grade(masters={**g.MASTER_DEFAULT, "gain": 0.5})(ends)
    _close(gained, [[0] * 3, [0.5] * 3])
    gamma = Grade(masters={**g.MASTER_DEFAULT, "gamma": 0.7})(ends)
    _close(gamma, ends)


def test_a_wheel_pushes_towards_the_colour_it_points_at_without_changing_brightness():
    up = g.wheel_shift(0, 1)  # towards red on a vectorscope
    assert up[0] > 0 > up[1] and abs(up[2]) < 1e-6
    right = g.wheel_shift(1, 0)  # towards blue
    assert right[2] > 0 > right[1]
    for x, y in ((0.3, -0.8), (1, 0), (-0.6, 0.6)):
        assert abs(float(g.LUMA @ g.wheel_shift(x, y))) < 1e-4  # the Rec. 709 matrix to four places
    grey = np.full((1, 3), 0.5, np.float32)
    lifted = Grade(wheels={**Grade().wheels, "gain": (0.0, 1.0)})(grey)
    assert lifted[0, 0] > 0.5 > lifted[0, 1]


def test_wheels_follow_the_same_formula_per_channel():
    x = _colours()
    wheels = {"lift": (0.2, 0.1), "gamma": (-0.3, 0.4), "gain": (0.1, -0.5), "offset": (0.5, 0.0)}
    shift = {w: g.wheel_shift(*wheels[w]) * reach for w, reach in
             (("lift", 0.2), ("gamma", 0.5), ("gain", 0.4), ("offset", 0.2))}
    y = x + shift["offset"]
    y = (1 + shift["gain"]) * y + shift["lift"] * (1 - y)
    y = np.maximum(y, 0) ** (2.0 ** -shift["gamma"])
    _close(Grade(wheels=wheels)(x), np.clip(y, 0, 1))


def test_contrast_turns_around_the_pivot():
    x = _colours()
    grade = Grade(contrast=1.5, pivot=0.4)
    _close(grade(x), np.clip((x - 0.4) * 1.5 + 0.4, 0, 1))
    _close(grade(np.full((1, 3), 0.4, np.float32)), [[0.4] * 3])
    assert Grade(contrast=0.5).per_channel()


def test_saturation_mixes_with_rec709_luma():
    x = _colours()
    _close(Grade(saturation=1.6)(x), np.clip(_luma(x)[:, None] + 1.6 * (x - _luma(x)[:, None]), 0, 1))
    grey = Grade(saturation=0)(x)
    _close(grey[:, 0], grey[:, 1])
    assert not Grade(saturation=0.5).per_channel()


def test_shadows_and_highlights_move_the_tones_they_name():
    x = _colours()
    y = _luma(x)
    sh, hi = 60, -40
    expected = x + (sh / 100 * y * (1 - y) ** 2 + hi / 100 * y * y * (1 - y))[:, None]
    _close(Grade(shadows=sh, highlights=hi)(x), np.clip(expected, 0, 1))
    ramp = np.repeat(np.linspace(0, 1, 101, dtype=np.float32)[:, None], 3, 1)
    for s, h in ((100, 100), (-100, -100), (100, -100), (-100, 100)):
        assert (np.diff(Grade(shadows=s, highlights=h)(ramp)[:, 0]) >= -1e-6).all()  # never turns tones around
    dark, bright = Grade(shadows=50)(np.array([[0.2] * 3, [0.9] * 3], np.float32))
    assert dark[0] > 0.2 + 0.05 and bright[0] < 0.9 + 0.02


def test_curves_go_through_their_points_without_overshooting():
    pts = ((0.0, 0.0), (0.25, 0.4), (0.5, 0.5), (0.75, 0.95), (1.0, 1.0))
    xs = np.linspace(0, 1, 401, dtype=np.float32)
    ys = g.monotone_curve(pts, xs)
    for px, py in pts:
        assert abs(float(g.monotone_curve(pts, np.array([px], np.float32))[0]) - py) < 1e-6
    assert (np.diff(ys) >= -1e-7).all() and ys.min() >= 0 and ys.max() <= 1
    _close(g.monotone_curve(g.LINEAR, xs), xs, 1e-6)


def test_the_master_curve_comes_before_each_channels_own():
    x = _colours()
    master = ((0.0, 0.0), (0.5, 0.7), (1.0, 1.0))
    red = ((0.0, 0.1), (1.0, 0.9))
    curves = {**Grade().curves, "all": master, "r": red}
    expected = g.monotone_curve(master, x)
    expected[:, 0] = g.monotone_curve(red, expected[:, 0])
    _close(Grade(curves=curves)(x), expected)
    assert Grade(curves=curves).per_channel()


def test_hue_curves_go_round():
    xs = np.array([0.0, 0.05, 0.95, 1.0], np.float32)
    pts = ((0.0, 0.8), (0.5, 0.5))
    ys = g.periodic_curve(pts, xs)
    assert abs(ys[0] - 0.8) < 1e-6 and abs(ys[3] - 0.8) < 1e-6 and abs(ys[1] - ys[2]) < 1e-5
    _close(g.periodic_curve(g.NEUTRAL_HUE, xs), np.full(4, 0.5))


@pytest.mark.parametrize("pts", [
    g.NEUTRAL_HUE[:1] + ((1.0, 0.7),) + g.NEUTRAL_HUE[1:],  # x at 1 is the hue at 0 again
    g.NEUTRAL_HUE + ((1 / 6, 0.7),),  # one hue given twice
])
def test_a_hue_curve_given_one_hue_twice_has_no_hard_edge(pts):
    xs = np.linspace(0, 1, 2001, dtype=np.float32)
    ys = g.periodic_curve(pts, xs)
    assert np.isfinite(ys).all() and np.abs(np.diff(ys)).max() < 0.01


def test_hue_vs_sat_takes_the_colour_out_of_one_hue_only():
    pts = tuple((h, 0.0 if abs(h - 1 / 3) < 1e-6 else 0.5) for h, _ in g.NEUTRAL_HUE)
    out = Grade(hue_sat=pts)(np.array([[0.2, 0.8, 0.2], [0.8, 0.2, 0.2]], np.float32))
    green, red = out
    _close(green, [_luma(np.array([[0.2, 0.8, 0.2]]))[0]] * 3)
    _close(red, [0.8, 0.2, 0.2])
    assert not Grade(hue_sat=pts).per_channel()


def test_hue_vs_hue_turns_one_hue_towards_the_next():
    _close(g.rotate_hue(np.array([[1, 0, 0]], np.float32), np.array([1 / 3], np.float32)), [[0, 1, 0]])
    pts = tuple((h, 0.5 + (1 / 12 if h == 0 else 0)) for h, _ in g.NEUTRAL_HUE)  # red, a twelfth of a turn on
    red = Grade(hue_hue=pts)(np.array([[0.9, 0.1, 0.1]], np.float32))
    assert 0.2 < g.hue_of(red)[0] * 12 < 1.8  # towards orange
    cyan = Grade(hue_hue=pts)(np.array([[0.1, 0.9, 0.9]], np.float32))
    _close(cyan, [[0.1, 0.9, 0.9]])


def test_a_lut_file_goes_last_and_strength_mixes_it_in():
    x = _colours()
    invert = CubeLut(cube=(1 - lattice(5)).reshape(5, 5, 5, 3))
    _close(Grade(lut=invert)(x), 1 - x)
    _close(Grade(lut=invert, lut_strength=0.25)(x), x + 0.25 * ((1 - x) - x))
    _close(Grade(contrast=2.0, lut=invert)(x), 1 - np.clip((x - g.PIVOT) * 2 + g.PIVOT, 0, 1))
    assert Grade(lut=invert, lut_strength=0).is_identity()
    assert not Grade(lut=invert).per_channel() and Grade(lut=CubeLut(shaper=lattice(2)[:2])).per_channel()


def test_a_detailed_lut_asks_for_a_bigger_bake():
    assert Grade().lut_size() == 33
    assert Grade(lut=CubeLut(cube=lattice(65).reshape(65, 65, 65, 3))).lut_size() == 65


def test_everything_together_is_each_step_in_turn():
    x = _colours()
    full = Grade(temperature=20, masters={**g.MASTER_DEFAULT, "gain": 1.2}, contrast=1.3, saturation=1.2)
    step = Grade(temperature=20)(x)
    step = Grade(masters={**g.MASTER_DEFAULT, "gain": 1.2})(step)
    step = Grade(contrast=1.3)(step)
    step = Grade(saturation=1.2)(step)
    _close(full(x), step)
