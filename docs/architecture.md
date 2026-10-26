# Architecture

How Telescope fits together, for contributors. The wire format is in [protocol.md](protocol.md), the desktop's plugin rules in [desktop/ARCHITECTURE.md](../desktop/ARCHITECTURE.md) and every desktop module in [desktop/MODULES.md](../desktop/MODULES.md).

## Overview

```
Android phone  (Telescope app, HTTPS on port 8080, session responder on 8766)
      |
      |  USB:   adb forward tcp:0 tcp:8080 (adb picks the local port)
      |  Wi-Fi: HTTPS, certificate pinned at pairing, address from mDNS or stored
      v
Desktop  (Python, PyQt6)
      |
      |-- StreamWorker       reads MJPEG, H.264 or H.264 over UDP,
      |                      runs every frame through the plugin pipeline,
      |                      letterboxes to the canvas, writes to the virtual camera
      +-- plugins            one per card (connection, camera control, transforms, ...)
```

- **Linux:** two `v4l2loopback` devices (`/dev/video10` and `/dev/video11`). Telescope writes to `video11` and leaves `video10` free for other software like OBS
- **Windows 11:** Telescope's own camera, a Media Foundation media source (`desktop/virtualcamera/`) that every app can see. See [The Windows 11 camera](#the-windows-11-camera) below
- **Windows 10:** a standalone DirectShow filter (UnityCapture), no OBS needed. Windows 11 falls back to it when its own camera can't be installed

## Repository layout

| Folder | What's in it |
|---|---|
| `android/` | The phone app (Kotlin, Camera2). Gradle project, sources in `app/src/main/kotlin/com/telescope/` |
| `desktop/` | The desktop app (Python, PyQt6). `main.py` is the entry point, `telescope/` the package, `tests/` the pytest suite, `scripts/` dev and CI tools |
| `docs/` | Everything past the README's Quick Start. `docs/README.md` is the index |
| `installer/` | The Windows setup (Inno Setup) |
| `site/` | The landing page, published to GitHub Pages |
| `.github/` | Workflows, issue and PR templates, release notes |
| `VERSION` | The version both apps ship as |

## Where to change what

Desktop paths are under `desktop/telescope/` and phone files under `android/app/src/main/`. [desktop/MODULES.md](../desktop/MODULES.md) has the full story for any desktop file.

| To change | Start in |
|---|---|
| A desktop card or its settings | Its plugin in `plugins/`, one file per card. Read the rules in [desktop/ARCHITECTURE.md](../desktop/ARCHITECTURE.md) first |
| The main window or the Settings window | `app.py`, `widgets/settings_window.py` |
| Colours and styling | `theme.py`. Shared controls are in `widgets/common.py` |
| Pairing and reaching the phone | `plugins/connection.py`, `pairing.py`, `phones.py` (`RouteResolver`) |
| Receiving and decoding video | `stream.py`, `mjpeg_reader.py`, `h264_reader.py`, `udp_reader.py` |
| The virtual camera | `vcam.py`, plus `platform/linux.py` or `platform/windows.py`. The Windows 11 camera's DLL is `desktop/virtualcamera/` |
| The microphone | `audio.py`, `platform/virtual_mic.py`, `plugins/microphone.py` |
| Saved settings | `config.py` |
| Desktop wording | The `tr()` call in the code, then `python scripts/i18n.py extract`. See [translating.md](translating.md) |
| Messages between the apps | `kotlin/com/telescope/Protocol.kt` and `phone_client.py`, written up in [protocol.md](protocol.md) |
| The phone's camera and encoding | `CameraSessionController.kt`, `CameraStreamService.kt`, `H264Encoder.kt`, `MjpegServer.kt` |
| Phone pairing and TLS | `Pairing.kt`, `PairedComputers.kt`, `TlsIdentity.kt` |
| The phone's screen and Get set up | `MainActivity.kt`, `SetupSteps.kt`, `res/layout/activity_main.xml` |
| Phone wording | `res/values/strings.xml` |
| The Windows installer | `installer/` |

Desktop tests are in `desktop/tests/` and phone unit tests in `android/app/src/test/`.

## The Windows 11 camera

Windows 11 added `MFCreateVirtualCamera`, which registers a camera backed by a media source DLL. Windows' Frame Server service loads that DLL and shares the camera with every app, Media Foundation and DirectShow ones alike, which UnityCapture's DirectShow filter can't do. Telescope's DLL is in `desktop/virtualcamera/` (C++, built with CMake and MSVC, its structure following Microsoft's VirtualCamera sample).

- **Registering:** `regsvr32 /n /i:<count> TelescopeCamera.dll` as admin registers one COM class per camera slot (`MF_CLSIDS` in `platform/windows.py`) and calls `MFCreateVirtualCamera` for each. The cameras last across restarts and are there for every account. If Windows refuses one (camera access blocked, Frame Server switched off) the DLL removes them all again and Telescope sets up UnityCapture instead. `regsvr32 /u` removes them. `platform/windows.install_camera()` does it from the first-run checklist or Settings with the same Program Files checks as UnityCapture. The uninstaller unregisters it
- **Frames:** Frame Server runs as Local Service in session 0 where the DLL can't see anything in the signed-in user's session. When an app starts reading a camera the DLL creates the shared memory `Global\TelescopeCamera<slot>` (a normal account can open a Global name but not create one) and writes the size and frame rate the app picked into its header. Telescope's `vcam.MFCamera` opens it, fits each frame to that size, converts it to NV12 and writes it after the header. A frame counter that's odd while a write is under way lets the DLL skip a half-written frame. The header layout is in `virtualcamera/shared_frame.h`
- **In use:** that shared memory only exists while an app reads the camera and the DLL bumps a heartbeat for every frame it hands over. `vcam.CameraWatch` reads both to feed `camera_watched` on the bus, which Automatic streaming and the wait screen use
- **Without Telescope:** the camera stays listed and shows a plain dark picture (the app's background colour) until frames arrive. It shows it again once no frame has come for 3 seconds
- **Sizes:** the DLL offers NV12 at 1080p (30 and 60 fps), 720p (30 and 60), 4K, 1440p, 540p, 480p, 360p and portrait 1080x1920 and 720x1280. Windows adds YUY2 versions of those for DirectShow apps

## The phone app

On first launch the top card is **Get set up**: camera access, notifications and the battery exemption, each with its reason and an Allow button (plus Microphone once a desktop asks for it). The app asks for nothing on its own.

### Servers

A foreground service (type `camera`, plus `microphone` once allowed) owns a Camera2 session and an HTTPS server on port 8080. Everything needs the bearer token issued at pairing. The endpoints (`/v1/video`, `/v1/video.h264`, `/v1/video.udp`, `/v1/audio`, `/v1/state` and `/v1/control`) are in [protocol.md](protocol.md).

A second responder, `SessionServer` on port 8766, runs independently so the desktop can reach the phone while it is idle. It answers `/v1/hello`, `/v1/ping`, `/v1/session` (start or stop the camera) and `/v1/unpair`. That's why the desktop's Start button is the only one anyone has to press, and why a stream can be stopped and restarted from the desktop after the phone's screen went dark.

`SessionServer` stays up while any of four owners holds it (refcounted in `SessionEndpoint`): `MainActivity` is on screen, `CameraStreamService` is running, `WaitingService` is running because **Wait for my computer** is on, or a stream stopped less than 6 seconds ago. With **Wait for my computer** off (the default) a backgrounded idle app can't be told to open the camera, which is also what keeps the start legal since Android 12 and newer blocks starting a camera foreground service from the background. `WaitingService` is the opt-in exception. It holds the port, opens neither camera nor mic, and starts `CameraStreamService` as a plain service running under it.

`CameraStreamService` stops itself after 60 seconds with no authorised request and no viewer, so a crashed desktop doesn't leave the camera running. The desktop polls `/v1/state` every 15 seconds while streaming. The phone app has no pickers of its own, only Stop, Local only and the preview.

The app also enumerates the **physical sub-cameras** of logical multi-camera groups (`CameraCharacteristics.physicalCameraIds`, API 28+), since many phones hide their wide, main and telephoto sensors behind one logical back camera.

### Pairing and TLS

- Both servers speak TLS with a self-signed P-256 certificate made on first run and kept in no-backup storage, so a restored or reinstalled phone gets a new one and pairs again. Each server lets at most 4 connections per address (32 in total) sit sending a request, and a request has 5 seconds to arrive
- Each paired computer has its own token. Pairing again from the same computer replaces only that token, and pairing another computer doesn't disturb a running stream
- **QR:** the code carries a list of desktop address candidates ([protocol](protocol.md#qr-pairing-payload)). The phone tries LAN candidates first over its actual Wi-Fi network (`Network.openConnection()`, so a VPN owning the default route doesn't swallow them), then everything again over the default network. Attempts are capped at 2 seconds each and 12 in total, and if nothing answers a dialog lists every address and how it failed. The pairing POST is plain HTTP (`usesCleartextTraffic`) but nothing in it is secret. The logic that doesn't need Android is in `Pairing.kt` and unit tested
- **USB:** no scan. While Add phone is open the desktop pushes the same payload with `adb shell am broadcast` to an intent gated on the `DUMP` permission, which `adb shell` holds and ordinary apps can't get. It resends every few seconds since the broadcast only lands while the app is on screen
- While the session port is up and **Local only** is off, the app announces `_telescope._tcp` over mDNS with its phone id so the desktop finds its current address. The announcement proves nothing since every connection still authenticates

### Copy diagnostics

Copies the app version, device info, current stream state, recent transitions and errors, what each camera supports and the last 3 streams' reports (kept in a small file). It never includes the pairing token, a URL or raw config.

### Permissions

| Permission | Reason |
|---|---|
| `CAMERA` | Open the Camera2 device |
| `FOREGROUND_SERVICE`, `FOREGROUND_SERVICE_CAMERA` | The streaming service (camera type is required on Android 14+), including **Wait for my computer** |
| `RECORD_AUDIO`, `FOREGROUND_SERVICE_MICROPHONE` | The desktop's microphone option, asked for only after a desktop wants it |
| `INTERNET` | The HTTP servers |
| `WAKE_LOCK` | Keep the CPU active with the screen off |
| `POST_NOTIFICATIONS` | The persistent streaming notification |
| `ACCESS_NETWORK_STATE` | Show the device IP in the UI |
| `REQUEST_IGNORE_BATTERY_OPTIMIZATIONS` | The Battery step of Get set up |
| `REQUEST_INSTALL_PACKAGES` | Installing its own updates |

## The desktop app

### Stack

| Part | Library |
|---|---|
| UI | PyQt6 |
| MJPEG decode | opencv-python-headless via `mjpeg_reader.py`, not `cv2.VideoCapture` (it can't send the bearer token) |
| H.264 decode | PyAV (bundles FFmpeg) via `h264_reader.py`. Without it only MJPEG is offered |
| Virtual camera | pyvirtualcam |
| Frames | numpy |
| QR code | qrcode, drawn with QPainter |
| Finding phones | zeroconf (mDNS), ifaddr (network adapters) |
| Virtual mic | `pactl` on Linux, sounddevice into VB-Cable on Windows |

### How it works

- **Plugins.** The window is a thin host. Everything with a card is a `TelescopePlugin` and plugins never call each other, they use the `EventBus` and the host. The rules are in [desktop/ARCHITECTURE.md](../desktop/ARCHITECTURE.md)
- **Layout.** A plugin names a `panel_region` (`left`, `right` or `center`) and the window decides where it lands. Wide windows get three columns, below about 1300 px the rails merge and below about 900 px everything stacks
- **Frame pipeline.** Each frame (BGR) runs through every plugin's `process_frame` on a decoder thread, then `_fit_frame()` letterboxes it to the canvas. A stream the panels aren't showing uses each plugin's `frame_step()` instead, which is the same work frozen at the settings it had when the panels left
- **Several streams.** Each streaming source has its own `StreamSession` and worker, feeding one of up to four virtual cameras. One is focused (the panels show it). `Sessions` holds the bookkeeping and `StreamFlow` does the starting, stopping and focus changes
- **Canvas.** The virtual camera size is independent of the phone's resolution and set at stream start. Changing it restarts the stream (and reloads v4l2loopback on Linux). FPS changes recreate the pyvirtualcam context but keep the phone connection up. Resolution changes need no restart
- **Reaching the phone.** `RouteResolver` decides on every connect and status check, from scratch each time: USB first (it asks `/v1/hello` which phone is on the cable), then Wi-Fi addresses probed in parallel. A failed route becomes the reason shown on the Connection card
- **Reconnecting.** A dropped stream retries every second and re-resolves the route every 3 seconds, so a pulled cable falls back to Wi-Fi. The virtual camera stays open meanwhile and every setting is resent after reconnecting
- **Control client.** One background thread posts commands to `/v1/control` in order. Queued requests for the same action collapse to the latest value, except camera switches. Failed requests retry for up to 15 seconds
- **Config.** One versioned JSON file with a debounced save. Camera, output, transform and monitoring settings are stored per phone id, connection settings and the canvas are global. A config from the previous major version keeps its global settings and drops pairings, and anything unreadable is backed up as `telescope_config.json.invalid-<timestamp>`
- **Linux root commands.** Every v4l2loopback operation is one `pkexec sh -c "..."` call so there's one password prompt. Without pkexec or an agent it tries `sudo -n`, and otherwise hands back a command to paste. `sudo` never runs where it could wait for a password
- **Single instance.** A local lock (a Unix socket on Linux, port 47823 on Windows). A second launch raises the first window
- **Bitrate.** Auto is about 8 Mbps at 1080p30 scaled by size and fps and capped at 30 Mbps. Dynamic goes up to about 2.5 times that and follows the link. An encoder that fails mid-stream falls back to Heavy
- **Phone address discovery.** `ip_utils.get_pairing_addresses()` lists the machine's real network adapters instead of asking the routing table, since under a VPN the default route belongs to the VPN. Containers and VM adapters are dropped, the rest are ranked LAN first then Tailscale, and the QR code carries at most 8
