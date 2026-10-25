# Renders every banner and message box whose texts are written straight into the code, with sample values
import ast as _ast
import string as _string

from telescope.widgets.banner import BannerArea, Issue

SAMPLES = {"name": "Pixel 8", "phone": "Pixel 8", "n": 2, "fps": 30, "error": "Permission denied", "camera": "Telescope",
           "size": "1920 × 1080", "width": 1920, "height": 1080, "version": "3.2.0", "seconds": 5, "minutes": 2,
           "plugin": "Example plugin", "path": "~/Telescope", "command": "sudo modprobe v4l2loopback",
           "temp": 45, "mbps": 12, "keys": "Ctrl+Shift+T", "action": "Start streaming", "computer": "Desk"}


def _fill(fmt_names):
    return {k: SAMPLES.get(k, "…") for k in fmt_names}


def _literal_tr(node):
    """tr("literal", ...) or tr(N_) as a callable producing the translated text, or None."""
    if isinstance(node, _ast.Call) and getattr(node.func, "id", None) in ("tr", "trn") and node.args \
            and isinstance(node.args[0], _ast.Constant) and isinstance(node.args[0].value, str):
        ctx = next((k.value.value for k in node.keywords if k.arg == "ctx" and isinstance(k.value, _ast.Constant)), "")
        if node.func.id == "trn" and len(node.args) >= 2 and isinstance(node.args[1], _ast.Constant):
            s, p = node.args[0].value, node.args[1].value
            names = {f[1] for f in _string.Formatter().parse(s + p) if f[1]} - {"n"}
            return lambda: trn(s, p, 2, ctx=ctx, **_fill(names))
        s = node.args[0].value
        names = {f[1] for f in _string.Formatter().parse(s) if f[1]}
        return lambda: tr(s, ctx=ctx, **_fill(names))
    if isinstance(node, _ast.BinOp):  # implicit string joins are already constants; skip anything else
        return None
    return None


banners, boxes = [], []
for path in sorted((REPO / "desktop/telescope").rglob("*.py")):
    try:
        tree = _ast.parse(path.read_text(encoding="utf-8"))
    except SyntaxError:
        continue
    rel = path.relative_to(REPO / "desktop")
    for node in _ast.walk(tree):
        if not isinstance(node, _ast.Call):
            continue
        fname = getattr(node.func, "id", None) or getattr(node.func, "attr", None)
        if fname == "Issue" and node.args:
            title = _literal_tr(node.args[0])
            text = _literal_tr(node.args[1]) if len(node.args) > 1 else None
            kind = next((k.value.value for k in node.keywords if k.arg == "kind" and isinstance(k.value, _ast.Constant)), "err")
            if title or text:
                banners.append((f"{rel}:{node.lineno}", title, text, kind))
        elif isinstance(node.func, _ast.Attribute) and getattr(node.func.value, "id", None) == "QMessageBox" \
                and fname in ("warning", "information", "question", "critical") and len(node.args) >= 3:
            title, text = _literal_tr(node.args[1]), _literal_tr(node.args[2])
            if title or text:
                boxes.append((f"{rel}:{node.lineno}", fname, title, text))

print(f"harvested {len(banners)} banners, {len(boxes)} message boxes")


def _call(f, fallback):
    try:
        return f() if f else fallback
    except Exception:
        return fallback


host = QWidget()
host.setObjectName("central")
host.setStyleSheet("")
from PyQt6.QtWidgets import QVBoxLayout  # noqa: E402
for chunk in range(0, len(banners), 5):
    page = QWidget()
    lay = QVBoxLayout(page)
    area = BannerArea()
    lay.addWidget(area)
    where = []
    for i, (loc, title, text, kind) in enumerate(banners[chunk:chunk + 5]):
        area.show_issue(f"b{i}", Issue(_call(title, "[filled in by the app]"), _call(text, ""), kind=kind))
        where.append(loc)
    page.resize(900, 10)
    page.show()
    app.processEvents()
    page.resize(900, page.sizeHint().height())
    shoot(f"banners-{chunk // 5 + 1}", f"Message bars at the top of the main window ({chunk + 1}-{chunk + len(where)} of {len(banners)})",
          page, note="Values like the phone name or numbers are samples. Code: " + ", ".join(where))
    page.hide()

for i, (loc, kind, title, text) in enumerate(boxes):
    icon = {"warning": QMessageBox.Icon.Warning, "information": QMessageBox.Icon.Information,
            "question": QMessageBox.Icon.Question, "critical": QMessageBox.Icon.Critical}[kind]
    box = QMessageBox(icon, _call(title, "[filled in by the app]"), _call(text, "[filled in by the app]"), parent=win)
    box.addButton(QMessageBox.StandardButton.Ok)
    box.show()
    app.processEvents()
    shoot(f"messagebox-{i + 1}", f"Pop-up message ({kind})", box, note=f"Values are samples. Code: {loc}")
    box.hide()
