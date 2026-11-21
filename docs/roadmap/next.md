# Next release

The next release follows 5.1.4. Ticked here means merged and not released yet. Everything ticked is in the nightly builds.

- [ ] [Stargazer photos, cleaner frames and Night focus](#stargazer-photos-cleaner-frames-and-night-focus)
- [ ] [Stills as raw YUV](#stills-as-raw-yuv)
- [ ] [Hardware decoding on Windows without a trip through memory](#hardware-decoding-on-windows-without-a-trip-through-memory)
- [ ] [Bug hunts, speed and a smaller app](#bug-hunts-speed-and-a-smaller-app)
- [x] [Rearranging the docs](#rearranging-the-docs)
- [x] [A page of setups to try](#a-page-of-setups-to-try)
- [x] [Doing the picture work on the GPU](#doing-the-picture-work-on-the-gpu)
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

Three more pieces of Stargazer. Frames from photos lets Live stack and Star trails take their frames from photos instead of video, one request per shot. It's written and in review. Lucky imaging is video only since it wants hundreds of frames. Cleaner frames asks the phone for higher quality video during a run, with a gap of a second or two at the start while the phone reconnects. Night focus lets the Night row set a focus instead of using the camera's own. It needs a phone release, since the phone app has to take the focus.

## Stills as raw YUV

A camera probe is now in the phone's developer settings and writes a report of what the phone's camera allows (long exposures, burst rates, full-size YUV). It needs a phone release to reach anyone. The report decides whether a burst of stills could go as raw YUV instead of JPEG, with no compression and nothing to decode. That is a protocol change and gets its own design first.

## Hardware decoding on Windows without a trip through memory

On Linux a VA-API picture goes to the effects on the card without a trip through memory. On Windows D3D11VA decoding still hands the frame to memory first. Keeping it on the card means sharing the decoder's texture with Vulkan or DX12 and waiting for the copy to finish before the picture work reads it. A design is written and nothing is built.

## Bug hunts, speed and a smaller app

Ongoing. Bug passes over the code and measured work on speed, system use and the size of the app, one change per pull request. Nothing here is a feature.

## Rearranging the docs

The prose pass over `docs/` and the app's texts went out in 5.1.4. The files are now rearranged so each topic is where people would look for it. The feature docs were brought up to date after.

## A page of setups to try

[Things to try](../unusual-setups.md) lists ways to use Telescope that it wasn't built around, like a phone from far away, a DSLR as a photo console and a whiteboard scene. It's a first version. The Tailscale entry still needs trying on real links (Dynamic over a slow connection, a relayed route, reconnecting between mobile data and Wi-Fi, Browser camera over a 100.x address).

## Doing the picture work on the GPU

Background blur now runs on the graphics card (4K with everything on went from 5.5 to 2.4 ms for the blur) and a wasted pass in it is skipped. Spout capture is converted on the card too, the 1998 webcam costs much less and Green screen goes dark instead of showing the room when the card fails. Cinema's tints in the preview are painted on the card, which had been costing about 14 points of app CPU at 4K. On Linux VA-API pictures reach the steps on the card without a trip through memory. What's left is in [future.md](future.md#doing-the-picture-work-on-the-gpu).

## A graphics card choice in Settings

Settings has a graphics card choice for someone whose system picks the wrong card. The guard that skips a card after a crash is tougher and VA-API follows the card you pick. Running steps on the integrated card was dropped, see [dropped.md](dropped.md).

## Fixes for people tracking and Composite

- Composite tiles follow Max zoom and remember their zoom
- The stick figure copes with close-ups and crossed legs
- Auto framing can follow one person, and follows through a crossing. Picking a person is refused on a preview that shows another stream
- Background cuts around chairs and light clothing more cleanly, and reuses the last mask instead of going black when a new one is late

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
- The windows fit the screen at 125% and 150% scale and after a taskbar that hides itself comes back

## Stargazer and Phototrap

Both are optional Lab plugins. Stargazer has lucky imaging, live stacking and star trails and shows how sharp the picture is. Night takes one long exposure with a phone or a DSLR. Phototrap takes a burst of photos when something moves in a zone you drew. The alignment and stacking they use is in `core` for other plugins to use too. Photos aren't taken while the picture is hidden, and the gaps that were left in that are closed.

## Plugins that fail safely and share values

A plugin that panics is switched off and the app carries on, unless it hides part of the picture. Then the app closes with a crash report. A plugin can name the built-in plugins it needs in `plugin.json` and share values with other plugins through a host registry. The MCP server can change the settings of most plugins now. Safety and outward settings (Privacy blur, start on, the battery and temperature stops, the update channel, keeping the app portable and the camera's manual switches) are kept out of it on purpose.

## Phones that find the computer again

Paired phones used to lose the computer after a Wi-Fi change until they were paired again or plugged in. Both apps now keep a small UDP exchange open on port 8769 to swap their addresses. The old checks are the fallback. Both apps show a banner when their versions differ, naming the older side. It was tried on a real phone over Wi-Fi. The case of a router that drops the computer's traffic to the phone hasn't been forced yet, and the phone side only counts once it's in a phone release.

## DSLRs on Windows

Live view for Nikon and Canon works through Windows' own camera driver, and so do photos, focus, aperture, exposure compensation, white balance and a Nikon's ISO and shutter speed. A real Nikon D5100 was tried for live view and photos on both Linux and Windows. Canon is only tried with a mock camera so far.

## Spout capture fixes

A Spout sender that gets stuck on Linux no longer holds up Stop and its pictures are dropped when they finish too late. A sender that is stuck shows that on the card instead of an old picture.

## Lighter and faster

Less work while nothing is looking and less per frame while something is.

- JPEG frames aren't decoded while no app reads the camera, and PipeWire sharing does nothing while nothing reads the source
- The Windows 11 camera is written at the rate its reader asks for
- The idle app wakes far less often, from about 12 times a second to 2, and hidden or minimised windows don't count as watching
- Each H.264 packet is built once and the UDP fragments are sent straight from the frame
- The virtual camera output costs less CPU and memory, and so does denoising over time

## A faster interface underneath

Fewer allocations while the window updates, about a third as many for each update from the phone. The interpreter also lets go of the compiler's trees after each build and Settings frees its window. The window isn't visibly lighter.

## Going back to the Python version removed

Settings no longer has the "Go back to the Python version" button and the code that downloaded 4.4.2 is gone. An update from the Python app still works. See [dropped.md](dropped.md).
