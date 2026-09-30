# Features

## Camera control
- Lens picker: switches between wide, main, and telephoto sensors (physical sub-cameras, not digital zoom). **Auto** is the phone's multi-lens camera: it switches lenses by itself as you zoom
- Manual ISO and shutter speed with log-scale sliders and direct numeric entry; range updates per-lens
- Exposure compensation slider (range and step size reported per-lens, typically ±8 EV in 1/6-EV steps)
- Manual white balance: linear Kelvin slider (2000-10000 K) plus a green-magenta tint slider - *partially working: applies inconsistently depending on device/lens*
- Manual focus: distance slider (diopters), range reported per-lens; greyed out on lenses that don't support it
- Point focus: click the preview (or pop-out) to focus there (a drag pans instead); exposure meters on that spot too while it's automatic. The click goes back through flip, rotation and zoom to the right spot on the sensor. **Auto** returns to continuous autofocus
- OIS toggle
- Noise reduction and sharpening (edge mode): Off / Fast / High Quality
- Black level lock toggle
- Torch/flash toggle, on lenses that report a flash unit
- Controls are greyed out per-lens if the camera hardware reports it doesn't support them

## Stream transforms

No phone restart needed.

- Horizontal and vertical flip
- Rotation: 90 CW, 180, 90 CCW
- Zoom 1-10x (the maximum is in Advanced) with pan X/Y sliders, or scroll over the preview to zoom around the mouse and drag it to pan. While you zoom or pan, the preview briefly outlines what each longer lens sees, so you can see where panning would leave the telephoto (the lens outline button next to **Pop out** keeps them on; they're only in the preview, never in the camera output, and approximate, since the lenses sit a little apart). The phone does as much of the crop as its lens allows, straight from the full-resolution sensor (and on the **Auto** lens, switching to the telephoto when the framing fits in it); the desktop crops whatever is left. The framing is the same either way. It gets noticeably sharper mainly once the telephoto takes over (zoomed in far enough and not panned out of its view). Hover the zoom slider to see where the zoom happens. Small dots on the zoom slider mark where the phone switches to a longer lens, and dragging the slider sticks to them, since that's the sharpest view that lens gets. The pan sliders stick at the centre the same way, and so does dragging the preview. Other sliders stick to their neutral spots (Compensation and Tint at 0, Temperature at 3200, 5500 and 6500 K). The sticking is only a few pixels wide, so you can still set anything in between, and the arrow keys never stick. Double-click a slider to put it back to its default. A dot next to the zoom value shows which lens you're on: lavender when the phone switched to a longer lens, red when it's on the main one although the zoom would get the telephoto (you panned past what the telephoto sees, or the phone stayed on the main lens itself, usually in low light or up close); hover it for the details

## Presets

The Presets button in the header.

- Save the camera, output and transform settings under a name and switch back with one click, lens included
- Kept per phone

## Canvas size control

In Advanced, from the settings menu.

- Set the virtual camera canvas independently of the phone feed resolution
- Presets: 720p/1080p/4K in 16:9 landscape and portrait, XGA and UXGA in 4:3, or fully custom
- On Linux: reloads v4l2loopback in a single elevated prompt (close OBS first); stream restarts automatically
- On Windows: stops and restarts the stream with the new canvas size

## Resolution and FPS
- Resolution dropdown is populated from the current lens's actual supported capture sizes (read from the phone), not a fixed list - picking one sends a live `resolution` control to the phone instead of resizing after decode. After a stream stops the sizes stay, so you can pick another before starting again, and Start opens the phone at that size and FPS
- The readout goes amber while a resolution change is in flight and clears once the stream confirms the new size, or turns red if it never does
- One FPS dropdown (15, 24, 25, 30, 48 or 60) drives both the phone's capture rate and the virtual camera's playback rate - there's no separate "phone" and "playback" rate to keep in sync. Rates past what the current lens lists are grayed out, and it runs at the fastest it can below; the one you picked comes back on a lens that can do it

## Bandwidth controls
- Format: **Light** (H.264, the default) or **Heavy** (MJPEG). Light comes from the phone's hardware encoder and needs a fraction of Heavy's bandwidth, about 8 Mbps at 1080p30, so it keeps up even on slower Wi-Fi. Heavy sends every frame as a full JPEG: sharper in fast motion, but it needs USB or strong Wi-Fi. A phone without an H.264 encoder uses Heavy by itself, and if the encoder fails mid-stream the stream goes back to Heavy and says so. Switching reconnects the stream
- Heavy: JPEG quality slider (1-95%, with a dot at the recommended 85%; higher barely looks different but sends a lot more data), applied on the phone without restarting the stream
- Light: bitrate slider, Auto (about 8 Mbps for 1080p30, scaled by size and fps) or 1-100 Mbps, applied live (Auto stays at most 30 Mbps, and the phone's encoder may cap it lower)
- All the way right is Dynamic: the phone sends as much as the connection carries, up to about 2.5 times what Auto's sizing gives (20 Mbps at 1080p30, 80 at 4K30, the 100 Mbps top at 4K60), and lowers it within a second or two when the connection can't keep up, before the video starts to lag. A short Wi-Fi hiccup doesn't count as a slow connection. It climbs back on its own, quickly at first, then carefully near where it last ran into trouble, so it settles instead of bouncing. Needs a phone app with Dynamic; an older one uses Auto
- A size the phone's H.264 encoder can't do (4:3 4K is past most of them) stops the stream with a note to try a lower resolution or FPS (pick one and start again), or **Switch to Heavy**. It doesn't switch to Heavy by itself: at that size Heavy can be hundreds of Mbps

## Microphone

Its own card, per phone.

- The phone's microphone as a microphone on the computer, while streaming. The phone only records while the desktop listens, with its noise suppression and gain control when it has them
- Linux: Telescope creates **Telescope Microphone**, an input only, through PulseAudio or PipeWire (`pactl`, from pulseaudio-utils) and removes it when the mic is switched off or the app quits. Nothing to install on most desktops
- Windows: needs [VB-Audio Virtual Cable](https://vb-audio.com/Cable/) (free). Telescope plays into CABLE Input; pick **CABLE Output** as the microphone in other apps. The card links to it if it's missing
- The first time, the phone asks for microphone access: its Get set up card gains a Microphone step once the desktop has asked
- About 60 ms of buffering; the phone's and the computer's clocks drift apart, so it drops or pads audio to stay there

## Monitoring
- FPS and throughput (Mbps) readouts in the footer while streaming; throughput turns amber if frames arrive well under the target rate for a sustained stretch (frames that arrive in a burst all count, though only the newest is shown to stay live). A "Can't keep up" note then suggests what's left to try, with a **Switch to Light** button on Heavy or a **Switch to Dynamic** button on Light, and goes away once the stream keeps up again. If the phone's camera itself makes fewer frames (dim light slows it down), that isn't the connection: no amber and no note, just a tooltip on the FPS readout saying so. A dropped stream doesn't count
- A dropped stream shows an animated "Stream dropped - reconnecting..." status instead of a static line, and the desktop keeps looking for the phone on every route: pull the cable and it carries on over Wi-Fi, plug it back in and it can use USB again. If the phone answers but stopped streaming, the desktop stops too and offers **Start**. If it answers but won't take this computer back (unpaired, Local only, or a version mismatch), the desktop stops and shows the same banner a failed Start would
- The phone's status reads "Waiting for the computer" while its camera is on but no computer is taking the video
- Battery level and phone temperature polled every 15 seconds, shown in the Monitoring panel with color coding
- Configurable battery alert threshold (default 20%) - counts as low when the level drops to it, including while plugged into a charger that can't keep up
- Configurable temperature alert threshold (default 45 C)
- Each threshold has its own **Notify** (on by default: a tray/desktop notification) and **Stop streaming** (off by default). Stop streaming stops the stream on every reading past the threshold, so a stream started again while the phone is still low or hot stops again at its first reading
- A log of warnings, errors, crashes and status changes, in the temp folder so the system clears it (`/tmp/telescope-<user>/` on Linux, `%TEMP%\Telescope\` on Windows). It's capped at about 1 MB plus the one before it, and a repeated line is counted instead of written again. Addresses, tokens and your home folder are stripped before anything is written. On Linux, `telescope.log` next to `start.sh` links to it
- **Open log** and **Copy diagnostics** at the bottom of Advanced. Copy diagnostics copies the version, system, connection and stream settings plus the last 200 log lines, for a bug report

## Phones, pairing and connection
- Pairing is one dialog, **Add phone**: scan its code with the phone, or plug the phone in over USB and it pairs by itself (it tells you if USB debugging still needs allowing on the phone). Telescope stores each phone by the id it reports, so a new name or a new address doesn't matter
- Several phones per computer and several computers per phone: each computer gets its own token on the phone, and removing one (on either side) leaves the others paired. Removing a phone on the desktop also unpairs it on the phone when it's reachable
- Telescope decides how to reach the phone on every connect: USB when this phone answers over a cable, Wi-Fi otherwise. A plugged-in device only counts once it reports the paired phone's id, so a different phone on the cable isn't mistaken for yours. **Connect via** forces USB or Wi-Fi
- When a cable is plugged in but not used, the Connection panel says why: the app isn't open on the phone, USB debugging isn't allowed yet, or it's a different phone. Plugging in while streaming over Wi-Fi offers **Switch to USB**
- Over Wi-Fi, Telescope finds the phone by its LAN announcement (mDNS, `_telescope._tcp`) before trying any stored address, so a router handing it a new IP doesn't break anything
- Camera, stream-output, transform, and monitoring settings (resolution, FPS, flip, rotation, exposure, zoom, quality, alert thresholds, etc.) are saved per phone to `telescope_config.json`; connection settings and the virtual-camera canvas are global
- Config from the previous version keeps its global settings; pairings and per-phone settings are dropped, since phones are now stored differently. Telescope backs up anything older or malformed next to the real file and starts from defaults. Each section is validated on its own, so one bad section resets without discarding the rest

## Privacy
- Everything between the phone and the desktop is TLS. The phone makes its own certificate on first run, and the desktop learns its fingerprint at pairing and refuses any other, so a device pretending to be your phone never gets the token and can't feed the desktop its own video
- The pairing token only travels in the QR code (or over adb) and inside that TLS connection. The pairing request itself proves the phone read the code with an HMAC of the token instead of sending it
- A device on the network can still see that a stream is running and roughly how much data it moves, and can refuse to forward it. For the tightest setup, use **Local only - USB**, which keeps the camera service off the network entirely
- Local only mode: binds the server to `127.0.0.1` so the stream is unreachable from the network; only USB works in this mode
- Toggle in the Android app restarts the stream automatically to apply the change
- Changing **Connect via** on the desktop reconnects a running stream over the new route
- Local only also stops the phone announcing itself on the LAN
- A stream only starts while Telescope is open on the phone or already streaming. **Wait for my computer** (under Local only, off by default) keeps the phone reachable while the screen is off or the app is closed, so a paired computer can start the camera. Android only lets an app start the camera from the background if it already has camera access then, so while waiting Telescope holds camera and mic access, but it doesn't open either until a paired computer starts a stream. A start still needs that computer's token over TLS. A "Waiting for your computer" notification shows while it's on, and its **Stop waiting** button turns it off. If Android stops it anyway, it comes back the next time you open the app, never on its own at boot

## Updates
- Both apps check for a newer build at every launch and then daily, on the channel you pick: **Stable** (tagged releases) or **Nightly** (every change to `master`). Nightly builds follow nightly by default, everything else follows stable
- Desktop: an **Update** button appears in the header. The update downloads, checks its SHA-256, replaces the app and restarts it. Not while streaming. A source checkout or a folder the app can't write to only links to the release
- An update cut short (power loss, a crash mid-swap) is finished on the next launch. If the new version won't start, the old one is put back and that build isn't offered again
- Phone: the update card at the top downloads the APK, checks its checksum and signing key, and hands it to Android's installer. The first time, Android asks to allow installs from Telescope. **Nightly updates** and **Check for updates** are in About
- When the phone app is older than the desktop, the Connection panel says so, and offers **Update over USB** when the phone is plugged in and the desktop bundle carries a newer APK

## System integration
- **Automatic streaming** (settings menu, all off by default), in two parts:
  - **Start:** only when you press Start, **when the phone is ready** (each time it becomes reachable; after you press Stop it stays stopped until the phone goes away and comes back), or **when an app opens the camera** (a call or OBS starts reading it; after Stop it waits until that app lets go). It never asks for a password on its own; if the Linux virtual camera is off, a banner offers to switch it on
  - **Stop:** only when you press Stop, **when no app uses the camera, if it started the stream**, or **when no app uses the camera, even if you started it**. Either one stops the stream once no app has read the camera for **Wait before stopping** (15 seconds by default, anything from 10 seconds to 60 minutes). Telescope's own preview doesn't count as an app. **Tell me when it stops a stream** shows a notification each time it does
  - A stream it stops isn't started again by "when the phone is ready" until the phone goes away and comes back, and a battery or heat stop from Monitoring holds the same way as pressing Stop
- **Wait screen…** (settings menu): what apps see on the camera while the phone isn't streaming, instead of the driver's own "no signal" picture. The default screen, or an image or GIF of your own, with a **Mirror** option for apps that flip the camera
- On Linux, if an app already has the camera open at some size, the stream and the wait screen open at that size too, so the call doesn't lose the picture
- On Linux, Telescope adds itself to the app menu on first launch (`~/.local/share/applications/telescope.desktop`), and fixes the entry up if you move the folder
- **Open Telescope when I sign in** (settings menu): an autostart entry (`~/.config/autostart/telescope.desktop` on Linux, the per-user Run key on Windows) that starts it in the tray with `--minimized`
- Minimizes to system tray on close while streaming, or while waiting to start by itself; otherwise quits
- Right-click the tray icon to quit, or click it to show/hide the window
- Launching a second instance brings the existing window to the front
- On Windows, opening `TelescopeDesktop.exe` from inside the zip (without extracting) says to extract it first, since the app can't run or update from there
- When Start can't go ahead, a banner at the top of the window says why, with the button that fixes it (Try again, Add phone, Switch to Automatic, Copy command). It clears on the next working stream
- Battery/temperature notifications use `notify-send` on Linux (if available) or the system tray on Windows
