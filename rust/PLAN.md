# Rust rewrite plan

The whole desktop app moves to Rust and Slint in one go, on the `oxidized-telescope` branch. Every piece of the rewrite merges there until it replaces the Python app on master. The Python app stays the shipping version and only gets bug fixes until the Rust one reaches parity. Updating has to feel like any other update: click Update, restart, same phones, same settings, same cameras in Zoom.

The prototype on `meow/rust-prototype-b6ffde` is the starting point. It already pairs with the real Android app, decodes into v4l2loopback and draws the Camera card within a pixel or two of Qt.

## How parity gets tracked

Nobody remembers 50,000 lines of behaviour, so parity can't depend on someone remembering. Everything below is generated from the Python app or recorded from it, and CI fails when the Rust app falls short of something the ledger says is done.

### The ledger (what exists)

`rust/parity/ledger.toml` lists every piece of the Python app with a status: `todo`, `ported`, `verified` or `dropped` (with a reason, for things that really go away like the PyAV VA-API shim). A script, `rust/parity/inventory.py`, pulls the list straight out of the Python code so nothing depends on someone writing it down:

| Inventory | Taken from | Count today |
|---|---|---|
| UI texts | `telescope/locale/telescope.pot` | 1,828 |
| Python tests | every `def test_` in `desktop/tests` | 2,714 in 125 files |
| Window scenes | the `SCENES` registry the translation previews already use | every card, dialog and Settings tab |
| Settings keys | every key any plugin's `get_config()` returns, read from the config fixtures | |
| Shortcut actions | every action id from `create_actions()` | |
| Phone protocol calls | every request `phone_client.py` and `session_client.py` make | |
| Command line flags | `main.py`'s argparse and `--action` names | |
| System touch points | autostart entries, tray, notifications, global shortcuts, files written outside the config folder, registry keys on Windows | |

CI runs the inventory on every Rust PR. A new item in the Python app (a bug fix that adds a text, a new setting) shows up as `todo` on its own, which is what stops things slipping through while both apps exist. `inventory.py --report` prints how far each area is, like "Microphone: 41 of 44 texts, 18 of 23 tests, 3 of 3 scenes".

### The checks (that it behaves the same)

- **Settings.** The config fixtures in `desktop/tests/fixtures/configs` (already done, see below) get a Rust twin. `telescope-rs config-snapshot <folder>` has to print exactly the `expected.json` Python recorded. That one check covers upgrading, pairings, presets, shortcuts and broken files.
- **Texts.** The Rust app's own extracted `.pot` has to contain every text from the Python one, word for word. That's also what makes every existing translation carry over for free (same msgid, same `.po` file), so the rewrite needs no translation pass.
- **Tests.** Each Python test gets marked ported (with the Rust test that replaced it) or not applicable (with a reason). A test file can't be marked done while any of its tests is still `todo`.
- **Screens.** The Rust app renders the same scenes from the same states and a script diffs them against the Qt renders, like the Camera card comparison in the prototype. Anything over a small threshold goes on a review page with the two side by side.
- **Phone traffic.** A fake phone records every request the Python desktop sends in a set of scripted sessions (pair, stream, switch lens, USB to Wi-Fi handover, drop and recover). The Rust desktop runs the same scripts and the two recordings get compared.
- **Real devices.** The existing `docs/release-checklist.md` becomes the last gate, run by hand on your phones and on Windows before the switch.

A spot can only bite us later if it's in none of these. The ledger has an `unknown` section for anything someone notices that no inventory caught, and each one gets a new inventory rule so the whole class is covered from then on.

## How the work splits across agents

The rule that makes parallel work safe: **one session owns one crate**, and crates only meet through traits in `core` that get settled before anyone fans out. Two sessions never edit the same files.

### Phase 0, sequential (me as coordinator, plus one session)

The foundation everyone else builds on, so it has to be done first and done carefully.

- Cargo workspace with one crate per area below, CI for Linux and Windows
- `core`: the event bus, the `Plugin` trait (shaped so a WASM host can implement it later), source and frame types, config load and save matching Python's file exactly
- `ui-kit`: the Slint theme tokens and shared widgets from the prototype (cards, switches, segments, sliders, tiles)
- The parity tooling: `inventory.py`, the ledger, the config snapshot check, the scene diff, the fake phone with traffic recording

Roughly 1 to 2 sessions on Opus medium. Nothing else starts until the traits and the parity checks exist.

### Phase 1, parallel (one session per crate)

| Crate | Covers | Model |
|---|---|---|
| `link` | discovery, pairing, sessions, USB and Wi-Fi routes, handover, UDP with AES-GCM and NACKs | Opus medium |
| `media` | H.264 decode with VA-API and D3D11VA, MJPEG, frame timing | Opus medium |
| `outputs` | v4l2 slots, the Windows 11 camera's shared memory, UnityCapture | Opus medium |
| `audio` | phone mic, virtual mic on Linux and Windows, RNNoise, auto level | Opus medium |
| `shell` | main window, card framework, Settings window, Plugins tab, banners, onboarding | Opus high (visual and most of what users see) |
| `system` | tray, autostart, single instance and control key, global shortcuts, notifications, the updater | Opus medium |
| `plugin-host` | Lua runtime, the shader pipeline plugins hook into, `.slint` cards, the WASM-ready interface | Opus high (an API people will build on) |

Seven at once is the ceiling. Four or five at a time is kinder to the weekly limit and still keeps everything moving.

### Phase 2, parallel (smaller and well defined)

Once `shell`, `media` and `plugin-host` exist, the rest is many small pieces with a clear spec (the Python code, its tests, the scenes and the fixtures):

- **Effects as GPU shaders:** colour grading and LUTs, chroma key, transforms, Fun pack, Cinema tools scopes. Sonnet high, 2 or 3 per session, checked against Python's output on the same frames.
- **Vision on ONNX:** people tracking, Auto framing, Background, Privacy blur, Expose for my face. Opus medium, since the tracking logic has judgment in it.
- **The remaining cards:** Notes, Name tag, Desk mode, QR codes, Photos, On Air, Several cameras, Browser camera (WebRTC). Sonnet high in batches, except Browser camera and Several cameras on Opus medium.
- **Test porting** for each finished area, by the session that built it.

### Phase 3, back to one thread

- Integration on `oxidized-telescope`, Windows build and installer
- The last Python release, tested updating into the Rust one on both systems
- The real-device checklist with you, then the switch

## The update path

The Python updater doesn't need to know about Rust. On Windows it swaps `TelescopeDesktop.exe` and starts it. On Linux it swaps the tarball's top-level entries and runs `start.sh --after-update`. So the Rust release ships a `TelescopeDesktop.exe` (or a `start.sh` that starts the binary) and implements the same journal handshake as `update_guard.py`: finish a cut-short swap, confirm a trial once the window is up, roll back one that never starts. That's a couple of hundred lines in `system`, with tests against the same journal states. The first Rust start also cleans up what the Python build left behind (`lib-<build>` folders, `.previous/`).

## What's done already

- **The prototype** (`meow/rust-prototype-b6ffde`): protocol, decode, v4l2 and the Camera card, with measurements in `rust/FINDINGS.md`.
- **Config fixtures** (this branch): nine settings files in `desktop/tests/fixtures/configs` (your real setup and dev profile with everything personal swapped out, plus made-up ones for Windows, old versions, damaged sections and broken files), each with what the Python app ends up saving. `python scripts/config_fixtures.py` re-records them and a test keeps them honest.
