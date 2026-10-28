# Parity tools

What keeps the Rust rewrite honest about matching the Python desktop app (the plan is in `../PLAN.md`, under "How parity gets tracked"). Everything here reads the Python app or records it instead of relying on someone's memory. The tools need the desktop app's requirements installed (`desktop/requirements-dev.txt`) and run from any folder.

## The inventory and the ledger

`inventory.py` lists every piece of the Python app with a stable id and an area (the plugin or module that owns it). `ledger.toml` has one line per id with its status: `todo`, `ported` (with `rust = "where"`), `verified` (also with `rust`) or `dropped` (with a `reason`). A `note` can go on any line. The `[unknown]` table at the top is for gaps someone notices by hand. Give each one a note, then teach `inventory.py` to find that whole kind of thing.

```bash
python inventory.py --check
```

Fails when the ledger misses something the Python app has, still lists something it no longer has, or has an entry with a bad status or a missing `rust` or `reason`. CI runs this on every change under `rust/` or `desktop/` (`.github/workflows/rust-parity.yml`).

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

The first ledger has everything as `todo` except the phone calls the prototype already makes (hello, ping, session start and stop, state, video.h264, the Camera card's controls and the pairing server). Their `rust` paths point at the prototype's files, which the workspace restructure will move.

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

Runs `telescope-rs config-snapshot <folder>` for each fixture in `desktop/tests/fixtures/configs` and compares the JSON it prints with that folder's `expected.json`. It always compares whether the file was set aside, which plugins came up, the top-level fields, which devices are kept and the sections of plugins that aren't installed. Plugin settings are compared only once the ledger marks them `ported` or `verified`. `--all` compares every setting. Each difference shows its path and both values. `stand_in_snapshot.py` prints `expected.json` (with `--set path=json` changes) so the comparison can be tried before the real command exists:

```bash
python compare_config.py --binary "python3 stand_in_snapshot.py --set saved.version=4"
```

## Screens

```bash
python render_scenes.py qt-renders
python compare_scenes.py qt-renders rust-renders review
```

`render_scenes.py` renders every real scene in English at 1x and 1.5x (`qt-renders/1x/<scene>.png`, `qt-renders/1.5x/...`) through the translation preview's own offscreen workers (each one is built exactly as the workbench builds it). All 709 at both scales took about 7 minutes on an 8-core machine. Give scene ids to render only those, `--scale 1` for one scale, `--all` to add the generated text pages.

`compare_scenes.py` expects the Rust renders in the same layout and names. A pixel counts as different when a channel is more than `--tolerance` (24) off, and a scene goes on `review/index.html` with Qt, Rust and a red difference picture side by side when over `--threshold` (0.5%) of its pixels differ or its size does. Only scenes the ledger marks ported fail the run. Others are just reported, as is any ported scene with no Rust render.

## Phone traffic

```bash
python phone_session.py python.jsonl
python phone_session.py rust.jsonl --desktop "../target/debug/telescope-rs"
python compare_phone.py python.jsonl rust.jsonl
```

`phone_session.py` starts `../tools/fake_phone.py` with recording on (a short test pattern made with ffmpeg as the clip), starts the desktop in a throwaway dev profile with the fake phone already paired and drives it with `--action` commands. The Python desktop runs offscreen unless `--show` is given. Each phase leaves a mark in the recording. The `basic` script pairs, starts the stream, changes compensation, stabilisation, noise reduction and the torch, then stops. `controls` goes through every camera and stream control. The Rust desktop needs the same `--action` ids and dev profile handling for this to work.

`compare_phone.py` compares two recordings phase by phase. Every POST (session, control, still, unpair) has to match with its body and each action's values have to come in the same order. Different actions may interleave differently, since the Python app itself sends the burst of settings at the start of a stream in a different order from run to run. `--strict-order` holds that to the same order too and `--unordered` drops order. GETs are compared as a set, since polling depends on timing (`--counts` shows how often each came).

`fake_phone.py --record FILE` also records without the driver, for a session done by hand.

Still missing from the scripts are real QR pairing (the profile starts paired), switching lens from the shortcut (it needs a phone with more than one lens in its state), USB to Wi-Fi handover, and a dropped connection coming back. Each of those needs the fake phone to learn something new first.

## The tools' own tests

```bash
python -m pytest -q tests
```

## Hooks the Python app could use

Nothing in `desktop/` changed for these tools. These would make them sturdier:

- A way to list scenes and their builders without importing the preview's Firestore and review modules, which `render_scenes.py` loads only to reach the render worker.
- An `--action` that waits until the app is ready, instead of the driver retrying for up to 20 seconds while the app starts.
- A machine-readable list of the protocol's control actions (today they're only in `docs/protocol.md`'s table and scattered `send()` calls).
