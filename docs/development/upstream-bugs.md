<!-- covers: rust/crates/link/src/https.rs rust/crates/shell/src/parent.rs rust/crates/link/src/adb.rs rust/crates/core/src/gpu_pick.rs; checked: 9acedcd63 -->
# Upstream bugs

Bugs in Android, adb, winit or other dependencies that Telescope works around. Every line of a workaround carries the bug ID in a comment to let `git grep <ID>` find all of it even after the line numbers below drift.

When a release bumps the pinned version of a dependency listed here, check its entries. If the bug is fixed in that version, drop the workaround and the entry.

## adb

### adb's forward leaves Nagle on

- **Bug:** the adb server sets no `TCP_NODELAY` on sockets a `forward` listener accepts (seen with strace on 37.0.0).
- **Symptom:** on USB the end of a Light frame can wait for the desktop's delayed ACK, 40 ms. A frame then arrives 40 ms late with the next one right behind it, which reads as an 80 ms gap and a skipped frame about once a second.
- **Workaround:** on Linux the desktop sets `TCP_QUICKACK` after each read from a loopback address. Windows has none. A loopback test on Windows 11 with the same write pattern (a frame's body, then a short tail right behind it) showed no wait, 0.03 ms with or without `SIO_TCP_SET_ACK_FREQUENCY`. Windows seems to ack loopback at once. Not tried with a phone over USB.
- **Code:** `ack_now` in [`rust/crates/link/src/https.rs`](../../rust/crates/link/src/https.rs)

## winit

### winit 0.30 has no parent window on Wayland or X11

- **Bug:** a window can't be given a parent after it's made, or at all on Wayland and X11. Only Windows has a call (`with_owner_window`) and `focus_window` does nothing on Wayland.
- **Symptom:** the Rust app's Settings could go underneath the main window. Bringing it up again meant hiding and showing it, which lost where the person had put it.
- **Workaround:** Windows owns the window when it's made. X11 sets `WM_TRANSIENT_FOR` through Xlib. Wayland sends `xdg_toplevel.set_parent` and `xdg_activation` for the keyboard, on a second connection to winit's own display, with winit's toplevel and surface pointers wrapped. Nothing in winit is patched.
- **Code:** [`rust/crates/shell/src/parent.rs`](../../rust/crates/shell/src/parent.rs)
- **Fixed when:** a winit release has a parent and an activation call for Wayland and X11. Then `parent.rs` can be a few lines.

## wgpu

### wgpu 30 crashes Intel's Windows Vulkan driver

- **Bug:** [gfx-rs/wgpu#9845](https://github.com/gfx-rs/wgpu/issues/9845). wgpu 30 makes a descriptor pool with no pool sizes while it opens a device. Vulkan only allows that since 1.3.215 and Intel's Windows driver (`igvk64.dll`, seen on HD 630 and UHD 630) crashes on it.
- **Symptom:** Telescope closes at start on laptops with these Intel cards (#489). wgpu 29 was fine.
- **Workaround:** `rust/vendor/wgpu-hal` is wgpu-hal with the same small fix as [gfx-rs/wgpu#10124](https://github.com/gfx-rs/wgpu/pull/10124) (see its `PATCH.txt`). Intel cards on Windows can then draw the window on Vulkan, where DX12 leaks memory. Their frame pipeline stays on DX12 first, where decoded pictures are shared on the card. If Vulkan fails to open, the card's DX12 adapter is next in the list.
- **Code:** `[patch.crates-io]` in [`rust/Cargo.toml`](../../rust/Cargo.toml)
- **Fixed when:** a wgpu release has gfx-rs/wgpu#10124 (merged on trunk in September 2026 and missing from 30.0.2). Then this entry, the vendored folder and the `[patch]` go.
