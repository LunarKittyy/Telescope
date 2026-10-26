# Troubleshooting

Find your problem below. Nothing there? Ask in [Q&A](https://github.com/LunarKittyy/Telescope/discussions/categories/q-a), or open an [issue](https://github.com/LunarKittyy/Telescope/issues/new?template=bug.yml) with your **Copy diagnostics** report if something is broken.

- [Starting Telescope](#starting-telescope)
- [Virtual camera](#virtual-camera)
- [Pairing and connecting](#pairing-and-connecting)
- [Video and quality](#video-and-quality)
- [Camera controls](#camera-controls)
- [Browser camera](#browser-camera)
- [Shortcuts](#shortcuts)
- [Plugins and extra downloads](#plugins-and-extra-downloads)
- [Updates](#updates)

## Starting Telescope

**"Telescope can't start"**
Another program is using port 47823, or an earlier Telescope didn't close properly and still holds it. Close that program (or end `TelescopeDesktop` in Task Manager on Windows, `main.py` on Linux) and open Telescope again.

**"Already running for another user" (Windows)**
Another account on this PC has Telescope open and both use the same local port. Close it there. Linux accounts don't share it.

**"Your settings couldn't be read"**
`telescope_config.json` was damaged or edited wrongly, so Telescope started with defaults and paired phones need pairing again. The banner shows where the old file was copied (`telescope_config.json.invalid-...`). Fix it and put it back, or pair again.

## Virtual camera

**Telescope or Phone Camera isn't listed in OBS, Discord or Zoom**
Apps only look for cameras when they start. Restart the app if it was already open when the camera was set up. On Windows, Discord stays in the tray when you close its window, so quit it from the tray icon (right-click, Quit) and open it again. Restarting the computer works too.

**Linux: the virtual camera won't open**
The `v4l2loopback` package isn't installed or loaded. Install it (`v4l2loopback-dkms` on Debian, Ubuntu and Arch, `v4l2loopback` on Fedora and Nobara through RPM Fusion, see [Manual setup](setup.md#linux)) and click **Start streaming** again. **Virtual camera & phone** in Settings can load it by hand.

**Linux: `/dev/video11` is gone after a reboot**
The module isn't loaded at boot. Starting a stream loads it again. Leave **Also switch it on at every startup** ticked in the password prompt, or switch on **Load when the computer starts** under Virtual camera & phone in Settings.

**Linux: "Virtual camera is set up differently"**
Another app (OBS's own virtual camera, for example) already loaded the module with different settings. Close that app, click **Unload** under Virtual camera & phone in Settings (or run `sudo modprobe -r v4l2loopback`) and click **Start streaming** again.

**Linux: canvas change fails with "module in use"**
OBS or another app still holds the device. Close everything using the virtual camera and retry.

**Windows: the virtual camera won't open**
The driver isn't registered. Open **Virtual camera & phone** in Settings and reinstall it.

**Windows 11: an app doesn't list Telescope**
Open **Virtual camera & phone** in Settings. If the Driver row says **Some apps can't see it** you still have the older driver, which apps that only use Media Foundation (the Windows Camera app, for one) never see. Click **Update**. An app that was already open may need restarting before it lists the new camera.

**Windows 11: the camera shows a plain dark picture**
Telescope isn't running, or it's still starting. The camera stays in every app's list even with Telescope closed and shows that dark background until Telescope sends it something. Open Telescope and the wait screen or your phone takes over.

**Windows 11: keep using the older driver**
Set the environment variable `TELESCOPE_VCAM` to `unitycapture` before starting Telescope (in System Properties under Environment Variables) and click **Install driver**. Telescope then streams to UnityCapture like on Windows 10. Apps list both cameras while the Windows 11 one is still installed, and the one Telescope uses is the one without "(Windows Virtual Camera)". Remove the variable to go back.

**Windows: the camera is called "Unity Video Capture"**
An older Telescope installed the driver before it had its own name. Click **Rename** under **Virtual camera & phone** in Settings.

**Windows: driver install says `Program Files\Telescope has links in it or other users can change it`**
Something else made that folder or changed who can edit it, and Telescope won't install into a folder it can't trust. Delete `C:\Program Files\Telescope` and click **Install driver** again.

**Windows: driver install says it `hit an error while running as admin`**
Click **Retry**. If it fails again open an issue with your **Copy diagnostics** report.

## Pairing and connecting

**QR pairing fails ("Could not reach the desktop")**
The phone and computer aren't on the same network, or the computer's firewall blocks port 8765. The dialog on the phone lists every address it tried and how each failed. Make sure both are on the same Wi-Fi and the Add phone dialog is still open (the pairing server only runs while it is). Or plug the phone in and pair over USB.

**QR pairing fails on guest or public Wi-Fi**
Those networks usually block devices from talking to each other. Nothing can work around that. Use USB pairing or a network you control.

**QR pairing fails with a VPN on**
The VPN is blocking local network traffic. Turn on its "allow local network access" option, pause it while pairing or use USB. Streaming over Wi-Fi needs the same.

**Plugged in but USB pairing doesn't happen**
The USB line in the Add phone dialog says which of these is in the way: adb isn't installed, USB debugging isn't allowed on the phone, or the app isn't on screen. Install `adb` (Linux) or click **get from Google** in that line (Windows), accept the USB debugging prompt and keep Telescope open on the phone.

**"Google wouldn't send adb to this computer" (Windows)**
Google blocks these downloads in some countries. Get Google's platform-tools for Windows another way, then click **pick one you already have** in Add phone (or **Use my own adb…** under Virtual camera & phone in Settings) and choose `adb.exe` or the zip. Wi-Fi works without adb.

**It streams over Wi-Fi although the phone is plugged in**
The note under the Connection panel's status names the reason: Telescope isn't open on the phone, USB debugging isn't allowed yet or the cable has a different phone on it. Fix that and the next connect uses USB. To never use Wi-Fi set **Connect via** to USB only.

**"Can't reach the phone"**
Telescope isn't open on the phone, or the phone is on another network and not plugged in. Open Telescope on the phone and keep it on screen.

**"Can't reach the phone" after reinstalling the phone app**
A reinstall (or Clear data) gives the phone a new identity. Click **Add phone** and pair again. Telescope asks whether it's the same phone and, if you say yes, takes over the old settings and presets.

**"Can't reach <phone>" banner while streaming**
The phone app was closed or killed, or the phone left the network. Open Telescope on the phone. If the stream is still running the picture comes back without you doing anything. Otherwise click **Start streaming**.

**"Needs pairing again"**
The phone removed this computer. Click **Add phone** and pair again. Its settings on this computer stay.

**"Another app on the phone is using the camera"**
Android gives the camera to the app on screen (the Camera app, a video call). Close or switch away from it and the stream picks back up.

**"The phone is streaming to <name>"**
Another computer paired with the phone started its camera first. Stop it there or on the phone and click **Start streaming** again.

**Stream stops a few seconds after Start with a battery or temperature banner**
**Stop the stream** is on in the Phone card and the phone is past its threshold. Click **Start anyway** or change the threshold.

**Windows asks about the firewall again after an update**
The update moved an unzipped copy to the installed one and Windows treats it as a new program. Click Allow on private networks like the first time. The old copy's rule stays behind and does nothing.

## Video and quality

**Lag or dropped frames over Wi-Fi**
Heavy (MJPEG) sends several times the data of Light. Switch Format to Light, use USB, lower JPEG quality or reduce FPS.

**Light freezes for a moment now and then over Wi-Fi**
A busy network loses a packet and TCP holds everything up until the resend. Try **UDP on Wi-Fi** in the Connection card (experimental).

**FPS stays at 30 with 60 picked**
Some phone makers keep their camera's faster modes for their own camera app. The phone lists 60 fps and accepts the request, then gives other apps 30. Telescope can't change that. **Copy diagnostics** while streaming shows it: the "Camera did" line reads 33.4 ms with range [60, 60]. Rates a camera doesn't list at all are greyed out.

**Telescope uses a lot of power, or the fans spin up**
Background and Auto framing cost the most. On a test laptop, a 720p stream without them added about 0.2 W of processor power, and with both on about 1.5 W. In rough order of what helps:
- Switch off the effect you don't need, or turn on **Power-saving effects** under Video in Settings
- Lower the resolution or FPS. Every step down means less work for the phone and the computer
- Use the Light format and turn on **Hardware acceleration** if your graphics card supports it
- On battery, unplug the phone: over USB it runs off the laptop's power. On the test laptop it drew about 2.4 W even while not streaming. Use Wi-Fi, or charge the phone from the wall

Minimizing the window barely helps, since drawing the preview is cheap.

**Telescope uses a lot of RAM**
Most of it isn't Telescope's own. Measured on a Linux laptop, streaming 1080p at 30 fps:

| What | About |
|---|---|
| Telescope open, not streaming | 240 MB |
| Streaming | 360 MB |
| Streaming with Background and Auto framing | 470 MB |

Where it goes:
- **Libraries, about 130 MB**: Qt draws the window, OpenCV and NumPy handle the pictures, FFmpeg decodes the video. They're loaded once and don't grow.
- **Frames from the camera, about 100 MB at 1080p**: a few copies of each picture on its way from the phone to the virtual camera. Lower resolutions need less.
- **The vision models, about 150 MB**: only while Background or Auto framing is on. About 10 seconds after you switch both off most of it is given back, and restarting Telescope frees the rest.
- **Telescope itself, about 50 MB**: its code, settings and the wait screen.

Task managers count memory in different ways (Windows Task Manager, for one, leaves out memory other apps could share), so your numbers may not match these.

**"No graphics card Telescope can use" in Video settings**
On Linux, Telescope's decoder only uses NVIDIA cards. For AMD and Intel click **Set up VA-API…** next to it. Without a usable card Light still decodes on the processor.

**VA-API setup says an existing build no longer works**
A system update changed FFmpeg or Python. Install any packages setup lists, click **Rebuild** and restart Telescope.

**VA-API setup asks for PyAV 14 or newer**
Telescope's Python environment has an older PyAV. Upgrade it there, restart Telescope and open setup again.

## Camera controls

**A control is greyed out or does nothing**
The current lens doesn't support it. Some front cameras and telephoto lenses have no manual exposure or white balance. Use Auto there. Hover **Full manual control** or **Limited manual control** to see what the lens can do.

**Zoom and Pan are greyed out on the Transforms card**
Auto framing is moving them. Switch it off on its card and your own zoom and pan come back.

## Browser camera

**The page won't load**
The device isn't on the same network, or a firewall blocks port 8767. Use the address on the card that matches the device's network (the card lists each one if there are several) and allow port 8767 in the firewall. If you clicked Cancel on the Windows firewall prompt, allow Telescope in Windows Security under Firewall & network protection, Allow an app through firewall.

**The page says it can't reach the computer or that the code was replaced**
The code changed (**New link**, or Telescope restarted). Scan the code on the card again.

**The page stays on "Connecting the video…" or keeps reconnecting**
The video goes over WebRTC on UDP ports picked each time, which a strict firewall can block even with port 8767 open. On Windows allow Telescope itself in the firewall, not only the port. On Linux allow incoming UDP from your local network, or turn the firewall off for a moment to confirm that's it.

**It stops when the phone locks or you switch apps**
Phones pause the camera for pages in the background. Keep the page open with the screen on. Going back to it starts it again.

## Shortcuts

**A shortcut does nothing**
Its control is greyed out or hidden right now (no phone, Auto mode for a manual slider, or the mic is off for Mute). `telescope --action` prints why.

**"Another app already uses this key" (Windows)**
Windows gives the key to the other program every time, even with Telescope in front. Pick another key in the **Keyboard shortcuts** tab of Settings.

**Shortcuts only work inside Telescope (Linux)**
Your desktop has no global shortcuts portal (only Plasma 5.27+, GNOME 48+ and Hyprland do) or you declined the shortcut when the desktop asked to confirm it. On Plasma, **Open** next to **Desktop shortcut settings** lets you set the key. Elsewhere bind the shortcut's **Copy command line** in your desktop's keyboard settings.

## Plugins and extra downloads

**A card or menu entry is gone**
That part of Telescope is switched off in **Plugins**. Open Settings and go to the Plugins tab (it reads **Plugins (N off)** when something is off), switch it back on or click **Turn all back on**, then click **Restart now**.

**Auto framing or Background asks for a one-time download**
The vision download hasn't happened yet or was removed. A new Telescope version can need a newer one too. Click **Download** on the card. If it fails check that GitHub is reachable from this computer.

**Auto framing or Background says MediaPipe didn't load**
The download is there but MediaPipe won't start here (it only works on 64-bit Intel and AMD). On Linux it also needs the EGL and OpenGL ES libraries, which graphics drivers normally bring. If **Copy diagnostics** says `libEGL.so.1 missing` or `libGLESv2.so.2 missing`, install `libegl1` and `libgles2` (Debian and Ubuntu) or `libglvnd-egl` and `libglvnd-gles` (Fedora) and restart Telescope. Otherwise open an issue with your **Copy diagnostics** report.

## Updates

**"Didn't start after updating, so it's held back"**
The new version crashed before its window came up and the old one was put back. For an unzipped copy that was moving to the installed one, it means the installed copy didn't open. Click **Try again**. If it keeps happening open an issue with your **Copy diagnostics** report.
