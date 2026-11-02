# Upstream bugs

Bugs in Android, adb, winit or other dependencies that Telescope works around. Every line of a workaround carries the bug ID in a comment, so `git grep <ID>` finds all of it even after the line numbers below drift.

When a release bumps the pinned version of a dependency listed here, check its entries: if the bug is fixed in that version, drop the workaround and the entry.

## adb

### adb's forward leaves Nagle on

- **Bug:** the adb server sets no `TCP_NODELAY` on sockets a `forward` listener accepts (seen with strace on 37.0.0).
- **Symptom:** on USB the end of a Light frame can wait for the desktop's delayed ACK, 40 ms. A frame then arrives 40 ms late with the next one right behind it, which reads as an 80 ms gap and a skipped frame about once a second.
- **Workaround:** on Linux the desktop sets `TCP_QUICKACK` after each read from a loopback address. Windows has no workaround yet.
- **Code:** `ack_now` in [`rust/crates/link/src/https.rs`](../rust/crates/link/src/https.rs)

## winit

### winit 0.30 has no parent window on Wayland or X11

- **Bug:** a window can't be given a parent after it's made, or at all on Wayland and X11. Only Windows has a call (`with_owner_window`) and `focus_window` does nothing on Wayland.
- **Symptom:** the Rust app's Settings could go underneath the main window. Bringing it up again meant hiding and showing it, which lost where the person had put it.
- **Workaround:** Windows owns the window when it's made. X11 sets `WM_TRANSIENT_FOR` through Xlib. Wayland sends `xdg_toplevel.set_parent` and `xdg_activation` for the keyboard, on a second connection to winit's own display, with winit's toplevel and surface pointers wrapped. Nothing in winit is patched.
- **Code:** [`rust/crates/shell/src/parent.rs`](../rust/crates/shell/src/parent.rs)
- **Fixed when:** a winit release has a parent and an activation call for Wayland and X11. Then `parent.rs` can be a few lines.
