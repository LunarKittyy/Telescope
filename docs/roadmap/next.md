# Next release

Planned in October 2026, after 4.0. Both of these were left over from it.

- [ ] [720p when the phone gets critically hot](#720p-when-the-phone-gets-critically-hot)
- [ ] [Saying to extract the Windows zip first](#saying-to-extract-the-windows-zip-first)
- [x] [Cheaper background blur and auto framing](#cheaper-background-blur-and-auto-framing)

## 720p when the phone gets critically hot

4.0 caps the frame rate at 24 fps when Android's thermal status hits SEVERE and at 15 fps when it's CRITICAL. The plan was to drop to 720p at CRITICAL too but changing the size mid-stream reaches further than the frame rate does: the desktop's resolution picker, saved sizes and the encoder all have to follow it, and the desktop should also show that heat was the reason.

Medium. Needs a real phone that gets hot enough, since the emulator never reaches CRITICAL.

## Saying to extract the Windows zip first

Opening `TelescopeDesktop.exe` straight from the portable zip fails with "Failed to load Python DLL". Windows 11 only extracts the exe itself into a temp folder, without `lib-<build>`, so the loader gives up before any of Telescope's code runs and `warn_running_from_archive()` in `desktop/telescope/platform/windows.py` never gets a chance. Windows does warn about it first but plenty of people click Run anyway.

A small launcher exe that checks for its `lib` folder before starting the real one could show the "extract first" message itself. Small to medium, mostly packaging in `desktop/telescope.spec` and the release workflow.

## Cheaper background blur and auto framing

Measured on a Ryzen AI 5 340 laptop on battery, in the power-saver profile: a 720p30 stream with no effects adds about 0.2 W of CPU package power, but blur adds about 0.55 W and blur plus framing about 1.35 W (189% of a core). Most of that goes to MediaPipe, not to decoding.

- [x] The segmenter gets a copy shrunk to its 256 × 144 input instead of the whole frame, and the mask stays that small until it's scaled up once at the end. MediaPipe used to do that shrink itself by picking single pixels, which let camera noise straight through: on photos of people with noise added, the mask's edges moved 7 to 12 levels from frame to frame, against 3 to 4 with an area shrink. That's the flicker around the blur.
- [x] While nothing in the picture moves, the mask is worked out 15 times a second instead of for every frame, and half of the last mask carries into the next instead of 30%. Anything moving brings both straight back.
- [x] The same goes for the pose model behind auto framing: 12 times a second while something moves, 3 while nothing does.
- [ ] ~~Capping MediaPipe's threads.~~ Tried pinning its threads to one or two CPUs: the CPU time stayed within noise (±3%) whichever cores, and it would only work on Linux. With the small input the segmenter barely uses more than one thread anyway.

On clips made from a photo of three people walking (blur plus framing paced at 30 fps, Balanced power profile), 720p went from 66% to 58% of a core while panning, 61% to 49% with a 1 px sway and 60% to 34% while still. 1080p went from 95% to 85%, 92% to 74% and 90% to 57%. The mask's edge flicker went from 3.7 to 0.5 levels while still and from 11 to 4.7 with the sway. It still needs a look on a real stream, since the clips were made from still photos: how often a person talking on a call counts as "still", and whether a quick move leaves the mask behind.

What's left after that is mostly the pose model (about 10 ms a run) while you move. Instead of a low power mode that locks settings, there's a **Power-saving effects** switch under Video in Settings that runs both models less often while you move too (720p walking clip: 57% to 42% of a core, 1080p: 86% to 68%), and troubleshooting.md says what else helps.

A phone plugged in over USB runs on the laptop's battery too, and that showed up as about 2.4 W even while not streaming. troubleshooting.md says so now.
