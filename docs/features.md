# Features

## Camera control
- Lens picker: switches between the phone's real wide, main and telephoto sensors instead of faking it with digital zoom. On **Auto** the phone picks the lens itself as you zoom
- Manual ISO and shutter speed, with sliders or typed values. The range follows whatever the current lens can do
- **Brightness** (exposure compensation), typically ±8 EV
- Manual white balance: Kelvin (2000-10000 K) plus a green-magenta tint
- Focus: **Auto** for continuous autofocus, **Tap a point** to click anywhere on the preview and focus (and meter exposure) on that spot, or **Manual**
- **Stabilization** (OIS), noise reduction, sharpening (edge mode), black level lock, and the torch on lenses that have a flash
- Low light, for a dark room: auto exposure may slow the camera down so each frame catches more light (usually no lower than 10 fps, lower on some phones), and noise reduction runs at its best. It only slows down when it has to (in good light you still get the rate you picked)
- Anything the current lens doesn't support is greyed out. Hover **Full manual control** or **Limited manual control** to see what the lens supports

## Stream transforms

These apply live without restarting the phone's camera.

- Horizontal and vertical flip
- Rotation: 90 CW, 180, 90 CCW
- Zoom 1-10x with pan (**Max zoom** under Video in Settings takes it from 2x up to 30x). You can also scroll over the preview to zoom and drag it around to pan. The phone crops from the full sensor first, and once your framing fits inside the telephoto lens it hops over to that one, which is why zooming in actually gets sharper. Dots on the zoom slider mark where a longer lens takes over, and hovering the slider or the lens dot next to the value explains the rest
- Double-click any slider to put it back to its default. The pan pad also takes arrow keys (Shift for bigger steps) and Home to centre

## Auto framing

An optional plugin, switched on under **Optional** in the Plugins tab of Settings (see [the vision download](#the-vision-download) below). It finds you in the picture and moves the zoom and pan to keep you framed. The **Framing** slider sets how much of you that is, anywhere from just your head, down to your chest or waist, to all of you. Two people in the picture get framed together. If nobody's been in the picture for two seconds it zooms back out to the whole thing.

- **Widen for something in your hand** (on by default) widens the framing when you hold a hand out to the side to fit whatever you're showing in next to you. It goes back to the usual framing a moment after you put the hand down
- **Speed** goes from **Sloth**, which drifts after you slowly, to **Zippy**, which stays close behind you. **Easy** is in the middle and the default
- **Room to move** is how far you can shift around before the framing follows at all, so settling into your chair or leaning over for a second doesn't slide the picture around. It's named after what you can do without the framing reacting, from **Fidget** and **Shift** (in your seat, the default) to **Lean**, **Step** and **Roam**. Once you've moved further than that it follows and then holds still again where you stopped. At **Off** it follows every move
- While it's on, Zoom and Pan on the Transforms card are greyed out and flip and rotation still work as usual. Switching it off puts the zoom and pan back where you had them
- The skeleton button under the preview draws the points MediaPipe finds on you, with an arm in orange when it counts as held out. It's only drawn in the preview and never shows in the camera output. It's there for fun, and for checking what the framing is reacting to
- The phone still does as much of the zoom as it can. While the framing moves around the phone stays on a slightly wider crop and the small moves happen on the computer, which stops the phone from hopping between lenses every time you lean over. The phone's part only changes once the framing has settled somewhere new for a couple of seconds or is about to run off its edge. When it does, the computer waits for the phone's new picture to actually arrive before cropping for it, so the picture doesn't jump

## Background

Also optional and sharing the same download. **Blur** blurs everything behind you and **Picture** puts a picture of your choice there instead (PNG, JPEG, WebP or BMP). **Edges** sets how soft the edge around you is. Since it runs on the computer it works with any phone and with Browser camera too.

## The vision download

Auto framing and Background use MediaPipe to find people in the picture. MediaPipe and the two models it needs don't come with Telescope, since most people won't use them. The first time you switch one of them on Telescope asks and then downloads them (about 47 MB on Linux and 28 MB on Windows) from Telescope's releases on GitHub. Both plugins share that one download. Once both are off again **Remove download** on the Optional card deletes it.

It only works on 64-bit Intel and AMD computers for now.

## Presets

The Presets button in the header saves your camera, output and transform settings under a name (lens included) and brings them all back with one click later. Auto framing and Background are saved in presets too if they're switched on, so one preset can frame you while you talk and another can park the zoom on something you're showing, each on its own shortcut. Overwriting a preset keeps the saved settings of plugins that are switched off. Each phone has its own presets.

## Resolution and FPS
- The resolution list comes from what the current lens really supports, and picking one changes what the phone itself captures
- One FPS setting (15 to 60) drives both the phone and the virtual camera. Rates the lens can't do are greyed out
- **Canvas size** under Video in Settings sets the virtual camera's size separately from the phone's: 720p, 1080p or 4K in landscape or portrait, 4:3 sizes, or a custom one. Custom sizes have to be even because odd ones mess up the colours on Linux. On Linux, changing it reloads the driver, which takes one password prompt (close OBS first)

## Format and bandwidth
- **Light** (the H.264 format, and the default) needs about 8 Mbps at 1080p30, which makes it a good fit even for slower Wi-Fi. **Heavy** (MJPEG) is sharper in fast motion but needs USB or strong Wi-Fi. A phone that can't do Light switches to Heavy automatically
- Heavy has a JPEG quality slider. Light gets a bitrate slider instead: Auto, a fixed 1-100 Mbps, or **Dynamic**, which sends as much as the connection can carry and backs off before the video starts to lag. When even that leaves too little for each frame, Dynamic also lowers the frame rate (30, 24, 20, down to 15 at the lowest) to send fewer but sharper frames instead of a lot of smeared ones. It goes straight to the rate the connection is enough for and climbs back once there's room, more slowly than it came down. On a really slow connection it keeps going down to 0.3 Mbps, which looks soft but keeps the picture moving. A step doesn't restart the camera: the phone's encoder skips the frames, or where it ignores that, the first step restarts the camera once and Telescope skips them itself from then on. Hovering the FPS in the footer shows when Dynamic was the one that lowered it
- **UDP on Wi-Fi** (experimental, in the connection settings) sends Light over UDP instead of TCP when the phone is on Wi-Fi. On a busy network a lost packet then only costs a resend of that piece instead of holding up the whole stream, which can cut the lag spikes. It's encrypted with a key the phone hands out over the paired connection. If the video doesn't get through (a firewall, usually) it goes back to TCP by itself. Changing the switch reconnects a running Wi-Fi stream. Picking Dynamic switches it back to TCP. It isn't used over USB or for Heavy
- **Hardware acceleration** under Video in Settings decodes Light on the graphics card instead of the processor, which matters most at 4K or on a laptop. It's off by default and takes effect the next time the stream connects. It works on Windows with any graphics card and on Linux with an NVIDIA one. AMD and Intel cards on Linux need VA-API, which the PyAV Telescope installs leaves out. **Set up VA-API…** next to it uses your distro's PyAV if it's 14 or newer and has VA-API (Arch, openSUSE, Debian 13), or builds one against your system's FFmpeg in about a minute (it lists the packages to install first, if any are missing). It's kept apart from Telescope's own PyAV and used from the next start, and if it ever stops loading, Telescope goes back to its own on its own. Without any of these, the Video tab says there's no graphics card Telescope can use. If the graphics card ever fails to decode, Telescope goes back to the processor on its own for the rest of the run
- On Light, the resolution list leaves out sizes the phone's encoder says it can't take. If one still fails, the stream stops with a note suggesting a lower size or Heavy, and the resolution goes back to the last one that worked (so pressing Start again won't fail the same way)
- Switching to a new aspect ratio picks the size closest to your current height rather than the biggest one

## Microphone
- While streaming, the phone's mic can be a microphone on the computer, with the phone's own noise suppression if it has one
- Linux: Telescope creates **Telescope Microphone** through PulseAudio or PipeWire. Most desktops need nothing extra
- Windows: you'll need [VB-Audio Virtual Cable](https://vb-audio.com/Cable/) (free, donationware by VB-Audio), then pick **CABLE Output** as the microphone in other apps. If it's missing, the card's **Install VB-Cable** downloads VB-Audio's own pack from vb-audio.com, checks it's the one Telescope knows, and opens their setup (Windows asks for admin). VB-Audio asks for a restart afterwards. Once that's done, switch the mic off and on again in Telescope. If you close their setup before it finishes, the card offers **Open setup again**. If the download has changed or anything fails, the card links to their site instead
- Gain goes from -24 dB to +12 dB (**Max gain** under Audio in Settings can raise the top to +24, +36 or +48), and you can type in a value for an exact one. The level meter holds the latest peak and lights up yellow while the limiter is holding loud moments down, or red when the sound clips. The limiter is on by default and can be switched off under Audio in Settings
- **Auto level** (off by default) evens out quiet and loud speech. It stops adjusting while nobody is talking to not turn up the room noise in the pauses. Gain is applied before it
- **Mute** (in the card or the tray menu) sends silence without disconnecting the mic, handy for call apps that can't mute themselves. It resets when Telescope restarts
- The camera button under the preview turns the phone's camera off while the mic keeps going. In the meantime apps get the wait screen and the preview shows that the camera is off. Press it before starting and Start turns into **Start mic only**. This needs the mic on, and every new stream starts with the camera back on
- The tray icon shows what's live: a red dot while the camera is on and a mic while the mic is (crossed out when muted)

## Browser camera
- Stream from anything with a browser (an iPhone, a tablet, another laptop) without installing anything on it. Pick **Browser camera** in the phone picker, scan the code on its card and tap **Start** on the page
- The video goes over WebRTC, the way video calls in a browser do, so any up to date browser should work. It uses the H.264 format from the device's hardware encoder where the browser offers one and VP8 otherwise. Nothing goes through a server outside your network, the two devices talk to each other directly
- The browser's microphone works with the Microphone card too. It's sent raw like a USB mic would be (no echo cancellation or noise suppression) because Discord, Zoom and the like do their own and work best on untouched sound. Flipping the camera doesn't interrupt it, and a recording that failed is retried when you return to the page
- The first time on each device, the browser warns that the connection isn't private. Browsers only let a page use the camera over HTTPS, which normally relies on a certificate from a company the browser trusts. Those companies can't vouch for an address on your home network, so Telescope makes its own certificate and the browser complains because it can't check who made it. The connection is still encrypted. To get past the warning on iPhone, tap **Show Details** and then **visit this website**. In Chrome it's **Advanced** and then **Proceed**
- Keep the page open with the screen on. Phones pause the camera when you switch apps or lock the screen, but the stream picks up again once you're back on the page
- **Black screen** on the page turns everything black while the camera keeps streaming, which saves battery on OLED phones and stops a bright screen from lighting up the room. Where the browser allows it the page also goes full screen to hide the phone's status bar. iPhones don't let web pages do that, so there the page plays a black video in the iPhone's own video player instead, and closing the player brings the page back. If the camera stops while the player is up, the page closes it and stays on the plain black screen from then on. Double tap to get the page back. A single tap only brings up the hint and the connection status for a few seconds, in case the phone gets bumped on its stand
- **New link** makes a fresh code and the old one stops working right away. Any browser still connected with it gets dropped, its page says the code was replaced (with no Start button, since that code is dead) and apps that were watching it fall back to the wait screen
- Several devices can scan the same code. Each one starts streaming as soon as it connects: the first becomes the stream and the rest join next to it, each on its own virtual camera (see [Several cameras at once](#several-cameras-at-once)). The page shows which camera it's streaming as, and reloading it picks the same stream back up
- While a browser is connected it shows up in the phone picker under its device name, just like a phone. Two of the same kind are told apart as "Chrome on Linux" and "Chrome on Linux (2)". With **Browser camera** picked, **Start** streams a browser that's already connected or waits for one to scan the code
- A browser gets its own saved settings like a phone does, but only once you actually change one. That way a quick scan from a friend's phone doesn't leave anything behind. Saved browsers are listed under **Your phones**, where **Remove** forgets them, and they're forgotten anyway after 60 days without connecting
- Camera, output and alert settings don't apply to browsers. Resolution (480p to 1080p) and frame rate (15 to 30) are on the card instead
- On Linux the virtual camera is 1920x1080 unless you set another canvas under Video in Settings, because a browser's first frame could be any shape
- The card shows what the browser is really sending, like "H.264, 1280×720 at 30 fps", and the page shows the size and frame rate under its status. They can be lower than what's picked here: the browser lowers them when the device or the Wi-Fi can't keep up and goes back up once it can. When it's the device's processor holding it back the card's status turns into a warning, and a lower resolution or frame rate usually helps
- The **Light** button on the browser page (not the Light format above) turns on the camera's flash where the browser can reach it (Chrome on Android usually can). A front camera without one gets the screen turned white as a light instead, and tapping it turns that off. Web pages can't change the screen brightness, so turn it up yourself for more light
- **Flip camera** only shows on devices with more than one camera. The page mirrors the preview for front cameras (and for cameras that don't say which way they face, like a laptop's) but never what it sends

## Several cameras at once
- While one stream is running, the **+** next to Stop adds another phone (or the browser camera) alongside it, up to four in total. Each gets its own virtual camera: **Phone Camera 2** to **4** on Linux, **Telescope #2** to **#4** on Windows
- The first time, Telescope has to add those cameras, which asks for your password on Linux or for permission on Windows. On Linux they disappear after a reboot and get added again next time. **Extra cameras** under Virtual camera & phone in Settings removes them sooner, and on Windows that's the only way to get back to one
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
- Phone battery and temperature, with alerts you can set to notify you, stop the stream, or both. When an alert stops the stream, the window explains why and offers **Start anyway** to keep streaming until the battery or temperature is back to normal. On Android 10 and newer a phone that reports running hot lowers its frame rate first (to 24 fps and then 15) and the FPS readout's tooltip shows when that happened. From Android 11 it does that a bit earlier, as soon as Android expects the phone to get that hot within half a minute, so it often never gets there
- **Open log** and **Copy diagnostics** are under Updates & About in Settings. Copy diagnostics is what to paste into a bug report. The log lives in the temp folder (`/tmp/telescope-<user>/` on Linux, `%TEMP%\Telescope\` on Windows) with addresses, tokens and your home folder stripped out

## Phones, pairing and connection
- **Add phone** pairs by QR code or by plugging the phone in over USB. USB needs adb: on Windows, Telescope downloads it from Google the first time, after asking
- The **?** next to it brings the setup checklist back (with the phone app download and Add phone), even after you've streamed from Browser camera. Click it again to close it
- You can pair several phones to one computer and one phone to several computers. Removing one leaves the others paired
- Telescope uses USB when the phone is plugged in and answering, and Wi-Fi otherwise. If a cable is plugged in but isn't being used, the Connection panel explains why. **Connect via** can force one or the other if you wish. When you change it mid-stream, it first checks that the phone answers that way and leaves the stream alone if it doesn't
- Picking another phone or browser in the picker while streaming switches the stream over to it
- A new IP address from the router doesn't break anything, since the phone announces itself on the network
- When a stream over Wi-Fi can't keep up and the phone has a weak signal or is on 2.4 GHz, the note says so, since moving closer, a 5 GHz network or a cable fixes that better than lowering the quality. Copy diagnostics includes the phone's Wi-Fi band, signal and link speed
- The phone's screen stays on while the Telescope app is open, because Android only keeps Wi-Fi at its quickest with the screen on. If you'd rather have it dark just press the power button and the stream keeps going
- Camera, output, transform and alert settings are saved per phone, while connection settings and the canvas are shared

## Privacy
- Everything between the phone and the computer is encrypted with TLS. When you pair them, the desktop remembers your phone's certificate and won't talk to anything that shows up with a different one, so nobody else on the network can pretend to be your phone
- The pairing token only ever travels in the QR code (or over adb) and inside that encrypted connection
- Someone on the network can still see that a stream is running and roughly how much data it moves. **Local only - USB** in the Android app keeps the stream and the camera controls off the network entirely
- If you'd like to start the camera from your PC without picking up the phone, turn on **Wait for my computer**. The phone then stays ready even with the screen off or the app closed, until you tap **Stop waiting** in its notification. It's off by default
- Browser camera only listens on port 8767 while it (or a browser) is picked in the phone picker, or while a browser is streaming. Anyone can load the page itself, but sending video needs the token in the code, and that changes every time Telescope starts or you click **New link**. The video and sound themselves go over WebRTC, which encrypts them too, and only to a browser that got in with the token. Its certificate isn't pinned the way a phone's is, so on a network you don't trust it's better to use a phone
- The apps only go online to check GitHub for updates. On Windows the desktop also fetches adb from Google or VB-Cable from VB-Audio, but only after you click to get them
- On Linux, only your user can read the config folder with the pairing tokens. On Windows the camera driver is installed into Program Files, where other programs can't swap it out

## Updates
- Both apps check for updates at launch and once a day, on **Stable** or **Nightly**
- Desktop: an **Update** button shows up in the header and installs with one click (just not while streaming). If an update gets cut short or the new version won't start, the next launch finishes it or puts the old one back
- Windows, from the zip: updating installs the new version for your user instead (Start menu entry, uninstaller, no admin), and once it has started it deletes the unzipped folder. Only Telescope's own files are removed (anything else you had in that folder stays). Settings, paired phones and the old zip's adb come along. A desktop shortcut to the old copy gets swapped for one pointing at the installed copy. A copy on a USB stick or a network drive always updates in place, and anywhere else you can tick **Keep this copy where it is** in Updates to get the same
- Phone: the update card downloads, checks and installs the new APK. When the phone app is older than the desktop, the Connection panel points it out and can update it over USB

## System integration
- **Automatic streaming** (under Streaming in Settings, off by default) starts when the phone is ready or when an app opens the camera. It stops once no app has used the camera for **Wait before stopping**, which is 15 seconds unless you change it. If the phone mic is on it only turns the camera off, so a call that switched its camera off can still hear you, and the camera comes back when an app opens it again. After you press Stop yourself, it won't start again until the phone or the app goes away and comes back
- While the window is in the tray or minimized, Telescope stops checking on the phone to save a waiting phone's battery. It only carries on when something could still start a stream from there: "when the phone is ready" being on, or an app using the camera. After "when the phone is ready" has started a stream (or you pressed Stop) the check drops to about once a minute, which is enough to notice the phone leaving. A phone that drops off and comes back within that minute won't count as a new arrival though
- **Wait screen** (under Streaming in Settings): what apps see on the camera while the phone isn't streaming. That's either the default screen or your own image or GIF, and there's a **Mirror** option for apps that flip the camera
- **Open Telescope when I sign in** (under General in Settings) starts it quietly in the tray
- Closing the window while streaming (or while waiting to start automatically) keeps Telescope running in the tray
- When Start can't go ahead, a banner tells you why and has the button that fixes it
- Click a card's title (Camera, Transforms, Connection and so on) to fold the card down to its header, and click it again to open it. Section titles inside a card work the same way, so you can fold just **Image** in the Camera card and leave **Light** and **Focus** open. A card remembers which of its sections you folded, even while the whole card is folded. A card title gets darker when the pointer is on it and a section title lights up to the colour of the card titles and there's no other sign that it can be clicked
- Shortcuts keep working while their card or section is folded. Controls hidden by a mode or greyed out still can't be used.
- Everything starts open and folds are remembered after a restart. They're saved for the whole app and not per phone. If you switch a plugin off and on again its card comes back folded the way you left it
- Tab reaches card and section titles; Space or Enter folds or opens them. Screen readers get a named toggle and its state. Folded controls leave the tab order, and folding a card while a setting has focus moves focus to its title
- Every card in the main window has its own colour. **Monochrome** under **Card colours** in General puts them all back in the app's purple if you'd rather have that

## Keyboard shortcuts
- Settings has a search box above its tabs (Ctrl+F). It forgives typos and finds things by what they do, so "resolution", "gpu" or "login" lead to Canvas size, Hardware acceleration and Open Telescope when I sign in. Picking a result takes you to the tab and lights the setting up
- The **Keyboard shortcuts** tab in Settings binds a key to almost any control: mute, the presets, starting and stopping, the lens, every camera slider, the transforms, the output settings, the preview buttons and the phone picker. Right-clicking one of those controls in the main window adds a shortcut for it straight away
- Each shortcut has its own settings, so one key can raise exposure by 1 EV, another lower it by 0.3 EV and a third set it to exactly 0. Sliders step up or down by an amount you pick, set a value, or reset to their default. ISO and shutter step in stops. Drop-downs and button rows go to the next, the previous or a named option. Switches turn on, turn off, toggle, or flip only while you hold the key, which makes the Mute button push to talk. Holding a key down repeats a step
- A shortcut is global by default and also works while another app is in front. On Windows Telescope claims the key from Windows. If another app already has that key, Windows hands it to that app even while Telescope is in front, and the shortcut won't work until you pick another key. On Linux it goes through the desktop's global shortcuts portal (Plasma 5.27 and later, GNOME 48 and later, Hyprland). The first time, the desktop asks you to confirm the keys and may pick different ones (the dialog shows whichever key it settled on). **Open** next to **Desktop shortcut settings** opens the place to change them
- On other desktops shortcuts only work inside Telescope, but you can still make them global by binding a command in the desktop's own keyboard settings. Right-click a shortcut and pick **Copy command line**, or use `telescope --action ID name=value`, for example `--action microphone.mute mode=toggle`. `--list-actions` lists every action and its settings. Only the user running Telescope can drive it this way
- **Notify when one runs** gives a quick confirmation while Telescope is in the background, off by default

## Plugins
- The **Plugins** tab in Settings lists every part of the desktop app that can be switched off (like the Camera card, Transforms, Presets or the microphone) with a line about what each one does. Telescope asks before switching one off and tells you what will be missing. Its settings stay saved for when you switch it back on. Changes apply after a restart and the tab has a **Restart now** button for that
- When some are off the tab says how many (**Plugins (2 off)**) and after the restart a banner names what's off
- **Install** under **Added** adds a plugin someone shared as a `.zip` and dragging the file onto the tab works too. Added plugins start out switched off and Telescope warns you before you switch one on. One whose files changed gets switched off again until you switch it back on
- **Optional** lists the plugins that start out switched off, which right now are Auto framing and Background. They need [the vision download](#the-vision-download) the first time one of them goes on
- [Plugins](plugins.md) covers installing plugins and writing your own
