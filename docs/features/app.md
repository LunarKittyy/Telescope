# The desktop app

System integration, keyboard shortcuts, the MCP server and plugins.

## System integration

- **Automatic streaming** (Streaming in Settings, off by default) starts when the phone is ready or when an app opens the camera. It stops once no app has used the camera for **Wait before stopping** (15 seconds by default). With the phone mic on it only turns the camera off. After you press Stop it won't start again until the phone or the app goes away and comes back
- **Wait screen** is what apps see while the phone isn't streaming. Use the default or your own image or GIF. **Mirror** is for apps that flip the camera
- **Open Telescope when I sign in** (General in Settings) starts it quietly in the tray
- Closing the window while streaming hides Telescope to the tray
- When Start can't go ahead a banner says why and has a button that fixes it
- While the window is in the tray Telescope checks on the phone less often to save its battery

<details>
<summary><b>Folding cards</b></summary>

Click a card's title to fold it down to its header and click again to open it. Section titles inside a card work the same way. Folds are remembered after a restart and saved for the whole app, not per phone. Shortcuts keep working on folded controls. Tab reaches the titles and Space or Enter folds them.

The **Look** section of General in Settings picks the window's look (**Dark**, **Classic**, **OLED** or **Light**) and the **Card colours**. **Monochrome** puts every card in the app's purple instead of its own colour.
</details>

## Keyboard shortcuts

- The **Keyboard shortcuts** tab in Settings binds a key to almost any control. You can also right-click a control in the main window to add one for it
- Each shortcut has its own settings. One key can raise exposure by 1 EV while another lowers it by 0.3 EV. Switches can toggle or work only while the key is held, which makes **Mute** a push to talk
- **Settings search** (Ctrl+F) finds settings by what they do ("gpu" leads to Hardware acceleration)

<details>
<summary><b>Global shortcuts and the command line</b></summary>

Shortcuts are global by default and also work while another app is in front.

- **Windows:** Telescope claims the key from Windows. If another app already has it Windows gives it to that app even while Telescope is in front. Pick another key
- **Linux:** it goes through the desktop's global shortcuts portal (Plasma 5.27 and later, GNOME 48 and later, Hyprland). The first time the desktop asks you to confirm the keys and may pick different ones. **Open** next to **Desktop shortcut settings** opens the place to change them
- **Other desktops:** shortcuts only work inside Telescope. To make them global bind a command in the desktop's own keyboard settings. Right-click a shortcut and pick **Copy command line**, or use `telescope --action ID name=value` (for example `--action microphone.mute mode=toggle`). `--list-actions` lists every action

**Notify when one runs** shows a quick confirmation while Telescope is in the background. It's off by default.
</details>

## MCP server

Optional, switched on under **Extras** in the Plugins tab of Settings. It lets an AI app like Claude Desktop read and change Telescope's settings, with a **Read only** switch in the MCP tab and an **MCP** button in the top bar that undoes what the AI changed. Telescope sends nothing to a model by itself. See [MCP server](../mcp.md) for adding it to an AI app and what it can do.

## Plugins

Most of the desktop app is plugins, and the **Plugins** tab in Settings switches them on and off. Changes apply after a restart (the plugin's row gets a **Restart now** button). Switched off plugins keep their settings and a banner names what's off after the restart. You can also install plugins other people made. See [Plugins](../plugins.md).
