# Upstream bugs

Bugs in Qt, PyQt, Android or other dependencies that Telescope works around. Every line of a workaround carries the bug ID in a comment, so `git grep <ID>` finds all of it even after the line numbers below drift.

When a release bumps the pinned version of a dependency listed here, check its entries: if the bug is fixed in that version, drop the workaround and the entry.

## Qt

### QTBUG-106583: new windows show as an empty frame on Windows

- **Bug:** [QTBUG-106583](https://qt-project.atlassian.net/browse/QTBUG-106583), open, reported against 5.15 to 6.11. A fix (fill with the system background colour on first draw) was merged and [reverted](https://codereview.qt-project.org/c/qt/qtbase/+/472965) for breaking translucent Qt Quick windows.
- **Symptom:** the HWND is shown before Qt's first paint reaches it, so DWM composites an empty frame for a frame or two. Seen on every `PlacedDialog` (Settings, Add phone, Your phones, the shortcut editor).
- **Workaround:** `PlacedDialog` sets `DWMWA_CLOAK` on the HWND before the first show and clears it after the first `QEvent.Paint` (250 ms fallback timer). Not `setWindowOpacity`: that makes the window layered and breaks DWM's close animation.
- **Code:** [`desktop/telescope/widgets/common.py`](../desktop/telescope/widgets/common.py)
  - [775-781](../desktop/telescope/widgets/common.py#L775-L781): `_cloak()`
  - [789](../desktop/telescope/widgets/common.py#L789): `PlacedDialog._cloaked`
  - [796-798](../desktop/telescope/widgets/common.py#L796-L798): cloak in `setVisible()`
  - [801-804](../desktop/telescope/widgets/common.py#L801-L804): `event()` override
  - [806-808](../desktop/telescope/widgets/common.py#L806-L808): `_drawn()`
- **Side effects:** cloaked windows skip Windows' open animation. Opening and closing Settings rapidly can still show the frame now and then.
- **Fixed when:** with the workaround removed, opening Settings and Your phones on Windows shows no empty frame.
