<!-- covers: rust/crates/system/src/autostart.rs rust/crates/system/src/hotkeys* rust/crates/system/src/shortcuts.rs rust/crates/system/src/keys.rs rust/crates/system/src/tray* rust/crates/system/src/single_instance.rs rust/crates/system/src/mcp*.rs rust/crates/shell/src/keys.rs rust/crates/shell/src/tray.rs rust/crates/shell/src/settings.rs rust/crates/shell/src/plugins_tab.rs rust/crates/outputs/src/wait_screen.rs; checked: 0d35dc883 -->
# The desktop app

System integration, keyboard shortcuts, the MCP server and plugins.

## System integration

- **Automatic streaming** (Streaming in Settings, off by default) starts when the phone is ready or when an app opens the camera. It stops once no app has used the camera for **Wait before stopping** (15 seconds by default). With the phone mic on it only turns the camera off. After you press Stop it won't start again until the phone or the app has gone away and returned
- **Wait screen** is what apps see while the phone isn't streaming. Use the default or your own image or GIF. **Mirror** is for apps that flip the camera
- **Open Telescope when I sign in** (General in Settings) starts it quietly in the tray
- Closing the window while streaming hides Telescope to the tray
- When Start can't go ahead a banner explains why and has a button that fixes it
- While the window is in the tray Telescope checks on the phone less often to save its battery
- The preview stops drawing while the window is minimised, in the tray or, on Windows, completely hidden behind another program's window such as a full screen game, which leaves the graphics card to that program. Apps reading the camera still get every frame
- **Reset all settings** (General in Settings) resets every setting to its default and restarts Telescope. The plugin switches return to the standard selection. Paired phones, presets, notes, saved layouts, files you picked (a LUT, the photo folder) and plugins you installed are left alone and a copy of the old settings file is saved next to it
- Every card has a Reset button in its header and so do the sections in Settings. It resets only that card or section to its defaults and lights up once something on it differs from the default. Things you made or paired, like notes and layouts, are left alone

<details>
<summary><b>Folding cards</b></summary>

Click a card's title to fold it down to its header and click again to open it. Section titles inside a card work the same way. Folds are remembered after a restart and are the same for every phone. Shortcuts still work on folded controls. Tab reaches the titles and Space or Enter folds them.

The **Look** section of General in Settings picks the window's look (**Dark**, **Classic**, **OLED** or **Light**) and the **Card colours**. **Monochrome** puts every card in the app's purple rather than its own colour.

The rest of the Look section changes how the window is built. **Corners** is **Sharp**, **Default** or **Round**, **Text size** is **Small**, **Default** or **Large** and **Density** is **Compact**, **Default** or **Comfortable**. **Higher contrast** brightens text and lines in all four looks. **Motion** follows the system or is forced to **Full**, **Reduced** or **Off**. **Tooltip delay** goes from instant to 1.5 seconds and **Show the next tooltip sooner** cuts the wait for the one after. Long card titles and slider values end in "…" at large text rather than running past the card. The main window and Settings remain inside the screen's usable area, also after a change of display scale or a move to another monitor, and Settings scrolls rather than running under the taskbar.
</details>

<details>
<summary><b>Reordering cards</b></summary>

Right-click a card's header and pick **Reorder cards** or open **Layout** in General in Settings. The **Card order** window lists the cards in a **Left rail** and a **Right rail**. Drag a card up, down or into the other rail. With a row focused, Alt+Up and Alt+Down move it and Alt+Left and Alt+Right send it to the other rail. **Reset order** restores the default. The new order applies after a restart and the bar at the bottom has **Revert** and **Restart now**.
</details>

## Keyboard shortcuts

- The **Keyboard shortcuts** tab in Settings binds a key to almost any control. You can also right-click a control in the main window to add one for it
- Each shortcut has its own settings. One key can raise exposure by 1 EV while another lowers it by 0.3 EV. Switches can toggle or work only while the key is down, for example to use **Mute** as a push to talk
- **Search settings** is focused when Settings opens and finds settings by what they do ("gpu" leads to Hardware acceleration)

<details>
<summary><b>Global shortcuts and the command line</b></summary>

Shortcuts are global by default and also work while another app is in front.

- **Windows:** Telescope claims the key from Windows. If another app already has it Windows gives it to that app even while Telescope is in front. Pick another key
- **Linux:** it goes through the desktop's global shortcuts portal (Plasma 5.27 and later, GNOME 48 and later, Hyprland). The first time, the desktop shows a prompt to confirm the keys and may pick different ones. **Open** next to **Desktop shortcut settings** opens the place to change them
- **Other desktops:** shortcuts only work inside Telescope. To make them global bind a command in the desktop's own keyboard settings. Right-click a shortcut and pick **Copy command line**, or use `telescope --action ID name=value` (for example `--action microphone.mute mode=toggle`). `--list-actions` lists every action

**Notify when one runs** shows a quick confirmation while Telescope is in the background. It's off by default.
</details>

## MCP server

Optional, switched on under **Extras** in the Plugins tab of Settings. With it an AI app like Claude Desktop can read and change Telescope's settings, with a **Read only** switch in the MCP tab and an **MCP** button in the top bar that undoes what the AI changed. Telescope itself sends no data to a model. See [MCP server](mcp.md) for adding it to an AI app and what it can do.

## Plugins

Most of the desktop app is plugins and you switch them on and off in the **Plugins** tab in Settings. Changes apply after a restart (the plugin's row gets a **Restart now** button). The settings of switched off plugins are saved and a banner names what's off after the restart. You can also install plugins other people made. See [Plugins](plugins.md).
