# Some day

Ideas worth keeping that nobody is about to work on. Move one to [next.md](next.md) once it's planned.

## Scenes and a whiteboard

Presets already save Auto framing and Background, so a preset can be a scene: one that frames you while you talk, another with the framing off and the zoom parked on a whiteboard or a desk, each on its own key. A teacher, someone running a workshop or anyone drawing on paper could switch between them mid-call.

The bigger part is the board itself. Telescope would find the board's four corners, straighten it out when the phone looks at it from an angle, even out shadows and glare and make the writing stand out. None of that exists yet, and it's plain OpenCV rather than MediaPipe. Tuning it so it works in real rooms on real boards is most of the work. A Whiteboard source or card could follow the board the way Auto framing follows you.

Big, mostly the corner finding and the clean-up. Worth seeing first whether people use presets as scenes.

## Mic noise reduction

The phone switches on its own noise suppressor and gain control when it has them, but those vary a lot between phones and some have neither. Running RNNoise on the desktop would clean up typing, fans and room hiss the same way on every phone (and for Browser camera's mic). It needs a Python binding or a small native library, and the Microphone card would get a switch for it. **Auto level** is already in, so this is the part that's left.

It would run in the mic's writer loop in `desktop/telescope/audio.py`, which has to keep up in real time (10 ms chunks).

## A Windows 11 camera every app can see

On Windows the virtual camera is UnityCapture, a DirectShow filter. Apps that only look for cameras through Media Foundation never see it. Windows 11 added `MFCreateVirtualCamera`, which registers a camera with the system so every app sees it. That takes a small native DLL that Windows loads, with the app handing it frames through shared memory. A nice side effect is that the camera only opens that shared memory while some app is using it, which would give Automatic streaming a reliable way to tell when the camera is in use.

Big. It means a native DLL next to the Python app, registering it with admin rights, keeping UnityCapture for Windows 10, and a lot of testing across apps.

## Dynamic over UDP

UDP on Wi-Fi and Dynamic can't be on together right now, so it's one or the other. Dynamic decides from how long frames wait in the phone's send queue, which only shows a problem once the queue is already backing up. Over UDP the desktop asks for every lost piece again, and how often it has to ask is an earlier and cleaner sign the link is getting too thin. Dynamic could step the bitrate down on that and keep UDP's lower lag spikes at the same time.

Medium to big. It needs the resend rate reported back to the phone, the step-down tuned on a real thin link, and `scripts/phone_check.py` would be the place to measure it.

## Plugin updates

Updating a plugin right now means removing it with **Remove** and installing the new `.zip`. Installing the new version should just replace the old one instead and keep its settings. The new version still starts out switched off like any other new plugin, and switching it on is always a step you do yourself.

`plugin.json` could also get a `repo` link that the Plugins dialog shows next to the author. It's only a shorter way to look at the source and download a new version. Nothing gets pulled in the background. For plugins that publish their builds as GitHub Releases there could be an optional update check on top of that. It would only tell you a new version is out so you know to go get it.

If someone really wants updates installed and switched on without asking, that could be a switch in the Plugins tab. It should be a big red one surrounded by warnings, since it lets whoever controls the plugin's repo run new code on your computer every time they publish a release.

Medium. Most of it is in the Plugins dialog and the install code. The update check has to stay quiet when GitHub is unreachable or rate limiting it.

## Optional plugins

Ideas for plugins that start out off, for people who want more out of their webcam than a picture. Most of them need no new dependencies (numpy, OpenCV, PyAV and Qt are already there and the vision download already exists). Same rule as the Camera card. If the phone's camera exposes something, Telescope uses it.

Several plugins need the same things underneath. Those get built once as host services like the vision service and not as plugins that depend on each other (see `desktop/ARCHITECTURE.md`):

- **Motion detection.** Zones drawn on the preview, a sensitivity and an event when something moves in one. Plain OpenCV frame differencing. Used by the phototrap and later maybe by Power-saving effects
- **Calm people tracking.** Smoothing and slow backoff on top of `bus.people_seen`, since MediaPipe drops people and finds them again all the time. Used by Expose for my face, the auto blur and a name tag that follows you
- **Preview-only overlays and scopes.** Drawing on the preview without it ever reaching the output, plus the waveform, parade and vectorscope maths. Used by the cinema tools, colour grading, motion zones and the document corners
- **Picking points and areas on the preview.** Used for document corners, motion zones, the chroma key colour, the grey patch for colour matching and the stargazer's target
- **Frames from other streams.** Right now a plugin only sees the stream it's processing. Used by Several cameras in one picture, colour matching between phones and 3D from two phones
- **Full-quality stills.** Phone app and protocol work, see below. Used by Photos, the phototrap, the stargazer and document mode
- **Lining up and stacking frames.** OpenCV's `phaseCorrelate` and friends. Used by the stargazer and maybe a denoised "hold still" scan in document mode
- **The colour pipeline.** Every colour control baked into one 3D LUT and applied on the graphics card through Qt's OpenGL (applying a 3D LUT per pixel in numpy won't keep up at 1080p60). Used by colour grading, LUTs, colour matching and the chroma key's spill removal
- **Encoded packets.** A hook that hands plugins the H.264 packets before PyAV decodes them. Only datamosh needs it for now

### Cinema tools

Focus peaking, zebras, false colour and a histogram, plus the waveform, RGB parade and vectorscope. All of it shows on the preview only and never goes out to apps. Telescope already has manual ISO, shutter, focus and white balance and this is what you need to judge them properly. Small to medium, mostly the overlays and scopes service.

### Colour grading

A grading card a videographer wouldn't laugh at. Colour wheels for lift, gamma, gain and offset, temperature and tint, contrast with a pivot, saturation, highlight and shadow balance, curves (RGB, hue vs saturation, hue vs hue) and a before and after split. It also loads `.cube` LUTs and can export your grade as a `.cube` to use the same look in other software. Grades get saved in presets.

Colour match between phones samples a grey card or patch on each stream and evens out white balance and tint, for when two different phones look like two different films side by side.

The phone sends 8-bit SDR now, which bands under heavy grades. Phones with 10-bit HLG (dynamic range profiles in Android 13) or a maker's log profile would give a lot more room, with turning log back into normal SDR becoming part of the grade. That needs 10-bit HEVC through the phone app, the protocol and the decoder and makes it a stretch goal on top.

Medium for the card, big with 10-bit.

### Chroma key

A proper key for people who own a green screen, with spill removal. It's sharper and much lighter than Background's MediaPipe mask and doesn't need the vision download. Small to medium.

### Several cameras in one picture

Telescope runs up to four streams but most call apps take one camera. A small editor puts several streams on one canvas where you drag, resize and crop each one and add borders, rounded corners and a background. Layouts like "face big, desk small" or "side by side" get their own keys and ride along in presets. An auto director that picks a layout from who's in frame could come later.

Big. Plugins can't read other streams' frames yet and the virtual camera slots have to make sense when several streams go into one.

### Expose for my face

With a bright window behind you the phone exposes for the window and you end up as a silhouette. This moves the metering point (the same one **Tap a point** uses) to your face. It's only worth shipping if it's calm. The point should move slowly, ignore small head moves and hold the last exposure when you leave the frame instead of chasing the empty room. Small to medium, most of it is tuning.

### Blur when someone else shows up

When a second person walks into the picture, blur everyone but you, blur the whole background or cut to the wait screen. MediaPipe loses and finds people constantly and the timing has to be uneven because of it. The blur should come on after a second person shows up in a couple of updates in a row and only go off once a single person has been seen steadily for several seconds. Somebody walking around behind you shouldn't make it flicker. A key that does the same by hand works without the vision download. Medium.

### Desk and document mode

Drag the four corners of a sheet of paper on the preview and Telescope straightens it with a perspective warp, with an option to make the writing stand out. Good for homework help, showing a sketch or signing something on a call. It's a manual first step towards the automatic board finding in [Scenes and a whiteboard](#scenes-and-a-whiteboard). Small.

### Photos

Take a real photo on the phone from the desktop, not a frame grabbed from the video. Everything Camera2 has should be there when the phone has it:

- RAW (DNG), JPEG with a quality setting, HEIC and Ultra HDR JPEG (Android 14)
- The full sensor resolution through the maximum resolution sensor mode (Android 12), which gets the real 50 or 200 MP out of sensors that normally give 12
- The phone's own Night, HDR and Bokeh modes through Camera2 extensions (Android 12), where the maker exposes them
- Long exposures up to what the sensor allows (30 s or more on some phones)
- Exposure bracketing and focus bracketing for HDR merging and focus stacking macro shots
- High-quality noise reduction and sharpening, hot pixel correction and a distortion correction switch
- A timer and bursts, with the lens and manual settings taken from the video

Some of these (the full resolution mode and some RAW setups) need the camera session set up again, which pauses the video for a moment. A plain JPEG next to the stream usually doesn't. Medium to big, most of it in the phone app.

### Phototrap

For wildlife and anything else that shows up when you're not looking. Draw zones on the preview and when something moves in one the phone takes a short burst of full-quality photos. Nothing is saved until something sets it off and it only ever saves photos, never video. Night mode and long exposures from Photos make it work after dark. Medium once motion detection and Photos exist.

### Stargazer

People clip phones to telescopes. Lucky imaging for the moon and planets grabs a burst, keeps the sharpest frames, lines them up and averages them. Live stacking does the same slowly for faint things and star trails stack a long series. Manual ISO and shutter are already there, focus peaking helps focusing on a star and Photos brings long exposures and RAW. Medium.

### 3D from two phones

Two phones side by side make a stereo camera. One plugin shows it as red/cyan for glasses on any screen or side by side for a headset. For a Quest or similar the Browser camera server can serve a WebXR page that shows each half to one eye. Nothing has to be installed on the headset.

Getting the two pictures to match is most of the work:

- **Timing.** Frames get paired by when they were captured and not when they arrived. Each phone sends its sensor's capture time with every frame and the desktop works out each phone's clock offset from round trips over the connection it already has. The faster stream then waits for the slower one. That takes network and processing delay out of it. Two separate phones can't fire their shutters together though. The best case is within half a frame (about 16 ms at 30 fps), fine for talking with a little ghosting on fast moves
- **Alignment.** A sheet to print with a ChArUco board on it (a chessboard with codes in the squares that OpenCV finds out of the box). Hold it up, both phones find the same corners and the plugin works out how the phones sit compared to each other. It then shows how to turn or tilt one of them until they're close and straightens the rest with a warp on each stream. That warp is a rectifying one that turns the whole picture (Desk mode's only flattens the sheet) but it can go through the same warp code
- **Colour.** Grey and colour patches on the same sheet give colour matching between phones its reference
- **Checking the timing.** Paper can't show time but the desktop screen can. A fast changing code on the monitor filmed by both phones shows the real offset between them, a good test of the clock sync

Some phones can stream two of their own rear cameras at once from one camera session, with the frames hardware synced. The cameras sit only a centimetre or two apart and the depth comes out subtle. In return there's no clock to sync and nothing to align. Worth offering where the phone has it.

Big. It needs capture times through the phone app and the protocol, frames from other streams, the calibration sheet and its guidance, the rectifying warp and the WebXR page.

### Smaller ones

- **QR and Wi-Fi code reader.** Hold a code up to the camera and the link or the Wi-Fi details go to the clipboard. OpenCV has the detector built in. Tiny
- **On Air sign.** A page on another device through the Browser camera server, or a plain webhook for smart home stuff. The people you live with know you're on a call without you telling them. Small
- **Name and pronoun tag.** A clean lower third drawn with Qt and saved in presets. Small
- **Notes next to the lens.** Your notes shown on the phone's screen right by the camera, for looking people in the eye while you read. Needs phone app work. Medium

### A fun pack

One plugin for playing around, after the useful ones above. Clone party, an invisibility cloak (a clean plate plus frame differencing, nicer edges with `person_mask`), light painting, slit-scan, a stick figure stand-in and retro looks (Game Boy, 1998 webcam, VHS, CRT, ASCII). Real datamosh too, by dropping keyframes from the H.264 packets before they're decoded (only with Light, Heavy has no keyframes to drop). Each one is about an afternoon apart from datamosh, which needs the packet hook.

### Not for now

- **Live captions** would be the most useful accessibility plugin on this list but need Whisper or Vosk and a model. That means a second optional download like the vision one. Worth deciding on its own

## A guide to unusual setups

A doc with tips for using Telescope in ways it wasn't built around, starting with Tailscale. Pairing over Tailscale already works (the QR code carries the Tailscale address after the LAN ones) and it doesn't matter where either device is. That covers a lot without writing any new networking code:

- A friend joins your tailnet, pairs their phone with your Telescope and their camera shows up in your OBS as a remote guest, much sharper than a screen-captured video call
- A phone at the cottage or a stargazer rig in the garden that you watch from your desk
- Helping someone remotely by pairing your phone with their Telescope

Before writing it a few things need trying on real links:

- How Dynamic copes with a slow internet connection and which resolution and bitrate to recommend
- How much slower it gets when Tailscale relays traffic through DERP instead of connecting directly, and how to check that with `tailscale ping`
- Whether UDP on Wi-Fi helps or hurts there
- Reconnecting when the phone switches between mobile data and Wi-Fi
- Whether Browser camera works over a 100.x address

Small to write. The testing is the work.
