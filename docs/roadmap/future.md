# Some day

Ideas worth keeping that nobody is about to work on. Move one to [next.md](next.md) once it's planned.

## Scenes and a whiteboard

Presets already save Auto framing and Background, so a preset can be a scene: one that frames you while you talk, another with the framing off and the zoom parked on a whiteboard or a desk, each on its own key. A teacher, someone running a workshop or anyone drawing on paper could switch between them mid-call.

The bigger part is the board itself. Desk mode already straightens a sheet once you mark its corners and makes the writing stand out. What's missing is Telescope finding the corners itself, following a board as the phone moves and evening out shadows and glare. It's classical image processing rather than MediaPipe. Tuning it so it works in real rooms on real boards is most of the work. A Whiteboard source or card could follow the board the way Auto framing follows you.

Big, mostly the corner finding and the clean-up. Worth seeing first whether people use presets as scenes.

## Optional plugins

More ideas for plugins that start out off, for people who want more out of their webcam than a picture. Most of them need no new dependencies (FFmpeg is already there and the vision download already exists). Same rule as the Camera card. If the phone's camera exposes something, Telescope uses it.

Most of what they need underneath got built as host services along with the plugins in [4.3](4.3.md). One is still open:

- **Lining up and stacking frames.** Phase correlation and friends. Used by the stargazer and maybe a denoised "hold still" scan in document mode

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

### AI models in plugins

A maybe. A plugin could bundle a small `.onnx` model and use it as a frame stage the same way it uses a shader now, with Telescope running it on the GPU. The model is only data, so plugins still can't reach files, programs or the network. Real-time models fit here, for example low-light boost (Zero-DCE is tiny and suits weak laptop cameras), denoising, light upscaling and style transfer.

It needs a model runtime like ONNX Runtime, which is large with GPU support, so it would be an optional download like the vision pack. Live captions below would need one too. Running the model after auto framing's crop keeps it cheaper, and edits made frame by frame need smoothing so faces don't shimmer.

Heavy generative edits (StreamDiffusion and the like) would need Telescope to send frames to a program the user runs themselves. That crosses the line plugins aren't allowed to cross and mostly needs a high-end GPU, so it's not part of this.

### More for the MCP server

The MCP server is built (see [next.md](next.md#an-mcp-server)). What's left of the idea:

- `frame` sending a small frame, only when the user allows it, and off by default. A middle step could blur the person out with the vision mask, so the model sees the room and the light but not them. Telescope would show when a model is connected and separately when it's getting frames
- `show` opening the right tab and highlighting a setting, for when someone asks where something is
- A preview mode where changes wait until accepted, which needs the preview and the output to be kept apart
- More plugins declaring their settings so the model can reach them

### Not for now

- **Live captions** would be the most useful accessibility plugin on this list but need Whisper or Vosk and a model. That means a second optional download like the vision one. Worth deciding on its own

## Smaller things for after 5.1.0

Decided, but small enough that none of them needs a release of its own.

### Auto framing that follows one person

With several people in view, Auto framing frames the whole group. A choice of one person to follow would keep the frame on them, the way it already follows a single person, and ignore whoever walks past behind. The open part is how to choose. Clicking someone in the preview is the obvious way, and the other is following whoever talks. Small to medium, on top of the people tracking that's there.

### Cleaner edges for Background

Background replacement can eat the edges of things that aren't a person. A chair back or light clothing against a bright wall partly dissolves into the new background. Tuning the mask threshold and edge softening on those cases should help, and so might smoothing the mask over a few frames. The hard part is finding settings that fix this without making hair and hands worse. Small to medium.

### A sharper cutout for Background

A **High quality cutout** option in Background, using a video matting model (RVM) instead of the selfie segmenter. In tests it was clearly better on edges and hair, with fewer leaks from the background and a mask about ten times steadier on still areas. It can drop a hand or part of the body in a hard clip where the current one keeps them. It costs about 0.4 of a core at 15 masks a second, against about 0.03 now, and it needs an optional download of about 26 MB (the runtime and the model). Medium. Not decided yet.

### Background blur on the GPU

Blur is the heaviest step left on the CPU with everything on at 4K. Switching it off saves about half of Telescope's processor use there. Doing the shrink, blur and blend on the graphics card would take most of that away. Medium, and part of [Doing the picture work on the GPU](#doing-the-picture-work-on-the-gpu).

### The 1998 webcam costs more than it should

With the Fun pack's 1998 webcam on, the processor use goes up by about 44%, and the picture is repeated every other frame on purpose. Much of the cost is the way the blocky look is made, which could be done once and held for the repeated frame. Small.

### Composite tiles zoom further

A tile in Composite zooms in at most 4x, while Auto framing follows the Max zoom setting. The tiles could follow it too. Small.

### Stick figure with crossed legs and close-ups

The stick figure gets the pose wrong when legs are crossed, and in a close-up it turns into a huge head on a pair of shoulders. The close-up needs the drawing to scale to the body that's visible instead of assuming a whole one, and the crossed legs need the leg joints to be trusted less when they overlap. Small.

### Pausing effects while nothing reads the camera

When no app is using the virtual camera, the effects keep running on frames nobody sees. They could pause while the phone stays connected, and pick up again as soon as an app opens the camera. Telescope already knows when an app has the camera open, since the idle stop uses that. Lowers the cost of leaving Telescope running all day. Medium, since the decoder, effects and vision all have to stop and start cleanly.

### A hot phone

When the phone gets hot, the phone app lowers the frame rate, but some cameras offer no lower rate, so nothing changes and the phone keeps heating. It could drop to a smaller picture at that point instead of waiting for critical heat, the way it already goes to 720p there, and pause with a message at extreme heat. Small, mostly in the phone app, and it needs a real phone to try.

### A faster interface underneath

The work on Slint's interpreter continues, mainly cheaper struct field lookups and fewer allocations while the window updates. Nobody sees it except as a lighter window, like the rest of [Lighter on the processor and memory](next.md#lighter-on-the-processor-and-memory). Medium.

## Appearance options

Only options that scale or swap something every control already reads from the design tokens, in a few fixed steps, so a render test can cover them and nothing breaks unseen. No free colour pickers, fonts or per-card overrides.

- Corner roundness: sharp, default or round
- Text size: small, default or large. The riskiest one, since long labels and translations overflow first
- Density: compact or comfortable
- Card colours: each card in its category colour, or one accent for all
- Contrast: a higher contrast mode with full colours at rest and near-white lines and text
- Motion: full, reduced or off, following the system by default
- Tooltip delay: a slider from instant to about 1.5 seconds, plus an "Instantly show next tooltip" toggle

## Undo across the app

Each card has its own Reset, but nothing goes back one step. One undo and redo for the whole app would cover a grading change, a pan or zoom, a moved overlay or a switched plugin, from Ctrl+Z or a button in the header.

Plugins don't know about each other, so it can't be one list that reaches into each plugin. Each change a person makes would go through `core` as a step that says what it was and how to go back, and each plugin decides which of its changes count. A slider dragged for two seconds is one step, not a hundred. Changes from the phone, from Auto or from a shortcut need a rule too.

A week or so, most of it deciding what counts as one step in each plugin.

## DSLRs on Windows

DSLRs only work on Linux, through libgphoto2, which needs direct USB access. On Windows that means replacing the camera's driver with another tool, too much to ask. Windows has its own driver for cameras though (the one that shows them in Explorer) and programs can send it raw PTP commands through Windows Portable Devices. digiCamControl and the makers' own SDKs work that way.

So Windows needs a small PTP layer of its own on top of that driver: start and stop live view, fetch frames, read and set settings, autofocus and focus steps. Nikon and Canon each have their own commands for these, a handful per maker. Each maker and model has quirks that libgphoto2 handles on Linux, so it needs real cameras to test against. The first step is checking whether a camera can be passed through to the Windows VM, otherwise there's nothing to test on.

A few days for Nikon and Canon, most of it testing.

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

Much of this is in 5.0. Decoding uses the graphics card (VA-API on Linux, D3D11VA on Windows), plugin shaders and the preview run there through wgpu, and `core::balance` picks the card or the CPU for steps that can run either way, and moves them off the card while a game keeps it busy. A frame goes up at the first shader and stays there until a step on the CPU needs it.

What's left is the steps still written for the CPU that change the picture partway through, like Background, Chroma key, Privacy blur, the combined picture, Desk mode, the Fun pack, Name tag and the wait screen. Each one brings the frame down and makes the next shader send it up again. Turning the heaviest of them into shaders, or into steps that can run either way, would leave one trip up and one down, which is all the virtual camera needs.

The card isn't always the faster place though. 5.0.2 and 5.0.3 moved mirroring, flipping, zoom and stretch back to the CPU, since they're cheap there and a card busy with a game or OBS held up every frame. A step is only worth moving to the card when it's heavy enough to make up for that.

A view between two plugin shaders now costs a trip down and back up though. Making the view a step that can run either way would only help with one more rule: such a step goes to the card only when the frame is already up there. Right now the balancer picks the card or the CPU for every such step in a frame at once (colour grading is the other one), so with the card picked a lone view or grade sends the frame up just for itself.

Measure first. The pipeline already counts its transfers (`crates/media/src/transfers.rs`), so run a heavy stack on a weak laptop and see where the time goes before moving anything. Laptops whose GPU shares memory with the CPU copy almost for free and they're also the ones whose CPU struggles first.

## ARM64

A maybe. Every build is x86_64 only for now, so ARM Linux machines (a Raspberry Pi, Asahi on a Mac, ARM laptops) can't run Telescope at all. Windows on ARM laptops would probably run the x64 build through Windows' own emulation, but the Windows 11 camera there runs in a native ARM64 system service that can't load our x64 DLL, so the virtual camera likely wouldn't work.

Most of the desktop is plain Rust and Slint and should build for ARM as it is, and v4l2loopback on Linux comes from the distro. The parts that need real work are an ARM64 build of the Windows camera DLL and ARM builds of the vision pack's libraries. Without the vision pack, Background, Auto framing and the other people features stay off, the way `platform_key()` in `crates/vision/src/pack.rs` already handles it.

Start with GitHub's free ARM64 runners for Linux and Windows 11: build the workspace and run the tests there to see how much just works before deciding on the rest.
