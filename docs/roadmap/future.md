# Some day

Ideas worth keeping that nobody is about to work on. Move one to [next.md](next.md) once it's planned.

## Scenes and a whiteboard

Presets already save Auto framing and Background, so a preset can be a scene: one that frames you while you talk, another with the framing off and the zoom parked on a whiteboard or a desk, each on its own key. A teacher, someone running a workshop or anyone drawing on paper could switch between them mid-call.

The bigger part is the board itself. Desk mode already straightens a sheet once you mark its corners and makes the writing stand out. What's missing is Telescope finding the corners itself, following a board as the phone moves and evening out shadows and glare. It's plain OpenCV rather than MediaPipe. Tuning it so it works in real rooms on real boards is most of the work. A Whiteboard source or card could follow the board the way Auto framing follows you.

Big, mostly the corner finding and the clean-up. Worth seeing first whether people use presets as scenes.

## Optional plugins

More ideas for plugins that start out off, for people who want more out of their webcam than a picture. Most of them need no new dependencies (numpy, OpenCV, PyAV and Qt are already there and the vision download already exists). Same rule as the Camera card. If the phone's camera exposes something, Telescope uses it.

Most of what they need underneath got built as host services along with the plugins in [4.3](4.3.md). One is still open:

- **Lining up and stacking frames.** OpenCV's `phaseCorrelate` and friends. Used by the stargazer and maybe a denoised "hold still" scan in document mode

### Colour matching and 10-bit

The Colour grading card is built. Colour match between phones is for when two different phones look like two different films side by side. You pick the phone you like best and the others get as close to it as they can, with no clicking on patches. All of them film the same printed sheet as 3D from two phones uses (a ChArUco board with grey and colour patches) and the board's corners show exactly where every patch is even when the sheet is tilted. Each phone's own white balance, ISO and shutter get nudged towards the chosen one first, since fixing it in the camera loses less than fixing 8-bit video afterwards. What's left is fitted per phone as a colour matrix and a curve for each channel, which goes into the colour pipeline under your own grade. A match only holds for the light it was made in. Two sensors also never see every colour quite the same (odd fabrics and LED light show it most) but skin and greys come out very close.

The phone sends 8-bit SDR now, which bands under heavy grades. Phones with 10-bit HLG (dynamic range profiles in Android 13) or a maker's log profile would give a lot more room, with turning log back into normal SDR becoming part of the grade. That needs 10-bit HEVC through the phone app, the protocol and the decoder and makes it a stretch goal on top.

Medium for colour matching, big with 10-bit.

### An auto director

Several cameras in one picture is built. An auto director could pick its layout from who's in frame, like the face big while you talk and the desk big while you show something on it. Medium, on top of calm people tracking.

### Phototrap

For wildlife and anything else that shows up when you're not looking. Draw zones on the preview and when something moves in one the phone takes a short burst of full-quality photos. Nothing is saved until something sets it off and it only ever saves photos, never video. Night mode and long exposures from Photos make it work after dark. Medium, now that motion detection and Photos exist.

### Stargazer

People clip phones to telescopes. Lucky imaging for the moon and planets grabs a burst, keeps the sharpest frames, lines them up and averages them. Live stacking does the same slowly for faint things and star trails stack a long series. Manual ISO and shutter are already there, focus peaking helps focusing on a star and Photos brings long exposures and RAW. Medium.

### 3D from two phones

Two phones side by side make a stereo camera. One plugin shows it as red/cyan for glasses on any screen or side by side for a headset. For a Quest or similar the Browser camera server can serve a WebXR page that shows each half to one eye. Nothing has to be installed on the headset.

Getting the two pictures to match is most of the work:

- **Timing.** Frames get paired by when they were captured and not when they arrived. Each phone sends its sensor's capture time with every frame and the desktop works out each phone's clock offset from round trips over the connection it already has. The faster stream then waits for the slower one. That takes network and processing delay out of it. Two separate phones can't fire their shutters together though. The best case is within half a frame (about 16 ms at 30 fps), fine for talking with a little ghosting on fast moves
- **Alignment.** A sheet to print with a ChArUco board on it (a chessboard with codes in the squares that OpenCV finds out of the box). Hold it up, both phones find the same corners and the plugin works out how the phones sit compared to each other. It then shows how to turn or tilt one of them until they're close and straightens the rest with a warp on each stream. That warp is a rectifying one that turns the whole picture (Desk mode's only flattens the sheet) but it can go through the same warp code
- **Colour.** Grey and colour patches on the same sheet let colour matching between phones even the two out (see Colour grading)
- **Checking the timing.** Paper can't show time but the desktop screen can. A fast changing code on the monitor filmed by both phones shows the real offset between them, a good test of the clock sync

Some phones can stream two of their own rear cameras at once from one camera session, with the frames hardware synced. The cameras sit only a centimetre or two apart and the depth comes out subtle. In return there's no clock to sync and nothing to align. Worth offering where the phone has it.

Big. It needs capture times through the phone app and the protocol, the calibration sheet and its guidance, the rectifying warp and the WebXR page (frames from other streams are there now).

### Not for now

- **Live captions** would be the most useful accessibility plugin on this list but need Whisper or Vosk and a model. That means a second optional download like the vision one. Worth deciding on its own

## Older DSLRs over USB

Cameras with a webcam mode or clean HDMI out already work as webcams from 4.4. Older DSLRs have neither but can still send live view over USB, which gphoto2 speaks for most of them. A Nikon D5100 tested in October 2026 sent 640×426 MJPEG at a steady 30 fps for 15 minutes through `gphoto2 --capture-movie --stdout`. A source could read that same stream from the `gphoto2` command most distros ship, without a new Python package. The picture looks like a decent 480p webcam with real background blur.

The camera ends live view every few minutes and gphoto2 starts it again (the mirror clicks and about a second of frames goes missing). The source has to hold the last frame through that gap. Some desktops also open a camera as a drive the moment it's plugged in, and gphoto2 can't reach it until that lets go. It would be Linux only. Windows has no gphoto2 and the vendor SDKs come with licences and one SDK per brand. Not every camera can do it either (a Samsung NX300 only shows up as a USB drive).

Small once webcam support is in and settled.

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

## Doing the picture work on the GPU

Only H.264 decoding uses the graphics card now. Everything after it (transforms, Colour grading, Chroma key, the combined picture, the wait screen and the conversion for the virtual camera) runs on the CPU with NumPy and OpenCV. An earlier GPU rewrite measured slower than this, since every step that copied a frame up to the card and back cost more than it saved. 4.3 added a lot of per-pixel work though, and the colour pipeline went with OpenCV after an OpenGL path measured 2.8 ms a 1080p frame against about 5 ms (the Windows VM only got a broken GL context).

It only pays off if a frame stays on the card from the decoder to one copy back at the end, which is no more copies than now. The virtual camera on both systems needs the frame in RAM anyway, and so do plugins that read it (people tracking, QR codes, Desk mode's corners), which would force a copy back in the middle. Laptops whose GPU shares memory with the CPU copy almost for free and they're also the ones whose CPU struggles first.

Measure before building anything. Run `scripts/bench_frames.py` with a heavy stack (1080p60 with Colour grading, Privacy blur and two cameras in one picture) on a weak laptop. If that stays well under the 16 ms a frame gets at 60 fps there's nothing to do. If not, move only the biggest cost to the GPU as one step that copies back once and measure that. Big if it goes further than one step.
