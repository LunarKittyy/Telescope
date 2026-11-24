<!-- covers: rust/crates/link/src rust/crates/outputs/src rust/crates/system/src/hotkeys* rust/crates/system/src/updates* rust/crates/shell/src/plugins_tab.rs rust/crates/vision/src/pack.rs rust/crates/core/src/gpu_marker.rs; checked: 88de924b5 -->
# Troubleshooting

Find your problem below. Nothing there? Ask in [Q&A](https://github.com/LunarKittyy/Telescope/discussions/categories/q-a), or open an [issue](https://github.com/LunarKittyy/Telescope/issues/new?template=bug.yml) with your **Copy diagnostics** report if something is broken.

- [Starting Telescope](#starting-telescope)
- [Virtual camera](#virtual-camera)
- [Pairing and connecting](#pairing-and-connecting)
- [Video and quality](#video-and-quality)
- [Camera controls](#camera-controls)
- [Browser camera](#browser-camera)
- [Webcams](#webcams)
- [Shortcuts](#shortcuts)
- [Plugins and extra downloads](#plugins-and-extra-downloads)
- [Updates](#updates)

## Starting Telescope

**"Telescope can't start"**
Another program is using port 47823, or an earlier Telescope didn't close properly and still holds it. Close that program (or end `TelescopeDesktop` in Task Manager on Windows, `telescope-rs` on Linux) and open Telescope again.

**"Telescope is already running" (Windows)**
Another account on this PC has Telescope open and both use the same local port. Close it there. Linux accounts don't share it.

**"Your settings couldn't be read"**
Telescope started with defaults and paired phones need pairing again because `telescope_config.json` was damaged or edited wrongly. The banner shows where the old file was copied (`telescope_config.json.invalid-...`). Fix it and restore it, or pair again.

**Light (H.264) streams fail and the log has "os error 4551" (Windows)**
Smart App Control on Windows 11, or an application control policy at work, blocked one of FFmpeg's DLLs because they aren't signed. The stream error names Smart App Control. Without FFmpeg only Heavy (MJPEG) works, and the phone's sound can't be decoded either. Smart App Control can't allow a single app. Turn it off in Windows Security under App & browser control, or set the stream to Heavy. Windows only lets you turn it back on by reinstalling it.

**"Cannot mount AppImage" (Linux)**
The AppImage needs `fusermount` or `fusermount3`, which nearly every desktop has. Install `fuse3` from your distro, or start it with `./Telescope-x86_64.AppImage --appimage-extract-and-run`, which unpacks it into your temp folder and needs no FUSE at all. It starts slower that way and the unpacked copy is left in the temp folder until it's cleared.

## Virtual camera

**Telescope isn't listed as a camera in OBS, Discord or Zoom**
Apps only look for cameras when they start. Restart the app if it was already open when the camera was set up. On Windows Discord remains in the tray when you close its window. Quit it from the tray icon (right-click, Quit) and open it again. Restarting the computer works too.

On Linux, a setup from before the rename still shows the camera as **Phone Camera**. Click **Unload** and then **Load** under Virtual camera & phone in Settings to get **Telescope**, then pick it again in apps like Discord.

**Linux: Chrome doesn't list Telescope**
Chrome only lists cameras that do nothing but capture, and on Linux the Telescope camera is also where Telescope writes its picture, so Chrome skips it. For calls in the browser, use Firefox, or use the call's desktop app, such as Discord or Zoom.

**The virtual camera won't open on Linux**
The `v4l2loopback` package isn't installed or loaded. Install it (`v4l2loopback-dkms` on Debian, Ubuntu and Arch, `v4l2loopback` on Fedora and Nobara through RPM Fusion, see [Manual setup](setup.md#linux)) and click **Start streaming** again. **Virtual camera & phone** in Settings can load it by hand.

**Linux: `/dev/video11` is gone after a reboot**
The module isn't loaded at boot. Starting a stream loads it again. Leave **Also switch it on at every startup** ticked in the dialog that comes up when a stream starts, or switch on **Load when the computer starts** under Virtual camera & phone in Settings.

**Linux: "Virtual camera is set up differently"**
Another app (OBS's own virtual camera, for example) already loaded the module with different settings. Close that app, click **Unload** under Virtual camera & phone in Settings (or run `sudo modprobe -r v4l2loopback`) and click **Start streaming** again.

**Canvas change fails with "module in use" on Linux**
OBS or another app still holds the device. Telescope keeps the old size and starts the stream again at it, with a message that the change was blocked. Close every app using the virtual camera and apply the size again.

**The virtual camera won't open on Windows**
The driver isn't registered. Open **Virtual camera & phone** in Settings and reinstall it.

**Windows 11: an app doesn't list Telescope**
Open **Virtual camera & phone** in Settings. If the Driver row shows **Some apps can't see the camera** you still have the older driver, which apps that only use Media Foundation (the Windows Camera app, for one) don't see. Click **Update**. An app that was already open can need restarting before it lists the new camera.

**Windows 11: the camera shows Telescope's lens while Telescope is closed**
Telescope isn't running, or it's still starting. The camera is in every app's list even with Telescope closed and shows the default wait screen without that line until Telescope sends it something. Open Telescope and your wait screen or your phone appears in its place.

**Keep using the older driver on Windows 11**
Set `TELESCOPE_VCAM` to `unitycapture` before starting Telescope (in System Properties under Environment Variables) and click **Install driver**. Telescope then streams to UnityCapture like on Windows 10. Apps list both cameras while the Windows 11 one is still installed. The one Telescope uses is the one without "(Windows Virtual Camera)". Remove the variable to undo this.

**On Windows the camera is called "Unity Video Capture"**
An older Telescope installed the driver before it had its own name. Click **Rename** under **Virtual camera & phone** in Settings.

**Windows driver install shows `Program Files\Telescope has links in it or other users can change it`**
Something else made that folder or changed who can edit it and Telescope won't install into a folder it can't trust. Delete `C:\Program Files\Telescope` and click **Install driver** again.

**Windows driver install reports that it `hit an error while running as admin`**
Click **Retry**. If it fails again open an issue with your **Copy diagnostics** report.

## Pairing and connecting

**QR pairing fails ("Couldn't reach your computer.")**
The phone and computer aren't on the same network, or the computer's firewall blocks port 7479. The dialog on the phone lists every address it tried and how each failed. Make sure both are on the same Wi-Fi and the Add device dialog is still open (the pairing server only runs while it is). Or plug the phone in and pair over USB.

**"Update Telescope on <computer> to keep using this phone with it" or "Update the Telescope app on <phone> to connect"**
The two apps are on different versions from before Telescope moved its ports (they were 8765 to 8768 and are now 7479 to 7482). Update the one the message names, or both. Telescope on the computer and on the phone has to be the same version.

**Which ports does the firewall need?**
Allow 7479 on the computer while pairing. Browser camera uses 7481 and On Air 7482. Presence needs UDP 7483. The computer also has to reach the phone on 7480 and 8080. These are the defaults. Browser camera picks another free port if its own is taken and shows the port it picked in its link. Pairing does the same, but the port is only in the QR code. With 7479 taken, allow Telescope itself rather than the port. Windows shows a prompt about Telescope itself the first time and that rule covers every port. If you added rules yourself after an earlier version, replace 8765 to 8768 with 7479 to 7482. For a few releases the update messages between an older and a newer version go over the old ports (8766 on the phone and 8765 on the computer while pairing). With only the new ports open, an older and a newer version still don't connect, and neither can name the one to update.

**QR pairing fails on guest or public Wi-Fi**
Those networks usually block devices from talking to each other. There is no way around that. Use USB pairing or a network you control.

**QR pairing fails with a VPN on**
The VPN is blocking local network traffic. Turn on its "allow local network access" option, pause it while pairing or use USB. Streaming over Wi-Fi needs the same. With Tailscale you don't need to pause it, see [Things to try](unusual-setups.md#use-a-phone-from-far-away).

**Plugged in but USB pairing doesn't happen**
Click the **Pair** button with your phone's name on it under USB in the Add device dialog first. A plugged-in phone isn't paired until you do. The USB line names which of these is in the way if it still doesn't (adb isn't installed, USB debugging isn't allowed on the phone or the app isn't on screen). Install `adb` (Linux) or click **Get adb from Google** in that line (Windows), [enable Developer options](https://developer.android.com/studio/debug/dev-options#enable) on the phone and turn on [USB debugging](https://developer.android.com/studio/debug/dev-options#Enable-debugging) in them, accept the prompt it shows and leave Telescope open there.

**"Google refused the download" (Windows)**
Google blocks these downloads in some countries. Get Google's platform-tools for Windows another way, then click **Pick an adb you already have** in Add device (or **Use my own adb…** under Virtual camera & phone in Settings) and choose `adb.exe` or the zip. Wi-Fi works without adb.

**It streams over Wi-Fi although the phone is plugged in**
Hover the status line of the Connection panel and its tooltip names the reason (Telescope isn't open on the phone, USB debugging isn't allowed yet or the cable has a different phone on it). Fix that and the next connect uses USB. To never use Wi-Fi set **Connect via** to USB only.

**"Can't reach the phone"**
Telescope isn't open on the phone, or the phone is on another network and not plugged in. Open Telescope on the phone and leave it on screen.

**"Can't reach the phone" after reinstalling the phone app**
A reinstall (or Clear data) gives the phone a new identity. Click **Add device** and pair again. A banner asks whether it's the same phone and **Yes** moves the old settings and presets over to it and takes the old entry out of the list. If you answer **No**, or remove the old entry yourself first, its settings go with it.

**"Can't reach <phone>" banner while streaming**
The phone app was closed or killed, or the phone left the network. Open Telescope on the phone. If the stream is still running the picture returns without you doing anything. Otherwise click **Start streaming**.

**The stream stops after a while with the phone's screen off**
Android's power saving closed Telescope. Tap **Allow** next to Battery in the phone app's Get set up card if it's still there (that turns battery optimisation off for Telescope). Some phones close apps anyway and on those it helps to leave Telescope on screen and tap **Black screen** next to Preview instead of turning the screen off.

**"Pairing required"**
The phone removed this computer. It remains in the list here with its settings. Click **Add device** and pair again. Settings only go when you remove the phone from the list on the computer.

**"Another app on the phone is using the camera"**
Android gives the camera to the app on screen (the Camera app, a video call). Close or switch away from it and the stream picks back up.

**"The phone is streaming to <name>"**
Another computer paired with the phone started its camera first. Stop it there or on the phone and click **Start streaming** again.

**Stream stops a few seconds after Start with a battery or temperature banner**
**Stop the stream** is on in the Phone card and the phone is past its threshold. Click **Start anyway** or change the threshold.

**Paired devices need another pairing before Wi-Fi works**

Open Telescope on both devices or connect over USB once so existing pairings can learn the computer's addresses. Presence uses UDP 7483 in both directions; HTTPS fallback still works when UDP is blocked. A network that isolates devices can block both paths.

On Windows allow Telescope itself through the firewall. For a port-specific rule, run PowerShell as administrator and use the actual path of your copy:

```powershell
New-NetFirewallRule -DisplayName "Telescope presence" -Direction Inbound -Program "C:\path\to\TelescopeDesktop.exe" -Protocol UDP -LocalPort 7483 -Action Allow
```

On Linux allow incoming UDP 7483 from the phone's network. For example, with UFW (replace the subnet with yours):

```bash
sudo ufw allow from 192.168.1.0/24 to any port 7483 proto udp
```

With firewalld, add the port to the zone containing the phone's interface:

```bash
sudo firewall-cmd --zone=home --add-port=7483/udp --permanent
sudo firewall-cmd --reload
```

Include the Tailscale interface or subnet too if you use it. The phone must permit UDP 7483 as well. Presence can recover an address only while either remembered addresses or network discovery still reach the other device; USB or pairing again is needed when both fail.

**Windows asks about the firewall again after an update**
The update moved an unzipped copy to the installed one and Windows treats it as a new program. Click Allow on private networks like the first time. The old copy's rule is left behind and has no effect.

## Video and quality

**Lag or dropped frames over Wi-Fi**
Heavy (MJPEG) sends several times the data of Light. Switch Format to Light, use USB, lower JPEG quality or reduce FPS.

**Light freezes for a moment now and then over Wi-Fi**
A busy network loses a packet and TCP delays everything until the resend. Try Dynamic bitrate or USB first. **UDP on Wi-Fi** in Settings on the Streaming tab is experimental and worth a try here, though it can also stutter more than TCP.

**FPS is limited to 30 with 60 picked**
Some phone makers reserve their camera's faster modes for their camera app. The phone lists 60 fps and accepts the request, then gives other apps 30. Telescope can't change that. **Copy diagnostics** while streaming shows it. The "Camera did" line reads 33.4 ms with range [60, 60]. Rates a camera doesn't list at all are greyed out.

**Telescope uses a lot of power or the fans spin up**
Background and Auto framing cost the most. A 720p stream without them added about 0.2 W of processor power on a test laptop and about 1.5 W with both on. In rough order of what helps:
- Switch off the effect you don't need, or turn on **Power-saving effects** under Video in Settings
- Lower the resolution or FPS. Every step down means less work for the phone and the computer
- Use the Light format and turn on **Hardware acceleration** if your graphics card supports it
- On battery, unplug the phone because over USB it runs off the laptop's power. On the test laptop it drew about 2.4 W even while not streaming. Use Wi-Fi, or charge the phone from the wall

Minimising the window or putting it in the tray only saves power when no app is reading the virtual camera. Then Telescope pauses the effects and vision after about 8 seconds, provided the Wait screen plugin is on and it can tell that no app reads the camera (on Linux with v4l2loopback 0.13 or newer and on Windows 11 with Telescope's own camera but not on Windows 10 with UnityCapture). An app that opens the camera resumes them on the next frame.

**Telescope uses a lot of RAM**
Most of it isn't Telescope's own. Measured on a Linux laptop, streaming 1080p at 30 fps:

| What | About |
|---|---|
| Telescope open, not streaming | 240 MB |
| Streaming | 360 MB |
| Streaming with Background and Auto framing | 470 MB |

Where it goes:
- **Libraries, about 130 MB.** Slint draws the window, wgpu runs the picture steps on the graphics card and FFmpeg decodes the video. They're loaded once and don't grow.
- **Frames from the camera, about 100 MB at 1080p.** There are a few copies of each picture on its way from the phone to the virtual camera. Lower resolutions need less.
- **The vision models, about 150 MB.** These are loaded only while Background, Auto framing, Expose for my face or Privacy blur is on. About 10 seconds after you switch them all off most of it is given back and restarting Telescope frees the rest.
- **Telescope itself, about 50 MB.** This is its code, settings and the wait screen.

Your numbers can differ from these because task managers count memory in different ways (Windows Task Manager, for one, leaves out memory other apps could share).

**"No graphics card Telescope can use" in Video settings**
On Linux the decoder is VA-API or NVIDIA's CUDA. On Windows it's Direct3D 11. Without a usable card Light still decodes on the processor.

**Telescope switched graphics card**
The next start shows a banner on Linux and a message box on Windows. The last start closed while Telescope was opening that card, or the card opened but couldn't draw the window. A start that ends in its first second does this, whether the driver crashed or the system closed Telescope when memory ran out. Telescope leaves the card out and uses another one, or the processor if none is left, until 3 starts or a day have passed. Then the card gets another try and is forgotten if it draws the window. If that start closes early too, the wait doubles each time, up to 48 starts or 16 days. A driver update also gives the card another try. To undo it sooner, delete `gpu_skipped.json` from Telescope's settings folder (`%APPDATA%\Telescope` on Windows, `~/.config/telescope` on Linux) and start Telescope again.

**Telescope uses the other graphics card on Windows**
Pick the card yourself under **Graphics card** in Video settings and restart. With **Automatic** Telescope follows the graphics setting for its own app in Windows Settings > System > Display > Graphics. It uses the high performance card, which on a laptop with two is the discrete one, with no setting or with **Let Windows decide**. **Power saving** runs everything on the integrated card, hardware decode included. Try it when a laptop with two cards runs hot or drains the battery. The log shows which setting Telescope found.

**The window or the picture looks wrong on Windows**
Telescope tries Vulkan first on Windows. If the window or the picture looks wrong, or effects use a lot of processor, open Video settings, pick **DirectX 12 first** under **Graphics API** and restart. DirectX 12 lets the graphics card pass the decoded picture to effects without a copy through memory. How much that helps depends on the card and driver. A plain stream can use about the same processor either way. Intel cards ignore the choice. Their window always draws on Vulkan and their pictures are made on DirectX 12. A stream with no effects converts on the processor on Windows. `setx TELESCOPE_PLAIN_ON_CARD 1` makes the graphics card do it instead, which on some cards uses more processor.

## Camera controls

**A control is greyed out or does nothing**
The current lens doesn't support it. Some front cameras and telephoto lenses have no manual exposure or white balance. Use Auto there. Hover **Full manual control** or **Limited manual control** to see what the lens can do.

**Zoom and Pan are greyed out on the Transforms card**
Auto framing is moving them. Switch it off on its card and your own zoom and pan return.

## Browser camera

**The page won't load**
The device isn't on the same network, or a firewall blocks port 7481. Use the address on the card that matches the device's network (the card lists each one if there are several) and allow port 7481 in the firewall. If you clicked Cancel on the Windows firewall prompt, allow Telescope in Windows Security under Firewall & network protection, Allow an app through firewall.

**The page shows that it can't reach the computer or that the code was replaced**
The code changed (**New link**, or Telescope restarted). Scan the code on the card again.

**The page stays on "Connecting the video…" or keeps reconnecting**
The video goes over WebRTC on UDP ports picked each time, which a strict firewall can block even with port 7481 open. On Windows allow Telescope itself in the firewall and not only the port. On Linux allow incoming UDP from your local network, or turn the firewall off for a moment to confirm that's it.

**It stops when the phone locks or you switch apps**
Phones pause the camera for pages in the background. Leave the page open with the screen on. Returning to it starts it again.

**Firefox jumps around in size or frame rate**
Firefox can still drop frames or change size a bit when the phone or Wi-Fi struggles but it now mostly holds the resolution you picked at about the frame rate you asked for. The card shows what Firefox really sends. A smaller resolution or a lower frame rate usually helps when the frame rate gets too low.

## Webcams

**A webcam isn't in the device picker**
Check that **Webcams** is on in Plugins and that the camera works in another app. On Linux your user needs access to `/dev/video*` (usually the `video` group). A camera that only offers H.264 or another format OpenCV can't read is left out. Virtual cameras, like OBS Virtual Camera, are left out by design. On Windows a camera only Media Foundation apps can open (a phone through Phone Link, for one) isn't listed either.

**"Can't open" and the webcam's name**
On Linux only one app can stream from a webcam at a time. Close the camera in the app named in the banner (a browser tab with a call or a camera test page counts too). Telescope retries and starts streaming the moment it's free. On Windows the banner can't name the app. Check what has the camera open.

**The picture is choppy or the frame rate is low**
Try a lower resolution or frame rate on the Webcam card. In dim light many webcams slow down to get a brighter picture. More light helps and on Linux `v4l2-ctl -d /dev/video0 -c exposure_dynamic_framerate=0` (with your camera's device) gives the full rate at the cost of a darker picture (**Slow down in the dark** on the Webcam card does the same). A camera behind a USB hub shared with other busy devices can also run out of bandwidth. Plugging it straight into the computer helps then.

**A setting on the Webcam card does nothing**
Some webcams list a setting they ignore, or only take it in some modes. Pan and tilt usually only move once you've zoomed in and exposure time only works with Exposure on Manual. **Reset** restores the camera's defaults.

**A camera in webcam mode or behind a capture card shows a black picture**
Check its HDMI output is clean (no menus on top) and set to a resolution the capture card takes, then pick that resolution on the card.

## Shortcuts

**A shortcut does nothing**
Its control is greyed out or hidden right now (no phone, Auto mode for a manual slider, or the mic is off for Mute). `telescope --action` prints why.

**"Another app already uses this key" (Windows)**
Windows gives the key to the other program every time, even with Telescope in front. Pick another key in the **Keyboard shortcuts** tab of Settings.

**Shortcuts only work inside Telescope (Linux)**
Your desktop has no global shortcuts portal (only Plasma 5.27+, GNOME 48+ and Hyprland do) or you declined the shortcut when the desktop asked to confirm it. On Plasma, **Open** next to **Desktop shortcut settings** lets you set the key. Elsewhere bind the shortcut's **Copy command line** in your desktop's keyboard settings.

## Plugins and extra downloads

**A card or menu entry is gone**
That part of Telescope is switched off in **Plugins**. Open Settings and go to the Plugins tab (it reads **Plugins (N off)** when something is off), switch it back on or click **Reset to defaults** (every built-in plugin goes on and the optional ones go off), then click **Restart now**.

**A plugin that finds people asks for a one-time download**
The vision download hasn't happened yet or was removed. A new Telescope version can need a newer one too. Click **Download** on the card. If it fails check that GitHub is reachable from this computer, or download the vision pack zip from the release page and install it with **Install** on the Plugins tab ([Plugins](features/plugins.md#installing-a-plugin-someone-made)).

**A plugin that finds people reports that MediaPipe didn't load**
The download is there but MediaPipe won't start here (it only works on 64-bit Intel and AMD). On Linux it also needs the EGL and OpenGL ES libraries, which graphics drivers normally bring. If **Copy diagnostics** contains `libEGL.so.1 missing` or `libGLESv2.so.2 missing`, install `libegl1` and `libgles2` (Debian and Ubuntu) or `libglvnd-egl` and `libglvnd-gles` (Fedora) and restart Telescope. Otherwise open an issue with your **Copy diagnostics** report.

## Updates

**"Didn't start after updating and is held back"**
The new version crashed before its window came up and the old one was restored. For an unzipped copy that was moving to the installed one, it means the installed copy didn't open. Click **Try again**. If it happens again open an issue with your **Copy diagnostics** report.
