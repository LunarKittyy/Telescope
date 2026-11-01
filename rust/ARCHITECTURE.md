# How the Rust app is put together

The rules in [desktop/ARCHITECTURE.md](../desktop/ARCHITECTURE.md) carry over unchanged: plugins don't know about each other, every plugin except Connection can be switched off, the window drives and nothing reaches back. This page covers what's new in the Rust version and why it's shaped the way it is. [PLAN.md](PLAN.md) has the plan and [FINDINGS.md](FINDINGS.md) what the prototype measured.

## The crates

| Crate | What it holds | Uses |
|---|---|---|
| `core` | The plugin API, the event bus, frames, sources, the traits between crates and the settings file | nothing of ours |
| `ui-kit` | The Slint theme, icons and the shared widgets every card is built from | nothing of ours |
| `link` | Discovery, pairing, routes over USB and Wi-Fi, sessions, the video feed | `core` |
| `media` | Decoding (FFmpeg on Linux today) and the preview scaler | `core` |
| `outputs` | Virtual cameras (v4l2loopback today) | `core` |
| `audio` | The phone's microphone into a virtual one (skeleton) | `core` |
| `system` | Tray, notifications, autostart, single instance, shortcuts, updates (skeleton) | `core` |
| `plugin-host` | Plugins people add: Lua and later WASM (the boundary only, so far) | `core`, `ui-kit` |
| `shell` | The window and the stream flow | `core`, `ui-kit` |
| `app` | `telescope-rs` itself, which builds every crate's part and hands them to the shell as `core::services::Services` | everything |

One session owns one crate and crates only meet through traits in `core`. The shell never names `link` or `media` (it gets a `dyn Link` and a `dyn Media` from the binary). `app/tests/architecture.rs` fails when a crate's `Cargo.toml` reaches past that. `core` belongs to the coordinator and only grows: a new event, host method or trait method gets added next to the old ones rather than changing them, the same rule plugin.py follows.

## Plain data across the plugin boundary

Built-in plugins are Rust but plugins people add will be Lua scripts and later WASM modules. Neither can hold a Rust reference. That's why everything a plugin and the host hand each other is data that could be written out as JSON:

- Hooks get `&mut dyn Host` for the length of the call and nothing to keep. Host methods take strings, `serde_json::Value` and small serialisable structs from `core::host`.
- Python's `on_done` callbacks become a `RequestId` now and an `Event::RequestDone` later. Picks on the preview come back as `Event::PreviewPicked`.
- `ctrl` in `on_stream_start` is gone. A plugin gets the stream's source id and calls `host.phone_control()`.
- A card or settings section is a `.slint` component plus its properties (`ui::UiSpec`). The plugin changes properties with `host.set_ui_props()` and hears its callbacks as `ui::UiEvent`. A Lua plugin's card works exactly the same way.
- Frame work is a `FrameStep` the plugin hands out and replaces when its settings change (`host.refresh_frame_step()`), never a method the stream thread calls on the plugin. Rust plugins give a `Cpu` step, Lua and WASM ones a `Shader` step (WGSL plus uniforms as data), and a change with both gives an `Either` step, which `core::balance` moves off the card while a game keeps it busy. The stream thread never touches plugin state, which is what Python's frame_step did for background streams and now applies to every stream.
- Events serialise as `{"event": "<signal name>", ...}` with the names from plugin.py. `plugin-host` shows the whole shape. Its `GuestPlugin` turns any runtime that speaks JSON into a `Plugin` and `call_host` is that runtime's only way back in.

A WASM host can implement all of this later without a change to `core`.

## The event bus

Any thread can emit (`BusSender`) and the host delivers on its own thread in emit order. An event emitted from inside a hook waits until that hook returns, whereas a Qt signal between two plugins on the GUI thread ran straight away. Code that emitted and then read back what another plugin did in response has to wait for the event that answers it instead. Every signal in plugin.py has an `Event` variant with the same name, and a test reads plugin.py so a new signal there fails until it has one.

## Frames

`Frame` holds packed planes in an `Arc`. Handing a frame to the preview, the virtual camera and other plugins (`FrameStore`) shares it. A step that changes pixels gets its own copy through `data_mut()`. The stages are `FrameStage`, ordered like `FRAME_STAGES`.

## Settings

`core::config` reads and writes `telescope_config.json` byte for byte the way config.py does. Python's json module has its own float formatting, escapes and indentation, which `core::json` copies instead of using serde_json's writer. A few things Python can hold and a `serde_json::Value` can't come out differently: NaN and Infinity load as null, an integer past 64 bits becomes a float and a lone surrogate in a string turns into U+FFFD. None of them shows up in a real settings file.

`telescope-rs config-snapshot <fixture>` prints what `desktop/scripts/config_fixtures.py` records. The test in `core/src/snapshot.rs` checks the parts config.py and the window decide against every fixture and lists each place where a plugin still has to be ported to close the gap.

Until the rewrite replaces the Python app, `telescope-rs` treats `~/.config/telescope-rs` (`%APPDATA%\telescope-rs` on Windows, or `$TELESCOPE_RS_PROFILE`) as a dev profile and trying it never touches the real settings. `TELESCOPE_DEV_PROFILE` works as in Python and `TELESCOPE_RS_REAL_CONFIG=1` uses the real file.

## UI

Every `.slint` file imports the kit as `@ui-kit/ui-kit.slint`. Icons are in the `Icons` global, with `Icons.named()` for a plugin that names its icon in `PluginInfo`.

The shell interprets every card with `slint-interpreter` at startup, built-in ones included. Measured on the 8-core Ryzen from FINDINGS.md with 30 copies of the Camera card (each renamed, so nothing is shared): 56 ms to the first painted window compiled, 212 ms interpreted (120 ms compiling, 45 ms creating, 47 ms to paint). The Python app takes about 1070 ms. 150 ms more buys one path for built-in cards and the ones Lua and WASM plugins hand over, so the built-in ones prove the API added plugins get. It also keeps the window one Slint tree, which is what makes Tab order and the screen reader tree run across cards (compiled built-ins plus interpreted added cards would need Slint's experimental `ComponentContainer`). `crates/shell/README.md` has how the window document is generated and how the numbers were taken.

## Not settled yet

- Release builds unwind instead of aborting on a panic. That lets the shell catch a plugin that panics and carry on like the Python app does with exceptions, but the catching itself isn't written yet.
