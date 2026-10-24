"""Every text fits where it's shown, in every language and the pseudo one (scripts/text_fit.py measures them)."""

import pytest
from PyQt6.QtGui import QFontInfo
from PyQt6.QtWidgets import QHBoxLayout, QLabel, QPushButton, QWidget

from scripts import text_fit
from telescope import i18n
from telescope.widgets.common import ElidingLabel

FONT = "Inter"  # what text_fit_known.txt was made with: other fonts measure differently
FIX = ("make it fit (a shorter translation, or room for a longer one), or if it has to stay, add the line to "
       "tests/fixtures/text_fit_known.txt. `python scripts/text_fit.py LANG --shots DIR` shows where it is")


def _font_in_use() -> str:
    label = QLabel("x")
    label.ensurePolished()
    return QFontInfo(label.font()).family()


@pytest.mark.parametrize("lang", text_fit.languages())
def test_every_text_fits(qapp, config_home, lang):
    if _font_in_use() != FONT:
        pytest.skip(f"the window isn't in {FONT} here, so its texts measure differently from the known list")
    found = text_fit.run(lang)
    known = text_fit.known()
    new = [f for f in found if f.key not in known]
    assert not new, "\n".join(["These don't fit:", *text_fit.report(new, set()), FIX])
    gone = sorted(k for k in known if k.startswith(f"{lang}\t") and k not in {f.key for f in found})
    assert not gone, "Fixed now, take them out of tests/fixtures/text_fit_known.txt:\n" + "\n".join(gone)


# ── What counts as not fitting ───────────────────────────────────────────────

@pytest.fixture
def english(qapp):
    i18n.load("en")
    return text_fit._Texts()


def _shown(widget, width, height=60):
    widget.resize(width, height)
    widget.show()
    widget.grab()
    return widget


def test_a_label_too_narrow_for_its_text_is_cut_off(qt_widgets, english):
    box = qt_widgets(QWidget())
    label = QLabel("Start streaming", box)
    label.setGeometry(0, 0, 20, 30)
    found = text_fit.check(_shown(box, 200), "en", "test", english)
    assert [(f.kind, f.english) for f in found] == [("cut off", "Start streaming")]
    assert found[0].room == 20 and found[0].need > 20


def test_a_button_pushed_out_of_its_box_is_found(qt_widgets, english):
    box = qt_widgets(QWidget())
    inner = QWidget(box)
    inner.setGeometry(0, 0, 100, 40)
    button = QPushButton("Add phone", inner)
    button.setGeometry(60, 0, button.sizeHint().width(), 30)
    found = text_fit.check(_shown(box, 300), "en", "test", english)
    assert [(f.kind, f.english) for f in found] == [("out of its box", "Add phone")]


def test_an_app_text_cut_short_is_found_but_a_name_is_cut_on_purpose(qt_widgets, english):
    box = qt_widgets(QWidget())
    lay = QHBoxLayout(box)
    lay.addWidget(ElidingLabel("Click Start streaming in the top right corner of this window."))
    lay.addWidget(ElidingLabel("Somebody's very long phone name that goes on and on"))
    found = text_fit.check(_shown(box, 160), "en", "test", english)
    assert [(f.kind, f.english) for f in found] == [
        ("cut short", "Click Start streaming in the top right corner of this window.")]


def test_text_that_fits_is_left_alone(qt_widgets, english):
    box = qt_widgets(QWidget())
    QHBoxLayout(box).addWidget(QPushButton("Start streaming"))
    assert text_fit.check(_shown(box, 400), "en", "test", english) == []


def test_english_left_in_the_pseudo_language_is_found():
    assert text_fit._untranslated("30 fps")
    assert not text_fit._untranslated(i18n.pseudo("Start streaming"))
    assert not text_fit._untranslated(i18n.pseudo("Paired with {name}").format(name=i18n.pseudo("Phone")))
    assert not text_fit._untranslated("Telescope")


def test_a_finding_matches_its_known_line_wherever_it_shows():
    a = text_fit.Finding("sv", "main window 560 px", "cut off", "Starta", "Start streaming", 90, 80)
    b = text_fit.Finding("sv", "Settings > General", "cut off", "Starta", "Start streaming", 95, 70)
    assert a.key == b.key == "sv\tcut off\tStart streaming"
