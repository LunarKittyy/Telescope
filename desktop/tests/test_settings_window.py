"""The settings window: which tabs show, in what order, and when its sections hear they're on screen."""

import pytest
from PyQt6.QtCore import Qt
from PyQt6.QtTest import QTest
from PyQt6.QtWidgets import QLabel, QPushButton

from telescope.plugin import SETTINGS_TABS, SettingsSection
from telescope.widgets.settings_window import SettingsWindow
from telescope.widgets.controls import setting_row


@pytest.fixture
def make(qt_widgets):

    def build(*sections):
        win = qt_widgets(SettingsWindow(list(sections)))
        return win

    return build


def _section(tab, text="x", **kwargs):
    return SettingsSection(tab, QLabel(text), **kwargs)


def test_only_tabs_with_something_in_them_show_in_the_fixed_order(make):
    win = make(_section("about"), _section("general"), _section("video"))
    assert win.tabs() == ["general", "video", "about"]
    assert [tab for tab, _name in SETTINGS_TABS if tab in win.tabs()] == win.tabs()
    assert win.tab_names()["about"] == "Updates & About"


def test_a_tab_a_plugin_makes_goes_before_plugins(make):
    win = make(_section("plugins"), _section("shortcuts"), _section("mine", tab_title="My thing"), _section("general"))
    assert win.tabs() == ["general", "shortcuts", "mine", "plugins"]
    assert win.tab_names()["mine"] == "My thing"


def test_a_tab_title_replaces_the_usual_name(make):
    win = make(_section("plugins", tab_title="Plugins (1 off)"))
    assert win.tab_names()["plugins"] == "Plugins (1 off)"


def test_sections_sit_in_order_then_registration_order(make):
    win = make(_section("general", "first", order=10), _section("general", "second"), _section("general", "third"))
    layout = win._stack.widget(0).widget().layout()
    assert [layout.itemAt(i).widget().text() for i in range(3)] == ["second", "third", "first"]


def test_show_tab_switches_and_refuses_one_that_isnt_there(make):
    win = make(_section("general"), _section("video"))
    assert win.current_tab() == "general"
    assert win.show_tab("video") and win.current_tab() == "video"
    assert win._title.text() == "Video"
    assert not win.show_tab("audio") and win.current_tab() == "video"


def test_a_section_hears_when_its_tab_comes_on_screen(make, qapp):
    heard = []
    win = make(_section("general", on_show=lambda: heard.append("general")),
               _section("video", on_show=lambda: heard.append("video")))
    win.show()
    qapp.processEvents()
    assert heard == ["general"]  # the one on screen, not the one behind it
    win.show_tab("video")
    qapp.processEvents()
    assert heard == ["general", "video"]
    win.hide()
    win.show()
    qapp.processEvents()
    assert heard == ["general", "video", "video"]


def test_a_section_that_cant_refresh_doesnt_break_the_window(make, qapp, caplog):
    def fail():
        raise RuntimeError("nope")
    heard = []
    win = make(_section("general", on_show=fail), _section("general", on_show=lambda: heard.append("ok")))
    with caplog.at_level("ERROR"):
        win.show()
        qapp.processEvents()
    assert heard == ["ok"] and "couldn't refresh" in caplog.text


def test_it_stays_inside_the_screen(make, qapp):
    win = make(_section("general"))
    win.show()
    qapp.processEvents()
    area = win.screen().availableGeometry()
    assert win.width() <= area.width() and win.height() <= area.height()


@pytest.mark.parametrize("key", [Qt.Key.Key_Return, Qt.Key.Key_Enter])
@pytest.mark.parametrize("focus", ["search", "results"])
def test_opening_a_search_result_does_not_click_the_setting(make, qapp, key, focus):
    clicked = []
    button = QPushButton("Remove download")
    button.clicked.connect(lambda: clicked.append(True))
    row, _ = setting_row("Remove download", "", button)
    win = make(SettingsSection("general", row))
    win.show()
    qapp.processEvents()
    win._search.setText("remove download")
    target = win._search if focus == "search" else win._results
    target.setFocus()
    QTest.keyClick(target, key)
    qapp.processEvents()
    assert win._search.text() == "" and win._stack.currentWidget() is not win._results_page
    assert clicked == []


def test_enter_on_a_focused_button_still_runs_it(make, qapp):
    clicked = []
    button = QPushButton("Copy diagnostics")
    button.clicked.connect(lambda: clicked.append(True))
    win = make(SettingsSection("general", button))
    win.show()
    qapp.processEvents()
    button.setFocus()
    QTest.keyClick(button, Qt.Key.Key_Return)
    assert clicked == [True]
