# Features

## Camera control
- Lens picker: switches between the phone's real wide, main and telephoto sensors instead of faking it with digital zoom. On **Auto** the phone picks the lens itself as you zoom
- Manual ISO and shutter speed, with sliders or typed values. The range follows whatever the current lens can do
- Exposure compensation, typically ±8 EV
- Manual white balance: Kelvin (2000-10000 K) plus a green-magenta tint - *partially working: applies inconsistently depending on device/lens*
- Manual focus, or click anywhere on the preview to focus (and meter exposure) on that spot. **Auto** goes back to continuous autofocus
- OIS, noise reduction, sharpening (edge mode), black level lock, and the torch on lenses that have a flash
- Anything the current lens doesn't support is greyed out

## Stream transforms

These apply live without restarting the phone's camera.

- Horizontal and vertical flip
- Rotation: 90 CW, 180, 90 CCW
- Zoom 1-10x with pan. You can also scroll over the preview to zoom and drag it around to pan. The phone crops from the full sensor first, and once your framing fits inside the telephoto lens it hops over to that one, which is why zooming in actually gets sharper. Dots on the zoom slider mark where a longer lens takes over, and hovering the slider or the lens dot next to the value explains the rest
- Double-click any slider to put it back to its default

## Presets

The Presets button in the header saves your camera, output and transform settings under a name (lens included) and brings them all back with one click later. Each phone has its own presets.

## Resolution and FPS
- The resolution list comes from what the current lens really supports, and picking one changes what the phone itself captures
- One FPS setting (15 to 60) drives both the phone and the virtual camera. Rates the lens can't do are greyed out
- **Canvas size** in Advanced sets the virtual camera's size separately from the phone's: 720p, 1080p or 4K in landscape or portrait, 4:3 sizes, or a custom one. Custom sizes have to be even because odd ones mess up the colours on Linux. On Linux, changing it reloads the driver, which takes one password prompt (close OBS first)

## Format and bandwidth
- **Light** (the H.264 format, and the default) needs about 8 Mbps at 1080p30, which makes it a good fit even for slower Wi-Fi. **Heavy** (MJPEG) is sharper in fast motion but needs USB or strong Wi-Fi. A phone that can't do Light switches to Heavy automatically
- Heavy has a JPEG quality slider. Light gets a bitrate slider instead: Auto, a fixed 1-100 Mbps, or **Dynamic**, which sends as much as the connection can carry and backs off before the video starts to lag. When even that leaves too little for each frame, Dynamic also steps the frame rate down (30, 24, 20, down to 15 at the lowest), so you get fewer sharp frames instead of a lot of smeared ones. It climbs back once the connection has room, more slowly than it came down. Each step restarts the phone's camera for a moment, the same as changing FPS by hand, and hovering the FPS in the footer says when Dynamic lowered it
- On Light, the resolution list leaves out sizes the phone's encoder says it can't take. If one still fails, the stream stops with a note suggesting a lower size or Heavy, and the resolution goes back to the last one that worked (so pressing Start again won't fail the same way)
- Switching to a new aspect ratio picks the size closest to your current height rather than the biggest one

## Microphone
- While streaming, the phone's mic can be a microphone on the computer, with the phone's own noise suppression if it has one
- Linux: Telescope creates **Telescope Microphone** through PulseAudio or PipeWire. Most desktops need nothing extra
- Windows: you'll need [VB-Audio Virtual Cable](https://vb-audio.com/Cable/) (free), then pick **CABLE Output** as the microphone in other apps. If it's missing, the card links to it
- Gain goes from -24 dB to +12 dB (Advanced can raise the top to +24, +36 or +48), and you can type in a value for an exact one. The level meter holds the latest peak and lights up yellow while the limiter is holding loud moments down, or red when the sound clips. The limiter is on by default and can be switched off in Advanced
- **Mute** (in the card or the tray menu) sends silence without disconnecting the mic, handy for call apps that can't mute themselves. It resets when Telescope restarts
- The camera button under the preview turns the phone's camera off while the mic keeps going. In the meantime apps get the wait screen and the preview shows that the camera is off. Press it before starting and Start turns into **Start mic only**. This needs the mic on, and every new stream starts with the camera back on
- The tray icon shows what's live: a red dot while the camera is on and a mic while the mic is (crossed out when muted)

## Browser camera
- Stream from anything with a browser (an iPhone, a tablet, another laptop) without installing anything on it. Pick **Browser camera** in the phone picker, scan the code on its card and tap **Start** on the page
- It works best in Chrome. Chrome sends the H.264 format from the device's hardware encoder, while Firefox for example falls back to JPEG, which is a lot heavier on the Wi-Fi and the phone. On iPhone every browser runs on Safari's engine anyway, so any of them will do
- The browser's microphone works with the Microphone card too
- The first time on each device, the browser warns that the connection isn't private. Browsers only let a page use the camera over HTTPS, which normally relies on a certificate from a company the browser trusts. Those companies can't vouch for an address on your home network, so Telescope makes its own certificate and the browser complains because it can't check who made it. The connection is still encrypted. To get past the warning on iPhone, tap **Show Details** and then **visit this website**. In Chrome it's **Advanced** and then **Proceed**
- Keep the page open with the screen on. Phones pause the camera when you switch apps or lock the screen, but the stream picks up again once you're back on the page
- **Black screen** on the page turns everything black while the camera keeps streaming, which saves battery on OLED phones and stops a bright screen from lighting up the room. Where the browser allows it the page also goes full screen to hide the phone's status bar. iPhones don't let web pages do that, so there the page plays a black video in the iPhone's own video player instead, and closing the player brings the page back. If the camera stops while the player is up, the page closes it and stays on the plain black screen from then on. Double tap to get the page back. A single tap only brings up the hint and the connection status for a few seconds, in case the phone gets bumped on its stand
- **New link** makes a fresh code and the old one stops working right away. Any browser still connected with it gets dropped and apps that were watching it fall back to the wait screen
- Several devices can scan the same code. Each one starts streaming as soon as it connects: the first becomes the stream and the rest join next to it, each on its own virtual camera (see [Several cameras at once](#several-cameras-at-once)). The page shows which camera it's streaming as, and reloading it picks the same stream back up
- While a browser is connected it shows up in the phone picker under its device name, just like a phone. Two of the same kind are told apart as "Chrome on Linux" and "Chrome on Linux (2)". With **Browser camera** picked, **Start** streams a browser that's already connected or waits for one to scan the code
- A browser gets its own saved settings like a phone does, but only once you actually change one. That way a quick scan from a friend's phone doesn't leave anything behind. Saved browsers are listed under **Your phones**, where **Remove** forgets them, and they're forgotten anyway after 60 days without connecting
- Camera, output and alert settings don't apply to browsers. Resolution (480p to 1080p) and frame rate (15 to 30) are on the card instead
- On Linux the virtual camera is 1920x1080 unless you set another canvas in Advanced, because a browser's first frame could be any shape
- When the browser has access to a hardware encoder, the page sends the H.264 format from it, which handles 1080p at 30 fps fine. Otherwise it falls back to JPEG, and most phones struggle with that at 1080p. The card shows which one is in use, and if it's JPEG its tooltip tells you why
- If the device can't keep up, the page drops to 720p and then to 480p on its own. Whenever you change the size here, it goes back to trying that one

## Several cameras at once
- While one stream is running, the **+** next to Stop adds another phone (or the browser camera) alongside it, up to four in total. Each gets its own virtual camera: **Phone Camera 2** to **4** on Linux, **Telescope #2** to **#4** on Windows
- The first time, Telescope has to add those cameras, which asks for your password on Linux or for permission on Windows. On Linux they disappear after a reboot and get added again next time. **Extra cameras** in Advanced removes them sooner, and on Windows that's the only way to get back to one
- An extra camera with nothing streaming to it shows the wait screen like the first one does. The same goes for the first one while only extra ones are streaming
- Adding **Browser camera** with **+** gives it a tile that says **Waiting** with the code on screen. The first browser to scan it takes over that tile, and the x cancels it
- A row of tiles shows every stream with a small picture, which camera it goes to and whether it's live. Click a tile to bring its settings into the panels. Zoom, flips, camera and output settings are kept per phone, and a stream you're not looking at carries on with whatever it had. The x on a tile stops just that one, while Stop stops them all
- Each phone gets the same virtual camera slot as last time when it's free, to save you redoing OBS scenes and other setups that point at a specific one
- Battery and heat warnings cover every streaming phone, each with its own thresholds. Changing the canvas size only works with a single camera streaming
- Stopping when no app reads the camera switches off as soon as a second camera has streamed, and stays off until everything has stopped
- Only one stream has the mic at a time, the first one to start. To use another one's mic, click its tile and turn on **Phone mic**, and it goes off on the other. If the stream with the mic stops, the next one takes it over (still on)
- With more than one stream, mic only isn't available, Automatic streaming won't stop anything, and the wait screen only comes back after the last one stops
- The extra cameras are always the size set under Setup (1920x1080 when that's Auto). A picture with a different shape gets black bars

## Monitoring
- FPS and throughput sit in the footer. When the stream can't keep up, a note suggests what to try and gives you a button for it
- A dropped stream reconnects over whichever route works, so you can pull the cable and it carries on over Wi-Fi. If it's still gone after 10 seconds, apps get the wait screen instead of a frozen last frame. After 30 seconds a banner warns that it can't reach the phone or browser, though it keeps trying either way
- Phone battery and temperature, with alerts you can set to notify you, stop the stream, or both. When an alert stops the stream, the window explains why and offers **Start anyway** to keep streaming until the battery or temperature is back to normal
- **Open log** and **Copy diagnostics** are at the bottom of Advanced. Copy diagnostics is what to paste into a bug report. The log lives in the temp folder (`/tmp/telescope-<user>/` on Linux, `%TEMP%\Telescope\` on Windows) with addresses, tokens and your home folder stripped out

## Phones, pairing and connection
- **Add phone** pairs by QR code or by plugging the phone in over USB
- The **?** next to it brings the setup checklist back (with the phone app download and Add phone), even after you've streamed from Browser camera. Click it again to close it
- You can pair several phones to one computer and one phone to several computers. Removing one leaves the others paired
- Telescope uses USB when the phone is plugged in and answering, and Wi-Fi otherwise. If a cable is plugged in but isn't being used, the Connection panel explains why. **Connect via** can force one or the other if you wish. When you change it mid-stream, it first checks that the phone answers that way and leaves the stream alone if it doesn't
- Picking another phone or browser in the picker while streaming switches the stream over to it
- A new IP address from the router doesn't break anything, since the phone announces itself on the network
- Camera, output, transform and alert settings are saved per phone, while connection settings and the canvas are shared

## Privacy
- Everything between the phone and the computer is encrypted with TLS. When you pair them, the desktop remembers your phone's certificate and won't talk to anything that shows up with a different one, so nobody else on the network can pretend to be your phone
- The pairing token only ever travels in the QR code (or over adb) and inside that encrypted connection
- Someone on the network can still see that a stream is running and roughly how much data it moves. **Local only - USB** in the Android app keeps the stream and the camera controls off the network entirely
- If you'd like to start the camera from your PC without picking up the phone, turn on **Wait for my computer**. The phone then stays ready even with the screen off or the app closed, until you tap **Stop waiting** in its notification. It's off by default
- Browser camera only listens on port 8767 while it (or a browser) is picked in the phone picker, or while a browser is streaming. Anyone can load the page itself, but sending video needs the token in the code, and that changes every time Telescope starts or you click **New link**. Its certificate isn't pinned the way a phone's is, so on a network you don't trust it's better to use a phone
- On Linux, only your user can read the config folder with the pairing tokens. On Windows the camera driver is installed into Program Files, where other programs can't swap it out

## Updates
- Both apps check for updates at launch and once a day, on **Stable** or **Nightly**
- Desktop: an **Update** button shows up in the header and installs with one click (just not while streaming). If an update gets cut short or the new version won't start, the next launch finishes it or puts the old one back
- Phone: the update card downloads, checks and installs the new APK. When the phone app is older than the desktop, the Connection panel points it out and can update it over USB

## System integration
- **Automatic streaming** (settings menu, off by default) starts when the phone is ready or when an app opens the camera. It stops once no app has used the camera for **Wait before stopping**, which is 15 seconds unless you change it. If the phone mic is on it only turns the camera off, so a call that switched its camera off can still hear you, and the camera comes back when an app opens it again. After you press Stop yourself, it won't start again until the phone or the app goes away and comes back
- **Wait screen…**: what apps see on the camera while the phone isn't streaming. That's either the default screen or your own image or GIF, and there's a **Mirror** option for apps that flip the camera
- **Open Telescope when I sign in** starts it quietly in the tray
- Closing the window while streaming (or while waiting to start automatically) keeps Telescope running in the tray
- When Start can't go ahead, a banner tells you why and has the button that fixes it
