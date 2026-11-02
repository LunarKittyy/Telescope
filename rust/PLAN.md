# Desktop app notes

The desktop app is Rust and Slint, in this folder. [ARCHITECTURE.md](ARCHITECTURE.md) says how it's put together and [FINDINGS.md](FINDINGS.md) has what the first prototype measured. This page keeps what still matters from the rewrite: how an update from the old Python app lands in the Rust one, and where the app goes next.

## The update path

The Python updater doesn't need to know about Rust. On Windows it swaps `TelescopeDesktop.exe` and starts it. On Linux it swaps the tarball's top-level entries and runs `start.sh --after-update`, and it only accepts a tarball with `main.py` and a `telescope/` folder in it. So a Rust package has the same shape: `TelescopeDesktop.exe` on Windows, and on Linux a `start.sh` that starts the binary, a `main.py` that hands over to `start.sh` and a `telescope/_build.py` stamp (`tools/package_linux.sh` and `tools/build_windows.ps1` make them and `tools/check_package.py` checks them).

`crates/system/src/update_guard.rs` does what the Python `update_guard.py` did, with the same journal: finish a cut-short swap, confirm a trial once the window is up and roll back one that never starts. Its tests use the journal states a Python update leaves behind. The first Rust start also cleans up what the Python build left behind (the `lib-<build>` folders, `.previous/` and the old requirements and guard files).

## Later ideas
- **Going back (owner, 2026-11-02), needed before Rust reaches the stable channel:** one "Go back to the Python version" action, pinned to the last Python stable release, for anyone who wants that stability back. After that, downgrades in the updater with a warning that it may cause issues and resets the config. Back the old config up first and mind the phone app version match.
- **H.265 (owner, 2026-10-31):** add it some day for phones that support it. The small FFmpeg build already has the HEVC decoder.
- **Load balancing (owner, 2026-11-01):** the goal for running beside a heavy game, with nothing needing elevated permissions and each step measured against the stress test's `gpu` and `split` loads. Built so far: `core::balance` moves steps that can run either way to the CPU after a few late frames and back after a clear stretch, the colour LUT runs on the CPU, person masks have their own thread, and `core::pace` evens out browser feeds that arrive in bursts. Still to do, in this order: even timing for phone streams that only exists under load, the decoder in a child process so hardware decoding can come back, the integrated GPU when there is one, and the front of the chain on the phone when it's plugged in and measured fast enough.
