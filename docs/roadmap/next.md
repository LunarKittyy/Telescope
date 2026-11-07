# Next release

The next release follows 5.0.5.

- [ ] [Auto colour grading](#auto-colour-grading)
- [ ] [Stats](#stats)
- [x] [Fixes from a review of the port](#fixes-from-a-review-of-the-port)

## Auto colour grading

A button on the Colour grading card for people who don't want to set the grade up themselves. It measures about a second of frames and fills in the existing sliders to give a starting point anyone can still tweak instead of a black box. A cheap laptop camera with a hand-made grade, auto framing and a light background blur already looks like a proper webcam, and this should get most people there with one press.

What it would measure, all as numbers from the frames:

- White balance from the face, not the whole frame because a coloured wall throws off the usual "make the average grey" approach. Skin of every tone sits close to one hue line and Temperature and Tint turn the face's average colour onto it.
- Gamma so the face lands around middle brightness and Gain so the brightest part of the background doesn't clip.
- Lift and Contrast from the histogram, so the darkest bit sits just above black while a flat picture gets some depth.
- Highlights from how much of the frame is close to clipping, like a window or a lamp behind you.
- Saturation towards a target, held back by the skin so faces never go orange.

Later, maybe a whole-look button that also turns on auto framing and a light background blur, and an always-on mode. That one would need heavy smoothing to stop the picture from pumping when the light changes.

## Stats

A Stats panel that shows everything about a stream as it happens, for anyone who wants to know why a picture stutters or likes numbers. OBS has one and people check it first when something drops, but it can only see what reaches it.

It has two views.

**Standard** has what people usually expect from a stats panel, enough to tell whether the phone, the link or the computer is the slow part:

- Frames per second in and out, dropped frames and the reason for each
- The delay from the phone's camera to the virtual camera
- Wi-Fi or USB, the Wi-Fi band and signal and the bitrate against what was asked for
- The phone's temperature and whether it's throttling
- Which apps read the virtual camera

**Nerd** adds every number that can sensibly be measured, following a frame from the phone to the virtual camera:

- On the phone: the camera's frame rate, encoding time, the encoder's queue and what the phone was asked for against what it does
- On the link: round-trip time, jitter, lost and resent packets, the clock offset between phone and computer and each part's share of the delay
- On the computer: decoding time and where it runs, each frame step's time and whether it runs on the card or the CPU, the card's waits, frames copied to the card and back, when the balancer moved the steps and why, and the time to hand a frame to the virtual camera
- Per plugin: the time its frame step, Lua events and card updates take, so a slow plugin can be found
- For the app itself: CPU and memory, the window's redraw time and the vision pack's time per frame

Each number gets a small graph of the last minute that still shows a spike after it passed. Copy diagnostics adds a summary of the Nerd view.

## Fixes from a review of the port

A review of the Rust desktop in November 2026 looking at threads, error handling, `unsafe` code and the dependencies. Most of what it found only shows when something already went wrong, so a normal stream looks the same. Merged:

- The Windows 11 camera waits at most 30 ms for an app that's reading it and drops that frame instead of freezing
- A webcam writer that won't stop can't get a second one started next to it, and the window no longer waits on it
- A JPEG decoder that crashes only loses that frame, and three crashes in a row reconnect with fresh decoders instead of leaving a frozen picture
- Over UDP the first keyframe request goes out straight away, and frames skipped while waiting for a keyframe count as arrived again like in the Python app
- `cargo deny` checks the dependencies' advisories and licences every week (`.github/workflows/deny.yml`)
