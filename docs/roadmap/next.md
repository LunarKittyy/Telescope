# Next release

The next release follows 5.2.0. Ticked here means merged and not released yet. Everything ticked is in the nightly builds.

- [ ] [Stills as raw YUV](#stills-as-raw-yuv)
- [ ] [Hardware decoding on Windows without a copy through memory](#hardware-decoding-on-windows-without-a-copy-through-memory)
- [ ] [Bug hunts, speed and a smaller app](#bug-hunts-speed-and-a-smaller-app)
- [ ] [ARM64](#arm64)
- [ ] [Doing the picture work on the GPU](#doing-the-picture-work-on-the-gpu)

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

## Doing the picture work on the GPU

Background blur now runs on the graphics card (4K with everything on went from 5.5 to 2.4 ms for the blur) and a wasted pass in it is skipped. Background's Picture mode runs there too. Spout capture is converted on the card too, the 1998 webcam costs much less and Green screen goes dark instead of showing the room when the card fails. Cinema's tints in the preview are painted on the card, which had been costing about 14 points of app CPU at 4K. On Linux VA-API pictures reach the steps on the card without a copy through memory. What's left is the steps still written for the CPU that change the picture partway through, like Chroma key, Privacy blur, the combined picture, the Fun pack, Name tag and the wait screen. Each one brings the frame down and makes the next shader send it up again. Turning the heaviest of them into shaders, or into steps that can run either way, would leave one copy up and one down, which is all the virtual camera needs. On Windows a decoded picture still comes down to memory first unless the steps run on DX12 (see the Windows decoding item above).

The card isn't always the faster place though. 5.0.2 and 5.0.3 moved mirroring, flipping, zoom and stretch back to the CPU, since they're cheap there and a card busy with a game or OBS held up every frame. A step is only worth moving to the card when it's heavy enough to make up for that.

The view's crop (a zoom or Auto framing) now runs either way, since at 4K it cost the CPU about 40% of a core. Flips, turns, the stretch and Lens correction stay on the CPU. Right now the balancer picks the card or the CPU for every such step in a frame at once (colour grading is the other one). With the card picked, a lone crop or grade sends the frame up only for itself, which was still cheaper than the CPU for the crop at 1080p and 4K.

Measure first with the transfer counter (`crates/media/src/transfers.rs`). Run a heavy stack on a weak laptop and see where the time goes before moving anything. Laptops whose GPU shares memory with the CPU copy almost for free and they're also the ones whose CPU struggles first. Medium. Sharing one working frame and doing effects on the YUV planes were measured and dropped, see [dropped.md](dropped.md).
