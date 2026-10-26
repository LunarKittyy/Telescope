"""Looking for a setting by what it does: real queries against the real tabs, so the ranking is judged by what turns up."""

import pytest
from PyQt6.QtWidgets import QLabel

import telescope.plugins.setup as setup_mod
from telescope import fuzzy
from telescope.plugin import SettingsSection
from telescope.widgets import settings_search
from telescope.widgets.settings_window import SettingsWindow
from test_plugin_removal import build  # noqa: F401 (the fixture)

# (what someone might type, the setting that should come up, how far down the list it may be)
FINDS = [
    # the name, or a part of it
    ("hardware acceleration", "Hardware acceleration", 1),
    ("canvas", "Canvas size", 1),
    ("max zoom", "Max zoom", 1),
    ("limiter", "Limiter", 1),
    ("wait screen", "Wait screen", 1),
    ("card colours", "Card colours", 1),
    ("copy diagnostics", "Bug reports", 2),
    ("wait before", "Wait before stopping", 1),
    ("sign in", "Open Telescope when I sign in", 1),
    ("keyboard", "Keyboard shortcuts", 1),
    # typos and the other spelling
    ("hardwre accel", "Hardware acceleration", 1),
    ("reslution", "Canvas size", 2),
    ("colour", "Card colours", 2),
    ("color", "Card colours", 2),
    ("limitr", "Limiter", 1),
    ("zomm", "Max zoom", 2),
    ("notificaton", "Tell me when it stops a stream", 3),
    ("mirorr", "Mirror it, for apps that flip the camera", 2),
    # what it does, in other words
    ("gpu", "Hardware acceleration", 1),
    ("nvidia", "Hardware acceleration", 2),
    ("hw accel", "Hardware acceleration", 1),
    ("hwaccel", "Hardware acceleration", 1),
    ("resolution", "Canvas size", 1),
    ("1080p", "Canvas size", 2),
    ("4k", "Canvas size", 3),
    ("portrait", "Canvas size", 2),
    ("aspect ratio", "Canvas size", 2),
    ("volume", "Max gain", 2),
    ("microphone boost", "Max gain", 2),
    ("quiet mic", "Max gain", 3),
    ("clipping", "Limiter", 2),
    ("distortion", "Limiter", 2),
    ("startup", "Open Telescope when I sign in", 2),
    ("run on startup", "Open Telescope when I sign in", 2),
    ("autostart", "Open Telescope when I sign in", 1),
    ("login", "Open Telescope when I sign in", 2),
    ("tray", "Open Telescope when I sign in", 3),
    ("start automatically", "Start", 3),
    ("stop when idle", "Stop", 3),
    ("delay", "Wait before stopping", 2),
    ("timeout", "Wait before stopping", 2),
    ("notify", "Tell me when it stops a stream", 3),
    ("black screen", "Wait screen", 2),
    ("custom image", "Wait screen", 3),
    ("placeholder", "Wait screen", 2),
    ("flip", "Mirror it, for apps that flip the camera", 3),
    ("theme", "Card colours", 2),
    ("monochrome", "Card colours", 1),
    ("purple", "Card colours", 2),
    ("update", "Check daily", 4),
    ("nightly", "Channel", 1),
    ("stable", "Channel", 2),
    ("auto update", "Check daily", 2),
    ("version", "This version", 2),
    ("bug report", "Bug reports", 1),
    ("logs", "Bug reports", 2),
    ("debug", "Bug reports", 2),
    ("hotkey", "Shortcuts", 2),
    ("push to talk", "Shortcuts", 3),
    ("keybinding", "Shortcuts", 2),
    ("global shortcuts", "Desktop shortcut settings", 3),
    ("install plugin", "Added plugins", 2),
    ("extensions", "Added plugins", 4),
    ("apk", "Telescope.apk", 1),
    ("install the phone app", "Telescope.apk", 2),
    # asked as a question
    ("how do i change the resolution", "Canvas size", 1),
    ("where is the gpu setting", "Hardware acceleration", 1),
    ("turn on autostart", "Open Telescope when I sign in", 2),
]

LINUX_ONLY = [
    ("kernel module", "Kernel module", 1),
    ("v4l2loopback", "Kernel module", 2),
    ("load at boot", "Load when the computer starts", 1),
    ("reboot", "Load when the computer starts", 2),
    ("virtual camera not showing", "Kernel module", 4),
]

WINDOWS_ONLY = [
    ("driver", "Driver", 1),
    ("unitycapture", "Driver", 1),
    ("adb", "ADB", 1),
    ("usb", "ADB", 3),
    ("virtual camera not showing", "Driver", 3),
]

# words that mean nothing here should come back with nothing, not with a guess
NOTHING = ["zzzzzz", "qwertyuiop", "banana bread", "xx", "kubernetes"]


@pytest.fixture
def settings(build, qapp, monkeypatch, request):  # noqa: F811
    monkeypatch.setattr(setup_mod, "IS_LINUX", request.param)
    window = build(None)
    window.open_settings()
    win = window._settings_win
    yield win
    win.close()


def _names(win, query, limit=12):
    return [entry.name for entry in win.search_results(query)][:limit]


def _check(win, cases):
    misses = []
    for query, wanted, within in cases:
        found = _names(win, query)
        if wanted not in found[:within]:
            misses.append(f"{query!r}: wanted {wanted!r} in the first {within}, got {found[:5]}")
    assert not misses, "\n" + "\n".join(misses)


@pytest.mark.parametrize("settings", [True], indirect=True)
def test_finds_what_people_would_look_for_on_linux(settings):
    _check(settings, FINDS + LINUX_ONLY)


@pytest.mark.parametrize("settings", [False], indirect=True)
def test_finds_what_people_would_look_for_on_windows(settings):
    _check(settings, FINDS + WINDOWS_ONLY)


@pytest.mark.parametrize("settings", [True], indirect=True)
def test_nonsense_finds_nothing(settings):
    for query in NOTHING:
        assert _names(settings, query) == [], query


@pytest.mark.parametrize("settings", [True], indirect=True)
def test_every_setting_can_be_found_by_its_own_name(settings):
    entries = settings_search.collect(settings._tabs)
    assert len(entries) > 25
    lost = [e.name for e in entries if e not in settings_search.search(entries, e.name, limit=5)]
    assert not lost


@pytest.mark.parametrize("settings", [True], indirect=True)
def test_results_say_where_the_setting_is(settings):
    first = settings.search_results("canvas size")[0]
    assert first.name == "Canvas size" and first.tab == "video" and first.path == "Video"


@pytest.mark.parametrize("settings", [True], indirect=True)
def test_one_letter_slips_and_swapped_letters_still_find_it(settings):
    for query in ("hardwrae", "acceleraton", "lmiter", "cavnas", "micorphone"):
        assert _names(settings, query), query


# ── The matcher on its own ────────────────────────────────────────────────────

def _score(query, **fields):
    return fuzzy.Doc(**fields).score(query, {k: 1.0 for k in fields})


def test_a_word_has_to_start_a_word_unless_it_is_long():
    assert _score("crop", name="Microphone") == 0  # inside a word is no match for a short one
    assert _score("loopback", name="v4l2loopback") > 0


def test_every_word_has_to_match_but_filler_doesnt_count():
    assert _score("max zoom", name="Max zoom") > _score("max", name="Max zoom")
    assert _score("max volume", name="Max zoom") == 0
    assert _score("how to zoom", name="Max zoom") > 0
    assert _score("hw accel", name="Hardware acceleration") > 0  # "hw" is too short to rule it out


def test_a_closer_match_scores_higher():
    exact = _score("zoom", name="Zoom")
    prefix = _score("zoo", name="Zoom")
    typo = _score("zoim", name="Zoom")
    assert exact > prefix > typo > 0


def test_accents_and_case_dont_matter():
    assert _score("CAFÉ", name="cafe") > 0


# ── Going to a result ─────────────────────────────────────────────────────────

@pytest.mark.parametrize("settings", [True], indirect=True)
def test_typing_shows_results_and_clearing_goes_back_to_the_tab(settings, qapp):
    settings.show()
    settings.show_tab("audio")
    settings._search.setText("zoom")
    assert settings._stack.currentWidget() is settings._results_page
    assert settings._results.count() > 0 and settings._title.text() == "Search results"
    assert settings.current_tab() == "audio"  # where it was is remembered
    settings._search.clear()
    assert settings._stack.currentWidget() is not settings._results_page and settings._title.text() == "Audio"


@pytest.mark.parametrize("settings", [True], indirect=True)
def test_opening_a_result_goes_to_its_tab_and_lights_the_setting(settings, qapp):
    from PyQt6.QtTest import QTest
    settings.show()
    settings._search.setText("max gain")
    settings._open_current()
    QTest.qWait(30)
    assert settings.current_tab() == "audio" and settings._search.text() == ""
    flashed = settings._flashed
    assert flashed is not None and flashed.property("found") is True
    page = settings._stack.currentWidget()
    top = flashed.mapTo(page.viewport(), flashed.rect().topLeft()).y()
    assert 0 <= top < page.viewport().height()  # scrolled into view


@pytest.mark.parametrize("settings", [True], indirect=True)
def test_nothing_found_says_so_and_offers_a_way_on(settings):
    settings.show()
    settings._search.setText("zzzzzz")
    assert not settings._empty.isHidden() and "Nothing matches" in settings._empty.text()
    assert settings._results.isHidden()
    settings._search.setText("zoom")
    assert settings._empty.isHidden() and not settings._results.isHidden()


@pytest.mark.parametrize("settings", [True], indirect=True)
def test_when_no_entry_has_every_word_the_ones_with_some_are_offered_and_say_so(settings):
    assert _names(settings, "zoom meeting")[:1] == ["Max zoom"]
    settings.show()
    settings._search.setText("zoom meeting")
    assert not settings._note.isHidden() and settings._results.count() > 0
    settings._search.setText("max zoom")
    assert settings._note.isHidden()


@pytest.mark.parametrize("settings", [True], indirect=True)
def test_arrow_keys_and_enter_work_from_the_search_box(settings, qapp):
    from PyQt6.QtCore import Qt
    from PyQt6.QtTest import QTest
    settings.show()
    settings._search.setText("zoom")
    assert settings._results.currentRow() == 0
    QTest.keyClick(settings._search, Qt.Key.Key_Down)
    assert settings._results.currentRow() == 1
    QTest.keyClick(settings._search, Qt.Key.Key_Up)
    QTest.keyClick(settings._search, Qt.Key.Key_Up)
    assert settings._results.currentRow() == 0
    QTest.keyClick(settings._search, Qt.Key.Key_Escape)
    assert settings._search.text() == "" and settings.isVisible()  # clears first, closes on the next one


@pytest.mark.parametrize("settings", [True], indirect=True)
def test_clicking_a_tab_while_searching_leaves_the_search(settings):
    settings.show()
    settings._search.setText("zoom")
    settings._nav.setCurrentRow(settings.tabs().index("streaming"))
    assert settings._search.text() == "" and settings.current_tab() == "streaming"
    assert settings._stack.currentWidget() is not settings._results_page


def test_a_plugins_own_section_is_searchable_without_doing_anything(qapp):
    from telescope.widgets.common import add_card_header, card_layout, create_card
    card = create_card()
    add_card_header(card_layout(card), "Sparkles", "sliders")
    settings_search.keywords(card, "glitter shimmer")
    win = SettingsWindow([SettingsSection("general", card, keywords="extras")])
    try:
        assert [e.name for e in win.search_results("glitter")] == ["Sparkles"]
        assert [e.name for e in win.search_results("extras")] == ["Sparkles"]
        assert win.search_results("sparkels")
    finally:
        win.close()


def test_hidden_rows_arent_offered(qapp):
    from telescope.widgets.controls import setting_row
    shown, _ = setting_row("Shown thing", "")
    hidden, _ = setting_row("Hidden thing", "")
    hidden.setHidden(True)
    holder = QLabel()
    win = SettingsWindow([SettingsSection("general", shown), SettingsSection("general", hidden), SettingsSection(
        "general", holder)])
    try:
        assert [e.name for e in win.search_results("thing")] == ["Shown thing"]
    finally:
        win.close()
