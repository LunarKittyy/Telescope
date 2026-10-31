# Parity tools

What keeps the Rust rewrite honest about matching the Python desktop app (the plan is in `../PLAN.md`, under "How parity gets tracked"). Everything here reads the Python app or records it instead of relying on someone's memory. The tools need the desktop app's requirements installed (`desktop/requirements-dev.txt`) and run from any folder.

## The inventory and the ledger

`inventory.py` lists every piece of the Python app with a stable id and an area (the plugin or module that owns it). `ledger.toml` has one line per id with its status: `todo`, `ported` (with `rust = "rust/crates/..."`, a path from the repo root, optionally followed by `::symbol` or `:line`; a test has to end in the `::function` that replaced it, and `--check` looks that function up), `verified` (also with `rust`) or `dropped` (with a `reason`). A `note` can go on any line. The `[unknown]` table at the top is for gaps someone notices by hand. Give each one a note, then teach `inventory.py` to find that whole kind of thing.

```bash
python inventory.py --check
```

Fails when the ledger misses something the Python app has, still lists something it no longer has, or has an entry with a bad status, a missing `reason` or a `rust` path that doesn't exist (so moving code means updating the ledger in the same change). CI runs this on every change under `rust/` or `desktop/` (`.github/workflows/rust-parity.yml`).

```bash
python inventory.py --update
```

Adds new items as `todo`. Items that are gone get removed only while they're still a bare `todo`. An entry someone changed stays and gets listed instead (usually a rename, to move over by hand). It rewrites the file, so comments in it are lost (use `note`).

```bash
python inventory.py --report
```

Progress per area and per kind, like `microphone  0 of 32 texts, 0 of 61 tests, 0 of 3 actions`. Without a flag it prints every id and its area.

| Kind | Where it comes from | Id looks like |
|---|---|---|
| `text` | `telescope/locale/telescope.pot`, read with the app's own `read_po` | the msgid, or `ctx\|msgid` |
| `test` | every `def test_` in `desktop/tests`, by AST | `test_app.py::TestX::test_y` |
| `scene` | the translation preview's `SCENES`, leaving out the pages it generates from the `.pot` (`search-*`, `messages-*`) | `settings-general` |
| `setting` | each built-in plugin's `get_config()` on the real window built offscreen (like `main_window`), plus every key the config fixtures' `expected.json` saves | `device.camera_control.iso` |
| `config` | top-level fields that at least two fixtures save | `selected_device` |
| `action` | the shortcut actions the real window collects from `create_actions()`, owned by the plugin that made them (`--action` takes the same ids) | `microphone.mute` |
| `cli` | `main.py`'s `parse_args` run on its own (importing `main.py` would start the update guard and the splash), plus `--` flags `main.py` and `update_guard.py` check by hand | `--minimized` |
| `phone` | `/v1/...` paths in the source, paths built on a client's `base`, the actions sent in `control`, `still` and `session` bodies, and what the pairing server serves | `/v1/state`, `control.ae_comp` |
| `system` | a source scan for subprocesses (and the commands handed to `_run`-style helpers), registry keys, D-Bus names, env vars, absolute paths, `*_PORT` constants, tray use, folder calls like `Path.home()`, and every public function in `telescope/platform/` | `registry:Software\...\Run` |

The first ledger has everything as `todo` except the phone calls the prototype already makes (hello, ping, session start and stop, state, video.h264, the Camera card's controls and the pairing server). Their `rust` paths point at where that code lives in the workspace now.

### What the inventory can't catch

- **Behaviour inside a text or setting.** It knows a text or key exists, not when it shows or what a value does. That's for the tests, the config check and the scenes.
- **Parametrized tests** count once per `def`, not once per case. Tests built in loops or helpers outside `test_*` functions aren't seen.
- **Settings nested inside a value** (a shortcut's `params`, a preset's contents) aren't separate items. The config comparison still covers them for a ported key. Settings a plugin only writes in some states show up only if a fixture or the default config has them.
- **Phone requests with built-up paths**: `/v1/sensors/alive` is made by string concatenation and isn't listed. Control actions sent under a variable name (`send(action=name)`) and fields inside a body (`width`, `fps`, `ratio`) aren't either. The traffic recordings below catch those.
- **System touch points** come from patterns and anything written differently slips through. That means a program name held in a variable (`<cmd>` is recorded as the call site), shell and PowerShell scripts passed as one string, what a path is used for (reading or writing), files written relative to a folder from somewhere else, sockets opened on a port that isn't a `*_PORT` constant, and anything a library does on its own (Qt's tray, pyvirtualcam, zeroconf, OpenCV's camera access).
- **Windows-only code** is only scanned, never run. The window and its settings come from a Linux run and a setting or action that only exists on Windows isn't listed unless a fixture saved it.
- **Plugins outside `telescope/plugins`** and the plugin API itself (`telescope/plugin.py`) aren't items yet. The API is listed only through its tests and texts.
- **Scenes** cover the states the translation preview builds, which isn't every state the app can be in.

## Settings

```bash
python compare_config.py --binary ../target/debug/telescope-rs
```

Runs `telescope-rs config-snapshot <folder>` for each fixture in `desktop/tests/fixtures/configs` and compares the JSON it prints with that folder's `expected.json`. The command starts the real window offscreen with every built-in plugin, kept away from this computer the way `config_fixtures.py` keeps the Python one: no cameras, servers, webcams or global shortcuts, in a throwaway profile, on the fixtures' date. Values compare as JSON, so 50 and 50.0 differ. `tests/test_parity.py` runs this for real when it finds a built binary (`TELESCOPE_RS`, or `debug/telescope-rs` under `CARGO_TARGET_DIR` or `rust/target`) and skips otherwise. It always compares whether the file was set aside, which plugins came up, the top-level fields and the sections of plugins that aren't installed. For devices it compares whether each phone in the fixture's own file keeps its entry, which is config.py's call (it drops them all for a file from 2.x or a broken `devices` section). Browser entries (`browser`, `browser:<id>`) belong to Browser camera, which forgets a browser after 60 days, so they count only once every `browser_camera` setting is ported. An entry the app adds itself counts through its ported settings. Plugin settings are compared only once the ledger marks them `ported` or `verified`. `--all` compares every setting. Each difference shows its path and both values. `stand_in_snapshot.py` prints `expected.json` (with `--set path=json` changes) so the comparison can be tried before the real command exists:

```bash
python compare_config.py --binary "python3 stand_in_snapshot.py --set saved.version=4"
```

## The log sanitiser

```bash
python record_sanitize.py
```

Rewrites `../crates/core/fixtures/sanitize.json`: what the Python app's `sanitize()` makes of text around zero-width, combining and other odd characters, and which characters Python's `re` counts as word, digit and space. `diagnostics_tests.rs` replays it. Run it again after changing the list of characters in the script, or when the desktop's patterns change.

## The search matcher

```bash
python record_fuzzy.py
```

Rewrites `../crates/core/fixtures/fuzzy.json`: what `telescope/fuzzy.py` makes of text with case folds, ligatures and marks, and the scores it gives a few searches. `fuzzy.rs`'s tests replay it.

## Screens

```bash
python render_scenes.py qt-renders
python compare_scenes.py qt-renders rust-renders review
```

`render_scenes.py` renders every real scene in English at 1x and 1.5x (`qt-renders/1x/<scene>.png`, `qt-renders/1.5x/...`) through the translation preview's own offscreen workers (each one is built exactly as the workbench builds it). All 709 at both scales took about 7 minutes on an 8-core machine. Give scene ids to render only those, `--scale 1` for one scale, `--all` to add the generated text pages.

`compare_scenes.py` expects the Rust renders in the same layout and names. A pixel counts as different when a channel is more than `--tolerance` (24) off, and a scene goes on `review/index.html` with Qt, Rust and a red difference picture side by side when over `--threshold` (0.5%) of its pixels differ or its size does. Only scenes the ledger marks verified fail the run. Others are just reported, as is any verified scene with no Rust render.

Until every card is ported a full Qt scene can't match, since it shows cards Rust doesn't have yet. `../crates/shell/parity/scenes.py OUT` renders the same scenes on a Qt window with only the plugins Rust has ported and compares with `--reduced`, where ported scenes have to match too. Matching that is what `ported` means for a scene; `verified` means it matches the full Qt render.

## Phone traffic

```bash
python phone_session.py python.jsonl
python phone_session.py rust.jsonl --desktop "../target/debug/telescope-rs"
python compare_phone.py python.jsonl rust.jsonl
```

`phone_session.py` starts `../tools/fake_phone.py` with recording on (a short test pattern made with ffmpeg as the clip), starts the desktop in a throwaway dev profile with the fake phone already paired and drives it with `--action` commands. The Python desktop runs offscreen unless `--show` is given. Each phase leaves a mark in the recording. The scripts:

| Script | What happens |
|---|---|
| `basic` | start the stream, change compensation, stabilisation, noise reduction and the torch, stop |
| `controls` | every camera and stream control |
| `pair` | no phone yet: Add phone opens, the fake phone answers the QR offer with its proof, then a stream |
| `lens` | switch to the fake phone's second lens and back |
| `handover` | streaming over Wi-Fi, plugged in over USB (the stream moves there), pulled out (back to Wi-Fi) |
| `recover` | the phone drops off the network mid-stream and comes back |

Every script runs with `../tools/fake_adb.py` first on PATH as `adb` (and in `TELESCOPE_ADB`), so a real phone on the cable stays out of it. It lists the fake phone only while the script has it plugged in and forwards to the fake phone's ports on 127.0.0.1. The commands it gets go into the recording too, with what changes every run (forwarded local ports, the pairing payload) as stand-ins.

The pair script needs two things the app has no way to do from outside: `--action connection.add_phone` to open Add phone, and the QR code's content written to the file `TELESCOPE_PARITY_OFFER` names, which stands in for the camera. The Python desktop gets both from `python_desktop.py`, a launcher that adds them and then runs `desktop/main.py`. The Rust desktop needs the same two, plus the `--action` ids and dev profile handling every script uses.

`recordings/python-<script>.jsonl` are the Python desktop's recordings of each script, so the Rust side can be compared straight away:

```bash
python phone_session.py rust-lens.jsonl --script lens --desktop "../target/debug/telescope-rs"
python compare_phone.py recordings/python-lens.jsonl rust-lens.jsonl
```

`compare_phone.py` compares two recordings phase by phase. Every POST (session, control, still, unpair) has to match with its body and each action's values have to come in the same order. Different actions may interleave differently, since the Python app itself sends the burst of settings at the start of a stream in a different order from run to run. `--strict-order` holds that to the same order too and `--unordered` drops order. GETs and adb commands are compared as a set, since polling depends on timing (`--counts` shows how often each came). A request over USB counts apart from the same request over Wi-Fi, and the pairing answer (the offer's fields, its version and the desktop's HTTP status) is compared like a POST.

`fake_phone.py --record FILE` also records without the driver, for a session done by hand.

## The tools' own tests

```bash
python -m pytest -q tests
```

## Hooks the Python app could use

Nothing in `desktop/` changed for these tools. These would make them sturdier:

- A way to list scenes and their builders without importing the preview's Firestore and review modules, which `render_scenes.py` loads only to reach the render worker.
- An `--action` that waits until the app is ready, instead of the driver retrying for up to 20 seconds while the app starts.
- A machine-readable list of the protocol's control actions (today they're only in `docs/protocol.md`'s table and scattered `send()` calls).
