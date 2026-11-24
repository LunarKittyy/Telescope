# Next release

The next release follows 5.1.4. Ticked here means merged and not released yet. Everything ticked is in the nightly builds.

- [ ] [Stargazer photos, cleaner frames and Night focus](#stargazer-photos-cleaner-frames-and-night-focus)
- [x] [3D in a headset](#3d-in-a-headset)
- [ ] [Stills as raw YUV](#stills-as-raw-yuv)
- [ ] [Hardware decoding on Windows without a copy through memory](#hardware-decoding-on-windows-without-a-copy-through-memory)
- [ ] [Bug hunts, speed and a smaller app](#bug-hunts-speed-and-a-smaller-app)
- [ ] [ARM64](#arm64)
- [x] [Rearranging the docs](#rearranging-the-docs)
- [x] [A page of setups to try](#a-page-of-setups-to-try)
- [ ] [Doing the picture work on the GPU](#doing-the-picture-work-on-the-gpu)
- [x] [A graphics card choice in Settings](#a-graphics-card-choice-in-settings)
- [x] [Fixes for people tracking and Composite](#fixes-for-people-tracking-and-composite)
- [x] [One picture as a scene editor](#one-picture-as-a-scene-editor)
- [x] [Cut out and sharing the picture with other programs](#cut-out-and-sharing-the-picture-with-other-programs)
- [x] [Brackets, merging and Transforms for photos](#brackets-merging-and-transforms-for-photos)
- [x] [More in presets](#more-in-presets)
- [x] [Appearance options](#appearance-options)
- [x] [A tidier window](#a-tidier-window)
- [x] [Stargazer and Phototrap](#stargazer-and-phototrap)
- [x] [Plugins that fail safely and share values](#plugins-that-fail-safely-and-share-values)
- [x] [Phones that find the computer again](#phones-that-find-the-computer-again)
- [x] [DSLRs on Windows](#dslrs-on-windows)
- [x] [Spout capture fixes](#spout-capture-fixes)
- [x] [Lighter and faster](#lighter-and-faster)
- [x] [A faster interface underneath](#a-faster-interface-underneath)
- [x] [Going back to the Python version removed](#going-back-to-the-python-version-removed)

## Stargazer photos, cleaner frames and Night focus

Three more pieces of Stargazer. Frames from photos lets Live stack and Star trails take their frames from photos instead of video, one request per shot. It's merged (#665). Lucky imaging is video only since it needs hundreds of frames. Night focus lets the Focus slider under Customise camera set the photos' focus rather than the camera's own. It's written and in review. It needs the phone app from the same release, and with an older phone the slider is greyed out. Cleaner frames raises the phone's H.264 bitrate while a run collects video frames, as far as the link allows and up to 100 Mbps. The bitrate returns to the Stream output setting when the run ends. It changes on the running stream with no reconnect and no gap. It's written and in review and needs the phone app from the same release.

## 3D in a headset

A Headset switch on the 3D card serves a page that shows each half of the 3D picture to one eye in a Quest's browser (WebXR), with nothing to install on the headset. The page has VR controls (leave with B, Y or a grip, bring the picture back with A or X) and Depth, Height and a tilt for each eye are sliders on the card. The link has a secret in it and doesn't change until you press New link.

## Stills as raw YUV

A camera probe is now in the phone's developer settings and writes a report of what the phone's camera supports (long exposures, burst rates, full-size YUV). It needs a phone release to reach anyone. The report decides whether a burst of stills could go as raw YUV rather than JPEG, with no compression and nothing to decode. That is a protocol change and gets its own design first.

## Hardware decoding on Windows without a copy through memory

On Linux a VA-API picture goes to the effects on the card without a copy through memory. A stream with no effects keeps it there too: the card converts it into what the virtual camera takes and only that is read into memory, the same bytes the processor would make (at 4K about a third less processor time with the window open). On Windows that is off until it has run on a real card there: set TELESCOPE_PLAIN_ON_CARD=1 to try it. Effects on the card work when the picture work runs on DX12: a D3D11VA picture is copied on the card into a texture both APIs can open, the DX12 queue waits for that copy on the card, and each shared texture isn't reopened for every picture. It works on both cards of the test laptop and the picture matches the old path to within one level. At 4K the app's processor time on a short clip fell from about 4.3 s to 1.5 s, and the processor now spends about 2.5 ms on each frame, down from 13 to 16. Any failure falls back to the old copy through memory for the rest of that stream. What's left is Vulkan, which is what Windows picks first. Sharing is still off there. On the discrete card's current driver, opening the shared texture a second time or taking it from an undefined layout loses the device. Keeping it open and taking it from the general layout doesn't, but then the colour plane reads wrong (zeros on the first picture) while brightness is right. Vulkan also can't wait for the copy on the card yet, since wgpu doesn't switch on semaphore sharing on Windows.

## Bug hunts, speed and a smaller app

Ongoing. Bug passes over the code and measured work on speed, system use and the size of the app, one change per pull request. None of it is a feature.

One piece is the window's weight and start time, from measuring the finished app against Python. The interpreter no longer retains the compiler's trees after a build and Settings is built the first time it opens (122 MB at start against 173 MB). Compiling the cards of hidden tabs only when they open would save more. On Linux the graphics card now opens on a thread while the window's document compiles, which took 30 to 60 ms off the start. Compiling is still about 80 % of the time to the first frame, 600 to 1000 ms depending on how busy the machine is. Compiling ahead of time isn't a small change because the document is written at launch from the cards that are on, in the person's language, and the shell reaches every card by name through the interpreter. A compiled window would be a second path next to the one plugins use. Reusing the Vulkan instance would save 24 ms.

## ARM64

Not decided. Releases are x86_64 only for now. ARM Linux machines (a Raspberry Pi, Asahi on a Mac, ARM laptops) can't run Telescope. Windows on ARM laptops run x64 programs through Windows' own emulation. The x64 build should work there, untested. The virtual camera is unlikely to work there because the Windows 11 camera runs in a native ARM64 system service that can't load the x64 DLL.

The first step is done. A separate workflow (`rust-arm64.yml`) builds the workspace and runs the tests on GitHub's free ARM64 runners for Linux and Windows 11. It runs after merges to master and by hand, not on pull requests, and it isn't a required check. A red run doesn't hold up a merge. Both build and nearly everything passes (6796 of 6802 tests on Linux and 6643 of 6645 on Windows). Most of the desktop works on ARM.

What's left:
- The two `yuv.rs` colour tests that failed on ARM were comparing against a bad reference. On ARM, swscale leaves the last pixel of each row unwritten when a picture's width is odd and its height even, which put a black column in the reference. The conversion gives the same bytes on ARM as on x86_64. The BGR step now gives swscale an extra column on ARM and drops it again.
- Linux needs FFmpeg 8 (the code doesn't build against 6.1) and Ubuntu 24.04 doesn't have it. The ARM Linux job runs in a 26.04 container for that reason.
- On Windows the FFmpeg for the tests is BtbN's ARM64 build (GyanD's is x64 only). The packages would need an ARM64 build from `build_ffmpeg.sh` first.
- The Windows camera DLL and the vision pack's libraries still need ARM64 builds. Nothing has been tried there. The people features are off without the pack.
- No release has an ARM build yet and nothing has run on a real ARM machine. The picture, capture and the virtual camera on Linux are untested.
- Four Linux tests fail only because the job runs as root, which can read files the tests make unreadable. One Windows pacing test failed once and passed in the next run. It depends on timing on the slower runner.

## Rearranging the docs

The prose pass over `docs/` and the app's texts went out in 5.1.4. The files are now rearranged so each topic is where people would look for it. The feature docs were brought up to date after.

## A page of setups to try

[Things to try](../unusual-setups.md) lists ways to use Telescope that it wasn't built around, like a phone from far away, a DSLR as a photo console and a whiteboard scene. It's a first version. The Tailscale entry still needs trying on real links (Dynamic over a slow connection, a relayed route, reconnecting between mobile data and Wi-Fi, Browser camera over a 100.x address).

## Doing the picture work on the GPU

Background blur now runs on the graphics card (4K with everything on went from 5.5 to 2.4 ms for the blur) and a wasted pass in it is skipped. Background's Picture mode runs there too. Spout capture is converted on the card too, the 1998 webcam costs much less and Green screen goes dark instead of showing the room when the card fails. Cinema's tints in the preview are painted on the card, which had been costing about 14 points of app CPU at 4K. On Linux VA-API pictures reach the steps on the card without a copy through memory. What's left is the steps still written for the CPU that change the picture partway through, like Chroma key, Privacy blur, the combined picture, the Fun pack, Name tag and the wait screen. Each one brings the frame down and makes the next shader send it up again. Turning the heaviest of them into shaders, or into steps that can run either way, would leave one copy up and one down, which is all the virtual camera needs. On Windows a decoded picture still comes down to memory first unless the steps run on DX12 (see the Windows decoding item above).

The card isn't always the faster place though. 5.0.2 and 5.0.3 moved mirroring, flipping, zoom and stretch back to the CPU, since they're cheap there and a card busy with a game or OBS held up every frame. A step is only worth moving to the card when it's heavy enough to make up for that.

The view's crop (a zoom or Auto framing) now runs either way, since at 4K it cost the CPU about 40% of a core. Flips, turns, the stretch and Lens correction stay on the CPU. Right now the balancer picks the card or the CPU for every such step in a frame at once (colour grading is the other one). With the card picked, a lone crop or grade sends the frame up only for itself, which was still cheaper than the CPU for the crop at 1080p and 4K.

Measure first with the transfer counter (`crates/media/src/transfers.rs`). Run a heavy stack on a weak laptop and see where the time goes before moving anything. Laptops whose GPU shares memory with the CPU copy almost for free and they're also the ones whose CPU struggles first. Medium. Sharing one working frame and doing effects on the YUV planes were measured and dropped, see [dropped.md](dropped.md).

## A graphics card choice in Settings

Settings has a graphics card choice for someone whose system picks the wrong card. The guard that skips a card after a crash is tougher and VA-API follows the card you pick. Running steps on the integrated card was dropped, see [dropped.md](dropped.md).

## Fixes for people tracking and Composite

- Composite tiles follow Max zoom and remember their zoom
- The stick figure copes with close-ups and crossed legs
- Auto framing can follow one person and follows through a crossing. Picking a person is refused on a preview that shows another stream
- Background cuts around chairs and light clothing more cleanly and reuses the last mask rather than going black when a new one is late

## One picture as a scene editor

The One picture editor can do much more.

- Exact position and size fields, a Trade places with menu and crop boxes with a Reset crop
- The Streams list is one list of layers with an eye, a lock and drag to reorder. Clicking a row picks that stream
- Transitions between layouts. Glide is the default, with Shuffle, Crossfade, Slide, Push, Zoom, Wipe, Cut and the fun ones (Bounce, Iris, Curtain, Blinds, Pixelate and Dissolve). Length and Easing are set on the card
- Auto director is off by default and needs two saved layouts. Hands in view of another stream bring in the saved layout that shows it biggest
- Resizing the tiles is much cheaper. Four 4K streams that aren't moving went from 111 to 12 ms

## Cut out and sharing the picture with other programs

A One picture tile with Cut out shows only the person, cut out by Background or Chroma key, over whatever is behind the tile. It works with the graphics card and with Light painting. Frames can now carry transparency from the step that cuts the person out to where it's drawn. The virtual camera still can't show it, since call apps throw it away.

Spout send is a new Lab plugin that shares a stream's finished picture with OBS or another program on the same computer. It's a Spout sender on Windows and a PipeWire video source on Linux. Transparent background shares the cut-out as the picture's alpha. Only tried against Telescope's own reader so far.

## Brackets, merging and Transforms for photos

- Merge bracket on the Photos card turns a focus or exposure bracket into one picture. Photos saves only the photos, Both adds the merged picture and Merged saves only that. Merged brightness leans an exposure merge darker or lighter
- A bracket has a photo count and an EV step, a focus bracket has a step count and a focus range, and the shutter list runs from 1/4000 s to the longest the sensor takes. An older phone has the old limits
- Photos, Phototrap and Stargazer have a Customise camera row to pick the shutter, ISO and focus for one photo
- Photos follow Transforms. Mirror, Upside down and rotation are applied to every photo that isn't RAW without re-encoding it. A Use zoom switch crops the photo to the zoom and position set in Transforms

## More in presets

Presets save a Composite tile's zoom, the active Composite layout (the layout library is left out), a USB webcam's settings, the microphone's gain, auto level and noise reduction, a Spout sender's frame rate and the settings of Photos, Phototrap and Stargazer. A preset can leave the One picture layout alone. A test checks that every plugin's settings are covered.

## Appearance options

Look in Settings has options for corners, text size, density, higher contrast, motion and tooltip delay.

## A tidier window

- Cards with a status show it right of the title, cut short with the full text on hover when the window is narrow
- Icons that didn't fit got new ones, and buttons next to a slider of the same colour are softer
- Phototrap, Stargazer and the Preview card have a Reset
- The header button reads Stop all when several streams are live
- Every stream line in the log names the phone and the virtual camera it feeds, and the reason it stopped
- The windows fit the screen at 125% and 150% scale and after a taskbar that hides itself reappears

## Stargazer and Phototrap

Both are optional Lab plugins. Stargazer has lucky imaging, live stacking and star trails and shows how sharp the picture is. Night takes one long exposure with a phone or a DSLR. Phototrap takes a burst of photos when something moves in a zone you drew. The alignment and stacking they use is in `core` for other plugins to use too. Photos aren't taken while the picture is hidden, and the gaps that were left in that are closed.

## Plugins that fail safely and share values

A plugin that panics is switched off and the app keeps running, unless it hides part of the picture. Then the app closes with a crash report. A plugin can name the built-in plugins it needs in `plugin.json` and share values with other plugins through a host registry. The MCP server can change the settings of most plugins now. Safety and outward settings (Privacy blur, start on, the battery and temperature stops, the update channel, keeping the app portable and the camera's manual switches) are not available to it.

## Phones that find the computer again

Paired phones used to lose the computer after a Wi-Fi change until they were paired again or plugged in. Both apps now keep a small UDP exchange open on port 7483 to swap their addresses. The old checks are the fallback. Both apps show a banner when their versions differ, naming the older side. It was tried on a real phone over Wi-Fi. The case of a router that drops the computer's traffic to the phone hasn't been forced yet, and the phone side only counts once it's in a phone release.

## DSLRs on Windows

Live view for Nikon and Canon works through Windows' own camera driver, and so do photos, focus, aperture, exposure compensation, white balance and a Nikon's ISO and shutter speed. A real Nikon D5100 was tried for live view and photos on both Linux and Windows. Canon is only tried with a mock camera so far.

## Spout capture fixes

A Spout sender that gets stuck on Linux no longer blocks Stop and its pictures are dropped when they finish too late. A sender that is stuck shows that on the card rather than an old picture.

## Lighter and faster

Less work while nothing is looking and less per frame while something is.

- JPEG frames aren't decoded while no app reads the camera, and PipeWire sharing does nothing while nothing reads the source
- The Windows 11 camera is written at the rate its reader asks for
- The idle app wakes far less often, from about 12 times a second to 2, and hidden or minimised windows don't count as watching
- Each H.264 packet is built once and the UDP fragments are sent straight from the frame
- The virtual camera output costs less CPU and memory, and so does denoising over time

## A faster interface underneath

Fewer allocations while the window updates, about a third as many for each update from the phone. The interpreter also drops the compiler's trees after each build and Settings frees its window. The window isn't visibly lighter.

## Going back to the Python version removed

Settings no longer has the "Go back to the Python version" button and the code that downloaded 4.4.2 is gone. An update from the Python app still works. See [dropped.md](dropped.md).
