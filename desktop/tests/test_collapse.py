"""Cards and sections fold when their title is clicked, and what's folded is remembered."""

import pytest
from PyQt6 import sip
from PyQt6.QtCore import QEvent, QPointF, Qt
from PyQt6.QtGui import QEnterEvent
from PyQt6.QtTest import QTest
from PyQt6.QtWidgets import QLabel, QPushButton, QToolButton, QVBoxLayout, QWidget

from telescope.config import load_config
from telescope.plugin import TelescopePlugin
from telescope.widgets.collapse import Collapsing
from telescope.widgets.common import (
    add_card_header, add_section_heading, card_layout, create_card, reading_order, tab_in_reading_order,
)


def make_card(title="Things", sections=("Light", "Focus")):
    """A card the way a plugin builds one: header, a loose row, then each section with two rows."""
    card = create_card()
    lay = card_layout(card)
    add_card_header(lay, title, "camera")
    loose = QLabel("loose")
    lay.addWidget(loose)
    rows = {"loose": loose}
    for name in sections:
        add_section_heading(lay, name, "sun")
        for i in range(2):
            row = QLabel(f"{name} {i}")
            lay.addWidget(row)
            rows[f"{name} {i}"] = row
    return card, rows


def title_of(card, text):
    return next(t for t in card.findChildren(QLabel) if t.text() == text)


def click(widget):
    QTest.mouseClick(widget, Qt.MouseButton.LeftButton)


@pytest.fixture
def shown(qapp):
    made = []

    def show(widget):
        widget.resize(400, 600)
        widget.show()
        made.append(widget)
        return widget
    yield show
    for widget in made:
        if not sip.isdeleted(widget):
            widget.close()
            widget.deleteLater()


def test_clicking_a_card_title_hides_everything_under_it_and_again_shows_it(qapp, shown):
    card, rows = make_card()
    Collapsing().install(card, "p")
    shown(card)
    assert all(row.isVisible() for row in rows.values())
    click(title_of(card, "Things"))
    assert not any(row.isVisible() for row in rows.values())
    assert title_of(card, "Things").isVisible()
    assert title_of(card, "Light").isVisible() is False  # sections are under the card, so they fold with it
    click(title_of(card, "Things"))
    assert all(row.isVisible() for row in rows.values())
    assert title_of(card, "Light").isVisible()


def test_a_section_folds_alone(qapp, shown):
    card, rows = make_card()
    Collapsing().install(card, "p")
    shown(card)
    click(title_of(card, "Light"))
    assert title_of(card, "Light").isVisible()
    assert not rows["Light 0"].isVisible() and not rows["Light 1"].isVisible()
    assert rows["loose"].isVisible()
    assert rows["Focus 0"].isVisible() and title_of(card, "Focus").isVisible()


def test_clicking_the_label_releases_the_native_toggle(qapp, shown):
    card, rows = make_card()
    state = Collapsing()
    state.install(card, "p")
    shown(card)
    label = title_of(card, "Things")
    button = label.parentWidget()
    for folded in (True, False, True, False):
        click(label)
        assert state.is_folded("p/Things") is folded
        assert button.isDown() is False and button.isChecked() is not folded
        assert rows["loose"].isVisible() is not folded


@pytest.mark.parametrize("key", [Qt.Key.Key_Space, Qt.Key.Key_Return, Qt.Key.Key_Enter])
def test_card_and_section_titles_are_named_keyboard_toggles(qapp, shown, key):
    card, rows = make_card()
    state = Collapsing()
    state.install(card, "p")
    shown(card)
    card.activateWindow()
    qapp.processEvents()
    buttons = {b.accessibleName(): b for b in card.findChildren(QToolButton, "fold_heading")}
    title, section = buttons["Things"], buttons["Light"]
    # Qt also uses child order for its native accessibility tree.
    body = card.findChild(QWidget, "card_body", Qt.FindChildOption.FindDirectChildrenOnly)
    assert card.children().index(title) < card.children().index(body)
    assert reading_order(card) == [title, section, buttons["Focus"]]
    assert title.isCheckable() and title.isChecked() and "Expanded" in title.accessibleDescription()
    assert title.toolTip() == title.accessibleDescription()
    title.setFocus(Qt.FocusReason.TabFocusReason)
    QTest.keyClick(title, key)
    assert state.is_folded("p/Things") and not rows["loose"].isVisible()
    assert title.hasFocus() and not title.isChecked() and "Collapsed" in title.accessibleDescription()
    assert reading_order(card) == [title]
    QTest.keyClick(title, key)
    tab_in_reading_order(card, True)
    assert section.hasFocus()
    QTest.keyClick(section, key)
    assert not rows["Light 0"].isVisible() and rows["Focus 0"].isVisible()
    tab_in_reading_order(card, False)
    assert title.hasFocus()


def test_folding_moves_focus_out_of_the_rows_it_hides(qapp, shown):
    card, _rows = make_card()
    control = QPushButton("A setting")
    card.layout().addWidget(control)
    state = Collapsing()
    state.install(card, "p")
    shown(card)
    card.activateWindow()
    qapp.processEvents()
    control.setFocus()
    assert control.hasFocus()
    state.set_folded("p/Things", True)
    title = next(b for b in card.findChildren(QToolButton) if b.accessibleName() == "Things")
    assert title.hasFocus() and not control.isVisible()


def test_the_rule_and_icon_of_a_section_heading_fold_it_too(qapp, shown):
    card, rows = make_card()
    Collapsing().install(card, "p")
    shown(card)
    head = title_of(card, "Light").parentWidget()
    click(head)
    assert not rows["Light 0"].isVisible()


def test_unfolding_a_card_keeps_rows_a_plugin_hid_itself(qapp, shown):
    card, rows = make_card()
    state = Collapsing()
    state.install(card, "p")
    shown(card)
    rows["Light 1"].setVisible(False)  # what a mode toggle does
    click(title_of(card, "Things"))
    rows["Focus 0"].setVisible(False)  # a plugin changing rows while the card is folded
    rows["Light 1"].setVisible(True)
    click(title_of(card, "Things"))
    assert rows["Light 1"].isVisible() and not rows["Focus 0"].isVisible()
    assert rows["Light 0"].isVisible()


def test_a_folded_section_stays_folded_when_its_card_is_folded_and_opened(qapp, shown):
    card, rows = make_card()
    Collapsing().install(card, "p")
    shown(card)
    click(title_of(card, "Light"))
    click(title_of(card, "Things"))
    click(title_of(card, "Things"))
    assert not rows["Light 0"].isVisible() and rows["Focus 0"].isVisible()


def test_everything_starts_open(qapp, shown):
    card, rows = make_card()
    state = Collapsing()
    state.install(card, "p")
    shown(card)
    assert state.keys() == []
    assert all(row.isVisible() for row in rows.values())


def test_keys_say_who_owns_the_card_and_section(qapp):
    card, _ = make_card()
    state = Collapsing()
    state.install(card, "camera_control")
    click_state = state
    click_state.set_folded("camera_control/Things", True)
    click_state.set_folded("camera_control/Things/Light", True)
    assert state.keys() == ["camera_control/Things", "camera_control/Things/Light"]


def test_two_plugins_with_the_same_titles_fold_separately(qapp, shown):
    a, rows_a = make_card()
    b, rows_b = make_card()
    state = Collapsing()
    state.install(a, "first")
    state.install(b, "second")
    shown(a), shown(b)
    click(title_of(a, "Light"))
    assert not rows_a["Light 0"].isVisible() and rows_b["Light 0"].isVisible()


def test_same_title_twice_in_one_card_gets_its_own_key(qapp, shown):
    card, _ = make_card(sections=("More", "More"))
    state = Collapsing()
    state.install(card, "p")
    shown(card)
    click([t for t in card.findChildren(QLabel) if t.text() == "More"][1])
    assert state.keys() == ["p/Things/More#2"]
    first, second = [t for t in card.findChildren(QLabel) if t.text() == "More 0"]
    assert first.isVisible() and not second.isVisible()  # the first "More" is open


def test_loaded_state_is_applied_to_cards_already_there_and_to_ones_added_later(qapp, shown):
    first, rows_first = make_card("First")
    state = Collapsing()
    state.install(first, "a")
    shown(first)
    state.load(["a/First", "b/Second/Light"])
    assert not rows_first["loose"].isVisible()
    second, rows_second = make_card("Second")
    state.install(second, "b")  # a plugin whose card appears after the settings were read
    shown(second)
    assert rows_second["loose"].isVisible() and not rows_second["Light 0"].isVisible()


def test_a_card_that_goes_and_comes_back_is_folded_as_it_was(qapp, shown):
    state = Collapsing()
    card, _ = make_card()
    state.install(card, "p")
    shown(card)
    click(title_of(card, "Things"))
    card.close()
    card.deleteLater()
    qapp.processEvents()
    qapp.sendPostedEvents(None, QEvent.Type.DeferredDelete.value)
    assert state.keys() == ["p/Things"]  # the gone card doesn't take its fold with it
    state.load(state.keys())  # the dead container is dropped quietly
    again, rows = make_card()
    state.install(again, "p")
    shown(again)
    assert not rows["loose"].isVisible()


def test_state_that_isnt_a_list_of_strings_is_ignored(qapp):
    state = Collapsing()
    state.load(["a", 3, None, "b"])
    assert state.keys() == ["a", "b"]
    state.load("not a list")
    assert state.keys() == []
    state.load(None)
    assert state.keys() == []


def test_changes_are_reported_once_each(qapp):
    changes = []
    state = Collapsing(lambda: changes.append(1))
    state.set_folded("a", True)
    state.set_folded("a", True)
    state.set_folded("a", False)
    assert len(changes) == 2
    state.load(["x"])  # loading what was saved isn't a change to save
    assert len(changes) == 2


def test_a_card_is_only_made_collapsible_once(qapp, shown):
    card, rows = make_card()
    state = Collapsing()
    state.install(card, "p")
    state.install(card, "p")
    shown(card)
    click(title_of(card, "Things"))
    assert not rows["loose"].isVisible()
    assert card.layout().count() == 2  # the header and the one container


def test_a_panel_holding_cards_gets_each_of_them(qapp, shown):
    panel = QWidget()
    lay = QVBoxLayout(panel)
    one, rows_one = make_card("One")
    two, rows_two = make_card("Two")
    lay.addWidget(one), lay.addWidget(two)
    state = Collapsing()
    state.install(panel, "p")
    shown(panel)
    click(title_of(two, "Two"))
    assert rows_one["loose"].isVisible() and not rows_two["loose"].isVisible()


def test_widgets_without_a_card_header_are_left_alone(qapp):
    plain = QWidget()
    QVBoxLayout(plain).addWidget(QLabel("x"))
    card = create_card()
    card_layout(card).addWidget(QLabel("no header"))
    state = Collapsing()
    state.install(plain, "p")
    state.install(card, "p")
    assert card.layout().count() == 1 and state.keys() == []


def test_a_row_keeps_its_stretch_and_alignment_inside_the_fold(qapp):
    card = create_card()
    lay = card_layout(card)
    add_card_header(lay, "Things", "camera")
    grow = QLabel("grow")
    lay.addWidget(grow, 1)
    Collapsing().install(card, "p")
    body = card.layout().itemAt(1).widget()
    assert card.layout().stretch(1) == 1
    assert body.layout().stretch(0) == 1


def test_hovering_a_title_marks_it_and_leaving_puts_it_back(qapp, shown):
    card, _ = make_card()
    Collapsing().install(card, "p")
    shown(card)
    title = title_of(card, "Things")
    assert not title.property("hot")
    QTest.qWait(1)
    qapp.sendEvent(title, QEnterEvent(QPointF(2, 2), QPointF(2, 2), QPointF(2, 2)))
    assert title.property("hot") is True
    qapp.sendEvent(title, QEvent(QEvent.Type.Leave))
    assert not title.property("hot")
    section = title_of(card, "Light")
    qapp.sendEvent(section.parentWidget(), QEnterEvent(QPointF(2, 2), QPointF(2, 2), QPointF(2, 2)))
    assert section.property("hot") is True


def test_a_title_that_goes_dim_with_its_card_stops_looking_hovered(qapp, shown):
    card, _ = make_card()
    Collapsing().install(card, "p")
    shown(card)
    title = title_of(card, "Things")
    qapp.sendEvent(title, QEnterEvent(QPointF(2, 2), QPointF(2, 2), QPointF(2, 2)))
    card.setEnabled(False)
    assert not title.property("hot")


def test_a_dimmed_card_still_folds(qapp, shown):
    # Cards wait dimmed and disabled for a phone; folding them away is still allowed.
    card, rows = make_card()
    Collapsing().install(card, "p")
    shown(card)
    card.setEnabled(False)
    click(title_of(card, "Things"))
    assert not rows["loose"].isVisible()


def test_only_the_left_button_folds(qapp, shown):
    card, rows = make_card()
    Collapsing().install(card, "p")
    shown(card)
    QTest.mouseClick(title_of(card, "Things"), Qt.MouseButton.RightButton)
    assert rows["loose"].isVisible()


# ── In the window ─────────────────────────────────────────────────────────────

def window_cards(window):
    return [t for t in window.findChildren(QLabel, "card_title")]


@pytest.fixture
def window(qapp, config_home, monkeypatch):
    import telescope.app as app_module
    monkeypatch.setattr(app_module.QSystemTrayIcon, "isSystemTrayAvailable", lambda: False)
    win = app_module.TelescopeWindow()
    yield win
    win._session = None
    win._tray = None
    win.deleteLater()


class CardPlugin(TelescopePlugin):
    name = "cardy"
    title = "Cardy"

    def create_panel(self):
        card, self.rows = make_card("Cardy")
        return card


def test_a_plugin_added_later_is_collapsible_without_doing_anything(window):
    plugin = CardPlugin()
    window.register_plugin(plugin)
    assert "cardy/Cardy" in window._collapse._bodies
    assert "cardy/Cardy/Light" in window._collapse._bodies


def test_folds_are_saved_and_come_back_with_the_next_window(window, qapp, config_home, monkeypatch):
    import telescope.app as app_module
    plugin = CardPlugin()
    window.register_plugin(plugin)
    window._collapse.set_folded("cardy/Cardy/Light", True)
    window.save_now()
    assert load_config()["collapsed"] == ["cardy/Cardy/Light"]

    again = app_module.TelescopeWindow()
    try:
        plugin2 = CardPlugin()
        again.register_plugin(plugin2)
        again.apply_saved_config()
        assert again._collapse.is_folded("cardy/Cardy/Light")
        again.show()
        assert not plugin2.rows["Light 0"].isVisible() and plugin2.rows["Focus 0"].isVisible()
    finally:
        again._session = None
        again._tray = None
        again.deleteLater()


def test_a_switched_off_plugins_folds_survive_saving_while_it_is_gone(window, config_home):
    window._collapse.load(["gone/Card", "gone/Card/Section"])
    window.save_now()
    assert load_config()["collapsed"] == ["gone/Card", "gone/Card/Section"]


def test_a_plugin_that_hides_its_card_keeps_it_hidden_when_folded(window, qapp):
    plugin = CardPlugin()
    window.register_plugin(plugin)
    window.resize(1400, 900)
    window.show()
    card = plugin.rows["loose"].parentWidget().parentWidget()
    card.setVisible(False)
    window._collapse.set_folded("cardy/Cardy", True)
    window._refresh_layout(force=True)
    assert card.isHidden()
    window._collapse.set_folded("cardy/Cardy", False)
    assert card.isHidden()


def test_resizing_into_another_layout_keeps_folds(window, qapp):
    plugin = CardPlugin()
    window.register_plugin(plugin)
    window.resize(1400, 900)
    window.show()
    window._collapse.set_folded("cardy/Cardy/Light", True)
    window.resize(500, 900)
    qapp.processEvents()
    window._refresh_layout(force=True)
    assert not plugin.rows["Light 0"].isVisible()
    assert plugin.rows["Focus 0"].isVisible()


def test_a_malformed_saved_list_is_reset(config_home):
    import json
    path = config_home.config_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps({"version": config_home.CONFIG_VERSION, "collapsed": [1, "a"]}))
    assert config_home.load_config()["collapsed"] == []
    path.write_text(json.dumps({"version": config_home.CONFIG_VERSION, "collapsed": ["a"]}))
    assert config_home.load_config()["collapsed"] == ["a"]
