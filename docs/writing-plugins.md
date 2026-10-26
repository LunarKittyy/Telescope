# Writing a plugin

This page is for people who write Python. To switch plugins on and off or install ones other people made, see [Plugins](plugins.md).

- [Start from the example](#start-from-the-example)
- [Trying out changes](#trying-out-changes)
- [plugin.json](#pluginjson)
- [__init__.py](#__init__py)
- [Folding the card](#folding-the-card)
- [What else a plugin can do](#what-else-a-plugin-can-do)
- [The view and people in the picture](#the-view-and-people-in-the-picture)
- [Translating your plugin](#translating-your-plugin)
- [Ground rules](#a-few-ground-rules)
- [Sharing it](#sharing-it)

A plugin is a folder with two files in it. `plugin.json` describes the plugin and `__init__.py` holds its Python code. If you've written a little Python before you can write a plugin and you don't need to understand the rest of Telescope first.

## Start from the example

The repository has a small example plugin in [`desktop/examples/plugins/black_and_white`](../desktop/examples/plugins/black_and_white). It adds a card with one checkbox that turns the picture black and white and each phone remembers its own setting. Copying it is the quickest way to start.

To try it, set up the desktop app from source (see [Building and CI](building.md)) and run this in `desktop/`:

```bash
python scripts/dev_desktop.py --plugin examples/plugins/black_and_white
```

That starts a dev copy of Telescope with its own settings, which keeps your normal Telescope safe from whatever you try. The plugin is loaded straight from its folder and always switched on. Its Black and white card shows up in the right column.

For your own plugin copy the folder somewhere, rename it and change `name` in both files to match. Then point `--plugin` at your copy. You can give `--plugin` more than once to load several plugins.

## Trying out changes

Leave the dev copy running while you work. Every time you save a file in your plugin's folder Telescope restarts by itself with the new code. If it was streaming it starts streaming again once it's back. Errors from your plugin show up with their tracebacks in the terminal you started it from and if your plugin didn't start at all the Plugins tab says why under its name.

For a quick check that doesn't open Telescope run this:

```bash
python scripts/check_plugin.py path/to/my_plugin
```

It loads your plugin next to the built-in ones in a throwaway profile and builds its card. Then it sends it a test frame, saves and restores its settings and takes it through a stream starting and stopping. Anything that goes wrong is listed with a traceback pointing into your code. It also tells you when `process_frame()` is slow enough to make a stream fall behind.

## plugin.json

```json
{
  "name": "black_and_white",
  "title": "Black and white",
  "version": "1.0",
  "author": "Telescope",
  "description": "Turns the picture black and white. An example to start your own plugin from.",
  "api": 1,
  "class": "BlackAndWhitePlugin"
}
```

- `name` is your plugin's id. It has to match the folder's name and the `name` in your class, using only lowercase letters, digits and `_` (like `my_overlay`). Your settings are saved under it, so don't change it later.
- `title`, `version`, `author` and `description` are what people see in the Plugins tab. Write the description for someone who has never heard of your plugin.
- `api` is the version of the plugin API you wrote it for, which is `1` for now. Telescope only raises it for a change that would break existing plugins and everything in `plugin.py` keeps working until then (Telescope's own tests check that). A plugin with the wrong number isn't loaded.
- `class` is the name of your plugin class in `__init__.py`.
- `repo` is an optional `https://` link to where your plugin's source lives, like `"repo": "https://github.com/you/your_plugin"` (the example has none). The Plugins tab shows it next to the author. When it's a GitHub repo Telescope can also tell people about new versions, see [Sharing it](#sharing-it). Anything that isn't a plain `https://` address is left out and `check_plugin.py` points it out.

## __init__.py

Here's the example's code with what each part is for:

```python
class BlackAndWhitePlugin(TelescopePlugin):
    name = "black_and_white"  # the same as "name" in plugin.json
    when_off = "Without it there's no Black and white card and the picture keeps its colours."
    panel_region = "right"  # which column the card goes in: "left", "right" or "center"
    device_local = True  # each phone remembers its own setting

    def setup(self, host, bus):
        self._host = host  # how you talk to Telescope
        self._on = False
        self._box = None

    def create_panel(self):
        ...  # build and return the card (a QWidget)

    def process_frame(self, frame):
        return _grey(frame) if self._on else frame

    def frame_step(self):
        return _grey if self._on else None

    def get_config(self):
        return {"on": self._on}

    def set_config(self, cfg):
        self._on = cfg.get("on") is True
        ...  # and update the checkbox to match
```

`setup()` runs once at the start and gets two things. `host` is the window and has everything a plugin may ask of it, like saving settings, starting or stopping a stream or showing a banner. `bus` is the event bus where you can listen for things happening, like a stream starting or a phone being paired. Both are described in [`desktop/telescope/plugin.py`](../desktop/telescope/plugin.py), which is the whole plugin API in one file.

`process_frame()` gets every frame of the stream as a NumPy array (BGR, height × width × 3) and returns the frame to send on. Plugins get the frame in the order they're registered, built-in ones first and the preview always shows it after every plugin had its turn. When several phones stream at once, `frame_step()` hands Telescope a frozen copy of that for the streams the panels aren't showing. Return `None` there if your plugin shouldn't touch those.

`get_config()` and `set_config()` are how settings get saved. Return a plain dict of what you want remembered and read it back in `set_config()`. Telescope calls `get_config()` whenever it saves and `host.schedule_save()` asks it to save after something changed. `set_config()` might also get a dict saved by an older version of your plugin (or edited by hand) so read it carefully, the way the example checks `cfg.get("on") is True`.

## Folding the card

People can click a card's title to fold the card down to its header and a section title inside it to fold just that section. There's nothing for you to write. After `create_panel()` returns Telescope looks at the card and makes it foldable if you built it with the helpers in `telescope/widgets/common.py`:

- `create_card()` and `card_layout(card)` make the card and its layout
- `add_card_header(layout, "Title", "icon")` adds the header and its title is what people click. A card without one is left alone
- `add_section_heading(layout, "Section", "icon")` starts a section that runs until the next heading or the end of the card

What to keep in mind while building the card:

- **Add every row inside `create_panel()`.** The rows get grouped once it returns. One you add to the card's own layout afterwards lands outside the fold and is still visible when the card is folded. Changing things inside a layout or widget you already added is fine at any time.
- **Show and hide your own rows as you like.** A fold hides a container around the rows and never changes a row's own visibility, so a row you hid stays hidden when the card is opened again.
- **Hiding the whole card still works.** `setVisible()` on the card is yours and a fold doesn't touch it.
- **The titles make up the saved name.** A fold is saved under your plugin's `name`, the card title and the section title. If you rename a title it starts out open again for people who had it folded. Two sections with the same title in one card are told apart by their order.
- **Only headings placed straight in the card's layout fold.** One inside a nested layout or widget is drawn like any other widget.

## What else a plugin can do

All of these are optional and you only override what you need.

| Method | What it's for |
|---|---|
| `create_panel()` | A card in the main window |
| `create_header_widget()` | A small control in the header bar next to the phone picker (or on the right with `header_side = "right"`) |
| `create_settings_sections()` | Settings of your own as `SettingsSection`s, a card each for a tab in the Settings window (see below) |
| `create_tray_actions()` | Entries in the tray icon's menu |
| `create_actions()` | Things people can bind keyboard shortcuts to |
| `process_frame()`, `frame_step()` | Changing the picture |
| `on_stream_start()`, `on_stream_stop()` | A stream really started or ended (`ctrl` lets you send commands to the phone) |
| `on_focus_gained()`, `on_focus_lost()` | The window switched to or away from a stream that keeps running (with several phones streaming) |
| `on_phone_state()` | The phone sent its current state with its lenses, battery and so on |
| `on_plugins_ready()` | Every plugin is set up and the settings are loaded |
| `diagnostics()` | A few lines for Copy diagnostics (no tokens, addresses or names) |
| `shutdown()` | Telescope is quitting and you should stop anything you started |

You can also set `title`, `about` and `when_off` on your class. `title` and `about` fall back to your `plugin.json`. `when_off` says what's missing without your plugin (like "Without it there's no Black and white card") and the Plugins tab shows it when someone switches your plugin off. With `in_presets = True` your settings get saved in presets along with the camera and the transforms and come back through `apply_preset()` (which calls `set_config()` unless you override it).

#### Settings of your own

`create_settings_sections()` returns a list of `SettingsSection(tab, widget)`, one for each block of controls you want in the Settings window. The tab is the id of one Telescope already has (`general`, `streaming`, `video`, `audio`, `devices`, `shortcuts`, `about`) or a new one, which gets its own tab named by `tab_title` and sits just above Plugins. Build the widget the way you would a card and let its controls save as they change, since the window has no OK button. The Settings search finds what's in your section by reading its widgets, so most of it needs nothing from you. To help people find it by other words, pass `keywords="..."` to `SettingsSection` (or set a `keywords` property on a widget). `on_show` is called each time your tab comes up, which is the place to bring the controls in line with your plugin's state. The old `create_menu_actions()` still works: its actions and submenus appear under the plugin's name in Settings → General, behind **Open settings**. Telescope logs a deprecation warning at start. Move to `create_settings_sections()` when you update the plugin.

## The view and people in the picture

Zoom, pan, flip and rotation belong to the window, not to the Transforms card, so a plugin can work with them even when Transforms is switched off. `host.view()` is the shown stream's view as it is now and `bus.view_changed` fires when it changes. `host.set_view(zoom=2.0)` changes it the way the Transforms controls do. A plugin that wants to move the zoom and pan by itself (like Auto framing) calls `host.take_view("My plugin")` first, then `host.steer_view(...)` as often as it likes and `host.release_view(...)` when it's done. Only one plugin can have the view at a time and the Transforms card shows who has it.

The vision service runs MediaPipe for plugins that ask. `host.person_mask(frame)` in `process_frame()` gives a mask of where the people are (`None` while it's still loading or if the download isn't there). `host.want_people("My plugin", True)` makes `bus.people_seen` report the people on each stream with their landmarks (eyes, shoulders, wrists, hips and so on) a dozen or so times a second while anything in the picture moves, and about 3 times a second while nothing does. `host.vision_state()` and `bus.vision_state_changed` tell you whether it's ready. The pose model drops people and finds them again all the time, so most plugins want `bus.people_tracked` instead, which comes right after each `people_seen` with steady `Track`s from `telescope.people` (an id, a smoothed box and face, how long they've been seen or missing) and a people count that goes up after two updates in a row and down only after someone has been gone for three seconds. `host.tracked_people()` gives the last one and `people.main(tracks)` picks whoever's been there longest. Plugins someone adds can use all of this, but only once the vision download is there, which right now only happens by switching on one of the optional plugins that use it.

Motion needs no download. `host.watch_motion("My plugin", [(0.6, 0.0, 1.0, 1.0)], sensitivity=5)` watches the right part of the picture (zones are 0..1 of the picture as shown at zoom 1, sensitivity goes from 1 to 10) and `bus.motion_seen` fires with the indexes of the zones something moves in whenever that changes. Slow light changes don't count. An empty list of zones stops it.

### Drawing on the preview and picking points there

Anything a plugin draws through `host.add_preview_layer("My plugin", layer)` shows on the preview and never reaches the virtual camera. `layer` is a subclass of `telescope.overlays.PreviewLayer` with up to three methods. `tint(frame)` gets the preview's own scaled-down copy of each frame on the UI thread and can colour it (Cinema tools draws peaking and zebras there). `analyse(frame)` runs on a thread of its own at most `analyse_fps` times a second, for anything slower like a scope, and returns True when the picture should be painted again. `paint(painter, rect)` draws with Qt over the frame, which sits at `rect` on the preview. `host.remove_preview_layer("My plugin")` takes it off again. With the Preview plugin switched off nothing is drawn, which is fine since there's nothing to look at either.

`host.pick_on_preview("My plugin", kind, on_done)` asks the person for a `"point"`, a `"rect"` or a `"quad"` (four corners they drag into place and finish with Enter). While it runs, clicks on the preview go to the pick instead of focusing or panning and Esc or a right click cancels it. `on_done` gets a `telescope.overlays.Pick`, or None if it was cancelled. `Pick.shown` has the points in the frame your `process_frame()` gets and `Pick.picture` is what the preview showed, for reading a colour where someone clicked. `Pick.full` has them in the camera's whole picture before zoom, pan, flip and rotation, which is what to save when the points should stay on the same thing while someone zooms around (`telescope.view.to_shown(host.view(), x, y)` turns them back into frame coordinates). A quad can start from corners you saved with `start=` and you can end it from your own button with `host.finish_pick()`. `bus.preview_pick_changed` says who's picking.

## Translating your plugin

Telescope can be set to other languages and your plugin's texts can follow in any language you add. Texts without a translation show as you wrote them. Wrap each one in `tr()` from `telescope.i18n`:

```python
from telescope.i18n import N_, tr, trn

label.setText(tr("Strength"))
label.setText(tr("Blurring {name}", name=phone_name))   # {fields} instead of f-strings or + between pieces
count.setText(trn("{n} face", "{n} faces", faces))      # anything with a number in it
TITLE = N_("My overlay")                                # at import time: mark it, then tr(TITLE) where it shows
```

`tr()`, `trn()` and `N_()` work the same as in Telescope's own code and they're part of the plugin API. The docstring at the top of [`i18n.py`](../desktop/telescope/i18n.py) has the rules. The one to remember is that `tr()` must not run while your module is being imported (module level, class bodies, default arguments) because the language isn't loaded yet. Import it as `from telescope.i18n import tr`, since the extractor looks for calls named `tr`.

Each language is a `.po` file in a `locale` folder inside your plugin:

```
my_overlay/
├── plugin.json
├── __init__.py
└── locale/
    ├── my_overlay.pot
    └── sv.po
```

Run these in `desktop/` of a Telescope checkout:

```bash
python scripts/i18n.py extract --plugin path/to/my_overlay       # finds your texts and writes locale/my_overlay.pot
python scripts/i18n.py new sv --plugin path/to/my_overlay        # starts locale/sv.po
```

Run `extract` again after changing a text. New texts are added to every `.po` file and translations still in use are left alone. `status` and `todo` take `--plugin` as well, and [Translating](translating.md) explains the `.po` format.

A text is looked up in your plugin's catalog first, then in Telescope's own (common words like "Camera" arrive translated already) and then it stays as you wrote it. A language you only half translated works fine. `sv_SE` uses `sv.po` when there's no `sv_SE.po`.

Your plugin follows the language Telescope shows, which is the one picked in Settings > General or the system's. That means you can add your texts to a language Telescope has but a `de.po` does nothing while Telescope has no German. Which plugin a `tr()` call belongs to is decided by the module it's written in, so it works from every file of your plugin.

A `.po` file that can't be read never stops your plugin from loading. The plugin stays in English and the log tells you which file it was. `check_plugin.py` goes through your catalogs and lists the broken ones, along with translations that have a `{field}` the English text doesn't (those show in English). Catalogs count as part of the plugin's files, so after changing one people have to switch the plugin on again like after any other change.

## A few ground rules

These keep plugins from breaking each other or Telescope. The [architecture notes](../desktop/ARCHITECTURE.md) explain why.

- **Don't reach into other plugins.** Use the bus and the host instead. Other plugins might be switched off and yours has to work without them.
- **Only call the public host methods** (the ones listed in `plugin.py`). Anything starting with `_` can change without warning.
- **Keep the window responsive.** Hooks run on the window's thread, so don't wait for the network or the disk there. Do slow work in a thread and come back with a Qt signal.
- **Keep `process_frame()` quick.** It runs for every frame on a decoding thread and the stream falls behind if it takes too long. A frame it raises on goes through unchanged. If it keeps failing Telescope skips it until the next stream.
- **Don't write files into your plugin's folder.** Any new or changed file there counts as a change and your plugin would be switched off at the next start. Keep settings in `get_config()` and put anything else outside the plugins folder.
- **Ship source code.** A plugin with compiled files in it (`.pyc`, `.so`, `.pyd`) isn't loaded. Telescope also clears `__pycache__` before loading one to make sure what runs is the code people can read.
- **Use what Telescope comes with.** That's PyQt6, NumPy, OpenCV and Python's standard library. Telescope can't install anything else for your plugin. The Windows app only carries the parts of the standard library that Telescope itself uses, so try your plugin there too. If an import is missing there the Plugins tab shows it as the reason the plugin didn't start. A plugin with more than one file imports its own modules relatively (`from . import helpers`).

When something in your plugin raises an exception Telescope logs it and carries on and the other plugins still hear about everything. In the dev copy you see it in the terminal. A normal install writes it to `telescope.log` in Telescope's temp folder (`/tmp/telescope-<user>` on Linux, `%TEMP%\Telescope` on Windows).

## Sharing it

Run `check_plugin.py` one more time first. Then zip your plugin's folder so the `.zip` holds that one folder and not just the files inside it (leave out `.git` and the like):

```
black_and_white.zip
└── black_and_white/
    ├── plugin.json
    └── __init__.py
```

Bump `version` in `plugin.json` whenever you share a new one. People install it with **Install** on the Plugins tab. That replaces the old version and its settings stay as they were. They have to switch your plugin on again after each update.

If `repo` points at GitHub you can publish each version as a GitHub Release with the `.zip` attached and tag it with the version number (`v1.3` or `1.3`, matching `"version": "1.3"`). Telescope compares the tag of your latest release with the installed `version` once a day and mentions a newer one on the Plugins tab. Name the file after your plugin (`black_and_white.zip`) when a release has more than one `.zip`. Drafts and pre-releases are skipped. People who switched on **Install updates without asking** get your release installed and running from their next start without seeing it first, so treat your GitHub account like the keys to their computers.
