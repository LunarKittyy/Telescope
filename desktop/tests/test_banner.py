"""Problem banners: keyed, replaceable, dismissable, and actions that run once."""

from PyQt6.QtGui import QGuiApplication

from telescope.widgets.banner import BannerAction, BannerArea, Issue, copy_action


def test_keys_replace_and_clear(qapp):
    area = BannerArea()
    assert area.isHidden()
    area.show_issue("start", Issue("First"))
    area.show_issue("vcam", Issue("Other"))
    area.show_issue("start", Issue("Second"))
    assert area.keys() == ["vcam", "start"]
    assert area.issue("start").title == "Second"
    area.clear_issue("vcam")
    assert area.keys() == ["start"] and not area.isHidden()
    area.clear_issue()
    assert area.keys() == [] and area.isHidden()


def test_an_action_runs_and_takes_the_banner_with_it(qapp):
    area = BannerArea()
    ran = []
    area.show_issue("start", Issue("Can't connect", "Open the app.", [BannerAction("Try again", lambda: ran.append(1))]))
    area.banner("start").buttons[0].click()
    assert ran == [1]
    assert area.keys() == []


def test_an_action_that_replaces_the_issue_keeps_the_new_one(qapp):
    area = BannerArea()
    area.show_issue("start", Issue("Old", actions=[
        BannerAction("Try again", lambda: area.show_issue("start", Issue("New")))]))
    area.banner("start").buttons[0].click()
    assert area.issue("start").title == "New"


def test_copy_keeps_the_banner_and_fills_the_clipboard(qapp):
    area = BannerArea()
    area.show_issue("start", Issue("Run this", details="sudo modprobe -r v4l2loopback",
                                   actions=[copy_action("sudo modprobe -r v4l2loopback")]))
    banner = area.banner("start")
    assert not banner.details_lbl.isHidden()
    banner.buttons[0].click()
    assert QGuiApplication.clipboard().text() == "sudo modprobe -r v4l2loopback"
    assert area.keys() == ["start"]


def test_dismiss(qapp):
    area = BannerArea()
    area.show_issue("start", Issue("Gone soon"))
    area.banner("start").close_btn.click()
    assert area.keys() == []
