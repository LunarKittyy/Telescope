# Plugins

Most of the desktop app is made of plugins. The Camera card is one and so are Transforms, the microphone, the wait screen and keyboard shortcuts. You can switch off the ones you don't use, install plugins other people made or write your own.

The first two parts of this page are for everyone. The last one is for people who want to write some Python.

- [Switching plugins on and off](#switching-plugins-on-and-off)
- [Installing a plugin someone made](#installing-a-plugin-someone-made)
- [Making your own plugin](#making-your-own-plugin)

## Switching plugins on and off

Click the settings button next to **Start Streaming** and pick **Plugins…**. Every plugin is listed there with a sentence about what it does. Untick one to switch it off. Telescope asks first and tells you what will be missing without it.

Changes apply the next time Telescope starts. If there's something to apply the dialog shows a **Restart now** button (if you're streaming, the stream stops for the few seconds the restart takes).

Switching a plugin off doesn't delete anything. Its settings stay saved and everything is back as before once you switch it on again. **Turn all back on** switches every built-in plugin on in one go.

Phones and connection, Advanced settings and Updates can't be switched off because Telescope doesn't work without them. They're listed at the bottom under "Always on".

**A card or a menu entry is gone?** Look at the settings menu. When some plugins are off the entry reads something like **Plugins… (2 off)**. Right after a restart that switched one off there's also a banner at the top of the window naming it.

## Installing a plugin someone made

Plugins people share come as a `.zip` file. To install one:

1. Open **Plugins…** from the settings menu.
2. Click **Install plugin…** and pick the `.zip` (or drag the file onto the Plugins window).
3. Tick it under **Added** (new plugins start out switched off).
4. Click **Restart now**.

Telescope warns you before you switch on a plugin you installed. A plugin is a small program and it can do anything Telescope itself can on your computer, like reading your files or using the network. Only switch on plugins from people you trust.

A few more things worth knowing:

- **Updating a plugin.** Remove the old version with **Remove** and install the new `.zip`. The new version starts out switched off too, which means you always decide whether to trust it.
- **A plugin whose files changed is switched off.** If anything in a plugin's folder changes after you switched it on, Telescope won't run it until you switch it on again. Plugins never update themselves behind your back.
- **A broken plugin doesn't take Telescope down with it.** If a plugin fails while starting, Telescope starts without it and shows a banner about it. The Plugins dialog shows what went wrong under its name and **Copy diagnostics** lists it as one that didn't start.
- **The plugins folder.** Installed plugins live in a `plugins` folder next to Telescope's settings. That's `~/.config/telescope/plugins` on Linux and `%APPDATA%\Telescope\plugins` on Windows. **Open plugins folder** in the dialog takes you there. You can also copy a plugin's folder in there by hand instead of using the `.zip`.

If an added plugin won't switch on, the dialog shows why under its name. The usual reasons are:

| What it says | What it means |
|---|---|
| It needs a newer Telescope | The plugin was made for a newer version. Update Telescope. |
| It was made for an older Telescope | Telescope changed in a way the plugin doesn't know about yet. Ask its author for an update. |
| A plugin that comes with Telescope already has that name | Two plugins can't share a name. The plugin needs renaming. |
| Other users on this computer could change it | Someone else on this computer can change its files, which makes running it unsafe. Fix the folder's permissions or install it again with **Install plugin…**. |
| It contains a link to somewhere else | Plugins have to be plain files and folders. |
| It contains compiled code | Plugins have to be Python source code that people can read, without `.pyc`, `.so` or `.pyd` files. |

## Making your own plugin

A plugin is a folder with two files in it. `plugin.json` describes the plugin and `__init__.py` holds its Python code. If you've written a little Python before you can write a plugin, and you don't need to understand the rest of Telescope first.

### Start from the example

The repository has a small example plugin in [`desktop/examples/plugins/black_and_white`](../desktop/examples/plugins/black_and_white). It adds a card with one checkbox that turns the picture black and white, and each phone remembers its own setting. Copying it is the quickest way to start.

To try it, set up the desktop app from source (see [Building and CI](building.md)) and run this in `desktop/`:

```bash
python scripts/dev_desktop.py --plugin examples/plugins/black_and_white
```

That starts a dev copy of Telescope with its own settings, which keeps your normal Telescope safe from whatever you try. The plugin is loaded straight from its folder and is always on, and the Black and white card shows up in the right column.

For your own plugin copy the folder somewhere, rename it and change `name` in both files to match. Then point `--plugin` at your copy. You can give `--plugin` more than once to load several plugins.

### Trying out changes

Leave the dev copy running while you work. Every time you save a file in your plugin's folder Telescope restarts by itself with the new code. If it was streaming it starts streaming again once it's back. Errors from your plugin show up with their tracebacks in the terminal you started it from, and if your plugin didn't start at all the Plugins dialog says why under its name.

For a quick check that doesn't open Telescope run this:

```bash
python scripts/check_plugin.py path/to/my_plugin
```

It loads your plugin next to the built-in ones in a throwaway profile and builds its card. Then it sends it a test frame, saves and restores its settings and takes it through a stream starting and stopping. Anything that goes wrong is listed with a traceback pointing into your code. It also tells you when `process_frame()` is slow enough to make a stream fall behind.

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

- `name` is your plugin's id. It has to match the folder's name and the `name` in your class, using only lowercase letters, digits and `_` (like `my_overlay`). Your settings are saved under it, so don't change it later.
- `title`, `version`, `author` and `description` are what people see in the Plugins dialog. Write the description for someone who has never heard of your plugin.
- `api` is the version of the plugin API you wrote it for, which is `1` for now. Telescope only raises it for a change that would break existing plugins, and everything in `plugin.py` keeps working until then (Telescope's own tests check that). A plugin with the wrong number isn't loaded.
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

`setup()` runs once at the start and gets two things. `host` is the window and has everything a plugin may ask of it, like saving settings, starting or stopping a stream or showing a banner. `bus` is the event bus where you can listen for things happening, like a stream starting or a phone being paired. Both are described in [`desktop/telescope/plugin.py`](../desktop/telescope/plugin.py), which is the whole plugin API in one file.

`process_frame()` gets every frame of the stream as a NumPy array (BGR, height × width × 3) and returns the frame to send on. When several phones stream at once, `frame_step()` hands Telescope a frozen copy of that for the streams the panels aren't showing. Return `None` there if your plugin shouldn't touch those.

`get_config()` and `set_config()` are how settings get saved. Return a plain dict of what you want remembered and read it back in `set_config()`. Telescope calls `get_config()` whenever it saves, and `host.schedule_save()` asks it to save after something changed. `set_config()` might also get a dict saved by an older version of your plugin (or edited by hand) so read it carefully, the way the example checks `cfg.get("on") is True`.

### What else a plugin can do

All of these are optional and you only override what you need.

| Method | What it's for |
|---|---|
| `create_panel()` | A card in the main window |
| `create_header_widget()` | A small control in the header bar next to the phone picker (or on the right with `header_side = "right"`) |
| `create_menu_actions()` | Entries in the settings menu, like one that opens a dialog of your own |
| `create_tray_actions()` | Entries in the tray icon's menu |
| `create_actions()` | Things people can bind keyboard shortcuts to |
| `process_frame()`, `frame_step()` | Changing the picture |
| `on_stream_start()`, `on_stream_stop()` | A stream really started or ended (`ctrl` lets you send commands to the phone) |
| `on_focus_gained()`, `on_focus_lost()` | The window switched to or away from a stream that keeps running (with several phones streaming) |
| `on_phone_state()` | The phone sent its current state with its lenses, battery and so on |
| `on_plugins_ready()` | Every plugin is set up and the settings are loaded |
| `diagnostics()` | A few lines for Copy diagnostics (no tokens, addresses or names) |
| `shutdown()` | Telescope is quitting and you should stop anything you started |

You can also set `title`, `about` and `when_off` on your class. `title` and `about` fall back to your `plugin.json`. `when_off` says what's missing without your plugin (like "Without it there's no Black and white card") and the Plugins dialog shows it when someone switches your plugin off.

### A few ground rules

These keep plugins from breaking each other or Telescope. The [architecture notes](../desktop/ARCHITECTURE.md) explain why.

- **Don't reach into other plugins.** Use the bus and the host instead. Other plugins might be switched off and yours has to work without them.
- **Only call the public host methods** (the ones listed in `plugin.py`). Anything starting with `_` can change without warning.
- **Keep the window responsive.** Hooks run on the window's thread, so don't wait for the network or the disk there. Do slow work in a thread and come back with a Qt signal.
- **Keep `process_frame()` quick.** It runs for every frame on a decoding thread and the stream falls behind if it takes too long. A frame it raises on goes through unchanged. If it keeps failing Telescope skips it until the next stream.
- **Don't write files into your plugin's folder.** Any new or changed file there counts as a change and your plugin would be switched off at the next start. Keep settings in `get_config()` and put anything else outside the plugins folder.
- **Ship source code.** A plugin with compiled files in it (`.pyc`, `.so`, `.pyd`) isn't loaded. Telescope also clears `__pycache__` before loading one to make sure what runs is the code people can read.
- **Use what Telescope comes with.** That's PyQt6, NumPy, OpenCV and Python's standard library, and Telescope can't install anything else for your plugin. The Windows app only carries the parts of the standard library that Telescope itself uses, so try your plugin there too. If an import is missing there the Plugins dialog shows it as the reason the plugin didn't start. A plugin with more than one file imports its own modules relatively (`from . import helpers`).

When something in your plugin raises an exception Telescope logs it and carries on, and the other plugins still hear about everything. In the dev copy you see it in the terminal. A normal install writes it to `telescope.log` in Telescope's temp folder (`/tmp/telescope-<user>` on Linux, `%TEMP%\Telescope` on Windows).

### Sharing it

Run `check_plugin.py` one more time first. Then zip your plugin's folder so the `.zip` holds that one folder and not just the files inside it (leave out `.git` and the like):

```
black_and_white.zip
└── black_and_white/
    ├── plugin.json
    └── __init__.py
```

Bump `version` in `plugin.json` whenever you share a new one. People install it with **Install plugin…** and have to switch your plugin on again after each update.
