# How the desktop app is put together

The short version of how the desktop app is split up and which rules keep it that way. For anyone changing the code, human or agent. [MODULES.md](MODULES.md) goes through it file by file and [docs/writing-plugins.md](../docs/writing-plugins.md) is the guide to writing a plugin.

## The pieces

- **The window** (`telescope/app.py`, `TelescopeWindow`) is the host. It owns the event bus, saves settings, lays out the cards and builds the Settings window. It doesn't know what any card does
- **Plugins** (`telescope/plugins/`) are the cards and most features. Each is a `TelescopePlugin` subclass and `builtin_plugins()` lists them in registration order
- **The plugin API** (`telescope/plugin.py`) has the plugin hooks, `HostServices` (everything a plugin may call on the window) and the `EventBus` signals. A plugin only talks to the app through these
- **Sources** (`sources.py`) are what a stream comes from: a paired phone or a plugin's `StreamSource`
- **Sessions** (`session.py`) track which sources stream, which one the panels show and what's still starting. No Qt in it
- **The stream flow** (`stream_flow.py`) starts and stops streams and moves the panels between them
- **Added plugins** are loaded from the user's plugins folder by `plugin_loader.py`

## The rules

**Plugins don't know about each other.** Never import another plugin or call its methods. To share something:

- put an event or state on the `EventBus` (add a signal to `plugin.py` with a docstring saying when it fires)
- go through `HostServices` for anything the window already knows, like `host.canvas_size()`
- move a plain shared helper to a module outside `plugins/`

**Every plugin except Connection can be missing.** People switch plugins off and the app still has to start, stream and quit. If you need something another plugin provides, handle it not being there.

**Plugins only use the public host.** No `host._anything`. If a plugin needs more from the window, add a method to the right protocol in `plugin.py`, implement it on `TelescopeWindow` and note it in MODULES.md.

**Settings go in the Settings window as sections.** A plugin adds a `SettingsSection` to a tab from `create_settings_sections()`. Sections are built once and refresh in `on_show`.

**Don't count on registration order.** While one plugin is in `setup()` the ones after it don't exist yet. Emit anything others follow again from `on_plugins_ready()`.

**A frame's path is written down.** Each plugin that changes frames names its `frame_stage` from `FRAME_STAGES` in `plugin.py` (view, geometry, reading, background, effects, colour, privacy, overlay, combine, cover, final) and the frames go through the stages in that order. Pick the stage by what has to come before and after, never by moving a plugin in the list in `plugins/__init__.py`, which only orders plugins within a stage. `tests/test_stream_flow.py` writes the built-in order down.

**Cards fold without plugins knowing.** The window folds cards from what the helpers in `widgets/common.py` built. Don't list card titles in `collapse.py` and don't call fold code from a plugin. Build the card completely inside `create_panel()`.

**Each hook means one thing.** `on_stream_start` and `on_stream_stop` are for a stream that really starts or ends. `on_focus_gained` and `on_focus_lost` are for the panels moving between streams that keep running. Signals about a stream carry its source id, so a card about the shown stream compares it with `host.focused_source_id()`.

**Text is translated where it's shown.** Wrap what people see in `tr()` from `telescope.i18n` when it's shown, never at import time: the language loads after the modules do, so a class attribute or module constant holds `N_("...")` and gets its `tr()` at the place it's displayed. Anything kept as an identity or saved (a plugin's `title` passed to `take_view`, a fold key, a canvas preset, a shortcut's choice) stays in English, and the window reads a shown text back to English with `english()` where it needs to. Diagnostics stay in English for bug reports. [docs/translating.md](../docs/translating.md) has the details.

**The window drives and nothing reaches back.** `sources.py`, `session.py`, `stream_flow.py` and `plugin.py` never import `app.py`. Session state lives in `Sessions` and start, stop and focus changes in `StreamFlow`. Anything they need from the window goes through `SourceHost` or `FlowHost`.

## Keeping plugins working

Everything a plugin can use is listed in `tests/plugin_api.txt` and `tests/test_plugin_api.py` fails when any of it changes.

- **Adding is fine.** Write the new list with `UPDATE_PLUGIN_API=1 python -m pytest tests/test_plugin_api.py` and commit it
- **Never add a required argument to a hook or change what a signal carries.** Add a new one next to it and fire both
- **Renaming or removing** means keeping the old name working for at least one release, logging a warning once
- **A hook means what its docstring says.** Calling it at another moment breaks plugins as much as changing its arguments
- **Only when the old behaviour can't be kept,** raise `PLUGIN_API` in `plugin_loader.py` (and `OLDEST_PLUGIN_API` if old plugins can't run at all) and say so in the release notes

## Shared pieces

Plugins still don't depend on each other. Something two of them need becomes a host service built together with the first plugin that needs it, as the view (`view.py`, `HostView`), the vision service (`vision.py`, `HostVision`), calm people tracking (`people.py`) motion zones (`motion.py`, `HostMotion`), the preview overlays with their picks (`overlays.py` and `scopes.py`, `HostPreview`), the colour pipeline (`colour.py`, `HostColour`) the phone's sensors (`sensors.py`, `HostSensors`) and frames from other streams (`frames.py`, `HostFrames`) were. Planned and not built yet: a `needs` list in `plugin.json` naming built-in plugins a plugin can't work without, and a host registry (`host.service(name)`) for added plugins to share with each other.

## What's checked automatically

- `tests/test_architecture.py`: no plugin imports another plugin or the window, no private host calls, and the modules above never import the window
- `tests/test_plugin_removal.py`: every optional plugin can be left out
- `tests/test_plugin_api.py`: the plugin API only changes on purpose
- `tests/test_stream_flow.py`: who hears which hook as streams start, stop and change focus
- `tests/test_collapse.py`: card folding, in any language
- `tests/test_i18n.py`: no text is translated at import time, every `tr()` call fills in its text's fields, and the catalogs are up to date

When you add a rule that a test could check, add the test too.

## Known exceptions

Don't add to this list without a good reason.

- The window talks to a few built-in plugins directly through `_plugin(name)`: Connection (the picked phone, waking it, routes), Setup (canvas size), Stream output (stream size) and Shortcuts (`--action`). Only Connection is required, so the window checks the others are there
- Connection reads Stream output's `format` with `plugin_config` to pick the phone's video URL
- Presets saves and loads other plugins' settings through `plugin_config` and `apply_preset`, which is what Presets is for
