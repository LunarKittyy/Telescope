"""Keyboard shortcuts: the actions a key can run, and the bindings saved for them.

Plugins offer actions from create_actions(). Most wrap one of their controls (button_action, slider_action,
log_slider_action, choice_action), so a key does exactly what using that control does, and the control gets
"Add keyboard shortcut…" on a right-click. An action takes parameters (how much to step, which preset), so one
action can have several bindings: Exposure +1 and Exposure -1 are the same action with different amounts.

A binding is {"id", "keys", "action", "params", "global"}: keys in Qt's portable text ("Ctrl+Alt+M"), global
when it should also work while another app has focus. The id stays the same across restarts, so the system
remembers the key a user picked for it (Plasma's global shortcuts are keyed by it).
"""

import math
import secrets
from dataclasses import dataclass, field
from typing import Callable, Optional

from PyQt6.QtCore import QKeyCombination, Qt
from PyQt6.QtGui import QKeySequence
from PyQt6.QtWidgets import QAbstractButton, QAbstractSlider, QComboBox, QWidget

from telescope.platform import IS_WINDOWS
from telescope.i18n import N_, number, percent, tr, trn

_PORTABLE = QKeySequence.SequenceFormat.PortableText
_MODIFIER_KEYS = {Qt.Key.Key_Control, Qt.Key.Key_Shift, Qt.Key.Key_Alt, Qt.Key.Key_Meta, Qt.Key.Key_AltGr,
                  Qt.Key.Key_Super_L, Qt.Key.Key_Super_R, Qt.Key.Key_Hyper_L, Qt.Key.Key_Hyper_R}
_MODIFIERS = (Qt.KeyboardModifier.ControlModifier | Qt.KeyboardModifier.AltModifier
              | Qt.KeyboardModifier.ShiftModifier | Qt.KeyboardModifier.MetaModifier)
MAX_BINDINGS = 200
SHORTCUT_VALUE_ROLE = Qt.ItemDataRole.UserRole.value + 1  # stable choice value, independent of its displayed text


@dataclass(frozen=True)
class Param:
    """One setting of an action, shown in the shortcut editor.

    kind "choice": choices is [(value, label)] or a callable giving them (a preset's names change).
    kind "number": a float between minimum and maximum (either can be a callable, for a range that moves).
    shown_if: (other param's name, values) shows this one only while that param has one of the values.
    """
    name: str
    label: str
    kind: str
    default: object = None
    choices: object = ()
    minimum: object = 0.0
    maximum: object = 100.0
    decimals: int = 0
    suffix: str = ""
    shown_if: Optional[tuple] = None

    def options(self) -> list:
        raw = self.choices() if callable(self.choices) else self.choices
        return [(v, str(label)) for v, label in raw]

    def bounds(self) -> tuple:
        lo = self.minimum() if callable(self.minimum) else self.minimum
        hi = self.maximum() if callable(self.maximum) else self.maximum
        return float(lo), float(max(lo, hi))

    def shown(self, params: dict) -> bool:
        if self.shown_if is None:
            return True
        other, values = self.shown_if
        return params.get(other) in values

    def clean(self, value):
        """value as this param takes it, or the default when it can't be."""
        if self.kind == "number":
            if isinstance(value, str):  # from telescope --action
                try:
                    value = float(value)
                except ValueError:
                    return self.default
            if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value):
                return self.default
            lo, hi = self.bounds()
            return round(min(max(float(value), lo), hi), self.decimals)
        if self.kind == "choice":
            if value in [v for v, _ in self.options()]:
                return value
            # A choice that's gone from a list that changes (a deleted preset) stays, so the binding still says
            # what it was for; it just does nothing until there's one by that name again
            if callable(self.choices) and isinstance(value, str) and value:
                return value
            return self.default
        return self.default


@dataclass
class ShortcutAction:
    """Something a key can do. run(params) does it and returns what to say about it ("Microphone: muted"), or
    None when it can't right now (no phone, the control is hidden or greyed out). release(params) runs when a
    key held for it comes back up (for "while held"); repeats(params) says whether holding the key down repeats
    it (a step does, a toggle doesn't)."""
    id: str
    label: str
    group: str
    run: Callable[[dict], Optional[str]]
    params: tuple = ()
    release: Optional[Callable[[dict], None]] = None
    repeats: Callable[[dict], bool] = field(default=lambda _p: False)
    describe: Optional[Callable[[dict], str]] = None
    widgets: object = ()
    """The controls it stands for, which get "Add keyboard shortcut…" on a right-click (or a callable giving
    them, for controls that come and go, like the lens buttons)."""
    current: Optional[Callable[[], float]] = None
    """Where the control is now, which a new "Set to" binding starts at."""
    keywords: str = ""
    """More words the shortcut editor's search finds it by (the card section it's under, what people call it)."""

    def controls(self) -> tuple:
        return tuple(self.widgets() if callable(self.widgets) else self.widgets)

    def full_params(self, params: Optional[dict]) -> dict:
        """params with every one of this action's settings filled in and cleaned."""
        params = params if isinstance(params, dict) else {}
        return {p.name: p.clean(params.get(p.name, p.default)) for p in self.params}

    def summary(self, params: Optional[dict]) -> str:
        """What a binding with these params does, for the list: "Compensation: increase by 1 step"."""
        params = self.full_params(params)
        if self.describe is not None:
            return self.describe(params)
        return self.label


# ── Keys ─────────────────────────────────────────────────────────────────────

def normalize_keys(text) -> str:
    """One key with its modifiers, in portable text ("Ctrl+Alt+M"); "" for anything else (two chords, a bare
    modifier, nonsense)."""
    if not isinstance(text, str) or not text.strip():
        return ""
    seq = QKeySequence.fromString(text, _PORTABLE)
    if seq.count() != 1:
        return ""
    combo = seq[0]
    if combo.key() in _MODIFIER_KEYS or combo.key() == Qt.Key.Key_unknown:
        return ""
    return QKeySequence(combo).toString(_PORTABLE)


def keys_from_event(key: int, modifiers) -> str:
    """The portable text of a key event, in the form normalize_keys gives, or "" for a bare modifier."""
    try:
        qkey = Qt.Key(key)
    except ValueError:
        return ""
    if qkey in _MODIFIER_KEYS or qkey == Qt.Key.Key_unknown:
        return ""
    mods = Qt.KeyboardModifier(modifiers) & _MODIFIERS
    if qkey == Qt.Key.Key_Backtab:  # Qt reports Shift+Tab as its own key
        qkey, mods = Qt.Key.Key_Tab, mods | Qt.KeyboardModifier.ShiftModifier
    return QKeySequence(QKeyCombination(mods, qkey)).toString(_PORTABLE)


def split_keys(keys: str) -> tuple:
    """(modifier names, key name) of normalized keys: ("Ctrl+Alt", "M")."""
    combo = QKeySequence.fromString(keys, _PORTABLE)[0]
    key = QKeySequence(QKeyCombination(combo.key())).toString(_PORTABLE)
    mods = []
    m = combo.keyboardModifiers()
    for flag, name in ((Qt.KeyboardModifier.ControlModifier, "Ctrl"), (Qt.KeyboardModifier.AltModifier, "Alt"),
                       (Qt.KeyboardModifier.ShiftModifier, "Shift"), (Qt.KeyboardModifier.MetaModifier, "Meta")):
        if m & flag:
            mods.append(name)
    return mods, key, combo.key()


META_NAME = "Win" if IS_WINDOWS else "Meta"
"""What this system calls the Windows/Super key."""


def native_keys(keys: str) -> str:
    """keys as this system writes them (Ctrl on Windows and Linux; the Windows/Super key as META_NAME)."""
    text = QKeySequence.fromString(keys, _PORTABLE).toString(QKeySequence.SequenceFormat.NativeText)
    return text.replace("Meta+", f"{META_NAME}+") if META_NAME != "Meta" else text


def has_command_modifier(keys: str) -> bool:
    """Whether keys holds Ctrl, Alt or Meta, so typing into a text box can't set it off by accident."""
    mods, _, _ = split_keys(keys)
    return any(m in ("Ctrl", "Alt", "Meta") for m in mods)


# ── Bindings ─────────────────────────────────────────────────────────────────

def new_binding_id() -> str:
    return "b" + secrets.token_hex(5)


def _clean_params(raw) -> dict:
    out = {}
    for k, v in (raw.items() if isinstance(raw, dict) else ()):
        if isinstance(k, str) and len(k) <= 40 and (
                isinstance(v, (bool, int)) or (isinstance(v, float) and math.isfinite(v))
                or (isinstance(v, str) and len(v) <= 200)):
            out[k] = v
    return out


def clean_bindings(raw) -> list:
    """Keep well-formed bindings in order: real keys, an action id, a unique id (a missing or repeated one gets
    a new one, so the system's memory of it can't cross over to another binding)."""
    out, ids = [], set()
    for b in raw if isinstance(raw, list) else []:
        if not isinstance(b, dict) or len(out) >= MAX_BINDINGS:
            continue
        keys = normalize_keys(b.get("keys"))
        action = b.get("action")
        if not keys or not isinstance(action, str) or not action or len(action) > 80:
            continue
        bid = b.get("id")
        if not isinstance(bid, str) or not bid.isalnum() or len(bid) > 24 or bid in ids:
            bid = new_binding_id()
        ids.add(bid)
        out.append({"id": bid, "keys": keys, "action": action, "params": _clean_params(b.get("params")),
                    "global": b.get("global") is True})
    return out


def clashes(bindings: list) -> set:
    """Ids of the bindings whose keys an earlier binding already uses (they don't do anything)."""
    seen, out = set(), set()
    for b in bindings:
        if b["keys"] in seen:
            out.add(b["id"])
        seen.add(b["keys"])
    return out


# ── Actions made from controls ───────────────────────────────────────────────

def control_ready(widget: Optional[QWidget]) -> bool:
    """Enabled and available in its mode. Folding a card or closing its window doesn't disable its shortcuts."""
    if widget is None:
        return False
    try:
        if not widget.isEnabled():
            return False
        node, window = widget, widget.window()
        while node is not None and node is not window:
            # Collapsing hides a container, leaving the controls' own mode visibility alone.
            if node.isHidden() and node.objectName() != "card_body":
                return False
            node = node.parentWidget()
        return True
    except RuntimeError:  # the control was deleted
        return False


def _fmt(value: float, decimals: int, suffix: str, sign: bool = False) -> str:
    if suffix == "%":
        return percent(value, decimals, sign)
    return number(value, decimals, sign) + suffix


_TOGGLE_MODES = (("toggle", N_("Switch on or off")), ("on", N_("Turn on")), ("off", N_("Turn off")),
                 ("hold", N_("Switch while held, back on release")))


def button_action(id: str, label: str, group: str, button: QAbstractButton,
                  states: tuple = ("on", "off"), verbs: Optional[dict] = None) -> ShortcutAction:
    """A key that clicks button. A checkable one can toggle it, turn it on or off, or flip it while the key is
    held (push to talk on a Mute button). states: what checked and unchecked are called ("muted", "unmuted");
    verbs: labels for the modes ({"on": "Mute", ...}) in place of the generic ones."""
    if not button.isCheckable():
        def run_click(_p):
            if not control_ready(button):
                return None
            button.click()
            return label
        return ShortcutAction(id, label, group, run_click, widgets=(button,))

    held = {}
    modes = tuple((m, (verbs or {}).get(m) or tr(text)) for m, text in _TOGGLE_MODES)

    def state_text() -> str:
        return tr("{label}: {value}", label=label, value=states[0] if button.isChecked() else states[1])

    def run(p):
        if not control_ready(button):
            return None
        mode = p.get("mode", "toggle")
        want = {"on": True, "off": False}.get(mode, not button.isChecked())
        if mode == "hold":  # the opposite of where it was when the key went down, however often that repeats
            want = not held.setdefault("from", button.isChecked())
        if want != button.isChecked():
            button.click()
        return state_text()

    def release(p):
        if p.get("mode") != "hold" or "from" not in held:
            return
        before = held.pop("from")
        if button.isChecked() != before and control_ready(button):
            button.click()

    def describe(p):
        text = dict(modes)[p["mode"]]
        return text if verbs and p["mode"] in verbs else tr("{label}: {value}", label=label, value=text.lower())

    return ShortcutAction(id, label, group, run, params=(Param("mode", tr("Does"), "choice", "toggle", modes),),
                          release=release, describe=describe, widgets=(button,))


def slider_action(id: str, label: str, group: str, slider: QAbstractSlider, scale=1.0, suffix: str = "",
                  decimals: int = 0, step=None, readout=None, signed: bool = False,
                  widgets: tuple = (), ready_widget: Optional[QWidget] = None) -> ShortcutAction:
    """A key that moves slider: up or down by an amount, to a value, or back to its default. Amounts are in what
    the control shows: the slider's own steps times scale (scale can be a callable, for one that changes).
    readout() (or a QLabel) gives the text to show after; step is the amount a new binding starts with.
    ready_widget can be the visible replacement for a hidden slider; that replacement gates the action."""
    def sc() -> float:
        return float(scale() if callable(scale) else scale) or 1.0

    def lo() -> float:
        return slider.minimum() * sc()

    def hi() -> float:
        return slider.maximum() * sc()

    default_amount = step if step is not None else max(slider.singleStep(), 1) * sc()
    ops = [("up", tr("Increase by")), ("down", tr("Decrease by")), ("set", tr("Set to"))]
    has_default = getattr(slider, "default", None) is not None and slider.default() is not None
    if has_default:
        ops.append(("reset", tr("Reset to default")))
    params = (
        Param("op", tr("Does"), "choice", "up", tuple(ops)),
        Param("amount", tr("By"), "number", default_amount, minimum=lambda: 0.0, maximum=lambda: hi() - lo(),
              decimals=decimals, suffix=suffix, shown_if=("op", ("up", "down"))),
        Param("value", tr("Value"), "number", None, minimum=lo, maximum=hi, decimals=decimals,
              suffix=suffix, shown_if=("op", ("set",))),
    )

    def show_value() -> str:
        if readout is not None:
            text = readout() if callable(readout) else readout.text()
            return tr("{label}: {value}", label=label, value=text)
        return tr("{label}: {value}", label=label, value=_fmt(slider.value() * sc(), decimals, suffix, signed))

    def run(p):
        try:
            ready = control_ready(ready_widget if ready_widget is not None else slider) and slider.isEnabled()
        except RuntimeError:  # a replaced control can disappear before its shortcut is removed
            ready = False
        if not ready:
            return None
        op, cur = p.get("op", "up"), slider.value()
        if op == "set":
            value = p.get("value")
            if value is None:
                return None
            new = round(float(value) / sc())
        elif op == "reset":
            new = slider.default() if has_default else cur
        else:
            steps = max(1, round(float(p.get("amount") or 0) / sc())) if p.get("amount") else 0
            new = cur + (steps if op == "up" else -steps)
        slider.setValue(int(min(max(new, slider.minimum()), slider.maximum())))
        return show_value()

    def describe(p):
        op = p["op"]
        if op == "set":
            v = p.get("value")
            return tr("{label}: set to {value}", label=label,
                      value=_fmt(v, decimals, suffix, signed) if v is not None else "…")
        if op == "reset":
            return tr("{label}: reset to default", label=label)
        amount = _fmt(p.get("amount") or 0, decimals, suffix)
        return (tr("{label}: increase by {amount}", label=label, amount=amount) if op == "up"
                else tr("{label}: decrease by {amount}", label=label, amount=amount))

    return ShortcutAction(id, label, group, run, params=params, describe=describe,
                          repeats=lambda p: p.get("op") in ("up", "down"), widgets=widgets or (slider,),
                          current=lambda: round(slider.value() * sc(), decimals))


def log_slider_action(id: str, label: str, group: str, row, display_scale: float = 1.0, suffix: str = "",
                      decimals: int = 0) -> ShortcutAction:
    """A key for a LogSliderRow (ISO, shutter): up or down in stops (each one doubles or halves it), or to a
    value. display_scale turns the row's value into what its box shows (nanoseconds to milliseconds)."""
    params = (
        Param("op", tr("Does"), "choice", "up", (("up", tr("Raise by")), ("down", tr("Lower by")),
                                                ("set", tr("Set to")))),
        # Translators: the unit after a number of stops, with its space: "1.5 stops"
        Param("stops", tr("Stops"), "number", 1.0, minimum=0.1, maximum=10.0, decimals=1, suffix=tr(" stops"),
              shown_if=("op", ("up", "down"))),
        Param("value", tr("Value"), "number", None, minimum=lambda: row.v_min * display_scale,
              maximum=lambda: row.v_max * display_scale, decimals=decimals, suffix=suffix,
              shown_if=("op", ("set",))),
    )

    def run(p):
        if not row.is_enabled() or not control_ready(row):
            return None
        op = p.get("op", "up")
        if op == "set":
            if p.get("value") is None:
                return None
            new = float(p["value"]) / display_scale
        else:
            stops = float(p.get("stops") or 0)
            new = row.get_value() * (2 ** (stops if op == "up" else -stops))
        new = min(max(new, row.v_min), row.v_max)
        row.set_value(new)
        row.value_changed.emit(new)
        return tr("{label}: {value}", label=label, value=row.display_text())

    def describe(p):
        if p["op"] == "set":
            v = p.get("value")
            return tr("{label}: set to {value}", label=label, value=_fmt(v, decimals, suffix) if v is not None else "…")
        stops = p.get("stops") or 0
        n = 1 if stops == 1 else 2
        amount = number(stops, 0 if float(stops).is_integer() else 1)
        return (trn("{label}: raise by {stops} stop", "{label}: raise by {stops} stops", n, label=label, stops=amount)
                if p["op"] == "up" else
                trn("{label}: lower by {stops} stop", "{label}: lower by {stops} stops", n, label=label, stops=amount))

    return ShortcutAction(id, label, group, run, params=params, describe=describe,
                          repeats=lambda p: p.get("op") in ("up", "down"), widgets=(row,),
                          current=lambda: round(row.get_value() * display_scale, decimals))


def choice_action(id: str, label: str, group: str, combo: Optional[QComboBox] = None, buttons=None,
                  wrap: bool = True) -> ShortcutAction:
    """A key that picks in a drop-down (combo) or a row of buttons only one of which is on (buttons: a list, or a
    callable for one that changes, like the lens buttons): the next one, the previous one, or a named one.
    A button's shortcutValue property or a combo item's SHORTCUT_VALUE_ROLE preserves saved choices when its text changes:
    a translated text needs one, in English, or a binding saved in one language misses in another."""
    def options() -> list:
        """[(text, enabled)] in order."""
        if combo is not None:
            model = combo.model()
            out = []
            for i in range(combo.count()):
                item = model.index(i, 0)
                value = combo.itemData(i, SHORTCUT_VALUE_ROLE)
                if value is None:
                    value = combo.itemText(i)
                out.append((value, bool(model.flags(item) & Qt.ItemFlag.ItemIsEnabled)))
            return out
        return [(button_value(b), b.isEnabled()) for b in current_buttons()]

    def button_value(button) -> str:
        return button.property("shortcutValue") or button.text().strip()

    def current_buttons() -> list:
        return list(buttons() if callable(buttons) else buttons or [])

    def shown(i: int) -> str:
        return combo.itemText(i) if combo is not None else current_buttons()[i].text().strip()

    def current_index() -> int:
        if combo is not None:
            return combo.currentIndex()
        return next((i for i, b in enumerate(current_buttons()) if b.isChecked()), -1)

    def ready() -> bool:
        if combo is not None:
            return control_ready(combo)
        return any(control_ready(b) for b in current_buttons())

    def pick(i: int):
        if combo is not None:
            if combo.currentIndex() != i:
                combo.setCurrentIndex(i)
                combo.activated.emit(i)  # what a pick by hand sends; some cards only listen for that
        else:
            btn = current_buttons()[i]
            if not btn.isChecked():
                btn.click()

    def run(p):
        if not ready():
            return None
        opts = options()
        if not opts:
            return None
        op, cur = p.get("op", "next"), current_index()
        if op == "set":
            target = next((i for i, (value, on) in enumerate(opts) if on and p.get("option") in (value, shown(i))), None)
            if target is None:
                return None
        else:
            step = 1 if op == "next" else -1
            target, i = None, cur
            for _ in range(len(opts)):
                i += step
                if not wrap and not 0 <= i < len(opts):
                    break
                i %= len(opts)
                if opts[i][1] and i != cur:
                    target = i
                    break
            if target is None:
                return tr("{label}: {value}", label=label, value=shown(cur)) if 0 <= cur < len(opts) else None
        pick(target)
        return tr("{label}: {value}", label=label, value=shown(target))

    def option_choices():
        return [(value, shown(i)) for i, (value, _on) in enumerate(options())]

    params = (
        Param("op", tr("Does"), "choice", "next", (("next", tr("Next")), ("previous", tr("Previous")),
                                                  ("set", tr("Pick")))),
        Param("option", tr("Option"), "choice", "", option_choices, shown_if=("op", ("set",))),
    )

    def describe(p):
        if p["op"] == "set":
            option = p.get("option")
            return tr("{label}: {value}", label=label, value=dict(option_choices()).get(option, option) or "…")
        return tr("{label}: {value}", label=label, value=tr("next") if p["op"] == "next" else tr("previous"))

    return ShortcutAction(id, label, group, run, params=params, describe=describe,
                          repeats=lambda p: p.get("op") in ("next", "previous"),
                          widgets=(combo,) if combo is not None else current_buttons)
