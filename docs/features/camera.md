<!-- covers: rust/crates/shell/src/plugins/camera_control.rs rust/crates/shell/src/plugins/presets.rs rust/crates/link/src/controls.rs rust/crates/effects/src/transforms.rs android/app/src/main/kotlin/com/telescope/CameraCatalog.kt android/app/src/main/kotlin/com/telescope/CameraSessionController.kt android/app/src/main/kotlin/com/telescope/StreamPrefs.kt android/app/src/main/kotlin/com/telescope/FrameRate.kt android/app/src/main/kotlin/com/telescope/DynamicBitrate.kt android/app/src/main/kotlin/com/telescope/DynamicFrameRate.kt android/app/src/main/kotlin/com/telescope/H264Encoder.kt rust/crates/core/src/gpu_pick.rs rust/crates/setup/src/setup.rs; checked: 0d35dc883 -->
# Camera and quality

Lenses, exposure, the picture format and how much bandwidth it uses.

## Camera control

- **Lens picker** switches between the phone's real wide, main and telephoto sensors rather than faking it with digital zoom. On **Auto** the phone picks the lens as you zoom
- **ISO and shutter speed** by slider or typed value. The range follows the current lens
- **Brightness** (exposure compensation), usually ±8 EV
- **White balance** in Kelvin (2000-10000 K) with a green-magenta tint
- **Focus** is **Auto**, **Tap a point** (click the preview to focus and meter exposure there) or **Manual**
- **Stabilisation**, noise reduction, sharpening, black level lock and the torch, on lenses that have them
- **Low light** lets auto exposure slow the camera down (usually to 10 fps at the lowest) so each frame catches more light. It only does that when it has to
- Anything the current lens can't do is greyed out. Hover **Full manual control** or **Limited manual control** to see what it supports

## Stream transforms

These apply live without restarting the camera.

- Flip horizontally or vertically
- Rotate 90° CW, 180° or 90° CCW
- **Zoom 1-10x** with pan. **Max zoom** under Video in Settings goes up to 30x. Scroll over the preview to zoom and drag to pan
- **Width** and **Height** stretch or squish the picture from 50% to 200%, each separately. Whatever goes past the edge is cut off and a squish leaves black bars, like a rotated picture gets
- **Lens correction** straightens the curved lines a wide lens makes and stops faces near the edge looking stretched. Slide right for a wide lens and left for a long one. It crops or pulls in the picture as much as needed to fill the edges and 0% leaves the picture untouched
- Double-click any slider to reset it. The round arrow in a card's header resets the card and lights up once something on it has changed. Things you made or paired, like notes, layouts and a loaded LUT, are left alone. The pan pad takes arrow keys (Shift for bigger steps) and Home to centre

Mirror, Upside down and rotation also apply to non-RAW [photos](effects.md#photos). The photo cards have a **Use zoom** switch for a lossless crop to your manual zoom and position. HEIC and Ultra HDR photos get the orientation but not the crop. Stretch, Lens correction and Auto framing do not apply to photos.

Zooming in gets sharper because it crops from the full sensor first and hops to the telephoto lens once your framing fits inside it. With an up-to-date phone app, zooming further than that lens is cropped on your computer, because Android phones stop cropping once they switch to a longer lens. Dots on the zoom slider mark where a longer lens is used.

## Presets

The **Presets** button saves your camera, output and transform settings (lens included) under a name and restores them in one click. The optional picture plugins (Auto framing, Background, Fun pack, Colour grading and the others) are saved too when they're on. Presets also save a USB webcam's size, frame rate and picture settings, the microphone's gain, auto level and noise reduction (not whether it's on) and a Spout sender's frame rate. Applying one changes only the source it was saved for. A streaming webcam or Spout sender reopens if the preset changes its size or rate. Presets don't hold your saved One picture layouts. Each phone has its own presets and each preset can have its own shortcut.

## Resolution and FPS

- The resolution list comes from what the current lens really supports
- One **FPS** setting (15 to 60) drives both the phone and the virtual camera. Rates the lens can't do are greyed out
- **Canvas size** under Video in Settings sets the virtual camera's size apart from the phone's. Pick 720p, 1080p or 4K in landscape or portrait, a 4:3 size or a custom one (custom sizes have to be even). On Linux changing it reloads the driver, which asks for your password once (close OBS first). If the driver can't be reloaded, the size stays as it was and a stream that was running carries on at the old size

## Format and bandwidth

| Format | Good for | Needs |
|---|---|---|
| **Light** (H.264, the default) | Most things | About 8 Mbps at 1080p30, fine even on slower Wi-Fi |
| **Heavy** (MJPEG) | Sharper fast motion | USB or strong Wi-Fi |

A phone that can't do Light switches to Heavy.

- **Heavy** has a JPEG quality slider
- **Light** has a bitrate slider with **Auto**, a fixed 1-100 Mbps or **Dynamic**. Dynamic sends as much as the connection can carry and backs off before the video starts to lag. If that's still too much it lowers the frame rate too (down to 15) and climbs back once there's room
- **Hardware acceleration** under Video in Settings decodes Light on the graphics card rather than the processor, which helps most at 4K or on a laptop. It works on Windows with any graphics card and on Linux through VA-API or NVIDIA's CUDA. The line under the switch shows which one it found. On Linux it's on by default when the card is VA-API and off everywhere else. Telescope decodes on the processor for a few minutes if the card falls behind and then tries the card again
- **Graphics card** under Video in Settings picks which card Telescope uses. **Automatic** follows what the system picks and the list contains each card the computer has. The window and the picture steps run on it. Hardware decoding opens the render node of that card on Linux. A change applies after a restart and the row offers a **Restart now** button. A picked card that fails or was skipped after a crash is replaced by another one
- **Graphics API** under Video in Settings is on Windows only. **Vulkan first** is the default. **DirectX 12 first** lets the graphics card pass pictures to effects without a copy through memory, which can use less processor with effects and more memory, depending on the card. Intel cards ignore it. A change applies after a restart.
- Light only lists resolutions the phone's encoder can take
- A resolution bigger than the virtual camera can use costs the computer decoding work for nothing. While frames run late Telescope asks the phone for the smallest size that still fills the camera and says so under [Adjustments](output.md#monitoring)

<details>
<summary><b>UDP on Wi-Fi (experimental)</b></summary>

Found in Settings on the Streaming tab and off by default. It sends Light over UDP rather than TCP while the phone is on Wi-Fi. The idea is that a lost packet only costs that one piece and does not hold up the rest of the stream. It can also stutter more than TCP. If Light freezes repeatedly on Wi-Fi give it a try and switch it off again if it doesn't help. It's encrypted and falls back to TCP if the video doesn't get through (usually a firewall). It isn't used over USB or for Heavy.

Dynamic works over UDP too. The computer reports to the phone how many packets went missing and how long they took to arrive, which shows a thinning link earlier than waiting for video to pile up on the phone. If the phone app is older than the desktop app, Dynamic still runs over TCP.
</details>
