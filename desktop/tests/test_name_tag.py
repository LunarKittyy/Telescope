import numpy as np
import pytest

from telescope import name_tag as nt
from telescope.plugin import EventBus
from telescope.view import transform_frame

CANVASES = [(640, 360), (1280, 720), (1920, 1080), (3840, 2160), (1080, 1920), (720, 720), (320, 180)]


@pytest.fixture(autouse=True)
def _qt(qapp):
    pass


def _spec(**kw):
    base = dict(name="Luna Kozankiewicz", pronouns="she/her", second="Telescope developer")
    return nt.Spec(**{**base, **kw})


# ── Layout ───────────────────────────────────────────────────────────────────

@pytest.mark.parametrize("w, h", CANVASES)
@pytest.mark.parametrize("position", nt.POSITIONS)
def test_the_tag_stays_on_the_canvas_where_it_was_put(w, h, position):
    lay = nt.layout(_spec(position=position), w, h)
    box = lay.box
    assert 0 <= box.left() and box.right() <= w and 0 <= box.top() and box.bottom() <= h
    if position.startswith("bottom"):
        assert box.center().y() > h / 2
    else:
        assert box.center().y() < h / 2
    if position.endswith("left"):
        assert box.center().x() < w / 2
    elif position.endswith("right"):
        assert box.center().x() > w / 2
    else:
        assert abs(box.center().x() - w / 2) <= 1


@pytest.mark.parametrize("w, h", CANVASES)
def test_the_text_sits_inside_the_box(w, h):
    lay = nt.layout(_spec(), w, h)
    for piece in (lay.name, lay.pronouns, lay.second):
        text, font, at = piece
        assert lay.box.left() <= at.x() and at.y() <= lay.box.bottom()
    assert lay.second[2].y() > lay.name[2].y()  # the second line goes under the name


def test_it_grows_with_the_canvas_and_the_size_picked():
    small = nt.layout(_spec(), 1280, 720).box
    big = nt.layout(_spec(), 2560, 1440).box
    assert big.height() == pytest.approx(small.height() * 2, rel=0.08)
    sizes = [nt.layout(_spec(size=s), 1280, 720).box.height() for s in ("small", "medium", "large")]
    assert sizes == sorted(sizes) and sizes[0] < sizes[2]


def test_a_long_name_is_cut_to_fit_the_width():
    lay = nt.layout(_spec(name="Wolfeschlegelsteinhausenbergerdorff " * 4, position="bottom_left"), 640, 360)
    assert lay.box.right() <= 640 and lay.name[0].endswith("…")


def test_nothing_to_show_without_any_text():
    assert nt.layout(nt.Spec(), 1280, 720) is None
    assert nt.render(nt.Spec(name="   "), 1280, 720) is None
    assert nt.layout(nt.Spec(second="Just a role"), 1280, 720).name is None


def test_a_right_to_left_name_starts_on_the_right():
    lay = nt.layout(nt.Spec(name="سارة أحمد", pronouns="هي", style="bar"), 1280, 720)
    assert nt.right_to_left("سارة") and not nt.right_to_left("Luna") and not nt.right_to_left("山田")
    assert lay.pronouns[2].x() < lay.name[2].x()  # read from the right: the name first
    assert lay.stripe.right() == lay.box.right()


@pytest.mark.parametrize("name", ["山田 花子", "नीलम शर्मा", "Ζωή Παπαδοπούλου", "Олена Коваль", "김민지"])
def test_names_in_other_scripts_get_drawn(name):
    tag = nt.render(nt.Spec(name=name, style="bar"), 1280, 720)
    lay = nt.layout(nt.Spec(name=name, style="bar"), 1280, 720)
    assert lay.name[0] == name  # nothing cut
    box = lay.box.toRect().adjusted(1, 1, -1, -1).translated(-tag.x, -tag.y)
    assert (tag.keep[box.top():box.bottom(), box.left():box.right()] < 255).all()  # the box covers its area
    text = tag.ink[box.top():box.bottom(), int(box.left() + lay.em * 0.9):box.right()]
    assert (text.max(axis=2) > 200).sum() > lay.em * 3  # the glyphs are there


# ── Drawing and the cache ────────────────────────────────────────────────────

def test_drawing_only_touches_the_tile():
    frame = np.full((720, 1280, 3), 100, np.uint8)
    tag = nt.render(_spec(), 1280, 720)
    out = nt.apply(frame, tag)
    th, tw = tag.ink.shape[:2]
    assert out is frame  # in place
    outside = np.ones(frame.shape[:2], bool)
    outside[tag.y:tag.y + th, tag.x:tag.x + tw] = False
    assert (out[outside] == 100).all() and (out[~outside] != 100).any()


def test_a_read_only_frame_is_copied():
    frame = np.full((360, 640, 3), 50, np.uint8)
    frame.flags.writeable = False
    out = nt.apply(frame, nt.render(_spec(), 640, 360))
    assert out is not frame and (frame == 50).all()


def test_the_cache_paints_once_until_something_changes():
    cache = nt.TagCache()
    spec = _spec()
    first = cache.get(spec, 1280, 720)
    assert cache.get(spec, 1280, 720) is first and cache.get(_spec(), 1280, 720) is first and cache.renders == 1
    for changed in (_spec(name="Alex"), _spec(pronouns="they/them"), _spec(second=""), _spec(style="light"),
                    _spec(position="top_right"), _spec(size="large"), _spec(families=("Serif",))):
        assert cache.get(changed, 1280, 720) is not first
    renders = cache.renders
    assert cache.get(spec, 1920, 1080) is not first  # the canvas changed size
    assert cache.renders == renders + 1


# ── The plugin ───────────────────────────────────────────────────────────────

class _Host:
    def __init__(self):
        self.saves = 0

    def schedule_save(self):
        self.saves += 1


@pytest.fixture
def plugin(qt_widgets):
    from telescope.plugins.name_tag import NameTagPlugin
    p = NameTagPlugin()
    p.setup(_Host(), EventBus())
    qt_widgets(p.create_panel())
    p.set_config({})
    return p


def test_off_by_default_and_until_there_is_text(plugin):
    frame = np.full((360, 640, 3), 70, np.uint8)
    assert (plugin.process_frame(frame.copy()) == 70).all() and plugin.frame_step() is None
    plugin.set_config({"on": True})
    assert (plugin.process_frame(frame.copy()) == 70).all()
    plugin._name_edit.setText("Luna")
    assert (plugin.process_frame(frame.copy()) != 70).any() and plugin.frame_step() is not None


def test_settings_round_trip_through_presets(plugin):
    cfg = {"on": True, "name": "Luna", "pronouns": "she/her", "second": "Host", "style": "pill",
           "position": "top_right", "size": "large"}
    plugin.apply_preset(cfg)
    assert plugin.get_config() == cfg
    assert plugin.in_presets and plugin.optional


def test_bad_settings_fall_back_to_the_defaults(plugin):
    plugin.set_config({"on": "yes", "name": 5, "style": "neon", "position": "middle", "size": "huge",
                       "second": "x" * 500})
    cfg = plugin.get_config()
    assert cfg["on"] is False and cfg["name"] == "" and len(cfg["second"]) == nt.MAX_CHARS
    assert (cfg["style"], cfg["position"], cfg["size"]) == (nt.DEFAULT_STYLE, nt.DEFAULT_POSITION, nt.DEFAULT_SIZE)


def test_typing_saves_and_swaps_the_settings_whole(plugin):
    before, saves = plugin._settings, plugin._host.saves
    plugin._name_edit.setText("Luna")
    assert plugin._settings is not before and plugin._host.saves > saves
    frozen = plugin._settings
    plugin._style_combo.setCurrentIndex(plugin._style_combo.findData("light"))
    assert frozen.spec.style == "bar" and plugin._settings.spec.style == "light"  # a frozen step keeps its look


def test_mirroring_the_picture_doesnt_mirror_the_tag(plugin):
    # The window's view (Transforms' flip) runs before every plugin, so the tag lands on the picture as sent
    plugin.set_config({"on": True, "name": "Luna", "position": "bottom_left"})
    frame = np.zeros((360, 640, 3), np.uint8)
    frame[:, :320] = 200  # something to tell the two sides apart
    plain = plugin.process_frame(transform_frame(frame.copy(), False, False, None).copy())
    flipped = plugin.process_frame(transform_frame(frame.copy(), True, False, None).copy())
    tag = plugin._settings.cache.get(plugin._settings.spec, 640, 360)
    th, tw = tag.ink.shape[:2]
    region = (slice(tag.y, tag.y + th), slice(tag.x, tag.x + tw))
    assert tag.x < 320  # still on the left
    reference = nt.apply(np.full((th, tw, 3), 200, np.uint8), nt.Tag(0, 0, tag.ink, tag.keep))
    assert np.array_equal(plain[region], reference)  # drawn the right way round on the original
    flipped_ref = nt.apply(np.zeros((th, tw, 3), np.uint8), nt.Tag(0, 0, tag.ink, tag.keep))
    assert np.array_equal(flipped[region], flipped_ref)  # and on the flipped picture, same text, not mirrored


def test_it_runs_after_the_background_plugin():
    from telescope.plugins import builtin_plugins
    names = [p.name for p in builtin_plugins()]
    assert names.index("name_tag") > names.index("background")


def test_it_uses_the_apps_fonts():
    assert nt.app_families()[0] == "Inter" and "sans-serif" not in nt.app_families()
