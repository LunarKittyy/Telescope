# Raw list for the stable notes (Rust version)

Raw material, not finished notes. Every item is one line. (check) marks something not confirmed yet.

## 1. Headline

- The desktop app is now written in Rust with the Slint toolkit instead of Python and PyQt6. The phone app and the protocol don't change.
- Code size: the Python desktop has about 64,000 lines plus about 43,000 lines of tests. The Rust version has about 175,000 lines of Rust (about 25,600 of them in test folders) plus about 11,700 lines of Slint (check: many Rust tests sit inside the code files, so the split is rough).
- Tests: about 2,700 test functions in the Python version and about 3,760 `#[test]` functions in the Rust version (check: counted with grep, parametrised tests not counted).
- Linux tarball 45 MB, AppImage 41 MB, Windows setup 37 MB, in MiB. Source: release run 41496854076.
- The last Python release for Windows was a 104 MB installer and a 141 MB portable zip (rust/FINDINGS.md).
- The Python Linux packages took about 565 MB on disk (rust/FINDINGS.md). The size of the Python Linux tarball itself was not found (check).

## 2. Measured gains

Both apps measured the same way on 2 November 2026 on the same machine: an 8-core Ryzen 7 9800X3D with AMD graphics, KDE on Wayland, FFmpeg 8.1.3. A fake phone played a 1080p30 clip at about 8 Mbps, 30 s sampled after 10 s of streaming, with ffmpeg reading the virtual camera throughout. Python app from commit 33c61fc4, Rust app from 041af64b, every default plugin loaded in both, medians of 4 runs (8 for start). Source for all of it: rust/FINDINGS.md, The finished app against Python.

- Start to first painted window: 941 ms in the Python app, 297 ms in the Rust app (median of 8, two batches agree). The 65 ms of the prototype had no plugins and no longer applies. Neither app shows a splash on Linux.
- CPU while streaming 1080p30 (percent of one core): 23.2% in the Python app, 17.1% in the Rust window with the preview, 12.1% without a window. With hardware decoding: 17.9% in Python (its own VA-API build of PyAV), 7.0% in the Rust window, 3.2% without a window.
- Memory while streaming 1080p30: the Rust window is not smaller. RSS 249 MB against 226 MB in Python, PSS 216 MB against 213 MB. Without a window the Rust app uses 71 MB RSS. Don't claim a memory win for the window.
- Frames: both delivered 1066 frames in 35 s (1065 once in Python), so neither dropped any.
- On disk: the Rust binary is 61.7 MB stripped (about 68 MB with the bundled FFmpeg, estimated), the Python app's packages 651 MB plus 13 MB of app files.
- Not measured: Background blur and Auto framing (they need the vision pack in both apps), start on a cold file cache, Windows.
- Memory on a Linux laptop at 1080p30: about 240 MB with Telescope open, 360 MB streaming, 470 MB streaming with Background and Auto framing. Source: docs/troubleshooting.md (check: no Python figure on the same laptop, so no before and after).
- Effects: VHS, CRT, Desk mode and Background blur ran at 16 to 20 fps on a real stream. They now keep the full frame rate at 1080p. Source: commit 1580ca70 (check: the after figure isn't in the message).
- Firefox's Browser camera used to drop to 2 fps and stay there. Source: commit 93215dab (check: no new number).
- Preview on the graphics card costs about 4 points of CPU over no preview at all. Source: rust/FINDINGS.md, With the preview drawn (the finished app's window costs 5 points over no window, 17.1% against 12.1%).

## 3. New for users

- AppImage: `Telescope-x86_64.AppImage` runs on Ubuntu 22.04, Debian 12, Fedora 36 or newer, and doesn't need libfuse2. Source: docs/setup.md, commit 946b981b.
- Plugins are Lua scripts with WGSL shaders and `.slint` cards, and a plugin can open a window of its own. Source: docs/writing-plugins.md, commit fe901b14 (check: window part).
- `check-plugin` checks a plugin from the terminal and points at the line of any shader error. Source: docs/writing-plugins.md.
- Plugins can read the person mask in their shaders. Source: commit dbc848a6 (check).
- Added plugins are checked for updates from start, not only once Settings is opened. Source: commit 3fb172e3 (check).
- Hardware decoding picks the card itself: VA-API or NVIDIA's CUDA on Linux, Direct3D 11 on Windows, and the line under the switch says which. Source: docs/troubleshooting.md, docs/features.md.
- Every colour grading control can go on a keyboard shortcut, and there's an Add phone shortcut. Source: commit e4fb35e8.
- Right-click on a button, switch or choice offers Add keyboard shortcut. Source: commits d6cc22df and 72e4344d (check: Python may have had a menu too).
- Reset to defaults on the Plugins tab. Source: commit e4fb35e8, docs/plugins.md.
- Tooltips size to their text and hold back while you're using a control. Source: commit 2021940c (check: Python behaviour).
- Settings search and the shortcut editor match loosely, so a typo still finds the setting. Source: commits 8fa60b03 and 2c2e3f78 (check).
- On Linux, notifications fall back to the tray icon when notify-send isn't installed. Source: commit d7b7caa5 (check).
- Tray on Linux (StatusNotifierItem) and Windows, with a tooltip that says what is streaming. Source: commits 0736d02f, abeac67d, 339ad93b (check: Python had a tray too).

## 4. Fixed or changed

- Browser camera in Firefox holds the resolution you picked at about the frame rate you asked for, instead of stepping down for good. Source: commits 93215dab, b7a0e55d, 536bd32e.
- The browser gets the bitrate it may use every second. Source: commit 536bd32e.
- Full-range JPEG greys no longer get crushed: 16 and 235 used to turn into pure black and white. Source: commit 2cedf5c9 (check: which sources were hit).
- VHS, CRT, Desk mode and Background blur keep the full frame rate at 1080p. Source: commit 1580ca70.
- The preview repaints when a frame arrives instead of polling every 15 ms. Source: commit cd9e1f19.
- Graphics card memory for frames is capped, and unused textures get dropped. Source: commit e7dc3263.
- A stream's memory is handed back when it ends. Source: commit bb1a1ea9.
- Copy diagnostics reports for real, and Restart really restarts. Source: commit bb1a1ea9.
- Settings and plugin windows stay above the main window. Source: commit 23abd3e4.
- Saved virtual camera memory works again (check: which bug this was). Source: commit 506ed3ab.
- Browser settings are only saved once something changes, so a quick scan leaves nothing behind. Source: commit 506ed3ab.
- Downloads retry instead of failing on one timeout. Source: commit a6220f05.
- On Windows a failed update replaces the loaded DLLs and rolls back everything it changed. Source: commit 508fa1e6.
- Unzipped Windows copies update through the installer. Source: commit daca63b3 (check).
- Wayland windows get their icon. Source: commit 2bd695d8.
- The Windows exe carries its icon and name, so the tray and the firewall prompt show Telescope. Source: commit a219dee2.
- Held shortcut keys are recognised by key position as well as by name. Source: commit 7c1cee90 (check: wording).
- Reset to defaults replaces Turn all back on, and the optional plugins go off while the rest go on. Source: docs/plugins.md.
- The Windows "already running" message now reads "Telescope is already running". Source: docs/troubleshooting.md.

## 5. Engineering stories

- Firefox treats the one frame of delay its encoder adds as overload and steps the rate down to 2 fps for good. The fix stops it stepping down and sends the bitrate every second. Source: commits 93215dab, b7a0e55d, 536bd32e.
- Beside a heavy game the decoder's jobs wait behind the game's GPU work and the picture stutters at 30 fps. Frame steps that can run either way now move to the CPU after a few late frames and come back after a clear stretch (still in progress). Source: crates/core/src/balance.rs, media.rs, rust/PLAN.md.
- The adb server doesn't set TCP_NODELAY, so a frame's last piece waits for a 40 ms delayed ACK. On Linux the app sets TCP_QUICKACK after each read from loopback. Windows has no workaround yet. Source: docs/upstream-bugs.md, commit ba0172c4.
- winit 0.30 can't give a window a parent on Wayland or X11. The app sets it through xdg_toplevel.set_parent on a second connection, and nothing in winit is patched. Source: docs/upstream-bugs.md, rust/crates/shell/src/parent.rs.
- A full-range JPEG was read as limited range after decoding, so the greys got crushed. The decoder now squeezes them in, like the Python BGR round trip did. Source: commit 2cedf5c9.
- Camera card matches the Qt version to about 2 out of 255 per pixel. Qt indents QSS labels by 6 px, centres fixed-height widgets in taller rows, and sizes headers from font metrics nothing in the source spells out. Source: rust/FINDINGS.md, The Camera card in Qt and in Slint.
- The preview paints each frame into a texture on the window's own wgpu device, so nothing is copied back from the card or uploaded again. Source: rust/FINDINGS.md, With the preview drawn.
- Lua plugins never touch pixels. Their shader runs on the GPU for every frame, so keeping up with 60 fps costs the Lua code nothing. Source: docs/writing-plugins.md.
- The bundled FFmpeg is a small LGPL build with only the decoders the app opens, about 5 MB instead of about 170 MB. Source: docs/building.md.
- Screen readers see the controls through AccessKit (about 1.4 MB of the binary). Source: rust/FINDINGS.md, Size and Screen readers (check: how far it has been tested).
- Startup is threads with no async runtime, which keeps the binary small. Source: rust/FINDINGS.md, How the pieces were chosen.

## 6. Things users have to know

- Plugins written in Python for the old Telescope (plugin API 1) don't run any more. Added plugins have to be rewritten in Lua. Source: docs/writing-plugins.md, rust/FINDINGS.md.
- Linux no longer needs Python 3.12 or the Python environment. It only needs v4l2loopback, and adb is optional. Source: docs/setup.md.
- The VA-API setup button is gone. Hardware decoding just finds the card. Source: docs/troubleshooting.md.
- You can go back to the Python version, 4.4.2, the last one: Settings, Updates, Go back to the Python version. It saves a dated copy of your settings first, keeps your phones, and turns off 4.4.2's daily update check so it doesn't offer Rust back. Lua plugins and settings added since 4.4.2 don't carry over, and the phone app has to be 4.4.2 too (uninstall the newer one first, Android won't downgrade it). An AppImage can't be swapped, so it opens the 4.4.2 page instead, and on Linux it needs Python 3.12 or newer. Source: rust/crates/system/src/going_back.rs, rust/PLAN.md. (check: Windows not run yet)
- The first start removes what the Python build left behind: the lib-<build> folders, .previous and the old requirements and guard files. Source: rust/PLAN.md, rust/crates/system/src/update_guard.rs.
- Before it removes the old Python environment (about 650 MB), the updater asks first (check). Source: rust/crates/system/src/update_guard.rs.
- Existing settings from the Python version are read as they are (check). Source: rust/crates/setup/tests/config_parity.rs.
- Manual white balance on the Camera card may still be greyed out (check: the prototype greyed it out). Source: rust/FINDINGS.md, What doesn't match.
- Hardware acceleration is still off by default. Source: docs/features.md (check: confirm in the Rust build).
- Windows support needs a check on a real Windows machine before the stable release (check). Source: docs/building.md, .github/workflows/rust-package.yml.
