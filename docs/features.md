# Features

Jump to: [Camera](#camera-control) · [Picture](#stream-transforms) · [Auto framing](#auto-framing) · [Quality](#resolution-and-fps) · [Microphone](#microphone) · [Browser camera](#browser-camera) · [Several cameras](#several-cameras-at-once) · [Connection](#phones-pairing-and-connection) · [Privacy](#privacy) · [Updates](#updates) · [Automation](#system-integration) · [Shortcuts](#keyboard-shortcuts) · [Plugins](#plugins)

## Camera control

- **Lens picker** switches between the phone's real wide, main and telephoto sensors instead of faking it with digital zoom. On **Auto** the phone picks the lens as you zoom
- **ISO and shutter speed** by slider or typed value. The range follows the current lens
- **Brightness** (exposure compensation), usually ±8 EV
- **White balance** in Kelvin (2000-10000 K) with a green-magenta tint
- **Focus** is **Auto**, **Tap a point** (click the preview to focus and meter exposure there) or **Manual**
- **Stabilization**, noise reduction, sharpening, black level lock and the torch, on lenses that have them
- **Low light** lets auto exposure slow the camera down (usually to 10 fps at the lowest) so each frame catches more light. It only does that when it has to
- Anything the current lens can't do is greyed out. Hover **Full manual control** or **Limited manual control** to see what it supports

## Stream transforms

These apply live without restarting the camera.

- Flip horizontally or vertically
- Rotate 90° CW, 180° or 90° CCW
- **Zoom 1-10x** with pan. **Max zoom** under Video in Settings goes up to 30x. Scroll over the preview to zoom and drag to pan
- **Width** and **Height** stretch or squish the picture from 50% to 200%, each on its own. Whatever goes past the edge is cut off and a squish leaves black bars, like a rotated picture gets
- Double-click any slider to reset it. The pan pad takes arrow keys (Shift for bigger steps) and Home to centre

Zooming crops from the full sensor first and hops to the telephoto lens once your framing fits inside it, which is why zooming in gets sharper. Dots on the zoom slider mark where a longer lens takes over.

## Auto framing

Optional. It finds you in the picture and moves zoom and pan to keep you framed. Switch it on under **Optional** in the Plugins tab of Settings (it needs [the vision download](#the-vision-download)).

- **Framing** sets how much of you is in the shot, from just your head to your whole body. Two people get framed together. With nobody in view for two seconds it zooms back out
- **Speed** goes from **Sloth** to **Zippy**. **Easy** is the default
- **Room to move** is how far you can shift before the framing follows (**Fidget** to **Roam**, while **Off** follows every move)
- **Widen for something in your hand** (off by default) makes room when you hold something out to the side
- Zoom and Pan on the Transforms card are greyed out while it's on. Flip and rotation still work and your own zoom and pan come back when you switch it off
- The skeleton button under the preview shows what the framing is reacting to. It never appears in the camera output

## Background

Optional and it shares the vision download. **Blur** blurs everything behind you and **Picture** puts an image there instead (PNG, JPEG, WebP or BMP). **Edges** sets how soft the outline around you is. It runs on the computer so it works with any phone and with Browser camera.

Background and Auto framing work less while nothing in the picture moves. **Power-saving effects** under Video in Settings slows them down while you move too, which uses less of the processor and battery but follows quick moves a little later.

## The vision download

Auto framing and Background use MediaPipe to find people. MediaPipe and its two models don't come with Telescope since most people won't use them. The first time you switch either plugin on Telescope asks and downloads them from its GitHub releases (about 47 MB on Linux and 28 MB on Windows). Both plugins share the download and **Remove download** on the Optional card deletes it again.

It only works on 64-bit Intel and AMD computers for now.

## Presets

The **Presets** button saves your camera, output and transform settings (lens included) under a name and brings them back in one click. Auto framing and Background are saved too when they're on. Each phone has its own presets and each preset can have its own shortcut.

## Resolution and FPS

- The resolution list comes from what the current lens really supports
- One **FPS** setting (15 to 60) drives both the phone and the virtual camera. Rates the lens can't do are greyed out
- **Canvas size** under Video in Settings sets the virtual camera's size apart from the phone's. Pick 720p, 1080p or 4K in landscape or portrait, a 4:3 size or a custom one (custom sizes have to be even). On Linux changing it reloads the driver, which asks for your password once (close OBS first)

## Format and bandwidth

| Format | Good for | Needs |
|---|---|---|
| **Light** (H.264, the default) | Most things | About 8 Mbps at 1080p30, so fine even on slower Wi-Fi |
| **Heavy** (MJPEG) | Sharper fast motion | USB or strong Wi-Fi |

A phone that can't do Light switches to Heavy automatically.

- **Heavy** has a JPEG quality slider
- **Light** has a bitrate slider with **Auto**, a fixed 1-100 Mbps or **Dynamic**. Dynamic sends as much as the connection can carry and backs off before the video starts to lag. If that's still too much it lowers the frame rate too (down to 15) and climbs back once there's room
- **Hardware acceleration** under Video in Settings decodes Light on the graphics card instead of the processor, which matters most at 4K or on a laptop. It's off by default. It works on Windows with any graphics card and on Linux with NVIDIA. For AMD and Intel on Linux, **Set up VA-API…** next to it gets it working
- Light only lists resolutions the phone's encoder can take

<details>
<summary><b>UDP on Wi-Fi (experimental)</b></summary>

Under the connection settings. It sends Light over UDP instead of TCP while the phone is on Wi-Fi. On a busy network a lost packet then only costs a resend of that one piece instead of holding up the whole stream, which can cut lag spikes. It's encrypted and if the video doesn't get through (usually a firewall) it goes back to TCP. It isn't used over USB or for Heavy.

Dynamic works over UDP too. The computer tells the phone how many packets went missing and how long they took to arrive, which shows a thinning link earlier than waiting for video to pile up on the phone. If the phone app is older than the desktop app, Dynamic still runs over TCP.
</details>

## Microphone

The phone's mic can be a microphone on your computer while you stream.

- **Linux:** Telescope creates **Telescope Microphone** through PulseAudio or PipeWire. Most desktops need nothing extra
- **Windows:** needs [VB-Audio Virtual Cable](https://vb-audio.com/Cable/) (free). The card's **Install VB-Cable** downloads and opens VB-Audio's setup for you (Windows asks for admin and VB-Audio asks for a restart). Afterwards pick **CABLE Output** as the microphone in other apps and switch the mic off and on again in Telescope
- **Gain** goes from -24 to +12 dB (**Max gain** under Audio in Settings raises the top). The level meter turns yellow while the limiter is holding loud moments down and red when the sound clips. The limiter is on by default
- **Auto level** (off by default) evens out quiet and loud speech and stops adjusting while nobody talks
- **Noise reduction** (off by default) takes typing, fans and room hiss out of the mic with RNNoise running on your computer. It cleans up the same way for every phone and for a browser's mic, which helps most with phones whose own noise suppression is weak or missing. It adds about 20 ms of delay and switches in or out at the next pause in speech. With Discord, Zoom or a similar app turn off the app's own noise suppression while this is on (both at once can make voices sound thin). The level meter shows the mic as it arrives, before noise reduction
- **Mute** sends silence without disconnecting. It's in the card and the tray menu and resets when Telescope restarts
- **Mic only:** the camera button under the preview turns the phone's camera off while the mic stays on. Apps see the wait screen. Press it before starting and Start becomes **Start mic only**
- The tray icon shows what's live: a red dot for the camera and a mic for the mic (crossed out when muted)

## Browser camera

Stream from anything with a browser (an iPhone, a tablet, another laptop) with nothing to install on it. Pick **Browser camera** in the phone picker, scan the code and tap **Start** on the page. It uses WebRTC like browser video calls do, so any up-to-date browser works. Nothing goes through a server outside your network.

- **First visit warning:** the browser says the connection isn't private because Telescope makes its own certificate. It's still encrypted. On iPhone tap **Show Details** then **visit this website**. In Chrome it's **Advanced** then **Proceed**
- **Keep the page open with the screen on.** Phones pause the camera when you switch apps or lock and it picks up again when you come back
- **Black screen** turns the page black while the camera streams on to save battery on OLED phones. Double tap to get the page back. A single tap only shows the connection status for a few seconds. iPhones don't let web pages go full screen, so there it plays a black video in the iPhone's own player instead and closing the player brings the page back
- **Light** on the page turns on the flash where the browser allows it (Chrome on Android usually does). Front cameras get a white screen instead
- **Flip camera** shows on devices with more than one camera
- **New link** makes a fresh code and the old one stops working at once. Connected browsers are dropped and apps fall back to the wait screen
- Several devices can scan the same code. Each gets its own virtual camera (see [Several cameras](#several-cameras-at-once))
- The browser's microphone works with the Microphone card too. It's sent raw (no echo cancellation or noise suppression) because Discord, Zoom and similar do their own
- Browsers show up in the phone picker under their device name. One only gets saved settings once you change a setting. Saved browsers are listed under **Your phones** (the bin button forgets one) and are forgotten after 60 days without connecting
- Camera, output and alert settings don't apply to browsers. Resolution (480p to 1080p) and frame rate (15 to 30) are on the card
- The card shows what the browser really sends, like "H.264, 1280×720 at 30 fps". It can be lower than what you picked when the device or Wi-Fi can't keep up. If the device's processor is the limit the card warns and a lower resolution or frame rate usually helps
- On Linux the virtual camera is 1920x1080 unless you set another canvas under Video in Settings

## Several cameras at once

Click the **+** next to Stop while streaming to add another phone (or Browser camera), up to four in total. Each gets its own virtual camera: **Phone Camera 2** to **4** on Linux and **Telescope #2** to **#4** on Windows.

- The first time Telescope adds those cameras and asks for your password (Linux) or permission (Windows). **Extra cameras** under Virtual camera & phone in Settings removes them again
- A row of tiles shows every stream, which camera it goes to and whether it's live. Click a tile to bring its settings into the panels. The **x** on a tile stops only that stream and Stop stops them all
- A phone gets the same virtual camera slot as last time if it's free, to lower the need for redoing OBS scenes
- Only one stream has the mic at a time (the first to start). To move it, click another tile and turn on **Phone mic**
- With more than one stream mic only isn't available, the canvas can't be resized and Automatic streaming won't stop anything until all of them have stopped
- Extra cameras are always the size set in **Canvas size** under Video in Settings (1920x1080 on Auto). Other shapes get black bars

## Monitoring

- FPS and throughput sit in the footer. When the stream can't keep up a note suggests what to try, with a button for it
- A dropped stream reconnects over whichever route works (pull the cable and it carries on over Wi-Fi). After 10 seconds apps get the wait screen instead of a frozen frame and after 30 a banner says the phone can't be reached
- Phone battery and temperature have alerts that can notify you, stop the stream or both. A stop explains why and offers **Start anyway**. A phone that runs hot also lowers its own frame rate first (Android 10 and newer)
- **Open log**, **Copy diagnostics** and **Report a bug** are under Updates & About in Settings. Report a bug copies the diagnostics and opens the bug form on GitHub for you to paste them in. Addresses, tokens and your home folder are stripped out

## Phones, pairing and connection

- **Add phone** pairs by QR code or by plugging the phone in over USB (USB needs adb, which Windows downloads from Google after asking)
- The **?** next to it brings the setup checklist back. Click it again to close it
- You can pair several phones to one computer and one phone to several computers
- Telescope uses USB when the phone is plugged in and answering and Wi-Fi otherwise. If a cable is plugged in but isn't used the Connection panel says why. **Connect via** can force either one
- A new IP address from the router breaks nothing, since the phone announces itself on the network
- On a slow Wi-Fi stream the note tells you when a weak signal or 2.4 GHz is the cause, since moving closer, a 5 GHz network or a cable fixes that better than lowering quality
- The phone's screen stays on while the app is open because Android only runs Wi-Fi at full speed with the screen on. You can press the power button and the stream keeps going
- Camera, output, transform and alert settings are saved per phone. Connection settings and the canvas are shared

## Privacy

- Everything between the phone and computer is encrypted with TLS. The desktop remembers your phone's certificate at pairing and refuses anything that shows up with a different one
- The pairing token only travels in the QR code (or over adb) and inside the encrypted connection
- Someone on the network can still see that a stream is running and roughly how much data it moves. **Local only - USB** in the Android app takes the stream and controls off the network entirely
- **Wait for my computer** (off by default) lets you start the camera from your PC with the phone's screen off or the app closed. Tap **Stop waiting** in its notification to end it
- Browser camera only listens on port 8767 while it is picked or a browser is streaming. Anyone can load the page but sending video needs the token in the code, which changes on every start and on **New link**. Its certificate isn't pinned like a phone's, so on a network you don't trust use a phone
- The apps only go online to check GitHub for updates. On Windows the desktop also fetches adb from Google and VB-Cable from VB-Audio, but only after you click to get them
- On Linux only your user can read the config folder with the pairing tokens. On Windows the camera driver sits in Program Files where other programs can't swap it

## Updates

- Both apps check at launch and once a day, on **Stable** or **Nightly**
- **Desktop:** an **Update** button appears in the header and installs in one click (not while streaming). If an update gets cut short or the new version won't start the next launch finishes it or puts the old one back
- **Phone:** the update card downloads, checks and installs the new APK. If the phone app is older than the desktop the Connection panel says so and can update it over USB

<details>
<summary><b>Updating an unzipped Windows copy</b></summary>

Updating installs the new version for your user instead (Start menu entry and uninstaller, no admin) and then deletes the unzipped folder. Only Telescope's own files go. Settings, paired phones and adb come along and a desktop shortcut gets swapped for one to the installed copy. Copies on a USB stick or network drive update in place. Anywhere else you can tick **Keep this copy where it is** in Updates to do the same.
</details>

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

**Monochrome** under **Card colours** in General puts every card in the app's purple instead of its own colour.
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

## Plugins

Most of the desktop app is plugins, and the **Plugins** tab in Settings switches them on and off. Changes apply after a restart (the plugin's row gets a **Restart now** button). Switched off plugins keep their settings and a banner names what's off after the restart. You can also install plugins other people made. See [Plugins](plugins.md).
