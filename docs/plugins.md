# Plugins

Most of the desktop app is made of plugins. The Camera card is one, so are Transforms, the microphone, the wait screen and keyboard shortcuts. You can switch the ones you don't use off, install plugins other people made, and write your own.

This page has three parts. The first two are for everyone, the last one is for people who want to write some Python.

- [Switching plugins on and off](#switching-plugins-on-and-off)
- [Installing a plugin someone made](#installing-a-plugin-someone-made)
- [Making your own plugin](#making-your-own-plugin)

## Switching plugins on and off

Click the settings button next to **Start Streaming** and pick **Plugins…**. Every plugin is listed with a sentence about what it does. Untick one to switch it off, and Telescope asks first and tells you what will disappear.

A change takes effect the next time Telescope starts. The dialog shows a **Restart now** button whenever there's something to apply (if you're streaming, the stream stops for the few seconds the restart takes).

Switching a plugin off doesn't delete anything. Its settings stay saved, and switching it back on brings everything back the way it was. **Turn all back on** switches every built-in plugin on again in one go.

Three plugins can't be switched off, because Telescope doesn't work without them: Phones and connection, Advanced settings and Updates. They're listed at the bottom under "Always on".

**A card or a menu entry is gone?** Check the settings menu. When some plugins are off, it reads something like **Plugins… (2 off)**, and right after a restart that switched one off a banner at the top of the window also names it.

## Installing a plugin someone made

Plugins people share come as a `.zip` file. To install one:

1. Open **Plugins…** from the settings menu.
2. Click **Install plugin…** and pick the `.zip` (or drag the file onto the Plugins window).
3. Tick it under **Added** (new plugins start out switched off).
4. Click **Restart now**.

When you switch on a plugin you installed, Telescope warns you first. That's because a plugin is a small program and it can do anything Telescope itself can on your computer, like reading files or using the network. Only switch on plugins from people you trust.

A few more things worth knowing:

- **Updating a plugin.** Remove the old version with **Remove** and install the new `.zip`. Telescope switches it off until you switch it on again, so you always get to decide about a new version.
- **A plugin whose files changed is switched off.** If anything in a plugin's folder changes after you switched it on, Telescope won't run it until you switch it on again. Plugins never update themselves behind your back.
- **A plugin that breaks doesn't take Telescope down with it.** If a plugin fails while starting, Telescope leaves it out, starts without it and shows a banner. The Plugins dialog shows what went wrong under its name, and **Copy diagnostics** lists it as one that didn't start.
- **The plugins folder.** Installed plugins live in a `plugins` folder next to Telescope's settings: `~/.config/telescope/plugins` on Linux and `%APPDATA%\Telescope\plugins` on Windows. **Open plugins folder** in the dialog takes you there. You can also put a plugin's folder in there by hand instead of using the `.zip`.

If an added plugin won't switch on, the dialog shows the reason under it. The usual ones:

| What it says | What it means |
|---|---|
| It needs a newer Telescope | The plugin was made for a newer version. Update Telescope. |
| It was made for an older Telescope | Telescope changed in a way the plugin doesn't know about yet. Ask its author for an update. |
| A plugin that comes with Telescope already has that name | Two plugins can't share a name. The plugin needs renaming. |
| Other users on this computer could change it | Its folder can be changed by someone else on this computer, so running it isn't safe. Fix the folder's permissions, or install it again with **Install plugin…**. |
| It contains a link to somewhere else | Plugins have to be plain files and folders. |
| It contains compiled code | Plugins have to be Python source code people can read, without `.pyc`, `.so` or `.pyd` files. |

## Making your own plugin

A plugin is a folder with two files: `plugin.json`, which describes it, and `__init__.py`, which holds the Python code. If you've written a little Python before, you can write a plugin. You don't need to understand the rest of Telescope first.

### Start from the example

The repository has a small example plugin in [`desktop/examples/plugins/black_and_white`](../desktop/examples/plugins/black_and_white). It adds a card with one checkbox that turns the picture black and white, and it remembers the setting for each phone. Copying it is the fastest way to start.

To try it:

1. Set up the desktop app from source and start a dev copy with `python scripts/dev_desktop.py --keep` in `desktop/` (see [Building and CI](building.md)). The dev copy has its own settings and its own plugins folder, so whatever you try can't mess up your normal Telescope.
2. In the dev copy, open **Plugins…** and click **Open plugins folder**.
3. Copy the `black_and_white` folder into it.
4. Back in **Plugins…**, it's listed under **Added**. Tick it, then **Restart now**.

The Black and white card is now in the right column. Every time you change a file, switch the plugin on again in **Plugins…** and restart. That's the "files changed" protection doing its job.

### plugin.json

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

- `name` is your plugin's id. It has to match the folder's name and the `name` in your class, and use only lowercase letters, digits and `_` (like `my_overlay`). Your settings are saved under it, so don't change it later.
- `title`, `version`, `author` and `description` are what people see in the Plugins dialog. Write the description for someone who has never heard of your plugin.
- `api` is the plugin API version you wrote it for. It's `1` for now. Telescope raises it only when a change would break existing plugins, and a plugin with the wrong number isn't loaded.
- `class` is the name of your plugin class in `__init__.py`.

### __init__.py

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

`setup()` runs once at the start and gets two things: `host`, the window, with everything a plugin may ask of it (save settings, start or stop a stream, show a banner, and so on), and `bus`, the event bus, where you can listen for things happening, like a stream starting or a phone being paired. Both are described in [`desktop/telescope/plugin.py`](../desktop/telescope/plugin.py), which is the whole plugin API in one file.

`process_frame()` gets every frame of the stream as a NumPy array (BGR, height × width × 3) and returns the one to send on. `frame_step()` hands Telescope a frozen copy of that for streams the panels aren't showing, when several phones stream at once. Return `None` there if your plugin shouldn't touch those.

`get_config()` and `set_config()` are how settings get saved. Return a plain dict of what you want remembered and take it back in `set_config()`. Telescope calls `get_config()` when it saves, so call `host.schedule_save()` when something changes. Since `set_config()` can also get a dict saved by an older version of your plugin (or edited by hand), read it carefully, like `cfg.get("on") is True` does in the example.

### What else a plugin can do

All of these are optional. Override only what you need.

| Method | What it's for |
|---|---|
| `create_panel()` | A card in the main window |
| `create_header_widget()` | A small control in the header bar, next to the phone picker (or on the right with `header_side = "right"`) |
| `create_menu_actions()` | Entries in the settings menu, like a dialog of your own |
| `create_tray_actions()` | Entries in the tray icon's menu |
| `create_actions()` | Things people can bind keyboard shortcuts to |
| `process_frame()`, `frame_step()` | Change the picture |
| `on_stream_start()`, `on_stream_stop()` | A stream really started or ended (`ctrl` lets you send commands to the phone) |
| `on_focus_gained()`, `on_focus_lost()` | The window switched to or from a stream that keeps running (with several phones streaming) |
| `on_phone_state()` | The phone sent its current state: lenses, battery and so on |
| `on_plugins_ready()` | Every plugin is set up and settings are loaded |
| `diagnostics()` | A few lines for Copy diagnostics (no tokens, addresses or names) |
| `shutdown()` | Telescope is quitting, so stop anything you started |

You can also set `title`, `about` and `when_off` on your class. `title` and `about` fall back to your `plugin.json`. `when_off` says what's missing without your plugin (like "Without it there's no Black and white card") and the Plugins dialog shows it when someone switches your plugin off.

### A few ground rules

These keep plugins from breaking each other or Telescope. The [architecture notes](../desktop/ARCHITECTURE.md) have the reasons.

- **Don't reach into other plugins.** Use the bus and the host. Other plugins might be switched off, so your plugin has to work without them.
- **Only call the public host methods** (the ones listed in `plugin.py`). Anything starting with `_` can change without warning.
- **Keep the window responsive.** Hooks run on the window's thread, so don't wait for the network or the disk there. Do slow work in a thread and come back with a Qt signal.
- **Keep `process_frame()` quick.** It runs for every frame on a decoding thread. If it takes too long the stream falls behind. If it raises, that frame goes through unchanged, and if it keeps failing Telescope skips it until the next stream.
- **Don't write files into your plugin's folder.** A new or changed file there counts as a change, so your plugin would be switched off at the next start. Keep settings in `get_config()`, and put anything else somewhere outside the plugins folder.
- **Ship source code.** A plugin with compiled files in it (`.pyc`, `.so`, `.pyd`) isn't loaded, and Telescope clears `__pycache__` before loading one, so what runs is always the code people can read.
- **Use what Telescope comes with.** That's PyQt6, NumPy, OpenCV and Python's standard library. Telescope can't install anything else for your plugin. The Windows app only carries the parts of the standard library Telescope uses itself, so try your plugin there too. If an import is missing there the Plugins dialog shows it as the reason the plugin didn't start. If your plugin is more than one file, import your own modules relatively (`from . import helpers`).

If something in your plugin raises an exception, Telescope logs it and carries on, and the other plugins still hear about everything. The log file (`telescope.log`) is in the `logs` folder of your dev profile, or in Telescope's temp folder for a normal install (`/tmp/telescope-<user>` on Linux, `%TEMP%\Telescope` on Windows).

### Sharing it

Zip your plugin's folder so the `.zip` holds that one folder (not just the files inside it):

```
black_and_white.zip
└── black_and_white/
    ├── plugin.json
    └── __init__.py
```

Bump `version` in `plugin.json` whenever you share a new one. People install it with **Install plugin…**, and remember that they'll have to switch your plugin on again after each update.
