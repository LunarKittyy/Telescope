#!/usr/bin/env python3
"""Writes the phone's four looks from the desktop's theme.slint, so the two can't drift.

    python3 tools/gen_look.py          write the two generated files
    python3 tools/gen_look.py --check  fail if they differ from what theme.slint says (the unit test and CI run this)

Reads every `out property <color> name: expr;` in the Theme global (plus the outline widths) and evaluates `expr` once per look,
in the order theme.slint's pick() takes them: dark, classic, oled, light. Each token becomes a theme attribute (`ts` + the
name in camel case: line-strong is ?attr/tsLineStrong) and each look a theme (Theme.Telescope.Dark and so on) that sets them.
Then there are a few tokens only the phone has (PHONE below), written in the same expression language.
"""
import colorsys
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SLINT = ROOT.parent / "rust/crates/ui-kit/ui/theme.slint"
RES = ROOT / "app/src/main/res/values"
LOOKS = ["Dark", "Classic", "Oled", "Light"]
# Looks whose ground is light: dark status bar icons (the navigation bar's follow in Look.kt, which needs API 27)
LIGHT_LOOKS = {3}

# Tokens that are not colours but that a drawable needs, by the name theme.slint gives them: dimensions (px on the desktop, dp here)
LENGTHS = ["outline-w", "outline-w-over", "card-outline-w", "pop-outline-w", "stop-edge-w"]
# Tokens theme.slint has no use for, kept out of the phone's theme
SKIP = {"mono", "mono-size", "font"}

# Only on the phone, written like theme.slint's own tokens (they may use any token above). Each is "colour" or "length"
PHONE = [
    # Chips over the camera preview: the chrome colour, a little see-through, with a faint edge
    ("overlay", "colour", "chrome.mix(transparent, 0.702)"),
    ("overlay-edge", "colour", "pick(#ffffff33, #ffffff33, #ffffff33, #00000033)"),
]


class Col:
    def __init__(self, r, g, b, a=1.0):
        self.v = (r, g, b, a)

    def mix(self, other, f):
        f = max(0.0, min(1.0, f))
        a1, a2 = self.v[3], other.v[3]
        # Slint's mix weighs the two by their alpha as well (the sass rule); for opaque colours it is a plain blend
        nw, ad = 2 * f - 1, a1 - a2
        w = nw if nw * ad == -1 else (nw + ad) / (1 + nw * ad)
        w1 = (w + 1) / 2
        w2 = 1 - w1
        rgb = [(self.v[i] * w1 + other.v[i] * w2) for i in range(3)]
        return Col(*[max(0.0, min(1.0, c)) for c in rgb], max(0.0, min(1.0, a1 * f + a2 * (1 - f))))

    def hsv_scale(self, k):
        h, s, v = colorsys.rgb_to_hsv(*self.v[:3])
        return Col(*colorsys.hsv_to_rgb(h, s, min(1.0, v * k)), self.v[3])

    def brighter(self, f):
        return self.hsv_scale(1 + f)

    def darker(self, f):
        return self.hsv_scale(1 / (1 + f))

    def xml(self):
        r, g, b, a = (int(round(c * 255)) for c in self.v)
        return f"#{r:02x}{g:02x}{b:02x}" if a == 255 else f"#{a:02x}{r:02x}{g:02x}{b:02x}"


def hexcol(s):
    s = s.lstrip("#")
    if len(s) == 3:
        s = "".join(c * 2 for c in s)
    r, g, b = (int(s[i:i + 2], 16) / 255 for i in (0, 2, 4))
    return Col(r, g, b, int(s[6:8], 16) / 255 if len(s) == 8 else 1.0)


TOKEN = re.compile(r"\s*(#[0-9a-fA-F]+|[0-9.]+(?:phx|px)?|==|!=|[A-Za-z_][A-Za-z0-9_-]*|\"[^\"]*\"|[()?:,.!/*+-])")


def tokenize(text):
    out, pos = [], 0
    while pos < len(text.rstrip()):
        m = TOKEN.match(text, pos)
        if not m:
            raise ValueError(f"cannot read {text[pos:]!r}")
        out.append(m.group(1))
        pos = m.end()
    return out


class Eval:
    def __init__(self, tokens, look, env, defs):
        self.t, self.i, self.look, self.env, self.defs = tokens, 0, look, env, defs

    def peek(self):
        return self.t[self.i] if self.i < len(self.t) else None

    def take(self, want=None):
        tok = self.peek()
        if want is not None and tok != want:
            raise ValueError(f"expected {want} but found {tok} in {' '.join(self.t)}")
        self.i += 1
        return tok

    def expr(self):
        cond = self.compare()
        if self.peek() == "?":
            self.take()
            a = self.expr()
            self.take(":")
            b = self.expr()
            return a if cond else b
        return cond

    def compare(self):
        left = self.sum()
        if self.peek() in ("==", "!="):
            op = self.take()
            right = self.sum()
            return (left == right) == (op == "==")
        return left

    def sum(self):
        v = self.term()
        while self.peek() in ("+", "-"):
            op = self.take()
            r = self.term()
            v = v + r if op == "+" else v - r
        return v

    def term(self):
        v = self.unary()
        while self.peek() in ("*", "/"):
            op = self.take()
            r = self.unary()
            v = v * r if op == "*" else v / r
        return v

    def unary(self):
        if self.peek() == "!":
            self.take()
            return not self.unary()
        return self.postfix()

    def postfix(self):
        v = self.atom()
        while self.peek() == ".":
            self.take()
            name = self.take()
            self.take("(")
            args = self.args()
            if name == "mix":
                v = v.mix(args[0], float(args[1]))
            elif name in ("brighter", "darker"):
                v = getattr(v, name)(float(args[0]))
            else:
                raise ValueError(f"unknown method {name}")
        return v

    def args(self):
        out = []
        if self.peek() != ")":
            out.append(self.expr())
            while self.peek() == ",":
                self.take()
                out.append(self.expr())
        self.take(")")
        return out

    def atom(self):
        tok = self.take()
        if tok == "(":
            v = self.expr()
            self.take(")")
            return v
        if tok.startswith("#"):
            return hexcol(tok)
        if tok[0].isdigit():
            return float(tok.rstrip("phx"))
        if tok.startswith('"'):
            return tok.strip('"')
        if tok == "transparent":
            return Col(0, 0, 0, 0)
        if tok == "black":
            return Col(0, 0, 0)
        if tok == "white":
            return Col(1, 1, 1)
        if tok == "pick" and self.peek() == "(":
            self.take()
            return self.args()[self.look]
        if tok == "look":
            return self.look
        if tok in ("classic", "oled", "light"):
            return self.look == {"classic": 1, "oled": 2, "light": 3}[tok]
        if tok in self.env:
            return self.env[tok]
        if tok in self.defs:
            self.env[tok] = evaluate(self.defs[tok], self.look, self.env, self.defs)
            return self.env[tok]
        raise ValueError(f"unknown name {tok}")


def evaluate(text, look, env, defs):
    ev = Eval(tokenize(text), look, env, defs)
    v = ev.expr()
    if ev.peek() is not None:
        raise ValueError(f"left over {ev.t[ev.i:]} in {text}")
    return v


def read_theme(src):
    body = src[src.index("export global Theme"):]
    props = []
    for m in re.finditer(r"out property <(color|length)> ([a-z0-9-]+):\s*(.*?);\n", body, re.S):
        kind, name, expr = m.groups()
        props.append((kind, name, " ".join(expr.split())))
    consts = {m.group(1): m.group(2) for m in re.finditer(r"in-out property <length> (unit|radius|body): ([0-9.]+)px", body)}
    hues = {}
    func = re.search(r"function plugin-hue\(.*?\n    }\n", body, re.S).group(0)
    for m in re.finditer(r'group == "([a-z_]+)" \? (pick\([^)]*\))', func):
        hues[m.group(1)] = m.group(2)
    return props, consts, hues


def camel(name):
    return "ts" + "".join(p.capitalize() for p in name.split("-"))


def generate():
    props, consts, hues = read_theme(SLINT.read_text())
    unit = float(consts["unit"])
    # Tokens that are only another token's old name are left out
    colours = [(n, e) for k, n, e in props if k == "color" and n not in SKIP and not re.fullmatch(r"[a-z0-9-]+", e) and not e.startswith('"')]
    lengths = [(n, e) for k, n, e in props if k == "length" and n in LENGTHS]
    # unit and stroke are the only sizes the lengths above rest on
    base = {"unit": unit, "stroke": unit}
    values = []  # per look: name -> (kind, text)
    defs = {n: e for n, e in colours + lengths}
    for group, expr in hues.items():
        defs["hue-" + group.replace("_", "-")] = expr
    for name, _, expr in PHONE:
        defs[name] = expr
    for look in range(4):
        env = dict(base)
        out = {}
        for name, expr in colours:
            out[name] = ("color", evaluate(name, look, env, defs).xml())
        for group in hues:
            name = "hue-" + group.replace("_", "-")
            out[name] = ("color", evaluate(name, look, env, defs).xml())
        for name, _ in lengths:
            out[name] = ("dimension", f"{evaluate(name, look, env, defs):g}dp")
        for name, kind, expr in PHONE:
            v = evaluate(name, look, env, defs)
            out[name] = ("color", v.xml()) if kind == "colour" else ("dimension", f"{v:g}dp")
        out["scrim-a"] = ("float", f"{evaluate('look == 3 ? 0.3 : 0.5', look, env, defs):g}")
        values.append(out)
    names = list(values[0])
    for v in values:
        assert list(v) == names
    head = "<!-- Generated by android/tools/gen_look.py from rust/crates/ui-kit/ui/theme.slint. Do not edit; change theme.slint and run the script. -->"
    attrs = ['<?xml version="1.0" encoding="utf-8"?>', head, "<resources>"]
    for n in names:
        attrs.append(f'    <attr name="{camel(n)}" format="{values[0][n][0]}" />')
    attrs.append("</resources>")
    themes = ['<?xml version="1.0" encoding="utf-8"?>', head, "<resources>"]
    for i, look in enumerate(LOOKS):
        themes.append(f'    <style name="Theme.Telescope.{look}" parent="Theme.Telescope.Base">')
        for n in names:
            themes.append(f'        <item name="{camel(n)}">{values[i][n][1]}</item>')
        light = "true" if i in LIGHT_LOOKS else "false"
        themes.append(f'        <item name="android:windowLightStatusBar">{light}</item>')
        themes.append("    </style>")
    themes.append("</resources>")
    return {"look_attrs.xml": "\n".join(attrs) + "\n", "look_themes.xml": "\n".join(themes) + "\n"}


def main():
    files = generate()
    if "--check" in sys.argv:
        stale = [n for n, text in files.items() if not (RES / n).exists() or (RES / n).read_text() != text]
        if stale:
            print(f"{', '.join(stale)} differ from theme.slint: run python3 android/tools/gen_look.py", file=sys.stderr)
            return 1
        return 0
    for n, text in files.items():
        (RES / n).write_text(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
