# Some day

Ideas worth keeping that nobody is about to work on. Move one to [next.md](next.md) once it's planned.

## Scenes and a whiteboard

A preset can already be a scene because presets save Auto framing and Background. One frames you while you talk and another has the framing off and the zoom parked on a whiteboard or a desk, each on a separate key. A teacher, someone running a workshop or anyone drawing on paper could switch between them mid-call.

The bigger part is the board itself. Desk mode already straightens a sheet once you mark its corners and makes the writing stand out. What's missing is Telescope finding the corners itself, following a board as the phone moves and evening out shadows and glare. It's classical image processing rather than MediaPipe. Tuning it so it works in real rooms on real boards is most of the work. A Whiteboard source or card could follow the board the way Auto framing follows you.

Big, mostly the corner finding and the clean-up. Worth seeing first whether people use presets as scenes.

## Optional plugins

More ideas for plugins that start out off, for people who want more out of their webcam than a picture. Most of them need no new dependencies (FFmpeg is already there and the vision download already exists). Same rule as the Camera card. If the phone's camera exposes something, Telescope uses it.

Most of what they need underneath got built as host services along with the plugins in [4.3](4.3.md). A denoised "hold still" scan in document mode could still use the new frame stacking.

### Colour matching and 10-bit

The Colour grading card is built. Colour match between phones is for when two different phones look like two different films side by side. You pick the phone you like best and the others get as close to it as they can, with no clicking on patches. All of them film the same printed sheet planned for lining up 3D (a ChArUco board with grey and colour patches) and the board's corners show exactly where every patch is even when the sheet is tilted. Each phone's own white balance, ISO and shutter get nudged towards the chosen one first, since fixing it in the camera loses less than fixing 8-bit video afterwards. 3D's Match already does a simple version of this for two phones (the same shutter, the right phone's ISO nudged and a curve for each channel from the middle of both pictures). What's left is fitted per phone as a colour matrix and a curve for each channel, which goes into the colour pipeline under your own grade. A match only holds for the light it was made in. Two sensors also never see every colour quite the same (odd fabrics and LED light show it most) but skin and greys come out very close.

The phone sends 8-bit SDR now, which bands under heavy grades. Phones with 10-bit HLG (dynamic range profiles in Android 13) or a maker's log profile would give a lot more room, with turning log back into normal SDR becoming part of the grade. That needs 10-bit HEVC through the phone app, the protocol and the decoder and makes it a stretch goal on top.

Medium for colour matching, big with 10-bit.

### An auto director

Several cameras in one picture is built. An auto director picks its layout from who's in frame, like the face big while you talk and the desk big while you show something on it. A first version is written and isn't merged yet, see [next.md](next.md#an-auto-director).

### 3D from two phones

An early version is out in [5.1.3](5.1.md#513-16-november-2026) as a Lab plugin. Two phones go out side by side or red/cyan on the Telescope camera, paired by when each frame was taken, with Fine tune sliders to line them up by hand and Match for exposure and colour. What's left of the plan:

- **Lining up with a printed sheet.** A ChArUco board (a chessboard with codes in the squares that OpenCV finds out of the box). Hold it up, both phones find the same corners and the plugin works out how the phones sit compared to each other. It then shows how to turn or tilt one of them until they're close and straightens the rest with a warp on each stream, which would replace most of Fine tune. The warp turns the whole picture to rectify it (Desk mode's only flattens the sheet) but it can go through the same warp code. Grey and colour patches on the same sheet could make Match more exact (see Colour matching above)
- **Headsets.** The Browser camera server could serve a WebXR page that shows each half to one eye on a Quest or similar, with nothing to install on the headset
- **YouTube.** An upload plays as real 3D once the file has stereo metadata, so a recording from Telescope could carry it
- **Checking the timing.** Paper can't show time but the desktop screen can. A fast changing code on the monitor filmed by both phones shows the real offset between them, a good test of the clock sync

Medium for the sheet and the warp, small to medium for each of the others.

### AI models in plugins

A maybe. A plugin could bundle a small `.onnx` model and use it as a frame stage the same way it uses a shader now, with Telescope running it on the GPU. Plugins still can't reach files, programs or the network because the model is only data. Real-time models fit here, for example low-light boost (Zero-DCE is tiny and suits weak laptop cameras), denoising, light upscaling and style transfer.

ONNX Runtime already ships in the high quality cutout download, as a CPU build only. A GPU build would be new. Live captions below would need a runtime too. Running the model after auto framing's crop is cheaper. Edits made frame by frame need smoothing so faces don't shimmer.

Heavy generative edits (StreamDiffusion and the like) would need Telescope to send frames to a program the user runs themselves. It's left out of this because it crosses the line plugins aren't allowed to cross and mostly needs a high-end GPU.

### More for the MCP server

The MCP server is built (see [5.1](5.1.md#an-mcp-server)). What's left of the idea:

- `frame` sending a small frame, only when the user allows it and off by default. A middle step could blur the person out with the vision mask to let the model see the room and the light but not them. Telescope would show when a model is connected and separately when it's getting frames
- `show` opening the right tab and highlighting a setting, for when someone asks where something is
- A preview mode where changes wait until accepted, which needs the preview and the output to be separate
- More plugins declaring their settings so the model can reach them (being written)

### Not for now

- **Live captions** would be the most useful accessibility plugin on this list but need Whisper or Vosk and a model. That means a second optional download like the vision one. Worth deciding separately

## Smaller things

Decided but small enough that none of them needs its own release.

### Spout capture with zero-copy

Spout capture is converted on the graphics card now. The frame is still copied out of the sender's memory first. Handing the sender's GPU buffer straight to the picture work (a DMA-BUF on Linux, a shared D3D11 texture on Windows) would leave the frame on the card. Big. It belongs with [Doing the picture work on the GPU](#doing-the-picture-work-on-the-gpu). Overlapping the copy with the conversion was left out, see [dropped.md](dropped.md).

### USB without adb

USB pairing and the USB route need adb on the computer (which Windows downloads from Google after asking) and USB debugging switched on in the phone's Developer options. Android's Open Accessory mode would let the phone talk to the computer over the cable with neither. The computer asks a plugged-in phone to switch into accessory mode and the Telescope app opens. The catch is that it needs a libusb-style driver on Windows. The phone app also has to handle a second kind of connection beside adb's reverse port (`rust/crates/link/src/adb.rs` and `service.rs` on the desktop). Big. It needs real phones from several makers to try.

## Card colours

The last of the appearance options, left over from [next.md](next.md#appearance-options). Each card would use its own category colour or all of them would share one accent. It would be a few fixed steps like the other options, with no free colour picker.

## Undo across the app

Each card has a Reset but nothing can be undone one step at a time. One undo and redo for the whole app would cover a grading change, a pan or zoom, a moved overlay or a switched plugin, from Ctrl+Z or a button in the header.

It can't be one list that reaches into each plugin because plugins don't know about each other. Each change a person makes would go through `core` as a step that records what it was and how to undo it. Each plugin decides which of its changes count. A slider dragged for two seconds is one step, not a hundred. Changes from the phone, from Auto or from a shortcut need a rule too.

A week or so, most of it deciding what counts as one step in each plugin.

## A guide to unusual setups

A doc with tips for using Telescope in ways it wasn't built around, starting with Tailscale. Pairing over Tailscale already works (the QR code carries the Tailscale address after the LAN ones) and it doesn't matter where either device is. That covers a lot without writing any new networking code:

- A friend joins your tailnet, pairs their phone with your Telescope and their camera shows up in your OBS as a remote guest, much sharper than a screen-captured video call
- A phone at the cottage or a stargazer rig in the garden that you watch from your desk
- Helping someone remotely by pairing your phone with their Telescope

Before writing it a few things need trying on real links:

- How Dynamic copes with a slow internet connection and which resolution and bitrate to recommend
- How much slower it gets when Tailscale relays traffic through DERP instead of connecting directly and how to check that with `tailscale ping`
- Whether UDP on Wi-Fi helps or hurts there
- Reconnecting when the phone switches between mobile data and Wi-Fi
- Whether Browser camera works over a 100.x address

A draft of the doc is written and the tests on real links are still to do. Small to write. The testing is the work.

## Doing the picture work on the GPU

Much of this is in 5.0. Decoding uses the graphics card (VA-API on Linux, D3D11VA on Windows). Plugin shaders and the preview run there through wgpu. `core::balance` picks the card or the CPU for steps that can run either way and moves them off the card while a game is busy using it. A frame goes up at the first shader and is on the card until a step on the CPU needs it.

What's left is the steps still written for the CPU that change the picture partway through, like Background, Chroma key, Privacy blur, the combined picture, Desk mode, the Fun pack, Name tag and the wait screen. Each one brings the frame down and makes the next shader send it up again. Turning the heaviest of them into shaders, or into steps that can run either way, would leave one trip up and one down, which is all the virtual camera needs.

The card isn't always the faster place though. 5.0.2 and 5.0.3 moved mirroring, flipping, zoom and stretch back to the CPU, since they're cheap there and a card busy with a game or OBS held up every frame. A step is only worth moving to the card when it's heavy enough to make up for that.

A view between two plugin shaders now costs a trip down and back up though. Making the view a step that can run either way would only help with one more rule. Such a step would go to the card only when the frame is already up there. Right now the balancer picks the card or the CPU for every such step in a frame at once (colour grading is the other one). With the card picked a lone view or grade sends the frame up only for itself.

Measure first. The pipeline already counts its transfers (`crates/media/src/transfers.rs`). Run a heavy stack on a weak laptop and see where the time goes before moving anything. Laptops whose GPU shares memory with the CPU copy almost for free and they're also the ones whose CPU struggles first.

Moving the steps listed above into longer runs on the card is still the idea, to send the frame up once and down once. Medium and worth measuring with the transfer counter first.

Background blur, Spout's conversion and the 1998 webcam are done (see [next.md](next.md#doing-the-picture-work-on-the-gpu)). Sharing one working frame and doing effects on the YUV planes were measured and dropped, see [dropped.md](dropped.md).

## The front of the chain on the phone

Doing the first steps on the phone would take load off the desktop. It needs a pass on the phone, a protocol message and testing on real phones.

## ARM64

A maybe. Every build is x86_64 only for now and ARM Linux machines (a Raspberry Pi, Asahi on a Mac, ARM laptops) can't run Telescope at all. Windows on ARM laptops would probably run the x64 build through Windows' own emulation. The virtual camera likely wouldn't work there because the Windows 11 camera runs in a native ARM64 system service that can't load the x64 DLL.

Most of the desktop is plain Rust and Slint and should build for ARM as it is. v4l2loopback on Linux comes from the distro. The parts that need real work are an ARM64 build of the Windows camera DLL and ARM builds of the vision pack's libraries. Background, Auto framing and the other people features are off without the vision pack, the way `platform_key()` in `crates/vision/src/pack.rs` already handles it.

Start with GitHub's free ARM64 runners for Linux and Windows 11. Build the workspace and run the tests there to see how much works as it is before deciding on the rest.

## Carried over from the desktop notes

- **Going back (2026-11-02):** the first part is built, which Rust needed before it reaches the stable channel. Settings has one "Go back to the Python version…" button under Updates, pinned to 4.4.2 (its build number and the checksum of each package are written in `crates/system/src/going_back.rs`, nothing is trusted from the network). It copies the settings aside first, swaps the files through the normal update's journal and turns off 4.4.2's daily check so it doesn't offer Rust back. An AppImage gets the release page instead. A Linux box without Python 3.12 can't downgrade and a source build doesn't show it. Still to do, after 5.0: downgrades in the updater generally, with a warning that it may cause issues and resets the config.
- **Load balancing (2026-11-01):** the goal for running beside a heavy game, with nothing needing elevated permissions and each step measured against the stress test's `gpu` load. Built so far: `core::balance` moves steps that can run either way to the CPU after a few late frames and back after a clear stretch; the colour LUT runs on the CPU; person masks have their own thread; `core::pace` evens out browser feeds that arrive in bursts; phone streams have age caps on the send queues and even timing under load; hardware decoding is back on by default with VA-API (the stall behind a game came from the driver's I420 conversion queuing behind the game's work. The card is asked for NV12 and the split happens on the processor instead). The integrated card and the front of the chain on the phone aren't planned. The decoder in a child process was dropped too, see [dropped.md](dropped.md).
- **Signed updates:** a changed manifest can name its own package and checksum because the updater trusts the update manifest over HTTPS. Sign the manifest with a key built into the app before anything in it is believed. The Python app worked the same way. Going to a signature needs a release that adds the key first.
- **Window weight and start time:** from the measurements of the finished app against Python. Memory: a three-line Slint patch that drops the type loaders the interpreter keeps after compiling saves about 30 MB, and compiling the cards of hidden tabs only when they open saves more. Settings is built the first time it opens now (122 MB at start against 173 MB). Start: compile ahead of time instead of at launch, overlap opening the cards with compiling them (40 to 80 ms), and reuse the Vulkan instance (24 ms).
