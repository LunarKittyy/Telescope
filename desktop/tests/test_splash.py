import sys
import time
import types

from telescope import i18n, splash


class _Splash(types.ModuleType):
    def __init__(self):
        super().__init__("pyi_splash")
        self.texts = []
        self.closed = False

    def is_alive(self):
        return not self.closed

    def update_text(self, text):
        self.texts.append(text)

    def close(self):
        self.closed = True


def test_the_splash_text_is_in_the_chosen_language(monkeypatch):
    fake = _Splash()
    monkeypatch.setitem(sys.modules, "pyi_splash", fake)
    monkeypatch.delenv(i18n.ENV_LANGUAGE, raising=False)
    monkeypatch.setattr(splash, "_seconds_since_launch", lambda: 10.0)
    try:
        splash.show_text(lambda: "sv")
        assert fake.texts and "Telescope" in fake.texts[0]
        assert fake.texts[0] != "Starting Telescope…\nThis can take a minute while Windows runs a security scan."
    finally:
        i18n.load("en")
    splash.close()
    assert fake.closed


def test_without_the_packaged_build_nothing_happens(monkeypatch):
    monkeypatch.setitem(sys.modules, "pyi_splash", None)
    chosen = []
    splash.show_text(lambda: chosen.append(1) or "sv")
    splash.close()
    assert chosen == []


def test_the_line_about_the_scan_waits_until_the_start_is_slow(monkeypatch):
    fake = _Splash()
    monkeypatch.setitem(sys.modules, "pyi_splash", fake)
    monkeypatch.setattr(splash, "WHY_AFTER_S", 0.05)
    monkeypatch.setattr(splash, "_seconds_since_launch", lambda: 0.0)
    splash.show_text(lambda: "en")
    assert fake.texts == ["Starting Telescope…"]
    for _ in range(100):
        if len(fake.texts) == 2:
            break
        time.sleep(0.01)
    assert fake.texts[1] == "Starting Telescope…\nThis can take a minute while Windows runs a security scan."
