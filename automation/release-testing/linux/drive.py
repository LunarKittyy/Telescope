# Test driver: runs the bundle's main.py and executes snippets dropped in $DRIVE_DIR/in.py inside the app.
import contextlib, io, os, runpy, sys, traceback
APP = os.getcwd()
sys.path.insert(0, APP)
DRV = os.environ["DRIVE_DIR"]
from PyQt6.QtWidgets import QApplication, QAbstractButton, QWidget
from PyQt6.QtCore import QTimer, Qt, QPoint
from PyQt6.QtTest import QTest

def wins():
    return [w for w in QApplication.topLevelWidgets() if w.isVisible()]

def main_win():
    return next(w for w in QApplication.topLevelWidgets() if type(w).__name__ == "TelescopeWindow")

def shot(path, w=None):
    w = w or (QApplication.activeModalWidget() or QApplication.activeWindow() or main_win())
    w.grab().save(path)
    return f"{type(w).__name__} {w.windowTitle()!r} {w.width()}x{w.height()}"

def buttons(root=None, text=None):
    roots = [root] if root else wins()
    out = []
    for r in roots:
        for b in r.findChildren(QAbstractButton):
            if b.isVisibleTo(r) and (text is None or text.lower() in (b.text() or b.accessibleName() or "").lower()):
                out.append(b)
    return out

def click(text, root=None, n=0):
    b = buttons(root, text)[n]
    QTest.mouseClick(b, Qt.MouseButton.LeftButton)
    return f"clicked {b.text() or b.accessibleName()!r}"

G = dict(globals())

def poll():
    src = os.path.join(DRV, "in.py")
    if not os.path.exists(src):
        return
    code = open(src).read()
    os.remove(src)
    QTimer.singleShot(0, lambda: run(code))  # its own event, so a modal dialog it opens doesn't stop the polling


def run(code):
    buf = io.StringIO()
    with contextlib.redirect_stdout(buf):
        try:
            exec(code, G)
        except Exception:
            traceback.print_exc(file=buf)
    tmp = os.path.join(DRV, "out.tmp")
    open(tmp, "w").write(buf.getvalue())
    os.replace(tmp, os.path.join(DRV, "out.txt"))

_exec = QApplication.exec
_timer = []
def _patched(*_a):
    t = QTimer()
    t.timeout.connect(poll)
    t.start(150)
    _timer.append(t)
    return _exec()
QApplication.exec = staticmethod(_patched)
sys.argv = [os.path.join(APP, "main.py")] + sys.argv[1:]
runpy.run_path(os.path.join(APP, "main.py"), run_name="__main__")
