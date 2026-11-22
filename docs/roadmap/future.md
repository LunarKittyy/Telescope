# Some day

Ideas worth keeping that nobody is about to work on. Move one to [next.md](next.md) once it's planned.

## Optional plugins

More ideas for plugins that start out off, for people who want more out of their webcam than a picture. Most of them need no new dependencies (FFmpeg is already there and the vision download already exists). Same rule as the Camera card. If the phone's camera exposes something, Telescope uses it.

Most of what they need underneath got built as host services along with the plugins in [4.3](4.3.md). A denoised "hold still" scan in document mode could still use the new frame stacking.

### 3D from two phones

An early version is out in [5.1.3](5.1.md#513-16-november-2026) as a Lab plugin. Two phones go out side by side or red/cyan on the Telescope camera, paired by when each frame was taken, with Fine tune sliders to line them up manually and Match for exposure and colour. What's left of the plan:

- **YouTube.** A recording from Telescope could carry the stereo metadata that makes an upload play as real 3D
- **Checking the timing.** Paper can't show time but the desktop screen can. A fast changing code on the monitor filmed by both phones shows the real offset between them, a good test of the clock sync

Small to medium for each. The plugin remains in Lab as a tech demo. Lab describes its state and it doesn't need to graduate.

### More for the MCP server

The MCP server is built (see [5.1](5.1.md#an-mcp-server)). What's left of the idea. `show` is the one to do, `frame` is a maybe:

- `show` opening the right tab and highlighting a setting, for when someone asks where something is
- `frame` sending a small frame, only when the user allows it and off by default. A middle step could blur the person out with the vision mask to let the model see the room and the light but not them. Telescope would show when a model is connected and separately when it's getting frames

## Smaller things

Decided but small enough that none of them needs its own release.

### USB without adb

USB pairing and the USB route need adb on the computer (which Windows downloads from Google after asking) and USB debugging switched on in the phone's Developer options. Android's Open Accessory mode would let the phone talk to the computer over the cable with neither. The computer sends a plugged-in phone a request to switch into accessory mode and the Telescope app opens. The catch is that it needs a libusb-style driver on Windows. The phone app also has to handle a second kind of connection beside adb's reverse port (`rust/crates/link/src/adb.rs` and `service.rs` on the desktop). Big. It needs real phones from several makers to try.

## Carried over from the desktop notes

- **Signed updates:** a changed manifest can name its own package and checksum because the updater relies on the update manifest over HTTPS. Sign the manifest with a key built into the app before anything in it is believed. The Python app worked the same way. Going to a signature needs a release that adds the key first.
- **Offering our Slint fixes upstream:** `rust/vendor/` carries patches to Slint's interpreter and core and to femtovg (each folder's `PATCH.txt` explains what and why). Release them first, wait a while for bugs to show up and only then think about offering them to Slint. The femtovg pipeline cache change goes away once Slint moves to femtovg 0.28.
