# How the desktop app is put together

How the desktop app in `rust/` is split up and which rules keep it that way, for anyone changing the code. [crates/shell/README.md](crates/shell/README.md) covers how the window is built and [docs/writing-plugins.md](../docs/writing-plugins.md) is the guide to writing a plugin. [FINDINGS.md](FINDINGS.md) has what the first prototype measured and [PLAN.md](PLAN.md) the update path from the old Python app.

## The crates

| Crate | What it holds | Uses |
|---|---|---|
| `core` | The plugin API, the event bus, frames, sources, the traits between crates and the settings file | nothing of ours |
| `ui-kit` | The Slint theme, icons and the shared widgets every card is built from | nothing of ours |
| `link` | Discovery, pairing, routes over USB and Wi-Fi, sessions, the video feed, the Browser camera's page and server | `core` |
| `media` | Decoding with FFmpeg (VA-API on Linux, D3D11VA on Windows), the frame steps on the graphics card and the preview | `core` |
| `outputs` | Virtual cameras (v4l2loopback, the Windows 11 camera, UnityCapture), the wait screen, Stream output and Webcam | `core` |
| `audio` | The phone's microphone into a virtual one, RNNoise and the Microphone card | `core` |
| `system` | Tray, notifications, autostart, single instance, global shortcuts, the updater and its journal | `core` |
| `vision` | The vision pack's download, MediaPipe from it and the people tracking | `core` |
| `effects` | The cards that change the picture or read it: Transforms, Colour grading, Chroma key, Fun pack, Background, Privacy blur, Auto framing, Cinema tools, Name tag, Notes, QR codes, Photos, On Air and the rest | `core`, `ui-kit` |
| `setup` | Advanced settings and the Phone card | `core`, `ui-kit` |
| `plugin-host` | Plugins people add: Lua, shaders and `.slint` cards, with a boundary a WASM runtime can fill | `core`, `ui-kit` |
| `shell` | The window, the Settings window, the stream flow, the Camera and Preview cards | `core`, `ui-kit` |
| `app` | `telescope-rs` itself, which builds every crate's part and hands them to the shell as `core::services::Services` | everything |

Crates only meet through traits in `core`. The shell never names `link` or `media`, it gets a `dyn Link` and a `dyn Media` from the binary. `app/tests/architecture.rs` fails when a crate's `Cargo.toml` reaches past that. `core` only grows: a new event, host method or trait method gets added next to the old ones rather than changing them.

## The rules

**Plugins don't know about each other.** Never use another plugin's types or call its methods. To share something:

- put an event on the bus (add an `Event` variant in `core::event` with a comment saying when it fires)
- go through `Host` for anything the window already knows, like the canvas size
- move a plain shared helper into `core` or a module outside the plugins

**Every plugin except Connection can be missing.** People switch plugins off and the app still has to start, stream and quit. If you need something another plugin provides, handle it not being there. `app/tests/plugin_removal.rs` starts the window with each optional plugin left out.

**Plugins only use the public host.** If a plugin needs more from the window, add a method to `Host` in `core::host`, implement it in the shell and add it next to the old ones.

**Settings go in the Settings window as sections.** A plugin hands over a section as a `.slint` component and it's built once.

**Don't count on registration order.** While one plugin is in `setup()` the ones after it don't exist yet. Emit anything others follow again from `on_plugins_ready()`.

**A frame's path is written down.** Each plugin that changes frames names its `FrameStage` (view, geometry, reading, background, effects, colour, privacy, overlay, combine, cover, final) and frames go through the stages in that order. Pick the stage by what has to come before and after, never by where a plugin sits in `shell::plugins::ORDER`, which only orders plugins within a stage.

**Cards fold without plugins knowing.** The window folds cards from what the kit's widgets built. A plugin builds its card completely and never lists titles or calls fold code.

**Each hook means one thing.** `on_stream_start` and `on_stream_stop` are for a stream that really starts or ends. `on_focus_gained` and `on_focus_lost` are for the panels moving between streams that keep running. Events about a stream carry its source id, so a card about the shown stream compares it with the focused one.

**Text is translated where it's shown.** Wrap what people see in `tr()` at the place it's shown, never in a `const` or `static` that runs before the language loads. A table of texts holds `n_("...")`, which marks the text and gets its `tr()` where it's displayed. Anything kept as an identity or saved (a fold key, a canvas preset, a shortcut's choice) stays in English. Diagnostics stay in English for bug reports. Slint texts use `@tr()`. [docs/translating.md](../docs/translating.md) has the details.

**The window drives and nothing reaches back.** The stream flow, sessions and sources never use the window. Anything they need from it goes through `FlowHost` or `SourceHost`, and the session state has no Slint in it.

## Plain data across the plugin boundary

Built-in plugins are Rust but plugins people add are Lua scripts, and a WASM module could be next. Neither can hold a Rust reference, so everything a plugin and the host hand each other is data that could be written out as JSON:

- Hooks get `&mut dyn Host` for the length of the call and nothing to keep. Host methods take strings, `serde_json::Value` and small serialisable structs from `core::host`.
- Requests that finish later give a `RequestId` and an `Event::RequestDone`. Picks on the preview come back as `Event::PreviewPicked`.
- A plugin gets the stream's source id and calls `host.phone_control()` to reach the phone.
- A card or settings section is a `.slint` component plus its properties (`ui::UiSpec`). The plugin changes properties with `host.set_ui_props()` and hears its callbacks as `ui::UiEvent`. A Lua plugin's card works exactly the same way.
- Frame work is a `FrameStep` the plugin hands out and replaces when its settings change (`host.refresh_frame_step()`), never a method the stream thread calls on the plugin. Rust plugins give a `Cpu` step, Lua and WASM ones a `Shader` step (WGSL plus uniforms as data), and a change that can go either way gives an `Either` step, which `core::balance` moves off the card while a game keeps it busy. The stream thread never touches plugin state.
- Events serialise as `{"event": "<name>", ...}`. `plugin-host` shows the whole shape. Its `GuestPlugin` turns any runtime that speaks JSON into a `Plugin` and `call_host` is that runtime's only way back in.

A WASM host can implement all of this without a change to `core` ([docs/plugin-wasm.md](docs/plugin-wasm.md)).

## Keeping plugins working

Everything a plugin can use is listed in `crates/plugin-host/tests/plugin_api.txt` and `the_plugin_api_only_changes_on_purpose` fails when any of it changes.

- **Adding is fine.** Write the new list with `UPDATE_PLUGIN_API=1 cargo test -p telescope-plugin-host` and commit it.
- **Never add a required argument to a hook or change what an event carries.** Add a new one next to it and fire both.
- **Renaming or removing** means keeping the old name working for at least one release, logging a warning once.
- **A hook means what its comment says.** Calling it at another moment breaks plugins as much as changing its arguments.
- **Only when the old behaviour can't be kept,** raise `PLUGIN_API` in `plugin-host/src/manifest.rs` (and `OLDEST_PLUGIN_API` if old plugins can't run at all) and say so in the release notes. Plugins written for the old Python app are API 1 and don't load.

## The event bus

Any thread can emit (`BusSender`) and the host delivers on its own thread in emit order. An event emitted from inside a hook waits until that hook returns. Code that emitted and then read back what another plugin did in response has to wait for the event that answers it instead.

## Frames

`Frame` holds packed planes in an `Arc`. Handing a frame to the preview, the virtual camera and other plugins (`FrameStore`) shares it. A step that changes pixels gets its own copy through `data_mut()`.

## Settings

`core::config` reads and writes `telescope_config.json` byte for byte the way the Python app did, which is what lets an update keep everyone's phones, presets and shortcuts. Python's json module has its own float formatting, escapes and indentation, which `core::json` copies instead of using serde_json's writer. A few things Python could hold and a `serde_json::Value` can't come out differently: NaN and Infinity load as null, an integer past 64 bits becomes a float and a lone surrogate in a string turns into U+FFFD. None of them shows up in a real settings file.

The file is versioned (3 today) and has `selected_device`, `plugin_configs` (each plugin's global settings, with Connection's paired phones among them), `devices` (the settings of each phone, for the plugins that are `device_local`) and `plugins` (which built-in ones are switched off and which added ones are on). Each top-level section is checked on its own, so a damaged one resets and the rest stays. A file that can't be read at all is kept as `.invalid-<time>` and the window shows a banner. It holds the pairing tokens, so on Unix the folder is 0700 and the file 0600.

`fixtures/configs` has nine settings files (real ones with everything personal swapped out and made-up ones for Windows, old versions, damaged sections and broken files), each with an `expected.json` recorded from the last Python app. `telescope-rs config-snapshot <folder>` prints what this app makes of one, and `app/tests/config_fixtures.rs` and `core/src/snapshot.rs` check every fixture. That one check covers upgrading, pairings, presets, shortcuts and broken files.

A release build uses the real settings folder. A build from source keeps `~/.config/telescope-rs` (`%APPDATA%\telescope-rs` on Windows, or `$TELESCOPE_RS_PROFILE`) as a dev profile unless `TELESCOPE_RS_REAL_CONFIG=1`, so trying it never touches the real settings. `telescope-rs dev` runs a dev desktop next to the real one (see [Building and CI](../docs/building.md#try-a-change-without-touching-your-real-install)).

## UI

Every `.slint` file imports the kit as `@ui-kit/ui-kit.slint`. Icons are in the `Icons` global, with `Icons.named()` for a plugin that names its icon in `PluginInfo`.

The shell interprets every card with `slint-interpreter` at startup, built-in ones included. Measured on the 8-core Ryzen from FINDINGS.md with 30 copies of the Camera card (each renamed, so nothing is shared): 56 ms to the first painted window compiled, 212 ms interpreted (120 ms compiling, 45 ms creating, 47 ms to paint). The old Python app took about 1070 ms. 150 ms more buys one path for built-in cards and the ones Lua and WASM plugins hand over, so the built-in ones prove the API added plugins get. It also keeps the window one Slint tree, which is what makes Tab order and the screen reader tree run across cards (compiled built-ins plus interpreted added cards would need Slint's experimental `ComponentContainer`).

## Texts

The English is written in the code. `python tools/i18n.py extract` in `rust/` finds every text and writes `locale/telescope.pot` and brings each `locale/<lang>.po` up to date, and `crates/core/src/i18n` loads them (`app/build.rs` puts them in the program). `app/tests/texts_in_code.rs` scans the code on its own and fails when the template is out of date.

## What's checked automatically

- `app/tests/architecture.rs`: crates only meet through `core`, no plugin uses another plugin or the window, and the pieces the window drives never use it
- `app/tests/plugin_removal.rs`: every optional plugin can be left out
- `plugin-host/tests/lua.rs`: the plugin API only changes on purpose
- `app/tests/stream_*.rs`: who hears which hook as streams start, stop and change focus
- `app/tests/texts_in_code.rs` and `core/tests/i18n.rs`: every text in the code is in the template and in each language's catalogue, and a translation can't break a `{field}`
- `app/tests/config_fixtures.rs`: the settings fixtures
- `app/tests/release_checklist.rs`: the release checklist names every file

When you add a rule that a test could check, add the test too.

## Not settled yet

- Release builds unwind instead of aborting on a panic. That lets the shell catch a plugin that panics and carry on, but the catching itself isn't written yet.
- A `needs` list in `plugin.json` naming built-in plugins a plugin can't work without, and a host registry for added plugins to share with each other.
