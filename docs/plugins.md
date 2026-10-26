# Plugins

Most of the desktop app is plugins. The Camera card is one and so are Transforms, the microphone, the wait screen and keyboard shortcuts. You can switch off the ones you don't use or install plugins other people made.

Want to write one? That's in [Writing a plugin](writing-plugins.md).

## Switching plugins on and off

1. Click the settings button next to **Start streaming** and open the **Plugins** tab
2. Flip the switch next to a plugin. Telescope asks first and tells you what will be missing
3. Click **Restart now** on its row (a running stream stops for the few seconds it takes)

Switching a plugin off deletes nothing. Its settings stay saved and everything is back once you switch it on again. **Turn all back on** does that for every built-in plugin.

- **Optional** holds the plugins that start out off (Auto framing, Background, Expose for my face, Privacy blur, Photos and On Air sign). Auto framing, Background, Expose for my face and Privacy blur need a separate download the first time, see [the vision download](features.md#the-vision-download)
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

- **Updating:** install the new `.zip` the same way. Telescope asks before it replaces the old version and the plugin's settings stay as they were. The new version starts out switched off like any plugin you add (you decide again whether to trust it)
- **Changed files:** if anything in a plugin's folder changes after you switched it on, Telescope won't run it until you switch it on again. Plugins never update themselves behind your back
- **Broken plugins:** if one fails to start, Telescope starts without it and shows a banner. The Plugins tab shows what went wrong under its name and **Copy diagnostics** lists it
- **The folder:** installed plugins live in `~/.config/telescope/plugins` on Linux and `%APPDATA%\Telescope\plugins` on Windows. **Open plugins folder** on the tab takes you there, and copying a plugin's folder in by hand works too

### Updates

A plugin that gives a link to its source code shows it next to its author. Click it to look at the code or grab a new version from there.

For plugins hosted on GitHub that publish their builds as GitHub Releases, Telescope checks once a day whether a newer version is out and mentions it under the plugin's name with a link to the release page. Nothing gets downloaded until you grab the new `.zip` from there and install it over the old one. If GitHub can't be reached or is limiting how often Telescope may ask, it waits and tries again later without bothering you. **Check for updates** at the bottom of **Added** switches the check off.

> [!CAUTION]
> **Install updates without asking** under **Added** installs new versions of the plugins you switched on and switches them on again before you get a look at them. Whoever controls a plugin's repo can then run new code on your computer every time they publish a release. That includes anyone who gets into the author's GitHub account. It starts out off and Telescope asks before it goes on. Leave it off unless you trust everyone behind every plugin you use with that.

An update installed this way runs from the next time Telescope starts and the Plugins tab offers **Restart now** on its row. Each release gets one try. If it won't install or holds a different plugin Telescope leaves it and waits for the next release.

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
