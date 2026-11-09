# Next release

The next release follows 5.0.5. Everything ticked here is merged and not released yet. The new look is merged too. All of it is in the Rust desktop's history since 5.0.5.

- [x] [Auto colour grading](#auto-colour-grading)
- [x] [Stats](#stats)
- [x] [An MCP server](#an-mcp-server)
- [x] [Older DSLRs over USB](#older-dslrs-over-usb)
- [x] [Fixes from a review of the port](#fixes-from-a-review-of-the-port)
- [x] [The new look](#the-new-look)

## Auto colour grading

A button on the Colour grading card for people who don't want to set the grade up themselves. It measures about a second of frames and fills in the existing sliders to give a starting point anyone can still tweak instead of a black box. A cheap laptop camera with a hand-made grade, auto framing and a light background blur already looks like a proper webcam, and this gets most people there with one press.

It looks for people through the vision pack (and fetches the pack if it isn't there yet), then fits the basic sliders in stages. The card shows a short note on what it did, an Undo and a brighter and darker nudge. What it measures, all as numbers from the frames:

- White balance from the face, not the whole frame because a coloured wall throws off the usual "make the average grey" approach. Skin of every tone sits close to one hue line and Temperature and Tint turn the face's average colour onto it. With no face it corrects a cast in the whole picture more gently
- Gamma so the face lands around middle brightness and Gain so the brightest part of the background doesn't clip. Dark scenes get lifted and overexposed ones brought down
- Lift and Contrast from the histogram, so the darkest bit sits just above black while a flat picture gets some depth
- Highlights from how much of the frame is close to clipping, like a window or a lamp behind you
- Saturation towards a target, held back by the skin so faces never go orange

Still open is a whole-look button that also turns on auto framing and a light background blur, and an always-on mode. That one would need heavy smoothing to stop the picture from pumping when the light changes.

## Stats

An optional Stats card that shows everything about a stream as it happens, for anyone who wants to know why a picture stutters or likes numbers. OBS has one and people check it first when something drops, but it can only see what reaches it.

It has two views.

**Standard** has what people usually expect from a stats panel, enough to tell whether the phone, the link or the computer is the slow part:

- Frames per second in and out, repeated and dropped frames and the reason for each
- The delay from the phone's camera to the virtual camera
- Wi-Fi or USB, the Wi-Fi band and signal and the bitrate against what was asked for
- The phone's temperature and whether it's throttling
- Which apps read the virtual camera

**Nerd** adds more of what can sensibly be measured, following a frame from the phone to the virtual camera:

- On the phone: encoding time, the encoder's queue and what the phone was asked for against what it does
- On the link: round-trip time, jitter, lost and resent packets, the clock offset between phone and computer and each part's share of the delay
- On the computer: decoding time and where it runs, each frame step's time and whether it runs on the card or the CPU, when the balancer moved the steps and why
- Per plugin: what its steps cost, so a slow plugin can be found
- For the app itself: CPU and memory

Each number gets a small graph of the last minute that still shows a spike after it passed. Copy diagnostics adds a summary of the Nerd view. Every line has a star that pins its number to the bottom bar (the star shows on hover). The MCP server's `stats` and `watch` read the same numbers.

A plain-language line saying what's slowing the stream was tried on the card and in Copy diagnostics, and taken out again.

## An MCP server

An optional plugin, off by default, that lets an AI app like Claude Desktop read and change Telescope's settings, so someone can describe the look they want or ask what's making the picture lag. The AI app starts a small `telescope mcp` command that talks to the running Telescope over a local socket without opening a network port. [docs/mcp.md](../mcp.md) has the details.

What got built from the idea:

- `describe`, `get`, `set` (several settings at once, checked first, so nothing changes if one value is wrong), `revert`, `measure`, `status`, `diagnostics`, `actions` and `run_action`, plus `stats` and `watch` for the Stats numbers. `measure` is the same measuring Auto colour grading uses
- Each AI app only undoes its own changes, and a setting you moved by hand afterwards stays as it is
- An MCP button in the top bar shows when an AI app is connected and undoes what it changed, and the MCP tab has a Read only switch and the last 30 things the AI did
- A control the AI changes gets an outline for a moment, when it's on screen
- Plugins declare their settings (Colour grading and Transforms so far)
- Starting or stopping the stream, switching the phone's camera off and picking another phone stay out of reach

Sending frames from the camera isn't built, and neither is a preview where changes wait until accepted. They're in [Some day](future.md#more-for-the-mcp-server).

## Older DSLRs over USB

Cameras that only do live view over USB (many older DSLRs) show up in the device picker next to the webcams. It's Linux only. The Linux download carries libgphoto2, so there's nothing to install, and when it's missing the app falls back to the `gphoto2` command. The picture is small (a Nikon D5100 sends 640×426 at about 30 fps), and when the camera ends live view after a few minutes the stream holds the last frame and carries on.

It went further than the first plan:

- The Webcam card gets what the camera allows right now (a focus jog with a Focus button, aperture, exposure compensation and white balance)
- The Photos card takes photos with the DSLR, including RAW and JPEG together, and comes back after the stream drops
- A table of camera quirks, `rust/crates/outputs/src/webcam_capture/quirks.rs`, so one camera's habits don't spread through the code

## Fixes from a review of the port

A review of the Rust desktop in November 2026 looking at threads, error handling, `unsafe` code and the dependencies. Most of what it found only shows when something already went wrong, so a normal stream looks the same. Merged:

- The Windows 11 camera waits at most 30 ms for an app that's reading it and drops that frame instead of freezing
- A webcam writer that won't stop can't get a second one started next to it, and the window no longer waits on it
- A JPEG decoder that crashes only loses that frame, and three crashes in a row reconnect with fresh decoders instead of leaving a frozen picture
- Over UDP the first keyframe request goes out straight away, and frames skipped while waiting for a keyframe count as arrived again like in the Python app
- `cargo deny` checks the dependencies' advisories and licences every week (`.github/workflows/deny.yml`)

## The new look

The desktop window got redone, with IBM Plex Sans as its font. Merged:

- Design tokens for every size, radius, spacing and colour, and a UI kit of controls built on them (`STYLE.md` is the guide). Everything that used to have its own hard-coded colour is on tokens now
- Four looks to pick in the Look section of General: Dark, Classic (the old deep blue), OLED and Light
- Every card rebuilt on the kit, with an icon rail, sliders whose fill shows what they mean, dials, wheels and a pad for Desk mode's sheet size
- A shared motion kit (switches, sliders, folds, pages, banners and menus all move the same way) that follows the system's reduced-motion setting
- Tooltips with a title, what the control does and a hint, quiet while someone is adjusting and quick while reading
- Settings window redone in the same style, and built in the background so the first click is fast
- Framing got steadier. Levels end at the head, chest, belt and feet, zoom follows the Max zoom setting, and the Framing bubble always shows a stand-in scene. Shape is called Aspect ratio now and Things in hand is Show hands
- The pan map outlines the phone's longer lenses, and a zoom change on the phone is followed in the frames so the picture never jumps
- KDE Plasma title bars take the app's colours
