import logging
import os
from pathlib import Path

import numpy as np
import pytest

from telescope import colour, vision
from telescope.colour import ColourPipeline, ColourStage, CubeError, Lut, bake, lattice, parse_cube, read_cube, write_cube


def _frame(h=72, w=128, seed=1):
    return np.random.default_rng(seed).integers(0, 256, (h, w, 3), dtype=np.uint8)


def _graded(frame, fn):
    """fn applied to every pixel of a BGR frame exactly, as uint8: what a LUT can only come close to."""
    rgb = frame[..., ::-1].reshape(-1, 3).astype(np.float32) / 255
    out = np.clip(fn(rgb), 0, 1) * 255 + 0.5
    return out.astype(np.uint8).reshape(frame.shape)[..., ::-1]


def _trilinear(cube, frame):
    """A cube on a frame the slow way (numpy, per pixel), for checking what OpenCV does."""
    rgb = frame[..., ::-1].reshape(-1, 3).astype(np.float32) / 255
    return _graded(frame, lambda _x: colour.sample_cube(cube, rgb))


def _warm(rgb):
    return np.stack([rgb[:, 0] ** 0.8, rgb[:, 1] * 0.9 + rgb[:, 2] * 0.1, np.sqrt(rgb[:, 2])], 1)


# ── Baking and applying ──────────────────────────────────────────────────


@pytest.mark.parametrize("size", colour.SIZES)
def test_an_identity_lut_leaves_frames_alone(size):
    frame = _frame(h=400, seed=size)
    lut = bake([ColourStage(lambda x: x, size=size)])
    assert lut.kind == f"3D {size}"
    out = lut.apply(frame)
    assert out.shape == frame.shape and out.dtype == np.uint8 and out is not frame
    assert np.array_equal(out, frame)  # the lattice isn't rounded to whole levels before it's interpolated


def test_per_channel_stages_become_exact_tables():
    frame = _frame()
    lut = bake([ColourStage(lambda x: 1 - x, per_channel=True), ColourStage(lambda x: x ** 2, per_channel=True)])
    assert lut.kind == "1D"
    assert np.array_equal(lut.apply(frame), _graded(frame, lambda x: (1 - x) ** 2))


@pytest.mark.parametrize("size", colour.SIZES)
def test_opencv_applies_a_3d_lut_like_trilinear_interpolation_does(size):
    frame = _frame(seed=size)
    cube = _warm(lattice(size)).reshape(size, size, size, 3)
    out = Lut(cube=cube).apply(frame)
    assert np.abs(out.astype(int) - _trilinear(cube, frame)).max() <= 1


def test_a_tall_frame_cut_into_bands_comes_out_the_same(monkeypatch):
    monkeypatch.setattr(colour, "_BANDS", 3)
    frame = _frame(h=401, w=64)
    lut = Lut(cube=_warm(lattice(33)).reshape(33, 33, 33, 3))
    assert np.array_equal(lut.apply(frame), lut._apply_3d(frame))


def test_a_lattice_is_red_fastest_and_cubes_are_blue_major():
    pts = lattice(3)
    assert pts[1].tolist() == [0.5, 0, 0] and pts[3].tolist() == [0, 0.5, 0] and pts[9].tolist() == [0, 0, 0.5]
    cube = np.zeros((3, 3, 3, 3), np.float32)
    cube[2, 0, 0] = (0.0, 0.0, 1.0)  # [b][g][r]: full blue in stays blue
    frame = np.array([[[255, 0, 0]]], np.uint8)  # BGR blue
    assert Lut(cube=cube).apply(frame)[0, 0].tolist() == [255, 0, 0]


def test_the_bake_size_is_what_the_stages_need():
    assert bake([ColourStage(lambda x: x)]).kind == "3D 33"
    assert bake([ColourStage(lambda x: x), ColourStage(lambda x: x, size=65)]).kind == "3D 65"
    assert bake([ColourStage(lambda x: x, per_channel=True)], size=65).kind == "3D 65"
    assert bake([]) is None


def test_stages_run_in_order_and_values_are_clipped_between_them():
    double = ColourStage(lambda x: x * 2)
    half = ColourStage(lambda x: x * 0.5)
    frame = np.full((1, 1, 3), 200, np.uint8)
    assert abs(int(bake([double, half]).apply(frame)[0, 0, 0]) - 128) <= 1  # 200 doubled clips to 255, then halves
    assert abs(int(bake([half, double]).apply(frame)[0, 0, 0]) - 200) <= 1


def test_a_failing_stage_is_left_out(caplog):
    def broken(_x):
        raise RuntimeError("no")
    with caplog.at_level(logging.ERROR):
        lut = bake([ColourStage(broken), ColourStage(lambda x: 1 - x)])
    assert "colour stage failed" in caplog.text
    assert lut.apply(np.zeros((1, 1, 3), np.uint8))[0, 0].tolist() == [255, 255, 255]


def test_a_stage_that_returns_the_wrong_shape_is_left_out(caplog):
    with caplog.at_level(logging.ERROR):
        lut = bake([ColourStage(lambda x: x[:, :2])])
    assert np.abs(lut.apply(_frame()).astype(int) - _frame()).max() <= 1


# ── The pipeline ─────────────────────────────────────────────────────────


def _pipeline(source="phone"):
    state = {"source": source}
    return ColourPipeline(lambda: state["source"], bake_inline=True), state


def test_without_stages_frames_go_through_untouched():
    pipeline, _ = _pipeline()
    frame = _frame()
    assert pipeline.process_frame(frame) is frame
    assert pipeline.frame_step()(frame) is frame


def test_stages_sort_by_order_then_owner_and_an_owner_replaces_its_own():
    pipeline, _ = _pipeline()
    a, b, c = (ColourStage(lambda x: x) for _ in range(3))
    pipeline.set_stage("zeta", a, order=0)
    pipeline.set_stage("alpha", b, order=0)
    pipeline.set_stage("match", c, order=-100)
    assert pipeline.stages("phone") == (c, b, a)
    d = ColourStage(lambda x: x)
    pipeline.set_stage("zeta", d, order=-200)
    assert pipeline.stages("phone") == (d, c, b)
    pipeline.set_stage("zeta", None)
    assert pipeline.stages("phone") == (c, b)


def test_the_composed_lut_follows_the_order():
    pipeline, _ = _pipeline()
    pipeline.set_stage("gamma", ColourStage(lambda x: x ** 2, per_channel=True), order=0)
    pipeline.set_stage("invert", ColourStage(lambda x: 1 - x, per_channel=True), order=-1)
    frame = _frame()
    assert np.array_equal(pipeline.process_frame(frame), _graded(frame, lambda x: (1 - x) ** 2))


def test_a_stream_gets_its_own_stages_next_to_the_shown_ones():
    pipeline, state = _pipeline("a")
    pipeline.set_stage("grade", ColourStage(lambda x: x * 0.5, per_channel=True))
    pipeline.set_stage("match", ColourStage(lambda x: 1 - x, per_channel=True), order=-1, source_id="b")
    frame = np.full((1, 1, 3), 200, np.uint8)
    assert abs(int(pipeline.process_frame(frame)[0, 0, 0]) - 100) <= 1
    state["source"] = "b"
    assert abs(int(pipeline.process_frame(frame)[0, 0, 0]) - 28) <= 1  # 55 inverted, then halved
    pipeline.set_stage("match", None, source_id="b")
    assert pipeline.stages("b") == pipeline.stages("a")


def test_a_stream_behind_the_panels_keeps_the_stages_it_had():
    pipeline, _ = _pipeline()
    pipeline.set_stage("grade", ColourStage(lambda x: 1 - x, per_channel=True))
    frozen = pipeline.frame_step()
    pipeline.set_stage("grade", None)  # the panels moved on to a phone without a grade
    frame = np.zeros((1, 1, 3), np.uint8)
    assert frozen(frame)[0, 0].tolist() == [255, 255, 255]
    assert pipeline.process_frame(frame) is frame


def test_new_stages_are_baked_off_the_stream_thread():
    pipeline = ColourPipeline(lambda: "phone")
    frame = np.zeros((1, 1, 3), np.uint8)
    pipeline.set_stage("grade", ColourStage(lambda x: 1 - x, per_channel=True))
    first = pipeline.process_frame(frame)
    assert pipeline.settle()
    assert first is frame or first[0, 0, 0] == 255  # either not baked yet, or the baking thread was quick
    assert pipeline.process_frame(frame)[0, 0].tolist() == [255, 255, 255]
    pipeline.set_stage("grade", ColourStage(lambda x: x * 0 + 0.5, per_channel=True))
    meanwhile = pipeline.process_frame(frame)[0, 0, 0]
    assert meanwhile in (255, 128)  # the old LUT until the new one is there, never no grade at all
    assert pipeline.settle()
    assert pipeline.process_frame(frame)[0, 0, 0] == 128


def _slow(fn, gate):
    """A stage that waits for gate before it's baked, so a test can look at the frames meanwhile."""
    def run(x):
        gate.wait(5)
        return fn(x)
    return ColourStage(run, per_channel=True)


def test_a_stream_keeps_its_lut_through_a_moment_without_stages():
    import threading
    pipeline = ColourPipeline(lambda: "phone")
    frame = np.zeros((1, 1, 3), np.uint8)
    pipeline.set_stage("grade", ColourStage(lambda x: 1 - x, per_channel=True))
    pipeline.process_frame(frame)
    assert pipeline.settle() and pipeline.process_frame(frame)[0, 0, 0] == 255
    pipeline.set_stage("grade", None)  # the defaults, then the next phone's settings
    assert pipeline.process_frame(frame) is frame
    gate = threading.Event()
    pipeline.set_stage("grade", _slow(lambda x: x * 0 + 0.5, gate))
    assert pipeline.process_frame(frame)[0, 0, 0] == 255  # the old LUT while the new one bakes
    gate.set()
    assert pipeline.settle() and pipeline.process_frame(frame)[0, 0, 0] == 128


def test_baked_luts_in_use_stay_and_a_hit_keeps_one_longer(monkeypatch):
    monkeypatch.setattr(colour, "_KEEP_BAKED", 4)
    pipeline = ColourPipeline(lambda: "a", bake_inline=True)
    shown = (ColourStage(lambda x: 1 - x, per_channel=True),)
    pipeline.lut(shown, "a")
    o = [(ColourStage(lambda x, v=v: x * 0 + v, per_channel=True),) for v in (0.1, 0.2, 0.3, 0.4, 0.5)]
    for stages in (o[0], o[1], o[0], o[2], o[3]):  # o[0] used again before o[2]
        pipeline.lut(stages, "b")
    assert shown in pipeline._baked  # stream a still shows it, though it's the oldest
    assert o[0] in pipeline._baked and o[1] not in pipeline._baked
    pipeline.lut(o[4], "b")
    assert shown in pipeline._baked and o[3] in pipeline._baked and len(pipeline._baked) == 4


def test_a_bake_that_fails_doesnt_leave_the_baking_thread_stuck(monkeypatch):
    pipeline = ColourPipeline(lambda: "phone")
    calls = []

    def broken(stages):
        calls.append(stages)
        raise MemoryError("no")
    monkeypatch.setattr(colour, "bake", broken)
    monkeypatch.setattr("threading.excepthook", lambda _args: None)
    pipeline.set_stage("grade", ColourStage(lambda x: x))
    pipeline.process_frame(np.zeros((1, 1, 3), np.uint8))
    assert pipeline.settle() and calls and pipeline._baking is None


def test_the_preview_shows_the_picture_before_the_colour_step_left_of_the_split():
    pipeline, _ = _pipeline()
    pipeline.set_stage("grade", ColourStage(lambda x: 1 - x, per_channel=True))
    frame = np.full((40, 80, 3), 10, np.uint8)
    out = pipeline.process_frame(frame)
    assert pipeline.preview(out) is out  # no split asked for
    pipeline.set_split("grade", 0.5)
    out = pipeline.process_frame(frame)
    small = out[::2, ::2].copy()
    shown = pipeline.preview(small)
    assert shown is not small and (small == 245).all()  # drawn on a copy
    assert (shown[:, :15] == 10).all() and (shown[:, 25:] == 245).all()
    assert (shown[:, 20] == 235).all()  # the line between them
    pipeline.set_split("grade", None)
    assert pipeline.preview(small) is small


def test_the_split_is_only_drawn_on_the_stream_it_came_from():
    pipeline, _ = _pipeline()
    pipeline.set_split("grade", 0.5)
    pipeline.process_frame(np.zeros((40, 80, 3), np.uint8))
    tall = np.zeros((80, 40, 3), np.uint8)
    assert pipeline.preview(tall) is tall


def test_diagnostics_say_how_long_the_lut_takes():
    pipeline, _ = _pipeline()
    assert pipeline.diagnostics() == {}
    pipeline.set_stage("grade", ColourStage(lambda x: x))
    pipeline.process_frame(_frame())
    assert pipeline.diagnostics()["Colour pipeline"].startswith("3D 33, ")


def test_the_window_finds_which_stream_a_frame_is_from():
    from types import SimpleNamespace
    from telescope.app import TelescopeWindow
    win = TelescopeWindow.__new__(TelescopeWindow)
    worker = object()
    win._flow = SimpleNamespace(sessions=SimpleNamespace(streams=[
        SimpleNamespace(id="other", session=None), SimpleNamespace(id="b", session=SimpleNamespace(worker=worker))]))
    assert win._frame_source_id() is None
    token = vision.current_frame.set((worker, 1))
    try:
        assert win._frame_source_id() == "b"
    finally:
        vision.current_frame.reset(token)


# ── .cube files ──────────────────────────────────────────────────────────


def test_a_3d_cube_reads_red_fastest():
    rows = [f"{r} {g} {b}" for b in (0, 1) for g in (0, 1) for r in (0, 1)]
    lut = parse_cube('TITLE "Plain"\n# a comment\nLUT_3D_SIZE 2\n' + "\n".join(rows) + "\n")
    assert lut.title == "Plain" and lut.size == 2 and not lut.per_channel
    assert lut.cube[1, 0, 0].tolist() == [0, 0, 1]  # [b][g][r]
    pts = np.array([[0.25, 0.5, 0.75]], np.float32)
    assert np.allclose(lut(pts), pts)


def test_a_1d_lut_with_its_own_domain():
    lut = parse_cube("TITLE \"Demo\"\nLUT_1D_SIZE 3\nDOMAIN_MIN 0 0 0\nDOMAIN_MAX 1 2 3\n0 0 0\n# here too\n"
                     "0.5 1 1.5\n1 1 1\n")
    assert lut.per_channel and lut.size == 3
    assert np.allclose(lut(np.array([[0.5, 0.5, 0.5]], np.float32)), [[0.5, 0.5, 0.5]])


def test_a_resolve_shaper_then_cube():
    rows = ["0 0 0", "2 2 2"] + [f"{r} {g} {b}" for b in (0, 1) for g in (0, 1) for r in (0, 1)]
    lut = parse_cube("LUT_1D_SIZE 2\nLUT_1D_INPUT_RANGE 0 2\nLUT_3D_SIZE 2\nLUT_3D_INPUT_RANGE 0 2\n"
                     + "\n".join(rows))
    assert lut.shaper_max == (2.0, 2.0, 2.0) and lut.cube_max == (2.0, 2.0, 2.0)
    # 0.5 through the shaper (0..2 onto 0..2) stays 0.5, then the cube (0..2) gives 0.25
    assert np.allclose(lut(np.array([[0.5, 0.5, 0.5]], np.float32)), [[0.25, 0.25, 0.25]], atol=1e-6)


@pytest.mark.parametrize("text, problem", [
    ("TITLE x\n0 0 0\n", "doesn't say its size"),
    ("LUT_3D_SIZE 129\n", "129 points per side"),
    ("LUT_3D_SIZE 2\n0 0 0\n", "should have 8 colours but has 1"),
    ("LUT_1D_SIZE 2\n0 0 0\nnope\n1 1 1\n", "Line 3"),
    ("LUT_1D_SIZE 2\nDOMAIN_MIN 1 1 1\nDOMAIN_MAX 0 0 0\n0 0 0\n1 1 1\n", "DOMAIN_MAX"),
    ("LUT_1D_SIZE x\n", "Line 1"),
    ("LUT_1D_SIZE 2\n0 0 0\n1 nan 1\n", "Line 3"),
])
def test_files_telescope_cant_use_say_why(text, problem):
    with pytest.raises(CubeError) as err:
        parse_cube(text)
    assert problem in str(err.value)
    assert err.value.text  # the untranslated text the card shows through tr()


def test_tabs_after_keywords_and_a_hash_inside_a_quoted_title():
    rows = [f"{r}\t{g}\t{b}" for b in (0, 1) for g in (0, 1) for r in (0, 1)]
    lut = parse_cube('TITLE\t"Look # 2" # a comment\nLUT_3D_SIZE\t2\nDOMAIN_MAX\t1\t1\t1\n' + "\n".join(rows))
    assert lut.title == "Look # 2" and lut.size == 2


def test_a_file_that_starts_with_a_byte_order_mark(tmp_path):
    path = tmp_path / "Bom.cube"
    path.write_bytes(b"\xef\xbb\xbfLUT_1D_SIZE 2\n0 0 0\n1 1 1\n")
    assert read_cube(path).size == 2


def test_a_cube_written_reads_back_the_same(tmp_path):
    cube = _warm(lattice(17)).reshape(17, 17, 17, 3)
    path = tmp_path / "Look ö.cube"
    write_cube(path, cube, 'A "quoted" title')
    back = read_cube(path)
    assert back.title == "A 'quoted' title" and back.size == 17
    assert np.abs(back.cube - cube).max() < 1e-6
    assert not list(tmp_path.glob("*.part"))


def test_a_huge_file_is_turned_away(tmp_path):
    path = tmp_path / "big.cube"
    with open(path, "wb") as file:
        file.truncate(65 * 1024 * 1024)
    with pytest.raises(CubeError):
        read_cube(path)


_REAL_LUTS = os.environ.get("TELESCOPE_CUBE_DIR", "")


@pytest.mark.skipif(not _REAL_LUTS, reason="TELESCOPE_CUBE_DIR names a folder of real .cube files to check")
def test_real_cube_files_load_and_survive_a_round_trip(tmp_path):
    files = sorted(Path(_REAL_LUTS).rglob("*.cube"))
    assert files
    ramp = np.repeat(np.linspace(0, 1, 64, dtype=np.float32)[:, None], 3, 1)
    for path in files:
        lut = read_cube(path)
        looked = lut(lattice(9))
        assert looked.shape == (9 ** 3, 3) and np.isfinite(looked).all(), path
        assert np.isfinite(lut(ramp)).all()
        # Exported the way the grading card does it, then read back: the same colours at every lattice point
        cube = np.clip(lut(lattice(33)), 0, 1).reshape(33, 33, 33, 3)
        out = tmp_path / path.name
        write_cube(out, cube, lut.title)
        assert np.abs(read_cube(out).cube - cube).max() < 1e-6, path
