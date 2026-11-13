<!-- covers: rust/crates/shell/src/plugins/camera_control.rs rust/crates/shell/src/plugins/presets.rs rust/crates/link/src/controls.rs rust/crates/effects/src/transforms.rs android/app/src/main/kotlin/com/telescope/CameraCatalog.kt android/app/src/main/kotlin/com/telescope/CameraSessionController.kt android/app/src/main/kotlin/com/telescope/StreamPrefs.kt android/app/src/main/kotlin/com/telescope/FrameRate.kt android/app/src/main/kotlin/com/telescope/DynamicBitrate.kt android/app/src/main/kotlin/com/telescope/DynamicFrameRate.kt android/app/src/main/kotlin/com/telescope/H264Encoder.kt; checked: 1cbb8644b -->
# Camera and quality

Lenses, exposure, the picture format and how much bandwidth it uses.

## Camera control

- **Lens picker** switches between the phone's real wide, main and telephoto sensors instead of faking it with digital zoom. On **Auto** the phone picks the lens as you zoom
- **ISO and shutter speed** by slider or typed value. The range follows the current lens
- **Brightness** (exposure compensation), usually ±8 EV
- **White balance** in Kelvin (2000-10000 K) with a green-magenta tint
- **Focus** is **Auto**, **Tap a point** (click the preview to focus and meter exposure there) or **Manual**
- **Stabilization**, noise reduction, sharpening, black level lock and the torch, on lenses that have them
- **Low light** lets auto exposure slow the camera down (usually to 10 fps at the lowest) so each frame catches more light. It only does that when it has to
- Anything the current lens can't do is greyed out. Hover **Full manual control** or **Limited manual control** to see what it supports

## Stream transforms

These apply live without restarting the camera.

- Flip horizontally or vertically
- Rotate 90° CW, 180° or 90° CCW
- **Zoom 1-10x** with pan. **Max zoom** under Video in Settings goes up to 30x. Scroll over the preview to zoom and drag to pan
- **Width** and **Height** stretch or squish the picture from 50% to 200%, each on its own. Whatever goes past the edge is cut off and a squish leaves black bars, like a rotated picture gets
- **Lens correction** straightens the curved lines a wide lens makes, so faces near the edge stop looking stretched. Slide right for a wide lens and left for a long one. It crops or pulls in the picture just enough to keep the edges filled, and 0% leaves the picture exactly as it is
- Double-click any slider to reset it. The pan pad takes arrow keys (Shift for bigger steps) and Home to centre

Zooming crops from the full sensor first and hops to the telephoto lens once your framing fits inside it, which is why zooming in gets sharper. Dots on the zoom slider mark where a longer lens takes over.

## Presets

The **Presets** button saves your camera, output and transform settings (lens included) under a name and brings them back in one click. The optional picture plugins (Auto framing, Background, Fun pack, Colour grading and the others) are saved too when they're on. Each phone has its own presets and each preset can have its own shortcut.

## Resolution and FPS

- The resolution list comes from what the current lens really supports
- One **FPS** setting (15 to 60) drives both the phone and the virtual camera. Rates the lens can't do are greyed out
- **Canvas size** under Video in Settings sets the virtual camera's size apart from the phone's. Pick 720p, 1080p or 4K in landscape or portrait, a 4:3 size or a custom one (custom sizes have to be even). On Linux changing it reloads the driver, which asks for your password once (close OBS first)

## Format and bandwidth

| Format | Good for | Needs |
|---|---|---|
| **Light** (H.264, the default) | Most things | About 8 Mbps at 1080p30, so fine even on slower Wi-Fi |
| **Heavy** (MJPEG) | Sharper fast motion | USB or strong Wi-Fi |

A phone that can't do Light switches to Heavy automatically.

- **Heavy** has a JPEG quality slider
- **Light** has a bitrate slider with **Auto**, a fixed 1-100 Mbps or **Dynamic**. Dynamic sends as much as the connection can carry and backs off before the video starts to lag. If that's still too much it lowers the frame rate too (down to 15) and climbs back once there's room
- **Hardware acceleration** under Video in Settings decodes Light on the graphics card instead of the processor, which matters most at 4K or on a laptop. It works on Windows with any graphics card and on Linux through VA-API or NVIDIA's CUDA. The line under the switch says which one it found. On Linux it's on by default when the card is VA-API, and everywhere else it starts off. If the card falls behind, Telescope decodes on the processor for a few minutes and then tries the card again
- Light only lists resolutions the phone's encoder can take

<details>
<summary><b>UDP on Wi-Fi (experimental)</b></summary>

In Settings on the Streaming tab, off by default. It sends Light over UDP instead of TCP while the phone is on Wi-Fi. The idea is that a lost packet only costs that one piece instead of holding up the whole stream, but it can also stutter more than TCP. If Light keeps freezing on Wi-Fi give it a try and switch it back off if it doesn't help. It's encrypted and goes back to TCP if the video doesn't get through (usually a firewall). It isn't used over USB or for Heavy.

Dynamic works over UDP too. The computer tells the phone how many packets went missing and how long they took to arrive, which shows a thinning link earlier than waiting for video to pile up on the phone. If the phone app is older than the desktop app, Dynamic still runs over TCP.
</details>
