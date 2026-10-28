"""Every text fits where it's shown, in every language and the pseudo one (scripts/text_fit.py measures them)."""

import pytest
from PyQt6.QtCore import QPointF, QRect, Qt
from PyQt6.QtGui import QFontInfo, QPainter, QTextLayout
from PyQt6.QtWidgets import QFrame, QHBoxLayout, QLabel, QPushButton, QScrollArea, QSlider, QVBoxLayout, QWidget

from scripts import text_fit
from telescope import i18n
from telescope.widgets.common import SWITCH_W, ElidingLabel, Switch, ui_px, value_label

# The fonts text_fit_known.txt was made with: other fonts measure differently. Inter has no Devanagari, so Hindi's
# letters come from Noto Sans Devanagari
FONTS = {"Inter", "Noto Sans Devanagari"}
FIX = ("make it fit (a shorter translation, or room for a longer one), or if it has to stay, add the line to "
       "tests/fixtures/text_fit_known.txt. `python scripts/text_fit.py LANG --shots DIR` shows where it is")


def _fonts_drawing(lang: str) -> set:
    """The fonts the window draws lang's texts with on this machine."""
    label = QLabel()
    label.ensurePolished()
    font = label.font()
    main = QFontInfo(font).family()
    if main not in FONTS:
        return {main}  # without laying anything out: reading glyph runs crashed on Windows' runner
    entries, _header = i18n.read_po(i18n.LOCALE_DIR / ("telescope.pot" if lang in ("en", "pseudo") else f"{lang}.po"))
    texts = [e.msgstr[0] if e.msgstr and e.msgstr[0] else e.msgid for e in entries[:200]]
    layout = QTextLayout(" ".join(i18n.pseudo(t) if lang == "pseudo" else t for t in texts), font)
    layout.beginLayout()
    line = layout.createLine()
    line.setLineWidth(1_000_000)
    layout.endLayout()
    runs = layout.glyphRuns()
    fonts = [run.rawFont() for run in runs]
    return {main} | {raw.familyName() for raw in fonts}


@pytest.mark.parametrize("lang", text_fit.languages())
def test_every_text_fits(qapp, config_home, lang):
    other = _fonts_drawing(lang) - FONTS
    if other:
        pytest.skip(f"{lang} is drawn with {', '.join(sorted(other))} here, which measures differently from the known list")
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


class _Painted(QWidget):
    """Draws its text the way a widget that skips elided() would: at a point, or wrapped in a box."""

    def __init__(self, text, wrapped, parent):
        super().__init__(parent)
        self._text, self._wrapped = text, wrapped

    def paintEvent(self, _event):
        p = QPainter(self)
        if self._wrapped:
            p.drawText(QRect(0, 0, self.width(), self.height()), int(Qt.TextFlag.TextWordWrap), self._text)
        else:
            p.drawText(QPointF(0, 15), self._text)
        p.end()


@pytest.mark.parametrize("wrapped", [False, True])
def test_text_a_widget_draws_past_its_box_is_found(qt_widgets, english, wrapped):
    box = qt_widgets(QWidget())
    _Painted("Microphone" if wrapped else "Start streaming", wrapped, box).setGeometry(0, 0, 30, 40)
    found = text_fit.check(_shown(box, 200), "en", "test", english)
    assert [(f.kind, f.english) for f in found] == [("cut off", "Microphone" if wrapped else "Start streaming")]


def test_a_switch_wraps_a_long_name_and_cuts_a_word_too_long_for_it(qt_widgets, english):
    box = qt_widgets(QWidget())
    lay = QHBoxLayout(box)
    lay.setContentsMargins(0, 0, 0, 0)
    words, word = Switch("Stop the stream"), Switch("Microphone")
    lay.addWidget(words)
    lay.addWidget(word)
    words.ensurePolished()  # measured in the font it's painted with
    fm = words.fontMetrics()
    two_lines = max(fm.horizontalAdvance("Stop the"), fm.horizontalAdvance("stream")) + 4
    too_short = fm.horizontalAdvance("Microphone") - 10
    for switch, room in ((words, two_lines), (word, too_short)):
        switch.setFixedSize(room + 12 + ui_px(SWITCH_W), 3 * fm.height())  # 12 px gap, then the track
    found = text_fit.check(_shown(box, 400, 3 * fm.height()), "en", "test", english)
    assert [(f.kind, f.english) for f in found] == [("cut short", "Microphone")]


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


def _column(content_width):
    """A side column like the main window's: no sideways scrolling, its content content_width wide at least."""
    area = QScrollArea()
    area.setWidgetResizable(True)
    area.setHorizontalScrollBarPolicy(Qt.ScrollBarPolicy.ScrollBarAlwaysOff)
    content = QWidget()
    row = QHBoxLayout(content)
    wide = QPushButton("Add phone")
    wide.setMinimumWidth(content_width)
    row.addWidget(wide)
    area.setWidget(content)
    return area


def test_a_column_pushed_wider_than_it_shows_is_one_finding(qt_widgets, english):
    area = qt_widgets(_column(300))
    found = text_fit.check(_shown(area, 200, 100), "en", "test", english, cause="Curves")
    assert [(f.kind, f.english) for f in found] == [("column too wide", "Curves")]


def test_a_column_as_wide_before_the_pick_isnt_put_down_to_it(qt_widgets, english):
    area = qt_widgets(_column(300))
    _shown(area, 200, 100)
    before = {id(area): area.widget().width()}
    assert text_fit.check(area, "en", "test", english, cause="Curves", before=before) == []


def test_walking_a_card_finds_a_readout_only_one_slider_stop_shows(qt_widgets, english):
    box = qt_widgets(QWidget())
    card = QFrame(box)
    card.setObjectName("card")
    QVBoxLayout(box).addWidget(card)
    row = QHBoxLayout(card)
    slider, readout = QSlider(Qt.Orientation.Horizontal), value_label()
    slider.setRange(0, 3)
    slider.valueChanged.connect(lambda v: readout.setText("Start streaming to send the notes to the phone." if v == 3 else str(v)))
    row.addWidget(slider)
    row.addWidget(readout)
    _shown(box, 300, 80)
    assert text_fit.check(box, "en", "test", english) == []
    found = text_fit.walk(box, "en", "test", english, 300, 80)
    assert [(f.kind, f.english) for f in found] == [("cut off", "Start streaming to send the notes to the phone.")]
    assert slider.value() == 0  # put back as it was


def test_a_readout_column_widens_for_the_words_it_can_show(qapp):
    assert value_label().width() == ui_px(56)
    label = value_label(fits=["Start streaming to send the notes to the phone."])
    assert label.width() >= label.fontMetrics().horizontalAdvance("Start streaming to send the notes to the phone.")


def test_japanese_wraps_between_letters_so_no_sentence_counts_as_one_long_word(qapp):
    pieces = text_fit._unbreakable("スマートフォンでTelescopeを開いてください。")
    assert len(pieces) > 5 and "Telescope" in "".join(pieces)
    assert text_fit._unbreakable("Start the stream") == ["Start", "the", "stream"]


def test_one_known_line_covers_every_stop_of_a_readout():
    a = text_fit.Finding("pseudo", "test", "cut off", "[+1.3 ÉṼ ~]", "")
    b = text_fit.Finding("pseudo", "test", "cut off", "[-0.7 ÉṼ ~]", "")
    assert a.key.replace("+", "-") == b.key == "pseudo\tcut off\t[-#.# ÉṼ ~]"
