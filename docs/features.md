# Features

## Camera control
- Lens picker: switches between the phone's real wide, main and telephoto sensors, not digital zoom. **Auto** lets the phone switch lenses by itself as you zoom
- Manual ISO and shutter speed, with sliders or typed values; the range follows the lens
- Exposure compensation, typically ±8 EV
- Manual white balance: Kelvin (2000-10000 K) plus a green-magenta tint - *partially working: applies inconsistently depending on device/lens*
- Manual focus, or click the preview to focus (and meter exposure) on that spot. **Auto** goes back to continuous autofocus
- OIS, noise reduction, sharpening (edge mode), black level lock, and the torch on lenses with a flash
- Controls a lens doesn't support are greyed out

## Stream transforms

These apply live, without restarting the phone's camera.

- Horizontal and vertical flip
- Rotation: 90 CW, 180, 90 CCW
- Zoom 1-10x with pan, or scroll over the preview to zoom and drag it to pan. The phone crops as much as it can from the full sensor, and switches to the telephoto when the framing fits in it, so zooming in gets properly sharper. Dots on the zoom slider mark where a longer lens takes over, and hovering the slider or the lens dot next to the value explains the rest
- Double-click a slider to put it back to its default

## Presets

The Presets button in the header saves the camera, output and transform settings under a name, lens included, and switches back with one click. Kept per phone.

## Resolution and FPS
- The resolution list comes from what the current lens actually supports, and changing it changes what the phone captures
- One FPS setting (15 to 60) drives both the phone and the virtual camera. Rates the lens can't do are greyed out
- **Canvas size** in Advanced sets the virtual camera's size separately from the phone's: 720p, 1080p or 4K in landscape or portrait, 4:3 sizes, or custom. On Linux it reloads the driver with one password prompt (close OBS first)

## Format and bandwidth
- **Light** (H.264, the default) needs about 8 Mbps at 1080p30, so it keeps up on most Wi-Fi. **Heavy** (MJPEG) is sharper in fast motion but needs USB or strong Wi-Fi. A phone that can't do Light uses Heavy by itself
- Heavy has a JPEG quality slider; Light has a bitrate slider: Auto, a fixed 1-100 Mbps, or **Dynamic**, which sends as much as the connection carries and backs off before the video starts to lag
- A size the phone's encoder can't do in Light stops the stream with a note to pick a lower one or switch to Heavy

## Microphone
- The phone's mic as a microphone on the computer while streaming, with the phone's noise suppression when it has it
- Linux: Telescope creates **Telescope Microphone** through PulseAudio or PipeWire. Nothing to install on most desktops
- Windows: needs [VB-Audio Virtual Cable](https://vb-audio.com/Cable/) (free). Pick **CABLE Output** as the microphone in other apps. The card links to it if it's missing
- Gain from -24 dB up to +12 dB (Advanced lets it go to +24, +36 or +48 instead; type a value for an exact one), and a level meter that holds the latest peak, lights yellow while the limiter holds loud moments down and red when the sound clips. The limiter is on by default (Advanced)
- **Mute** (in the card or the tray menu) sends silence without disconnecting the mic, for call apps that can't mute. It resets when the app restarts
- The camera button under the preview turns the phone's camera off while the mic keeps streaming. Apps see the wait screen, and the preview says the camera is off. Pressed before a start, the button turns Start into **Start mic only**. It needs the mic on, and each stream starts with the camera on again
- The tray icon shows what's streaming: a red dot while the camera is on, a mic while the mic is (crossed out when muted)

## Browser camera
- Stream from any device with a browser (an iPhone, a tablet, another laptop) without installing anything on it. Pick **Browser camera** in the phone picker, scan the code on its card, and tap **Start** on the page
- The browser's microphone works with the Microphone card too
- The browser warns that the connection isn't private, once per device: the page uses a certificate this computer made for itself. On iPhone tap **Show Details** and then **visit this website**; in Chrome tap **Advanced** and then **Proceed**
- Keep the page open with the screen on. Phones pause the camera when you switch apps or lock the screen, and the stream picks up again when you come back to the page
- **New link** makes a new code and stops the old one working. One browser streams at a time; opening the link somewhere else takes over
- Camera, output and alert settings don't apply here. Resolution (480p to 1080p) and frame rate (15 to 30) are on the card instead
- On Linux the virtual camera is 1920x1080 unless Advanced sets another canvas, since a browser's first frame can be any shape
- The page sends H.264 from the device's hardware encoder when the browser has one, which keeps 1080p at 30 fps. Otherwise it sends JPEG, which is slow at 1080p on most phones; the card shows which, and its tooltip says why it's JPEG
- When the device can't keep up, the page steps down to 720p and then 480p by itself, and goes back to trying the size picked here whenever you change it

## Monitoring
- FPS and throughput in the footer. When the stream can't keep up, a note suggests what to try, with a button for it
- A dropped stream reconnects by itself, over whichever route works: pull the cable and it carries on over Wi-Fi
- Phone battery and temperature, with alerts you can set to notify you, stop the stream, or both
- **Open log** and **Copy diagnostics** at the bottom of Advanced. Copy diagnostics is what to paste into a bug report. The log lives in the temp folder (`/tmp/telescope-<user>/` on Linux, `%TEMP%\Telescope\` on Windows), with addresses, tokens and your home folder stripped out

## Phones, pairing and connection
- **Add phone** pairs by QR code, or by plugging the phone in over USB
- The **?** next to it opens the setup checklist again, with the phone app download and Add phone, even after you've streamed from Browser camera. Click it again to close it
- Several phones per computer and several computers per phone. Removing one leaves the others paired
- USB when the phone is plugged in and answering, Wi-Fi otherwise. If a cable is plugged in but not used, the Connection panel says why. **Connect via** forces one or the other
- A new IP address from the router doesn't break anything: the phone announces itself on the network
- Camera, output, transform and alert settings are saved per phone; connection settings and the canvas are shared

## Privacy
- Everything between the phone and the desktop is TLS, pinned to the phone's own certificate at pairing, so another device can't pose as your phone
- The pairing token only travels in the QR code (or over adb) and inside that TLS connection
- Someone on the network can still see that a stream is running and roughly how much data it moves. **Local only - USB** in the Android app keeps the stream and the camera controls off the network entirely
- **Wait for my computer** (off by default) keeps the phone ready while the screen is off or the app is closed, so a paired computer can start the camera. Its notification has a **Stop waiting** button
- Browser camera only listens (on port 8767) while it's picked in the phone picker. The page itself is public, but sending video needs the token in the code, which is new every time Telescope starts or you click **New link**. Its certificate isn't pinned the way a phone's is, so on a network you don't trust, pick a phone instead
- On Linux, the config folder with the pairing tokens is readable by your user only. On Windows, the camera driver is installed into Program Files so other programs can't swap it out

## Updates
- Both apps check for updates at launch and daily, on **Stable** or **Nightly**
- Desktop: an **Update** button appears in the header and installs in one click (not while streaming). If an update is cut short or the new version won't start, the next launch fixes it or puts the old one back
- Phone: the update card downloads, checks and installs the new APK. When the phone app is older than the desktop, the Connection panel says so and can update it over USB

## System integration
- **Automatic streaming** (settings menu, off by default): start when the phone is ready or when an app opens the camera, and stop once no app has used the camera for **Wait before stopping** (15 seconds by default). With the phone mic on it only turns the camera off, so a call that switched its camera off still hears you, and the camera comes back when an app opens it. After you press Stop, it doesn't start again on its own until the phone or the app goes away and comes back
- **Wait screen…**: what apps see on the camera while the phone isn't streaming, the default screen or your own image or GIF, with a **Mirror** option for apps that flip the camera
- **Open Telescope when I sign in** starts it in the tray
- Closing the window while streaming (or waiting to start by itself) keeps it in the tray
- When Start can't go ahead, a banner says why, with the button that fixes it
