import sys
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
