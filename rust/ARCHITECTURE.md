<!-- covers: rust/crates/*/Cargo.toml rust/app/tests/architecture.rs rust/app/tests/plugin_removal.rs rust/crates/plugin-host/src rust/crates/core/src/plugin.rs rust/crates/core/src/snapshot.rs rust/app/build.rs rust/crates/plugin-host/tests/plugin_api.txt; checked: 0d35dc883 -->
# How the desktop app is put together

How the desktop app in `rust/` is split up and which rules keep it that way, for anyone changing the code. [crates/shell/README.md](crates/shell/README.md) covers how the window is built and [docs/development/writing-plugins.md](../docs/development/writing-plugins.md) is the guide to writing a plugin. [docs/development/packaging.md](../docs/development/packaging.md#the-update-path) has the update path from the old Python app.

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
| `setup` | Advanced settings, the Phone card and the Stats card | `core`, `ui-kit` |
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

**A plugin that panics is switched off until the next start.** Release builds unwind and `State::call` catches a panic in any hook. The plugin's state can't be trusted after that and `State::stop_after_panic` calls its `shutdown()` once, gives back what it held, hides its card and windows, drops its frame steps and shows a banner. A plugin that hides things (at the Background, Privacy or Cover stage, or with a step whose `hides()` was true this run) is the exception: when it panics Telescope closes, since a closed app shows nothing and one that carries on could show what was hidden. Required plugins carry on after a panic since Telescope can't work without them. A panic in `setup()` leaves the plugin out of the start as before.

**Plugins only use the public host.** If a plugin needs more from the window, add a method to `Host` in `core::host`, implement it in the shell and add it next to the old ones.

**Settings go in the Settings window as sections.** A plugin hands over a section as a `.slint` component and it's built once.

**Don't count on registration order.** While one plugin is in `setup()` the ones after it don't exist yet. Emit anything others follow again from `on_plugins_ready()`.

**A frame's path is written down.** Each plugin that changes frames names its `FrameStage` (view, geometry, reading, background, effects, colour, privacy, overlay, combine, cover, final) and frames go through the stages in that order. Pick the stage by what has to come before and after, never by where a plugin sits in `shell::plugins::ORDER`, which only orders plugins within a stage. A plugin whose work belongs at two places in the path (Filters cleans up before the grade and finishes after it) names the second place through `Plugin::staged_frame_steps`, which hands out steps with a stage each. Frame steps run in the Colour stage after the colour pipeline's own, which is how a step can follow the grade.

**Cards fold without plugins knowing.** The window folds cards from what the kit's widgets built. A plugin builds its card completely and never lists titles or calls fold code.

**Each hook means one thing.** `on_stream_start` and `on_stream_stop` are for a stream that really starts or ends. `on_focus_gained` and `on_focus_lost` are for the panels moving between streams that continue running. Events about a stream carry its source id. A card about the shown stream compares it with the focused one.

**Text is translated where it's shown.** Wrap what people see in `tr()` at the place it's shown, never in a `const` or `static` that runs before the language loads. A table of texts holds `n_("...")`, which marks the text and gets its `tr()` where it's displayed. Anything used as an identity or saved (a fold key, a canvas preset, a shortcut's choice) remains in English. Diagnostics remain in English for bug reports. Slint texts use `@tr()`. [docs/development/translating.md](../docs/development/translating.md) has the details.

**The window drives and nothing reaches back.** The stream flow, sessions and sources never use the window. Anything they need from it goes through `FlowHost` or `SourceHost`, and the session state has no Slint in it.

## Plain data across the plugin boundary

Built-in plugins are Rust but plugins people add are Lua scripts and a WASM module could be next. Neither can hold a Rust reference. Everything a plugin and the host hand each other is data that could be written out as JSON:

- Hooks get `&mut dyn Host` for the length of the call and nothing to hold on to. Host methods take strings, `serde_json::Value` and small serialisable structs from `core::host`.
- Requests that finish later give a `RequestId` and an `Event::RequestDone`. Picks on the preview arrive as `Event::PreviewPicked`.
- A plugin gets the stream's source id and calls `host.phone_control()` to reach the phone.
- A card or settings section is a `.slint` component plus its properties (`ui::UiSpec`). The plugin changes properties with `host.set_ui_props()` and hears its callbacks as `ui::UiEvent`. A Lua plugin's card works exactly the same way.
- Frame work is a `FrameStep` the plugin hands out and replaces when its settings change (`host.refresh_frame_step()`), never a method the stream thread calls on the plugin. Rust plugins give a `Cpu` step, Lua and WASM ones a `Shader` step (WGSL plus uniforms as data), and a change that can go either way gives an `Either` step, which `core::balance` moves off the card while a game keeps it busy. The stream thread never touches plugin state.
- A Rust step whose job is to hide something (Privacy blur, Background, the wait screen) declares that with `CpuStep::hides`. A step that panics or ends up skipped makes the pipeline send a dark frame, never the clear one it was given. A pipeline can also run only those steps again on the frame already out (`FramePipeline::hide_again`) to stop a stalled stream showing it the moment one changes (a Background step that wasn't there before turns that frame dark, as it can't be run on a finished picture).
- Events serialise as `{"event": "<name>", ...}`. `plugin-host` shows the whole shape. Its `GuestPlugin` turns any runtime that speaks JSON into a `Plugin` and `call_host` is that runtime's only way back in.

A WASM host can implement all of this without a change to `core` ([docs/development/plugin-wasm.md](../docs/development/plugin-wasm.md)).

## Keeping plugins working

Everything a plugin can use is listed in `crates/plugin-host/tests/plugin_api.txt` and `the_plugin_api_only_changes_on_purpose` fails when any of it changes.

- **Adding is fine.** Write the new list with `UPDATE_PLUGIN_API=1 cargo test -p telescope-plugin-host` and commit it.
- **Never add a required argument to a hook or change what an event carries.** Add a new one next to it and fire both.
- **Renaming or removing** means keeping the old name working for at least one release, logging a warning once.
- **A hook means what its comment says.** Calling it at another moment breaks plugins as much as changing its arguments.
- **Only when the old behaviour can't be preserved,** raise `PLUGIN_API` in `plugin-host/src/manifest.rs` (and `OLDEST_PLUGIN_API` if old plugins can't run at all) and mention it in the release notes. Plugins written for the old Python app are API 1 and don't load.

## The event bus

Any thread can emit (`BusSender`) and the host delivers on its own thread in emit order. An event emitted from inside a hook waits until that hook returns. Code that emitted and then read back what another plugin did in response has to wait for the event that answers it instead.

## Stats

Each stream's worker counts into a `core::stats::SourceStats`: relaxed atomics only, which leaves locks and allocation off the frame path. `perf::record` also adds a stage's time to the stream the thread is bound to (`stats::bind`). The link and decoder note drops the same way (`stats::note_drop`). Once a second `Hub` reads them into a `Snapshot` (numbers, with the last 60 s of every series) and puts it on the bus as `Event::StatsUpdated`. What costs something to measure (each plugin's frame step and hook time) only runs while a plugin has asked with `Host::want_stats`, like `want_people`.

## Frames

`Frame` holds packed planes in an `Arc`. Handing a frame to the preview, the virtual camera and other plugins (`FrameStore`) shares it. A step that changes pixels gets its own copy through `data_mut()`.

## Pausing while nobody watches

The main camera's frame steps and vision stop while no app has read the virtual camera for about 8 s and nothing else needs frames; decoding and the phone's link carry on and the camera continues sending the last picture. `shell::idle::IdleGate` holds the answer and the stream worker (`spawn_steps`) looks at it once per frame and no plugin knows about it. The reader comes from the Wait screen plugin's `Event::CameraWatched`. A computer with no such signal never pauses and only slot 0 does. A reader, or anything that needs frames, ends the pause at the next frame. The steps that hold on to pictures start again (`FramePipeline::forget_history`).

Frames are needed while the window is on screen, the preview is popped out, a plugin window is open, a motion watch runs, or any plugin has asked with `Host::want_frames`. A plugin that reads pictures and reports what it found, or draws from other streams' frames, asks for as long as it needs them (QR code reader, Composite, Auto colour grading's look, the MCP server's watch). A script's `host.want_people` asks for it. Anything new that acts on frames with no one looking has to ask too, or it silently stops.

## Settings

`core::config` reads and writes `telescope_config.json` byte for byte the way the Python app did, which lets an update carry over everyone's phones, presets and shortcuts. Python's json module has its own float formatting, escapes and indentation, which `core::json` copies instead of using serde_json's writer. A few things Python could hold and a `serde_json::Value` can't come out differently: NaN and Infinity load as null, an integer past 64 bits becomes a float and a lone surrogate in a string turns into U+FFFD. None of them shows up in a real settings file.

The file is versioned (3 today) and has `selected_device`, `plugin_configs` (each plugin's global settings, with Connection's paired phones among them), `devices` (the settings of each phone, for the plugins that are `device_local`) and `plugins` (which built-in ones are switched off and which added ones are on). Each top-level section is checked separately. A damaged one resets and the rest remain. A file that can't be read at all is saved as `.invalid-<time>` and the window shows a banner. As it holds the pairing tokens, the folder is 0700 and the file 0600 on Unix.

`fixtures/configs` has eight settings files (real ones with everything personal swapped out and made-up ones for Windows, old versions, damaged sections and broken files), each with an `expected.json` recorded from the last Python app. `telescope-rs config-snapshot <folder>` prints what this app makes of one, and `app/tests/config_fixtures.rs` and `core/src/snapshot.rs` check every fixture. That one check covers upgrading, pairings, presets, shortcuts and broken files.

A release build uses the real settings folder. A build from source uses `~/.config/telescope-rs` (`%APPDATA%\telescope-rs` on Windows, or `$TELESCOPE_RS_PROFILE`) as a dev profile unless `TELESCOPE_RS_REAL_CONFIG=1`. Trying it never touches the real settings. `telescope-rs dev` runs a dev desktop next to the real one (see [Building and CI](../docs/development/building.md#try-a-change-without-touching-your-real-install)).

## UI

Every `.slint` file imports the kit as `@ui-kit/ui-kit.slint`. Icons are in the `Icons` global, with `Icons.named()` for a plugin that names its icon in `PluginInfo`.

The shell interprets every card with `slint-interpreter` at startup, built-in ones included. Measured on an 8-core Ryzen with 30 copies of the Camera card (each renamed, with nothing shared): 56 ms to the first painted window compiled, 212 ms interpreted (120 ms compiling, 45 ms creating, 47 ms to paint). The old Python app took about 1070 ms. 150 ms more buys one path for built-in cards and the ones Lua and WASM plugins hand over. The built-in ones then prove the API added plugins get. It also leaves the window as one Slint tree, which is what makes Tab order and the screen reader tree run across cards (compiled built-ins plus interpreted added cards would need Slint's experimental `ComponentContainer`).

## Texts

The English is written in the code. `python tools/i18n.py extract` in `rust/` finds every text and writes `locale/telescope.pot` and brings each `locale/<lang>.po` up to date. `crates/core/src/i18n` loads them (`app/build.rs` puts them in the program). `app/tests/texts_in_code.rs` scans the code itself and fails when the template is out of date.

## What's checked automatically

- `app/tests/architecture.rs`: crates only meet through `core`, no plugin uses another plugin or the window, and the pieces the window drives never use it
- `app/tests/plugin_removal.rs`: every optional plugin can be left out
- `plugin-host/tests/lua.rs`: the plugin API only changes on purpose
- `app/tests/stream_*.rs`: who hears which hook as streams start, stop and change focus
- `app/tests/texts_in_code.rs` and `core/tests/i18n.rs`: every text in the code is in the template and in each language's catalogue, and a translation can't break a `{field}`
- `app/tests/config_fixtures.rs`: the settings fixtures
- `app/tests/release_checklist.rs`: the release checklist names every file

When you add a rule that a test could check, add the test too.
