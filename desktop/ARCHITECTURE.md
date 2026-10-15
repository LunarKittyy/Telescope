# How the desktop app is put together

This is the short version of how the desktop app is split up and the rules that keep it that way. It's here for anyone changing the code, human or agent. [MODULES.md](MODULES.md) goes file by file, and [docs/plugins.md](../docs/plugins.md) is the friendly guide to writing a plugin.

## The pieces

- **The window** (`telescope/app.py`, `TelescopeWindow`) is the host. It owns the event bus, starts and stops streams, saves settings and lays out the cards. It doesn't know what any card does.
- **Plugins** (`telescope/plugins/`) are the cards and most features: camera controls, transforms, the microphone, the wait screen and so on. Each one is a `TelescopePlugin` subclass. `builtin_plugins()` in `telescope/plugins/__init__.py` lists the built-in ones in the order they're registered.
- **The plugin API** is `telescope/plugin.py`: the `TelescopePlugin` hooks, `HostServices` (everything a plugin may call on the window) and the `EventBus` signals. A plugin only talks to the rest of the app through these three.
- **Sources** (`telescope/sources.py`) are what a stream comes from, a paired phone or a plugin's `StreamSource`. They reach the window through `SourceHost` and nothing else.
- **Sessions** (`telescope/session.py`) is the bookkeeping of which sources stream, which one the panels show and what's starting. It has no Qt in it at all.
- **Added plugins** are loaded by `telescope/plugin_loader.py` from the user's plugins folder, and the Plugins dialog (`telescope/widgets/plugins_dialog.py`) is where they get switched on or off, built-in ones included.

## The rules

**Plugins don't know about each other.** A plugin never imports another plugin's module and never calls another plugin's methods. When two plugins need to share something:

- an event or a piece of state goes on the `EventBus` (add a signal to `plugin.py` with a docstring saying when it fires),
- something the window already knows goes through `HostServices` (`host.canvas_size()` is how the wait screen learns the canvas size, instead of reading Setup's config),
- a plain helper both of them use moves to a module outside `plugins/`.

`host.plugin_config(name)` exists, but reading another plugin's settings ties you to how that plugin stores them. Use it only when nothing above fits.

**Every plugin except Connection can be missing.** People can switch built-in plugins off in the Plugins dialog, so the app has to start, stream and quit without any one of them. If your code needs something another plugin provides, handle it not being there. `tests/test_plugin_removal.py` builds the app with each plugin left out and fails on any error that gets logged.

**Plugins only use the public host.** No `host._anything`. If a plugin needs something new from the window, add a method to the right protocol in `plugin.py` (`HostSettings`, `HostStreams`, `HostCamera`, `HostSources` or `HostShell`), implement it on `TelescopeWindow`, and note it in MODULES.md. `test_window_implements_the_public_host_services_contract` checks the window has all of them.

**Don't count on registration order.** Plugins are set up one at a time, so during `setup()` a plugin registered later doesn't exist yet and won't hear anything you emit. Anything other plugins follow (how many phones there are, which source is picked) gets emitted again from `on_plugins_ready()`, which runs once every plugin is set up and the saved settings are loaded.

**Hooks mean one thing each.** `on_stream_start` and `on_stream_stop` are for a stream really starting or ending. `on_focus_gained` and `on_focus_lost` are for the panels moving between streams that keep running (a tile was clicked, or the mic moved to another stream). Don't guess one from the other. A plugin that wants to pick up where it left off on a stream uses `StreamsBehind` from the focus hooks.

**Signals about a stream say which one.** `stream_connected`, `stream_lost` and `stream_behind` carry the source id and fire for every stream, including ones behind the panels. A card about the shown stream compares the id with `host.focused_source_id()`.

**The window drives, the rest doesn't reach back in.** `sources.py`, `session.py` and `plugin.py` never import `app.py`. Session state (what streams, focus, main, what's waking or waiting) lives in `Sessions`, not in new attributes on the window. Code that doesn't need the window at all (like the single-instance lock in `single_instance.py`) goes in its own module.

## Why

The app used to have plugins guessing whether a stream had stopped or someone just clicked another tile, sources calling about 25 private window methods, and a plugin importing another one's helper. Each of those worked until someone changed the other side. These rules are the fixes written down so they stick.

## What's checked automatically

`tests/test_architecture.py` fails when a plugin imports another plugin or the window, calls a private host method, or when `sources.py`, `session.py` or `plugin.py` import the window. `tests/test_plugin_removal.py` checks every optional plugin can be left out. When you add a rule here that a test could check, add the test too.

## Known exceptions

These are the places that don't follow the rules yet. Don't add to the list without a good reason.

- The window talks to a few built-in plugins directly through `_plugin(name)`: Connection (which phone is picked, waking it, routes), Setup (the canvas size), Stream output (stream size and what the phone opens at) and Shortcuts (`--action`). Connection is required for exactly this reason. The others are optional, and the window checks they're there before using them.
- Connection reads Stream output's `format` setting with `plugin_config` to pick the phone's video URL.
- Presets saves and loads every device-local plugin's settings through `plugin_config` and `apply_preset`. That's its job, so it stays.

## Still to do

`Sessions` holds the session state, but starting, stopping and moving focus (`_start`, `_stop`, `_move_focus` in `app.py`) still live in the window. The next step would be to move that flow next to `Sessions` too, a piece at a time, once there's a reason to touch it.
