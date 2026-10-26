# Plugins

Most of the desktop app is plugins. The Camera card is one and so are Transforms, the microphone, the wait screen and keyboard shortcuts. You can switch off the ones you don't use or install plugins other people made.

Want to write one? That's in [Writing a plugin](writing-plugins.md).

## Switching plugins on and off

1. Click the settings button next to **Start streaming** and open the **Plugins** tab
2. Flip the switch next to a plugin. Telescope asks first and tells you what will be missing
3. Click **Restart now** on its row (a running stream stops for the few seconds it takes)

Switching a plugin off deletes nothing. Its settings stay saved and everything is back once you switch it on again. **Turn all back on** does that for every built-in plugin.

- **Optional** holds the plugins that start out off (Auto framing, Background, Expose for my face and Privacy blur). They need a separate download the first time, see [the vision download](features.md#the-vision-download)
- **Always on** holds Phones and connection, Advanced settings and Updates, since Telescope doesn't work without them

**A card or setting is gone?** Check the Plugins tab. When some are off its name reads **Plugins (2 off)** and right after a restart a banner at the top names them.

## Installing a plugin someone made

Shared plugins come as a `.zip` file.

1. Open the **Plugins** tab in Settings
2. Click **Install** under **Added** and pick the `.zip` (or drag the file onto the tab)
3. Switch it on under **Added**. New plugins start out off
4. Click **Restart now**

> [!WARNING]
> A plugin is Python code that runs inside Telescope with no sandbox. It can read and change your files, use the network and start programs, like anything else running as you. Only install plugins from a source you trust.

Good to know:

- **Updating:** click **Remove** on the old version and install the new `.zip`. The new one starts out off too, so you always decide whether to trust it
- **Changed files:** if anything in a plugin's folder changes after you switched it on, Telescope won't run it until you switch it on again. Plugins never update themselves behind your back
- **Broken plugins:** if one fails to start, Telescope starts without it and shows a banner. The Plugins tab shows what went wrong under its name and **Copy diagnostics** lists it
- **The folder:** installed plugins live in `~/.config/telescope/plugins` on Linux and `%APPDATA%\Telescope\plugins` on Windows. **Open plugins folder** on the tab takes you there, and copying a plugin's folder in by hand works too

### When an added plugin won't switch on

The Plugins tab says why under its name:

| It says | What to do |
|---|---|
| It needs a newer Telescope | Update Telescope |
| It was made for an older Telescope | Ask its author for an update |
| A plugin that comes with Telescope already has that name | The plugin needs renaming |
| Other users on this computer could change it | Fix the folder's permissions or install it again with **Install** |
| It contains a link to somewhere else | Plugins have to be plain files and folders |
| It contains compiled code | Plugins have to be readable Python, with no `.pyc`, `.so` or `.pyd` files |
