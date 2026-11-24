<!-- covers: docs/features/camera.md docs/features/picture.md docs/features/effects.md docs/features/sources.md docs/features/output.md docs/features/connection.md docs/features/app.md; checked: 88de924b5 -->
# Features

Telescope streams your phone's camera to your computer, where it shows up as a virtual webcam that Discord, Zoom, OBS and any other app can pick. This page is a tour of what it can do. Each feature has a few plain lines here and a link to the page with every setting and the small print.

Most of the desktop app is plugins. A feature marked optional is switched on under the **Plugins** tab in Settings and starts after a restart. **Lab** marks an early version that is still being finished (see [Plugins](#plugins)).

- [Camera and quality](#camera-and-quality) covers lenses, exposure, the picture format and bandwidth
- [Framing and background](#framing-and-background) covers keeping you in frame and changing or hiding what's behind you
- [Effects and overlays](#effects-and-overlays) covers fun effects, looks, photos and things drawn on the picture
- [Sources and layouts](#sources-and-layouts) covers other cameras, several at once and 3D
- [Sound, the On Air sign and monitoring the stream](#sound-the-on-air-sign-and-monitoring-the-stream) covers the microphone, the On Air sign and stream stats
- [Connection and privacy](#connection-and-privacy) covers pairing, what the apps send where and updates
- [The desktop app](#the-desktop-app) covers automatic streaming, shortcuts, the MCP server and plugins

## Camera and quality

### Camera control

Pick between the phone's real wide, main and telephoto lenses and set ISO, shutter speed, brightness, white balance and focus yourself, like the Pro mode in a camera app. Anything the current lens can't do is greyed out. Details in [Camera and quality](camera.md#camera-control).

### Stream transforms

Flip, rotate, zoom from 1 to 10x with pan and stretch or squish the picture while it's live, with no restart. Lens correction straightens the curved lines a wide lens makes. Details in [Camera and quality](camera.md#stream-transforms).

### Presets

Save your camera, output and transform settings under a name, plus the optional picture plugins that are on, then restore them in one click. Each phone has its own presets and each preset can have its own shortcut. Details in [Camera and quality](camera.md#presets).

### Resolution and FPS

Choose from the resolutions the current lens supports and a frame rate from 15 to 60. **Canvas size** under Video in Settings sets the virtual camera's own size, such as 720p, 1080p or 4K. Details in [Camera and quality](camera.md#resolution-and-fps).

### Format and bandwidth

**Light** (H.264) is the default and works for most things at about 8 Mbps for 1080p30. **Heavy** (MJPEG) is sharper for fast motion but wants USB or strong Wi-Fi. **Dynamic** adjusts Light's bitrate to the connection and decoding can run on the graphics card. Details in [Camera and quality](camera.md#format-and-bandwidth).

## Framing and background

### Auto framing

Optional. It finds you in the picture and moves zoom and pan to follow you, from only your head to your full body. Two people get framed together, or you can click one person to follow. It needs [the vision download](#the-vision-download). Details in [Framing and background](picture.md#auto-framing).

### Background

Optional. Blur whatever is behind you or put a picture there. It runs on the computer and works with any phone and with Browser camera. A **High quality** cutout follows hair and edges better and uses more processor. Details in [Framing and background](picture.md#background).

### Expose for my face

Optional. It moves the phone's metering point to your face, which helps when a bright window is behind you. Details in [Framing and background](picture.md#expose-for-my-face).

### Privacy blur

Optional and marked **Limited**. When a second person comes into view it blurs the picture or shows the wait screen. **Blur now** does the same on demand. It can miss someone, so don't rely on it to protect private content. Details in [Framing and background](picture.md#privacy-blur).

### Chroma key

Optional, for a green or blue screen. It swaps the screen for a colour or a picture, with a sharper edge than Background, much less load on the computer and no download. Details in [Framing and background](picture.md#chroma-key).

### Desk and document mode

Optional. Point the phone down at a sheet of paper, mark its four corners and Telescope straightens it so viewers see it flat and head-on. **Contrast** and **Black and white** make writing stand out. Details in [Framing and background](picture.md#desk-and-document-mode).

### The vision download

Auto framing, Background, Expose for my face and Privacy blur find people with MediaPipe, which isn't included in Telescope. It downloads the first time you switch one of them on (about 47 MB on Linux and 28 MB on Windows) and **Remove download** deletes it again. It only works on 64-bit Intel and AMD computers for now. Details in [Framing and background](picture.md#the-vision-download).

## Effects and overlays

### Fun pack

Optional. One **Effect** picker with Clone party, Invisibility cloak, Light painting, Slit-scan, Stick figure and Datamosh, plus retro looks like Game Boy, 1998 webcam, VHS, CRT and ASCII. Details in [Effects and overlays](effects.md#fun-pack).

### Cinema tools

Optional. Focus peaking, zebras, false colour, framing guides, scopes and a level for judging focus and exposure while you set them manually. They show on the preview and don't reach apps. Details in [Effects and overlays](effects.md#cinema-tools).

### QR codes

Optional. Hold a QR code up to the camera and its text lands on your clipboard. A Wi-Fi code gives you the password. Details in [Effects and overlays](effects.md#qr-codes).

### Photos

Optional. Take a real photo on the phone at the camera's full quality rather than a frame of the video. Depending on the phone you get JPEG, HEIC, Ultra HDR or RAW, bursts, exposure and focus brackets that can be merged for you, a timer and full resolution. A DSLR over USB can take photos from the same card. Details in [Effects and overlays](effects.md#photos).

### Phototrap

Optional. Draw zones on the preview and arm it. Each time something moves in one the phone takes a short burst of photos. Nothing is saved until then and it never records video. Details in [Effects and overlays](effects.md#phototrap).

### Colour grading

Optional. Four colour wheels, curves, a loadable LUT and an **Auto** button that sets a look from about a second of the picture. **Compare** splits the preview into before and after. **Export grade…** saves the look as a `.cube` file for other software. Details in [Effects and overlays](effects.md#colour-grading).

### Filters

Optional. The basics of a photo editor: denoise, dehaze, skin smoothing, sharpen, clarity, vignette, glow and film grain. A filter at 0 is off. Details in [Effects and overlays](effects.md#filters).

### Notes next to the lens

Optional. Type your notes and send them to the phone's screen in big text right under the front camera, to read them while looking almost into the lens. **Next** and **Previous** step through the lines, or choose **Auto** to scroll at a speed you set. Details in [Effects and overlays](effects.md#notes-next-to-the-lens).

### Stargazer

Optional and in Lab, for a phone clipped to a telescope. It stacks many frames into one sharper or cleaner picture with Lucky imaging, Live stack or Star trails and has a **Focus** readout to help you focus. Details in [Effects and overlays](effects.md#stargazer).

### Name tag

Optional. It puts your name and pronouns in a corner of the picture, with a smaller second line such as your role. There are four styles. With several cameras each can have its own tag or none. Details in [Effects and overlays](effects.md#name-tag).

## Sources and layouts

### Browser camera

Stream from anything with a browser, like an iPhone, a tablet or another laptop, with no app to install. Pick **Browser camera**, scan the code and tap **Start** on the page. Details in [Sources and layouts](sources.md#browser-camera).

### Webcams and capture cards

A webcam plugged into the computer streams like a phone, with every effect available. Cameras with a USB webcam mode, HDMI capture cards and many older DSLRs over USB work too. Details in [Sources and layouts](sources.md#webcams-and-capture-cards).

### Spout capture

Optional and in Lab. It streams the picture from programs that share through Spout, such as games with a stream camera, OBS and VTuber programs. Each sender shows up like a camera. On Linux it picks up Windows programs running in Proton. Details in [Sources and layouts](sources.md#spout-capture).

### Spout send

Optional and in Lab, the other way round from Spout capture. It shares a stream's picture, after every effect, with other programs on the same computer as a Spout sender on Windows or a PipeWire video source on Linux. Details in [Sources and layouts](sources.md#spout-send).

### Several cameras at once

Click **+** next to Stop while streaming to add another phone, Browser camera or webcam, up to four. Each gets its own virtual camera, called **Telescope #2** to **#4**. Effects belong to the camera you set them on. Details in [Sources and layouts](sources.md#several-cameras-at-once).

### Several cameras in one picture

Optional. It combines the streams you run into one picture for apps that take only one camera, such as your face big with the desk small in a corner, two phones side by side or a grid of up to four. Layouts have an editor, can be saved and can switch with a shortcut. Details in [Sources and layouts](sources.md#several-cameras-in-one-picture).

### 3D from two phones

Optional and in Lab. It turns two phones streaming at the same time into one 3D picture, side by side for a headset or a 3D screen or red/cyan for glasses, with sliders and a **Match** button to line them up. A **Headset** switch shows it in a headset's browser. Details in [Sources and layouts](sources.md#3d-from-two-phones).

## Sound, the On Air sign and monitoring the stream

### Microphone

The phone's mic becomes a microphone on your computer, with gain, **Auto level**, noise reduction and **Mute**. Linux needs no extra software. Windows needs the free VB-Audio Virtual Cable, which the card can download and open for you. Details in [Sound, the On Air sign and monitoring the stream](output.md#microphone).

### On Air sign

Optional. People you live with can see that your camera is in use, on a page you open on an old tablet or phone. A webhook can switch a light or a smart plug. Details in [Sound, the On Air sign and monitoring the stream](output.md#on-air-sign).

### Monitoring

FPS and throughput show in the footer. A dropped stream reconnects over whichever route works. Phone battery and temperature have alerts that can notify you or stop the stream. An optional **Stats** card shows the numbers behind the stream and **Adjustments** lists what Telescope changed to keep it smooth. Details in [Sound, the On Air sign and monitoring the stream](output.md#monitoring).

## Connection and privacy

### Phones, pairing and connection

Pair a phone by QR code or over USB. Several phones can pair with one computer and one phone with several computers. Telescope uses USB when the phone is plugged in and Wi-Fi otherwise. **Connect via** can force either one. Details in [Connection and privacy](connection.md#pairing-phones-and-how-they-connect).

### Over Tailscale

The phone can reach the computer away from home over [Tailscale](https://tailscale.com) with no setting to switch on. The page also covers the ports to open and how to tell whether you're on a direct path or a relay. Details in [Connection and privacy](connection.md#over-tailscale).

### Privacy

Video goes straight from the phone to the computer, encrypted. The apps only go online to check GitHub for updates and on Windows to fetch adb or VB-Cable after you click to get them. **Local only - USB** in the phone app takes the stream off the network entirely. Details in [Connection and privacy](connection.md#privacy).

### Updates

Both apps check at launch and once a day, on the **Stable** or **Nightly** channel. The desktop app updates in one click from a button in the header. The phone app downloads and installs the new APK. Details in [Connection and privacy](connection.md#updates).

## The desktop app

### System integration

**Automatic streaming** starts when the phone is ready or an app opens the camera and stops once no app has used the camera for a while. You can also set your own wait screen, have Telescope start quietly in the tray when you sign in and reset any card or section to its defaults. Details in [The desktop app](app.md#system-integration).

### Keyboard shortcuts

Bind a key to almost any control and use it from other apps too. A switch can work only while the key is down, for example to use **Mute** as push to talk. Details in [The desktop app](app.md#keyboard-shortcuts).

### MCP server

Optional. An AI app like Claude Desktop can read and change Telescope's settings, with a **Read only** switch and an **MCP** button in the top bar that undoes what the AI changed. Telescope itself sends no data to a model. Details in [The desktop app](app.md#mcp-server) and [MCP server](mcp.md).

### Plugins

Most of the desktop app is plugins, which you switch on and off in the **Plugins** tab in Settings, with changes applying after a restart. Plugins other people made can be installed too. Details in [The desktop app](app.md#plugins) and [Plugins](plugins.md).
