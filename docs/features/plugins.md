<!-- covers: rust/crates/shell/src/plugins_tab.rs rust/crates/core/src/plugin_admin.rs rust/crates/plugin-host/src/admin.rs rust/crates/plugin-host/src/package.rs rust/crates/plugin-host/src/updates.rs rust/crates/plugin-host/src/switches.rs; checked: 88de924b5 -->
# Plugins

Most of the desktop app is plugins. The Camera card is one and so are Transforms, the microphone, the wait screen and keyboard shortcuts. You can switch off the ones you don't use or install plugins other people made.

Want to write one? That's in [Writing a plugin](../development/writing-plugins.md).

## Switching plugins on and off

1. Click the settings button next to **Start streaming** and open the **Plugins** tab
2. Flip the switch on a plugin's tile. Telescope shows a prompt first listing what will be missing
3. Click **Restart now** in the bar at the bottom of the tab (a running stream stops for the few seconds it takes)

The tiles are grouped by what the plugins are for (Camera, Look, People, Preview tools, On screen, Sound, Streaming and devices and Extras). Each group has its own colour and its plugins' cards in the main window take that colour too. Within a group they're in alphabetical order. Type in **Search plugins** to find one by its name or description. **All**, **On** and **Off** next to it show every plugin, only the ones switched on or only the ones switched off.

Switching a plugin off deletes no data. Once you switch it on again it is all back, settings included. **Reset to defaults** resets the built-in plugins to the standard selection, with the optional ones off and the rest on. The restart bar counts every change still waiting. You can flip several switches and restart only once. **Revert** in that bar sets the switches to how they are in the running app, without needing a restart. Removals and updates still wait for one and the bar shows how many.

Some tiles carry a badge:

- **Vision download required** is on Auto framing, Background and Expose for my face. They start out off and need a separate download the first time, see [the vision download](picture.md#the-vision-download). Its **Remove download** button sits next to the **People** heading
- **Required** marks Devices and connection, Advanced settings and Updates. Their switches are locked on since Telescope doesn't work without them
- **Added** marks plugins you installed yourself
- **Lab** sits next to a plugin's name when it's an early version. It works but can change or behave oddly while it's being finished
- **Limited** sits next to a plugin's name when it has technical limits that are not expected to go away. Don't expect it to work perfectly. A plugin can carry both
- **Restart required** shows on every plugin whose switch changed since Telescope started

**A card or setting is gone?** Check the Plugins tab. When some are off its name reads **Plugins (2 off)** and right after a restart a banner at the top names them. **Off** in the filter lists them all.

## Installing a plugin someone made

Shared plugins come as a `.zip` file.

1. Open the **Plugins** tab in Settings
2. Click **Install** next to **Added plugins** at the bottom of the tab and pick the `.zip` (or drag the file onto the tab)
3. Find its tile (it's under **Extras** unless the plugin picked another group) and switch it on. New plugins start out off
4. Click **Restart now** in the bar at the bottom

> [!WARNING]
> A plugin is a Lua script that runs inside Telescope. It can't open files, start programs or reach the network but it can change what your camera shows, control the phone and show notifications. Only install plugins from a source you trust.

Good to know:

- **Updating:** install the new `.zip` the same way. Telescope shows a prompt before it replaces the old version and the plugin's settings are left as they were. The new version starts out switched off like any plugin you add (you decide again whether to trust it)
- **Changed files:** if something in a plugin's folder changes after you switched it on, Telescope won't run it until you switch it on again. Plugins never update themselves behind your back
- **Broken plugins:** if one fails to start, Telescope starts without it and shows a banner. Its tile on the Plugins tab shows what went wrong and **Copy diagnostics** lists it
- **Vision pack zips:** Install also takes the vision pack (`Telescope-vision-<system>-<id>.zip`, or `Telescope-matting-<system>-<id>.zip` for Background's High quality). That isn't a plugin. Telescope checks it and installs it in the background and the download banner shows how it goes. If that pack is already downloading or installing, the file isn't used and you can try again once it's done
- **The folder:** installed plugins live in `~/.config/telescope/plugins` on Linux and `%APPDATA%\Telescope\plugins` on Windows. **Open plugins folder** on the tab takes you there and copying a plugin's folder in yourself works too

### Updates

A plugin that gives a link to its source code shows it next to its author. Click it to look at the code or grab a new version from there.

For plugins hosted on GitHub that publish their builds as GitHub Releases, Telescope checks once a day whether a newer version is out and mentions it on the plugin's tile with a link to the release page. No download happens until you grab the new `.zip` from there and install it over the old one. If GitHub can't be reached or is limiting how often Telescope can ask, the check is retried later without bothering you. **Check for updates** under **Added plugins** switches the check off.

> [!CAUTION]
> **Install updates without asking** under **Added plugins** installs new versions of the plugins you switched on and switches them on again before you get a look at them. Whoever controls a plugin's repo can then run new code on your computer every time they publish a release. That includes whoever gets into the author's GitHub account. It starts out off and Telescope confirms before it goes on. Leave it off unless you trust each person behind every plugin you use with that.

An update installed this way runs from the next time Telescope starts and the restart bar on the Plugins tab counts it. Each release gets one try. If it won't install or holds a different plugin Telescope leaves it and waits for the next release.

### When a plugin crashes

A plugin that crashes is switched off until the next start and a banner names it. The card, header button, windows and effect on the picture go away and its tile shows "Stopped after an error". It is tried again on the next start. **Copy diagnostics** lists it with the error. Devices and connection, Advanced settings and Updates go on running because Telescope needs them. If the plugin was hiding part of the picture (Privacy blur or the wait screen, for example) Telescope closes instead. That way no part meant to be hidden shows.

### When an added plugin won't switch on

Its tile on the Plugins tab shows the reason. A plugin can also name built-in plugins it needs. With one off the tile reads "Needs Photos, which is switched off. Switch it on too." for example and the plugin doesn't start. Switching off a built-in that an added plugin needs asks first and names that plugin. Other reasons:

| It says | What to do |
|---|---|
| It needs a newer Telescope | Update Telescope |
| It was made for an older Telescope | Ask its author for an update |
| A plugin that comes with Telescope already has that name | The plugin needs renaming |
| Other users on this computer could change it | Fix the folder's permissions or install it again with **Install** |
| It contains a link to somewhere else | Plugins have to be plain files and folders |
| It contains compiled code | Plugins have to be source code, with no `.so`, `.dll`, `.pyc` or other compiled files |
| Two files in the `.zip` have names that differ only in capital letters | Windows treats them as one file. Rename one and zip it again |
| The `.zip` doesn't hold one folder named as the plugin | Zip the plugin's folder itself |
