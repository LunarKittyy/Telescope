# How the desktop app is put together

This is the short version of how the desktop app is split up and which rules keep it that way. It's for anyone changing the code, human or agent. [MODULES.md](MODULES.md) goes through it file by file and [docs/plugins.md](../docs/plugins.md) is the friendly guide to writing a plugin.

## The pieces

- **The window** (`telescope/app.py`, `TelescopeWindow`) is the host. It owns the event bus, starts and stops streams, saves settings and lays out the cards. It doesn't know what any of the cards do.
- **Plugins** (`telescope/plugins/`) are the cards and most of the features, like camera controls, transforms, the microphone and the wait screen. Each one is a `TelescopePlugin` subclass. `builtin_plugins()` in `telescope/plugins/__init__.py` lists the built-in ones in the order they're registered.
- **The plugin API** lives in `telescope/plugin.py`. It has the `TelescopePlugin` hooks, `HostServices` (everything a plugin may call on the window) and the `EventBus` signals. A plugin only talks to the rest of the app through these three.
- **Sources** (`telescope/sources.py`) are what a stream comes from, which is either a paired phone or a plugin's `StreamSource`. They reach the window only through `SourceHost`.
- **Sessions** (`telescope/session.py`) keeps track of which sources are streaming, which one the panels show and what's still starting. There's no Qt in it at all.
- **The stream flow** (`telescope/stream_flow.py`, `StreamFlow`) starts and stops streams, moves the panels between them and keeps the mic on one stream. It works on `Sessions` and reaches the window only through `FlowHost`.
- **Added plugins** are loaded from the user's plugins folder by `telescope/plugin_loader.py`. The Plugins dialog (`telescope/widgets/plugins_dialog.py`) is where people switch them on and off, built-in ones included.

## The rules

**Plugins don't know about each other.** A plugin never imports another plugin's module or calls another plugin's methods. When two plugins need to share something there are three ways to do it:

- An event or a piece of state goes on the `EventBus`. Add a signal to `plugin.py` with a docstring that says when it fires.
- Something the window already knows goes through `HostServices`. That's how the wait screen gets the canvas size from `host.canvas_size()` instead of reading Setup's config.
- A plain helper both of them use moves to a module outside `plugins/`.

`host.plugin_config(name)` exists too, but reading another plugin's settings ties you to how that plugin stores them. Only use it when none of the above fits.

**Every plugin except Connection can be missing.** People can switch built-in plugins off in the Plugins dialog and the app still has to start, stream and quit without any one of them. If your code needs something another plugin provides, handle the case where it isn't there. `tests/test_plugin_removal.py` builds the app with each plugin left out and fails on any error that gets logged.

**Plugins only use the public host.** There's no `host._anything` in a plugin. If a plugin needs something new from the window, add a method to the right protocol in `plugin.py` (`HostSettings`, `HostStreams`, `HostCamera`, `HostSources` or `HostShell`), implement it on `TelescopeWindow` and note it in MODULES.md. `test_window_implements_the_public_host_services_contract` checks that the window has all of them.

**Don't count on the registration order.** Plugins are set up one at a time. While one is in `setup()`, the plugins registered after it don't exist yet and won't hear anything it emits. Anything other plugins follow (like how many phones there are or which source is picked) gets emitted again from `on_plugins_ready()`, which runs once every plugin is set up and the saved settings are loaded.

**Each hook means one thing.** `on_stream_start` and `on_stream_stop` are for a stream that really starts or ends. `on_focus_gained` and `on_focus_lost` are for the panels moving between streams that keep running, for example when someone clicks a tile or the mic moves to another stream. Don't guess one from the other. A plugin that wants to pick up where it left off on a stream uses `StreamsBehind` from the focus hooks.

**Signals about a stream say which stream.** `stream_connected`, `stream_lost` and `stream_behind` carry the source id and fire for every stream, including the ones behind the panels. A card about the shown stream compares the id with `host.focused_source_id()`.

**The window drives and nothing reaches back into it.** `sources.py`, `session.py`, `stream_flow.py` and `plugin.py` never import `app.py`. Session state (what streams, focus, main, what's waking or waiting) lives in `Sessions` and not in new attributes on the window. Starting, stopping and focus changes go in `StreamFlow`. If it needs something new from the window, add it to `FlowHost` and `app._FlowHost` the same way sources use `SourceHost`. Code that doesn't need the window at all goes in its own module, the way the single-instance lock went into `single_instance.py`.

## Keeping plugins working

People write plugins against what `plugin.py` offers and expect them to keep working after an update. Everything a plugin can use is listed in `tests/plugin_api.txt`. That covers the hooks, the host methods, the bus signals, `StreamSource`, `StreamsBehind`, the banner classes and the shortcut helpers. `tests/test_plugin_api.py` fails as soon as any of it changes.

- **Adding is fine.** A new hook, host method, signal or optional argument with a default doesn't bother existing plugins. Write the new list with `UPDATE_PLUGIN_API=1 python -m pytest tests/test_plugin_api.py` and commit it with the change.
- **Never give an existing hook a new required argument and never change what a signal carries.** Plugins that override the hook or connect to the signal would crash. Add a new hook or signal next to the old one instead and keep firing both.
- **Renaming or removing something means keeping the old name working** for at least one release. Let it call the new one and log a warning once so plugin authors notice.
- **A hook means what its docstring says.** Calling it at a different moment breaks plugins just as much as changing its arguments, even though the test can't see that.
- **Only when there's no way to keep the old behaviour** raise `PLUGIN_API` in `plugin_loader.py`. Plugins made for the old number then show "It was made for an older Telescope" instead of crashing halfway. Raise `OLDEST_PLUGIN_API` too if the old ones can't run at all. Say so in the release notes.

## When plugins need the same thing

The view is built (`view.py` and `view_controller.py`, `HostView` in `plugin.py`). The rest is the plan for when the first plugin needs it, written down so nobody reaches for plugin-to-plugin dependencies first.

Some future plugins will want a shared base. Several effect plugins could share one post-processing backend (a GPU context, a smaller copy of the frame or face detection run once a frame). An AI framing plugin would want to move the same zoom and pan the Transforms card does rather than add a second transform on top of it.

Plugins still won't depend on other plugins. If B used A's code then any update of A could break B, and nothing would catch it the way `test_plugin_api.py` catches changes to the host. Switching A off or removing it would have to do something about B too, and the loader would need versions and a load order between plugins. So the plan is this:

- **The shared part moves into the app.** A post-processing backend becomes a host service that effect plugins call through `HostServices`. Zoom, pan, flip and rotation are a view the window owns, with Transforms as the manual controls for it (`host.set_view`). Something that moves the view by itself, like AI framing, takes it with `host.take_view` and keeps working when someone switches the Transforms card off. The zoom maths lives in `view.py`, outside `plugins/`.
- **A plugin can say what it needs.** A `needs` list in plugin.json names the built-in plugins it can't work without. If one of them is switched off the Plugins dialog shows which one it's waiting for (instead of the plugin quietly doing nothing), and switching it off warns about the plugins that need it.
- **Added plugins sharing with each other** would go through a host registry if that ever comes up for real. One plugin registers a service under a name and the others ask `host.service(name)` and get None when it isn't there. They still never import each other.

Build each piece together with the first plugin that needs it, since what the service should look like depends on that plugin.

## Why

Before these rules, plugins had to guess whether a stream had stopped or someone had just clicked another tile. Sources called about 25 private window methods and one plugin imported another one's helper. Each of those worked until someone changed the other side. These rules write the fixes down so they stick.

## What's checked automatically

`tests/test_architecture.py` fails when a plugin imports another plugin or the window or calls a private host method. It also fails when `sources.py`, `session.py`, `stream_flow.py` or `plugin.py` import the window. `tests/test_stream_flow.py` checks who hears which hook when streams start, stop and change focus, without a window. `tests/test_plugin_removal.py` checks that every optional plugin can be left out. `tests/test_plugin_api.py` checks that the plugin API only changes on purpose. When you add a rule here that a test could check, add the test too.

## Known exceptions

These places don't follow the rules yet. Don't add to this list without a good reason.

- The window talks to a few built-in plugins directly through `_plugin(name)`. It asks Connection which phone is picked, to wake it and for its routes. It asks Setup for the canvas size, Stream output for the stream size and what the phone opens at, and Shortcuts to run `--action`. Connection is required for exactly this reason. The others are optional and the window checks they're there before using them.
- Connection reads Stream output's `format` setting with `plugin_config` to pick the phone's video URL.
- Presets saves and loads the settings of every plugin that keeps per-phone settings through `plugin_config` and `apply_preset`. That's what Presets is for, so it stays.
