# Telescope

Stream your Android phone's camera - including telephoto and wide-angle lenses - to a virtual webcam on Linux or Windows. Camera controls (ISO, shutter, white balance, lens selection) are exposed over a local HTTP API so the desktop app can drive them live.

---

## Quick Start

You'll need an Android phone and a PC running Linux or Windows.

### 1. 🖥️ Get the desktop app

**🪟 Windows**

Download `Telescope-windows.zip` from the [releases page](../../releases), extract it, and run `TelescopeDesktop.exe`.

**🐧 Linux**

Download `Telescope-linux.tar.gz` from the [releases page](../../releases), extract it, and run `./start.sh`. You'll also need a couple of things from your package manager:

- **`v4l2loopback`** - what the virtual camera runs on. Telescope switches it on when you stream, but can't install it.
  - Debian/Ubuntu: `sudo apt install v4l2loopback-dkms`
  - Fedora/Nobara: `sudo dnf install v4l2loopback`

  <details>
  <summary>💡 Fedora says it can't find that package?</summary>

  You need [RPM Fusion](https://rpmfusion.org/) enabled first (most Fedora installs don't have it by default - Nobara already does):
  ```bash
  sudo dnf install https://mirrors.rpmfusion.org/free/fedora/rpmfusion-free-release-$(rpm -E %fedora).noarch.rpm
  ```
  Then run the `dnf install v4l2loopback` command above again.

  </details>

- 💡 **`adb`** *(optional)* - only for pairing or installing the phone app over USB. Wi-Fi works without it.
  - Debian/Ubuntu: `sudo apt install adb`
  - Fedora/Nobara: `sudo dnf install android-tools`
  - Arch: `sudo pacman -S android-tools`

### 2. ✅ Follow the checklist

On first launch, the middle of the window is a checklist:

1. **Virtual camera** - on Windows, click **Install driver**. On Linux it checks for the package above and names it if it's missing.
2. **Phone app** - scan the code with your phone's camera to download `Telescope.apk`, then open it to install. (Your phone asks to allow "install from this source" the first time.)
3. **Add your phone** - open Telescope on the phone and click **Add phone**. Then tap **Scan pairing code** on the phone, or plug it in over USB and allow USB debugging when the phone asks.
4. **Start streaming** - click **Start Streaming** in the top right corner. The phone's camera starts by itself.

After the first stream, the checklist is replaced by the video.

### 3. ▶️ Use it as a webcam

In OBS (or anywhere else), pick **Phone Camera** (Linux) or **Telescope** (Windows) as your webcam. Installed the Windows driver with an older Telescope? It's still called **Unity Video Capture** until you click **Rename** in Advanced.

Telescope uses USB whenever the phone is plugged in and answering, and Wi-Fi otherwise. The Connection panel shows which one it's using, and if a cable is plugged in but not used, it says why. **Connect via** forces one or the other.

> [!NOTE]
> Use only on a trusted network, or enable **Local only - USB** in the Android app. See **Privacy** under Features for the full security model.

Everything past this point is optional - detailed feature reference, how it works internally, and manual/advanced setup. Most people can stop here.

---

<details>
<summary>🎛️ <b>Features</b></summary>

## Features

**Camera control**
- Lens picker: switches between wide, main, and telephoto sensors (physical sub-cameras, not digital zoom)
- Manual ISO and shutter speed with log-scale sliders and direct numeric entry; range updates per-lens
- Exposure compensation slider (range and step size reported per-lens, typically ±8 EV in 1/6-EV steps)
- Manual white balance: linear Kelvin slider (2000-10000 K) plus a green-magenta tint slider - *partially working: applies inconsistently depending on device/lens*
- Manual focus: distance slider (diopters), range reported per-lens; greyed out on lenses that don't support it
- Point focus: click the preview (or pop-out) to focus there; exposure meters on that spot too while it's automatic. The click goes back through flip, rotation and zoom to the right spot on the sensor. **Auto** returns to continuous autofocus
- OIS toggle
- Noise reduction and sharpening (edge mode): Off / Fast / High Quality
- Black level lock toggle
- Torch/flash toggle, on lenses that report a flash unit
- Controls are greyed out per-lens if the camera hardware reports it doesn't support them

**Stream transforms** (applied on the desktop, no phone restart needed)
- Horizontal and vertical flip
- Rotation: 90 CW, 180, 90 CCW
- Software zoom 1-5x with pan X/Y sliders (center crop + resize)

**Presets** (the Presets button in the header)
- Save the camera, output and transform settings under a name and switch back with one click, lens included
- Kept per phone

**Canvas size control** (in Advanced, from the settings menu)
- Set the virtual camera canvas independently of the phone feed resolution
- Presets: 720p/1080p/4K in 16:9 landscape and portrait, XGA and UXGA in 4:3, or fully custom
- On Linux: reloads v4l2loopback in a single elevated prompt (close OBS first); stream restarts automatically
- On Windows: stops and restarts the stream with the new canvas size

**Resolution and FPS**
- Resolution dropdown is populated from the current lens's actual supported capture sizes (read from the phone), not a fixed list - picking one sends a live `resolution` control to the phone instead of resizing after decode
- The readout goes amber while a resolution change is in flight and clears once the stream confirms the new size, or turns red if it never does
- One FPS spinner (5-60) drives both the phone's capture rate and the virtual camera's playback rate - there's no separate "phone" and "playback" rate to keep in sync

**Bandwidth controls**
- Format: MJPEG (default) or H.264. H.264 comes from the phone's hardware encoder and needs a fraction of MJPEG's bandwidth at the same quality. It's offered once the phone reports an encoder, and switching reconnects the stream. If the encoder fails, the stream goes back to MJPEG and says so
- MJPEG: JPEG quality slider (1-100%), applied on the phone without restarting the stream
- H.264: bitrate slider, Auto (about 8 Mbps for 1080p30, scaled by size and fps) or 1-30 Mbps, applied live

**Monitoring**
- FPS and throughput (Mbps) readouts in the footer while streaming; throughput turns amber if the real decode rate falls behind the target for a sustained stretch
- A dropped stream shows an animated "Stream dropped - reconnecting..." status instead of a static line
- Battery level and phone temperature polled every 15 seconds, shown in the Monitoring panel with color coding
- Configurable battery alert threshold (default 20%) - fires a tray/desktop notification when discharging below it
- Configurable temperature alert threshold (default 45 C) - fires a notification when exceeded

**Phones, pairing and connection**
- Pairing is one dialog, **Add phone**: scan its code with the phone, or plug the phone in over USB and it pairs by itself (it tells you if USB debugging still needs allowing on the phone). Telescope stores each phone by the id it reports, so a new name or a new address doesn't matter
- Several phones per computer and several computers per phone: each computer gets its own token on the phone, and removing one (on either side) leaves the others paired. Removing a phone on the desktop also unpairs it on the phone when it's reachable
- Telescope decides how to reach the phone on every connect: USB when this phone answers over a cable, Wi-Fi otherwise. A plugged-in device only counts once it reports the paired phone's id, so a different phone on the cable isn't mistaken for yours. **Connect via** forces USB or Wi-Fi
- When a cable is plugged in but not used, the Connection panel says why: the app isn't open on the phone, USB debugging isn't allowed yet, or it's a different phone. Plugging in while streaming over Wi-Fi offers **Switch to USB**
- Over Wi-Fi, Telescope finds the phone by its LAN announcement (mDNS, `_telescope._tcp`) before trying any stored address, so a router handing it a new IP doesn't break anything
- Camera, stream-output, transform, and monitoring settings (resolution, FPS, flip, rotation, exposure, zoom, quality, alert thresholds, etc.) are saved per phone to `telescope_config.json`; connection settings and the virtual-camera canvas are global
- Config from the previous version keeps its global settings; pairings and per-phone settings are dropped, since phones are now stored differently. Telescope backs up anything older or malformed next to the real file and starts from defaults. Each section is validated on its own, so one bad section resets without discarding the rest

**Privacy**
- Network authentication is meant to stop accidental or opportunistic access from other devices on the LAN, not an active attacker or network observer: traffic is unencrypted, and possession/interception of the bearer token is enough for access
- For an actual security boundary, use **Local only - USB**, which keeps the camera service off the network entirely
- Local only mode: binds the server to `127.0.0.1` so the stream is unreachable from the network; only USB works in this mode
- Toggle in the Android app restarts the stream automatically to apply the change
- Changing **Connect via** on the desktop reconnects a running stream over the new route
- Local only also stops the phone announcing itself on the LAN

**Updates**
- Both apps check for a newer build once a day, on the channel you pick: **Stable** (tagged releases) or **Nightly** (every change to `master`). Nightly builds follow nightly by default, everything else follows stable
- Desktop: an **Update** button appears in the header. The update downloads, checks its SHA-256, replaces the app and restarts it. Not while streaming. A source checkout or a folder the app can't write to only links to the release
- Phone: the update card at the top downloads the APK, checks its checksum and signing key, and hands it to Android's installer. The first time, Android asks to allow installs from Telescope
- When the phone app is older than the desktop, the Connection panel says so, and offers **Update over USB** when the phone is plugged in and the desktop bundle carries a newer APK

**System integration**
- **Start streaming when the phone is ready** (settings menu): starts by itself each time the phone becomes reachable. After you press Stop it stays stopped until the phone goes away and comes back. It never asks for a password on its own; if the Linux virtual camera is off, a banner offers to switch it on
- **Open Telescope when I sign in** (settings menu): an autostart entry (`~/.config/autostart/telescope.desktop` on Linux, the per-user Run key on Windows) that starts it in the tray with `--minimized`
- Minimizes to system tray on close while streaming, or while waiting to start by itself; otherwise quits
- Right-click the tray icon to quit, or click it to show/hide the window
- Launching a second instance brings the existing window to the front
- When Start can't go ahead, a banner at the top of the window says why, with the button that fixes it (Try again, Add phone, Switch to Automatic, Copy command). It clears on the next working stream
- Battery/temperature notifications use `notify-send` on Linux (if available) or the system tray on Windows

</details>

<details>
<summary>💡 <b>Why</b></summary>

## Why

Most Android camera streaming solutions either lock you to a specific app ecosystem, use ADB screen mirroring which blocks the back camera on some devices, or route through OBS to create the virtual camera - which is a problem if you need OBS free for its own output. Telescope runs as a self-contained foreground service that serves MJPEG directly and exposes camera controls as a simple REST API, leaving OBS (or any other capture tool) completely unencumbered.

</details>

<details>
<summary>🏗️ <b>Architecture</b></summary>

## Architecture

```
Android device  (Telescope app, port 8080)
      |
      |  USB: adb forward tcp:0 tcp:8080 (adb picks the local port)
      |  Wi-Fi: direct HTTP, address found via mDNS or stored
      v
desktop/main.py  (Python, PyQt6)
      |
      |-- telescope/stream.py       StreamWorker (QThread)
      |     reads authenticated MJPEG via telescope/mjpeg_reader.py
      |     runs frames through plugin pipeline
      |     _fit_frame() letterboxes to canvas size
      |     pyvirtualcam -> virtual camera device
      |
      +-- telescope/plugins/        one plugin per UI card
            setup                   Advanced dialog: virtual camera module, canvas, APK over USB
            connection              phones, Add phone dialog (pairing.py, port 8765), route choice
                                    (phones.py, discovery.py), session channel (port 8766): status, start/stop
            onboarding              first-run checklist on the video stage
            camera_control          lens, ISO, shutter, WB, OIS
            stream_output           resolution, FPS, JPEG quality
            transforms              flip, rotation, zoom, pan
            preview                 in-card and pop-out video preview
            monitoring              battery, temperature alerts
```

A second responder on the phone (`SessionServer`, port 8766) runs independently of the streaming server, so the desktop can reach the phone in exactly the state the streaming server doesn't exist in: idle. It answers `GET /v1/hello` (which phone this is, no auth), `GET /v1/ping` (pairing status plus what the phone is currently doing), `POST /v1/session` (start or stop the camera from the desktop) and `POST /v1/unpair` (forget the calling computer).

That second endpoint is why the desktop's Start button is the only one anyone has to press. Hitting Start asks the phone to bring its camera up, waits for it, then connects; hitting Stop takes the phone's camera back down. Starting on the phone still works exactly as before, and the desktop leaves a stream it finds already running alone.

`SessionServer` stays bound while **either** `MainActivity` is on screen **or** `CameraStreamService` is running (see `SessionEndpoint`'s refcount). Those two owners are the safety boundary: a fully backgrounded, non-streaming app cannot be told to open the camera - which is also what keeps the start legal, since Android 12+ blocks starting a `camera`-type foreground service from the background. Covering the streaming case as well is what lets you stop and restart a session from the desktop after the phone's screen has gone dark.

On **Linux**, two `v4l2loopback` devices are created (`/dev/video10` and `/dev/video11`). Telescope writes to `video11`; `video10` is intentionally left free for other software (e.g. OBS Virtual Camera).

On **Windows**, the virtual camera is [UnityCapture](https://github.com/schellingb/UnityCapture) - a standalone DirectShow filter, no OBS required.

</details>

<details>
<summary>🗂️ <b>Repository layout</b></summary>

## Repository layout

```
telescope/
|-- VERSION                    # The version both apps ship as
|-- .github/
|   |-- write_manifest.py        # Writes a release's manifest.json
|   +-- workflows/
|       |-- release.yml          # Builds all three and publishes nightly or a stable release
|       |-- build-apk.yml        # APK (signed with the release key in a release)
|       |-- build-windows.yml    # Windows bundle (EXE + adb + UnityCapture)
|       +-- build-linux.yml      # Linux bundle (source + start.sh)
|
|-- docs/
|   |-- device-compatibility.md  # Manually maintained per-device test matrix
|   +-- release-checklist.md     # Manual pre-release checklist
|
|-- android/                     # Gradle project
|   +-- app/src/main/kotlin/com/telescope/
|       |-- MainActivity.kt      # UI: enumerate cameras, start/stop service, diagnostics, pairing
|       |-- PreviewActivity.kt   # Fullscreen live preview, standalone or attached to a running stream
|       |-- CameraStreamService.kt  # Foreground service: Camera2 + HTTP control
|       |-- CameraSessionController.kt  # Owns the live Camera2 session and capture-request state
|       |-- CameraCatalog.kt     # Enumerates cameras, incl. physical sub-cameras of logical multi-cams
|       |-- StreamStateMachine.kt   # Idle/StartingServer/.../Streaming/Failed state + history
|       |-- Protocol.kt          # kotlinx.serialization models for the v1 API
|       |-- SetupSteps.kt       # Get set up card rules: ask, or send to settings (JVM-tested)
|       |-- Pairing.kt           # QR payload (v3) parsing/validation, attempt ordering, failure text
|       |-- PairedComputers.kt   # Paired computers (one token each), this phone's id and name
|       |-- MjpegServer.kt       # Authenticated HTTP: /v1/video(.h264)  /v1/state  /v1/control
|       |-- H264Encoder.kt       # MediaCodec H.264 from the camera Surface
|       |-- H264Stream.kt        # Per-viewer H.264 queue, bitrate defaults
|       |-- SessionServer.kt     # Out-of-band responder (port 8766): /v1/hello, /v1/ping, /v1/session, /v1/unpair
|       |-- SessionEndpoint.kt   # Refcounted owner of SessionServer + the commands it runs
|       |-- StreamLauncher.kt    # Single place CameraStreamService is started from
|       |-- StreamPrefs.kt       # Last camera/resolution selection, for desktop-initiated starts
|       |-- Updater.kt           # Self-update: manifest check, download, verify, PackageInstaller
|       |-- UpdateLogic.kt       # Manifest parsing and version rules (JVM-tested)
|       |-- LanAnnouncer.kt      # mDNS announcement (_telescope._tcp) so desktops find the phone's address
|       +-- HttpWire.kt          # The HTTP/1.1 subset MjpegServer and SessionServer share
|
+-- desktop/
    |-- main.py                  # Entry point: registers plugins, restores config
    |-- requirements.txt         # Readable ">=" lower bounds
    |-- requirements-dev.txt     # requirements.txt + pytest, used by CI
    |-- constraints.txt          # Exact pinned versions for CI/release installs
    |-- scripts/smoke_check.py   # Packaging smoke checks (see CI section below)
    |-- scripts/write_build_info.py  # Stamps a release build's version into telescope/_build.py
    |-- tests/                   # pytest suite (desktop only; Android has its own JVM unit tests)
    |-- THIRD_PARTY_NOTICES.txt  # Bundled into both release archives
    |-- telescope.spec            # PyInstaller spec for Windows EXE
    |-- start.sh                 # Linux launcher (creates/reuses a Telescope-owned venv)
    |-- start.bat                # Windows source-checkout launcher (auto-installs deps); not in the release zip, the EXE needs neither
    |-- platform-tools/          # Bundled adb for Windows
    |-- unitycapture/            # Bundled UnityCapture DLLs (MIT)
    +-- telescope/
        |-- version.py           # This build's version, build number and channel
        |-- updates.py           # Qt-free update check, download, verify and install
        |-- app.py               # TelescopeWindow: plugin host, responsive shell, stream lifecycle
        |-- theme.py             # Palette tokens + the app stylesheet
        |-- stream.py            # StreamWorker: MJPEG -> pipeline -> pyvirtualcam
        |-- mjpeg_reader.py      # Authenticated multipart-MJPEG reader (replaces cv2.VideoCapture)
        |-- h264_reader.py       # Authenticated H.264 reader (PyAV), same interface
        |-- session.py           # StreamSession: owns worker/client for one connect-to-disconnect lifecycle
        |-- plugin.py            # TelescopePlugin base class, EventBus, HostServices protocol
        |-- config.py            # Versioned JSON config (v3) with per-section validation
        |-- models.py            # Typed contracts: PhoneState, CameraCapabilities (parsed from /v1/state)
        |-- phone_client.py      # Authenticated HTTP client for /v1/state and /v1/control (port 8080)
        |-- session_client.py    # HTTP client for /v1/hello, /v1/ping, /v1/session, /v1/unpair (port 8766)
        |-- pairing.py           # PairingServer: Qt-free pairing HTTP handshake (nonce/token, no PyQt import)
        |-- phones.py            # Phone model, RouteResolver (USB or Wi-Fi, and why), refcounted adb forwards
        |-- discovery.py         # Finds phones on the LAN via mDNS (zeroconf)
        |-- ip_utils.py          # Desktop address discovery for the pairing code, address ranking
        |-- platform/
        |   |-- autostart.py     # Open at sign-in (XDG autostart / HKCU Run)
        |   |-- linux.py         # v4l2loopback helpers (load, unload, reload)
        |   +-- windows.py       # UnityCapture helpers
        |-- plugins/
        |   |-- setup.py
        |   |-- connection.py
        |   |-- camera_control.py
        |   |-- stream_output.py
        |   |-- transforms.py
        |   |-- presets.py       # Saved camera/output/transform settings
        |   |-- preview.py
        |   |-- onboarding.py    # First-run checklist
        |   |-- updates.py       # Update button and dialog
        |   |-- startup.py       # Stream when the phone is ready; open at sign-in
        |   +-- monitoring.py
        +-- widgets/
            |-- banner.py        # In-window problem banners
            |-- common.py        # NoScroll*, LogSliderRow, rows, segmented toggles, icons
            |-- qr.py            # QR code widget
            +-- lens_panel.py    # Lens picker widget
```

</details>

<details>
<summary>📱 <b>Android app</b></summary>

## Android app

### What it does

On first launch the top card is **Get set up**: camera access, notifications and the battery exemption, in that order, each with its reason and an Allow button. The app asks for nothing on its own, and the card goes once all three are allowed. If Android stops showing a permission prompt (denied twice), the button becomes Open settings.

Runs a **foreground service** (declared type `camera`, required on Android 14+) that owns a Camera2 session and an HTTP server on port 8080. Three endpoints, all requiring a bearer token issued during pairing:

- `GET /v1/video` - MJPEG stream (`multipart/x-mixed-replace`)
- `GET /v1/video.h264` - H.264 stream (`video/h264`: Annex-B, Baseline, a keyframe every second). A new viewer starts with the codec config and the next keyframe. Each frame is followed by an access unit delimiter, so a decoder can show it without waiting for the next one. `ffplay` plays it given the bearer header. Opening either video route switches the phone to that format.
- `GET /v1/state` - JSON of all detected cameras + current exposure/WB/battery state
- `POST /v1/control` - live camera control, JSON body

A separate HTTP responder (`SessionServer`, port 8766) runs independently of the streaming service. `GET /v1/hello` says which phone this is, without auth, so the desktop can tell its phone from any other one on a USB cable. `GET /v1/ping` checks the request's bearer token against the paired computers' tokens, returning 200 or 401 plus a small JSON body saying whether the phone is streaming, mid-start, or bound local-only. `POST /v1/unpair` removes the calling computer. `POST /v1/session` starts or stops the camera on the desktop's behalf, reproducing the camera and resolution last chosen on the phone (persisted by `StreamPrefs`, since the spinners may not exist when the request arrives).

Its lifetime is refcounted by `SessionEndpoint` across two owners: `MainActivity` while it is started, and `CameraStreamService` while it is running. So the desktop can confirm pairing before any stream exists, start one, and stop or restart it later even if the phone's screen has since gone dark - but an app that is both backgrounded and idle is unreachable, and a remote start in that state is impossible by construction.

`CameraStreamService` stops itself after 60 seconds with no authorized request from the desktop (a state poll, a control command, or a fresh `/v1/video` connection), so a crashed or disconnected desktop doesn't leave the camera running and draining the battery. The desktop already polls `/v1/state` every 15 seconds while streaming, well inside that margin. The watchdog is exempted while `PreviewActivity`'s local preview surface is attached, since that path never touches HTTP. The desktop can also drive the phone's capture resolution live (the `resolution` control), and `MainActivity` mirrors whatever camera/resolution/OIS selection is actually live into its own spinners while streaming, so they don't fall out of sync with a desktop-initiated change.

The app enumerates **physical sub-cameras** of logical multi-camera groups via `CameraCharacteristics.physicalCameraIds` (API 28+). On many modern phones the logical back camera (ID `0`) hides individual wide/main/telephoto sensors behind it; this app surfaces all of them and lets you pick.

The main screen's pairing card lists the computers this phone is paired with (each with a **Remove** that asks first) and has a **Scan pairing code** button that opens a ZXing barcode scanner (portrait, via `journeyapps:zxing-android-embedded`). Scanning the code in the desktop's Add phone dialog sends the phone's id, name and IPv4 addresses to the desktop over HTTP. The pairing POST requires `android:usesCleartextTraffic="true"` since the desktop's pairing server runs plain HTTP.

The QR code carries a list of desktop address *candidates* (see [QR pairing payload](#qr-pairing-payload)), and the phone works through them in a deliberate order: LAN candidates first, sent over the phone's actual Wi-Fi network via `Network.openConnection()` rather than whatever holds the default route, then every candidate again over the default network. That first pass is what makes pairing work with a VPN running on the phone - a VPN owns the default route, so a LAN address goes nowhere through it, while the Wi-Fi interface underneath still reaches the desktop as long as the VPN permits local-network traffic. Only the pairing request is bound this way; the process is never pinned to Wi-Fi. Attempts are capped at 2s each and 12s in total, so a full candidate list can't leave the user watching nothing happen for half a minute; anything not reached by then is reported as untried rather than silently dropped. If nothing answers, a dialog lists each address tried and how it failed, and names the two situations the phone can't work around: a VPN that blocks LAN traffic outright, and client-isolated guest Wi-Fi - both of which leave USB pairing as the way through. Pairing logic that doesn't need Android (payload parsing/validation, attempt ordering, the failure text) lives in `Pairing.kt` and is unit-tested.

Over USB there's no scan at all: while the Add phone dialog is open, the desktop pushes the same payload via `adb shell am broadcast` to a dedicated intent, registered exported but gated on the `DUMP` permission - held by `adb shell` by default, but not obtainable by ordinary third-party apps, so only adb (not another app on the phone) can trigger it. It re-sends every few seconds, since the broadcast only lands while the app is on screen.

Each paired computer has its own token, stored by the id the desktop sends in the pairing code. Pairing again from the same computer replaces only that computer's token; other computers stay paired. The phone checks tokens per request, so pairing another computer doesn't disturb a running stream.

While its session port is up (and **Local only** is off), the app announces itself on the LAN as `_telescope._tcp` with its phone id in the TXT record, via `NsdManager`. The desktop uses that to find the phone's current address; the announcement proves nothing by itself, since every connection still authenticates.

A **Copy diagnostics** button copies app version, device info, current stream state, and recent state transitions/errors to the clipboard, for pasting into a bug report. Never includes the pairing token, a URL, or raw config.

### Build locally

Requires JDK 21 and Android SDK with `platform-tools`, `platforms;android-34`, `build-tools;34.0.0`.

```bash
cd android
echo "sdk.dir=$ANDROID_SDK_ROOT" > local.properties
./gradlew assembleDebug
# output: app/build/outputs/apk/debug/app-debug.apk
```

**Install via ADB:**
```bash
adb install app/build/outputs/apk/debug/app-debug.apk
```

This is a debug build - self-signed, for personal/development use.

### Permissions

| Permission | Reason |
|---|---|
| `CAMERA` | Open Camera2 device |
| `FOREGROUND_SERVICE` | Run foreground service |
| `FOREGROUND_SERVICE_CAMERA` | Required on Android 14+ for camera-type service |
| `INTERNET` | HTTP server on 0.0.0.0:8080 |
| `WAKE_LOCK` | Keep CPU active with screen off |
| `POST_NOTIFICATIONS` | Persistent streaming notification |
| `ACCESS_NETWORK_STATE` | Show device IP in UI |
| `REQUEST_IGNORE_BATTERY_OPTIMIZATIONS` | The Battery step of Get set up (exempts the app from battery restrictions) |
| `REQUEST_INSTALL_PACKAGES` | Installing its own updates; Android asks the first time |

</details>

<details>
<summary>🖥️ <b>Desktop app</b></summary>

## Desktop app

### Stack

| Component | Library |
|---|---|
| UI | PyQt6 |
| MJPEG decode | opencv-python (`cv2.imdecode`), read via `telescope/mjpeg_reader.py`'s authenticated reader - not `cv2.VideoCapture`, which has no way to attach the bearer token |
| H.264 decode | PyAV (`av`, bundling FFmpeg), via `telescope/h264_reader.py`. Optional: without it only MJPEG is offered |
| Virtual camera output | pyvirtualcam |
| Frame processing | numpy |
| QR code generation | qrcode (rendered via QPainter, no Pillow) |

### One-time setup (detailed)

**Linux:**

First, install the `v4l2loopback` kernel module package through your distro's package manager - Telescope can load and unload the module, but it doesn't install it. It's usually called `v4l2loopback-dkms` (Debian/Ubuntu, Arch).

On **Fedora/Nobara** it's `v4l2loopback` too - `sudo dnf install v4l2loopback` pulls in the actual kernel module (`akmod-v4l2loopback`) as a dependency automatically. It ships via [RPM Fusion](https://rpmfusion.org/), which plain Fedora installs don't have enabled out of the box (Nobara does):
```bash
sudo dnf install https://mirrors.rpmfusion.org/free/fedora/rpmfusion-free-release-$(rpm -E %fedora).noarch.rpm
sudo dnf install v4l2loopback
```

The `start.sh` script handles pip dependencies automatically. Once the package above is installed, Telescope loads the module when you start streaming. It asks for your password once, with **Also switch it on at every startup** ticked, so later boots don't ask again. Without a graphical password prompt (no pkexec or no polkit agent), it shows the command to run in a terminal instead, with a Copy button. **Advanced** in the settings menu can load and unload it too, or run it manually:

```bash
sudo modprobe v4l2loopback devices=2 video_nr=10,11 \
  card_label="OBS Virtual Camera,Phone Camera" exclusive_caps=1
```

To load it at every boot, tick **Load at boot** in Advanced -
it writes the same module options to `/etc/modprobe.d/99-telescope-v4l2loopback.conf` and
`/etc/modules-load.d/99-telescope-v4l2loopback.conf` (and can be unticked later to remove them
again). It refuses to write if another config already sets `v4l2loopback` options, so it won't
conflict with an existing manual setup.

To do the same by hand instead (Fedora/Nobara/any `dracut` distro):
```bash
echo 'options v4l2loopback devices=2 video_nr=10,11 card_label="OBS Virtual Camera,Phone Camera" exclusive_caps=1' \
  | sudo tee /etc/modprobe.d/98-v4l2loopback.conf

sudo rm -f /etc/modprobe.d/v4l2loopback.conf
echo "v4l2loopback" | sudo tee /etc/modules-load.d/v4l2loopback.conf
sudo dracut --force
```

If OBS is installed as Flatpak, grant it device access:
```bash
flatpak override --user --device=all com.obsproject.Studio
```

**Windows:**

The release zip bundles the UnityCapture DLLs already; the first-run checklist registers them with one click (Windows asks for admin access). Running from a source checkout instead (contributors), `start.bat` installs pip dependencies and downloads+registers the DLLs on first run - it isn't part of the release zip, since the packaged EXE needs neither step.

### Key implementation notes (for contributors)

**Plugin system:** The app is built around `TelescopePlugin` - a base class with hooks for `setup()`, `create_panel()`, `process_frame()`, `on_stream_start/stop()`, `on_phone_state()`, and `get/set_config()`. Plugins are registered in `main.py` in order; each creates one UI card. An `EventBus` (QObject with Qt signals) handles cross-plugin communication.

**Window layout:** A plugin declares a `panel_region` (`"left"`, `"right"` or `"center"`) and the window routes its panel there, so no plugin knows where it physically lands. Wide windows get three columns - the desktop-side cards (connection, output, transforms) on the left and the phone-side cards (camera, monitoring) on the right, both the same width so the video stage stays centred; below ~1300px the rails fold together, and below ~900px everything stacks into one scrolling column. A plugin can also contribute a header control via `create_header_widget()` (the Connection plugin puts the phone picker there) or entries in the header's settings menu via `create_menu_actions()` (how Advanced is reached, since nothing in it is adjusted mid-stream). Plugins don't call each other: the first-run checklist hears about paired phones through `EventBus.phones_changed`, asks for pairing with `add_phone_requested`, and tells the video stage to make room with `setup_needed`.

**Theming:** `telescope/theme.py` owns the entire look - palette constants, a dark `QPalette` so Qt-drawn chrome matches, and one stylesheet, applied over Fusion by `apply_theme()`. There are no image assets: icons are drawn procedurally with `QPainter` (`create_vector_icon`), and segmented toggles are ordinary radios/checkboxes carrying a `segmented` property the stylesheet picks up, so exclusivity and signal wiring stay plain Qt.

**Frame pipeline:** `StreamWorker` holds a list of `process_frame` callables (one per plugin). Each frame passes through the full pipeline on the reader thread. `_fit_frame()` then letterboxes/pillarboxes the result to the fixed vcam canvas size, preserving aspect ratio with black bars.

**Canvas size:** The vcam canvas (`pyvirtualcam.Camera` dimensions) is set at stream start from `SetupPlugin.get_canvas_dims()`. It's independent of the phone feed decode resolution. Changing it requires restarting the stream (and reloading v4l2loopback on Linux). `_fit_frame()` handles any mismatch between the processed frame size and the canvas.

**Clean stop/restart:** `_stop()` disconnects the worker's status signal before requesting stop, preventing the old worker's eventual `"idle"` emission from clobbering the new worker's state after a canvas restart. Both Linux and Windows `restart_vcam_canvas()` wait for the old `QThread` to fully exit (via `QThread.wait()`) before starting the new one, avoiding pyvirtualcam slot conflicts.

**Linux root commands:** every v4l2loopback operation (load, load-and-persist, unload, reload, the boot config) is one `pkexec sh -c "..."` call, so one password prompt. If pkexec is missing or has no agent, `sudo -n` covers cached credentials; `sudo` is never run where it could wait for a password, since a GUI app has no terminal to type it in. Otherwise the same steps come back as a pasteable `sudo` / `sudo tee` command.

**Live transform:** Plugin attributes like `flip_h`, `rotation`, `zoom` are plain Python instance attributes updated by the UI thread and read each frame by the worker thread. Python's GIL makes bool/float writes atomic at this granularity, so no lock is needed.

**Live FPS change:** Changing FPS requires recreating the `pyvirtualcam.Camera` context (constructed with fixed fps). The worker holds a `threading.Event` (`_restart_vcam`). When set, the vcam loop breaks, the context closes, and `_run_vcam()` opens a new one at the new rate. The phone connection and reader thread stay up throughout.

**Live resolution change:** Unlike FPS, mid-stream resolution changes don't require a vcam restart. The reader thread reads `self._width`/`self._height` dynamically each frame, and `_fit_frame()` adapts the output to the fixed canvas dimensions.

**Auto-reconnect:** If `cap.read()` fails, the stream reader calls `_reconnect_cap()`, which loops with a 3-second delay until the stream comes back. The pyvirtualcam context stays open during reconnect so the virtual camera doesn't disappear from OBS. Every plugin's current settings (ISO, WB, JPEG quality, etc.) are resent to the phone right after a successful reconnect, since the phone has no way to know its control state might be stale.

**Genuine-connection signal:** `EventBus.stream_connected` fires only when `StreamWorker` reports its first `"ok"` status (an actual frame decoded), not merely when a worker object exists. The Connection card shows **Connecting…** until it fires, so a worker quietly retrying against a phone that isn't answering never reads as a healthy stream.

**Desktop address discovery:** `ip_utils.get_pairing_addresses()` enumerates the machine's real network adapters (via `ifaddr`) instead of asking the routing table where a public address would go. The old UDP "route probe" reported whichever interface owns the default route - which under a VPN is the VPN's, so the physical LAN address the phone can actually reach went missing from the QR code exactly when it mattered. Loopback, link-local (`169.254/16`) and IPv6 addresses are dropped, as are container/VM-only adapters (`docker*`, `br-*`, `veth<hex>`, `virbr*`, `vboxnet*`, `vmnet*`, VirtualBox/VMware host-only) - though Windows' `vEthernet (...)` is kept, since Hyper-V bridges the host's real LAN through it. What's left is classified `lan` (RFC 1918), `tailscale` (`100.64/10`) or `other`, and ordered that way: the physical LAN path first, Tailscale as the cross-network fallback. Recognisable tunnel adapters (`tun*`, `wg*`, `utun*`, …) are still advertised but sorted behind physical ones in the same class, so a desktop VPN handing out a `10.x` address doesn't push the real LAN address down the phone's list. The whole thing is capped at 8 candidates with interface names trimmed to 32 characters, since every byte adds modules to a code someone has to scan with a phone camera.

**How the phone is reached:** `phones.RouteResolver` decides on every connect and every status check, starting from scratch each time rather than remembering a mode. USB comes first: for each device adb reports as usable, it opens a temporary `adb forward tcp:0` to the session port and asks `/v1/hello` which phone it is; only a matching id counts, then `/v1/ping` confirms the token. Anything else becomes the reason shown on the card (`no_adb`, `no_cable`, `unauthorized`, `other_phone`, `app_closed`). Wi-Fi then probes, in parallel, the addresses mDNS currently reports for this phone id, the address that answered last time, and the rest of the phone's stored addresses ranked Tailscale first. Whichever answers is remembered as the phone's active address. With **Connect via** set to USB only, a failed USB check is reported as such instead of falling back. adb forwards are refcounted per (device, port) in `UsbTunnels`, so the status probe, a start and a stop that overlap don't tear down each other's tunnel.

**First address after pairing:** a phone reports every IPv4 address it has, and ranking alone gets it wrong in both directions - a phone on a tailnet this desktop isn't on has its unreachable Tailscale address preferred over its Wi-Fi one, while two devices sharing only a tailnet need exactly the opposite. `PairingResult.source_ip` settles it: the address the pairing POST arrived from is one of the phone's *and* demonstrably reachable from here, so it becomes the phone's `active_ip`. It's ignored when it isn't one of the addresses the phone reported (USB pairing arrives through the `adb reverse` tunnel, so its source is this machine's loopback).

**Re-pair mid-stream:** pairing the phone you're streaming from again replaces this computer's token on the phone, so the desktop reconnects the stream with the new one.

**Control client:** `PhoneControlClient` runs a single background worker thread that POSTs each command as a JSON body to `/control`, in the order it was queued. Requests that share the same `action` are coalesced to just the latest value while still waiting to be sent - a burst of slider drags can't have an older request's response arrive after a newer one - except camera switches, which are always sent individually and in order. Failures are silently dropped - a missed control command is non-critical.

**ISO/shutter sliders:** Log scale over 2000 steps across the range the phone reports per camera. Range updates when switching lenses. Shutter spinbox shows milliseconds while the API uses nanoseconds.

**White balance:** Linear Kelvin slider 2000-10000 K plus a green-magenta tint slider (-150..+150). `_kelvin_to_rggb()` converts both to Camera2 RGGB channel gains with an exponential model centred at ~5500 K (not a lookup table), sent via the `wb_gains` action and applied with `COLOR_CORRECTION_MODE_TRANSFORM_MATRIX` / `COLOR_CORRECTION_GAINS`. Reverting to auto restores `CONTROL_AWB_MODE_AUTO`.

**Per-phone config:** Camera, stream-output, transform, and monitoring settings serialize to `telescope_config.json` with a 500ms debounce. Connection settings and virtual-camera canvas settings are global. The `devices` dict is keyed by phone id; switching phones saves the current phone's settings before loading the new one's, and removing a phone deletes its entry. A version 2 config is upgraded by dropping its pairings and per-phone settings (keyed by name back then) and keeping the rest; anything older is backed up as `telescope_config.json.invalid-<timestamp>` and replaced with defaults.

**Single-instance:** `acquire_single_instance()` tries to bind a local TCP socket on port 47823. If already bound, it signals the running instance to restore its window and exits.

**Battery/temperature polling:** A `QTimer` fires every 15 seconds while streaming. Notifications fire once per threshold crossing with 5-degree/5-percent hysteresis to avoid repeated alerts.

</details>

<details>
<summary>📡 <b>Control API reference</b></summary>

## Control API reference

Server is on the phone at port 8080 for `/v1/video`, `/v1/state`, and `/v1/control` (all only exist while actively streaming); a separate responder on port 8766 serves `/v1/hello`, `/v1/ping`, `/v1/session` and `/v1/unpair`. Every request below except `/v1/hello` requires an `Authorization: Bearer <token>` header carrying the token this computer got when it paired; missing or unknown tokens get `401`.

### `GET /v1/state`

```json
{
  "cameras": [
    {
      "id": "0",
      "logicalId": null,
      "label": "Back ~24mm OIS",
      "current": false,
      "hasOis": true,
      "isoMin": 50,
      "isoMax": 12800,
      "shutterMinNs": 100000,
      "shutterMaxNs": 1000000000,
      "supportsManualSensor": true,
      "supportsManualWB": true,
      "supportsManualFocus": true,
      "supportsFocusPoint": true,
      "minFocusDistance": 8.3,
      "aeCompMin": -8,
      "aeCompMax": 8,
      "aeCompStep": 0.167,
      "supportsFlash": true,
      "hwLevel": "FULL",
      "supportedSizes": [
        { "width": 4032, "height": 3024 },
        { "width": 1920, "height": 1080 }
      ]
    }
  ],
  "auto": true,
  "iso": null,
  "shutter_ns": null,
  "wb_manual": false,
  "wb_r": null,
  "wb_ge": null,
  "wb_go": null,
  "wb_b": null,
  "ois": true,
  "focus_mode": "continuous",
  "focus_distance": 0.0,
  "nr_mode": 1,
  "edge_mode": 1,
  "ae_comp": 0,
  "black_level_lock": false,
  "torch": false,
  "jpeg_quality": 85,
  "phone_fps": 30,
  "codecs": ["mjpeg", "h264"],
  "codec": "mjpeg",
  "bitrate": 8087040,
  "stream_width": 1920,
  "stream_height": 1080,
  "battery": 87,
  "charging": false,
  "battery_temp_c": 32.5
}
```

`minFocusDistance`, `aeCompMin`/`aeCompMax`/`aeCompStep` are per-lens, reported by Camera2 (`aeCompStep` is typically `0.167` = 1/6 EV). `wb_r`/`wb_ge`/`wb_go`/`wb_b` are the current RGGB channel gains when `wb_manual` is true, `null` otherwise. `supportedSizes` is the lens's actual list of capture sizes, which the desktop uses to populate its resolution dropdown instead of a fixed list. `stream_width`/`stream_height` are the current lens's live capture size. `codecs` lists what the phone can send (`h264` only with a hardware encoder), `codec` is what it's sending, and `bitrate` is the H.264 target in bits per second. `codec_error` appears when H.264 failed and the phone went back to MJPEG. Fields at their default value are left out, so read them with a default.

### `POST /v1/control`

JSON body `{"action": "<action>", ...params}`.

| `action` | extra params | effect |
|---|---|---|
| `camera` | `id=<id>` | Switch camera |
| `resolution` | `width=<int> height=<int>` | Set the capture resolution to one of the lens's reported supported sizes |
| `auto` | - | Restore auto exposure |
| `iso` | `value=<int>` | Set ISO; switches AE to OFF (once shutter is also set) |
| `shutter` | `value=<long ns>` | Set shutter in nanoseconds; switches AE to OFF (once ISO is also set) |
| `wb_auto` | - | Restore auto white balance |
| `wb_gains` | `r=<float> ge=<float> go=<float> b=<float>` | Set manual white balance via `COLOR_CORRECTION_GAINS` RGGB channel gains |
| `ois` | `value=1\|0` | Toggle OIS |
| `focus_mode` | `value=continuous\|manual` | Switch autofocus / manual focus (also ends point focus) |
| `focus_point` | `x=<0..1> y=<0..1> [size=<0..1>]` | Focus on a point of the stream frame, and meter exposure there while it's automatic. `size` is the region's side as a fraction of the frame's shorter side (default 0.1). Refused on a lens without `supportsFocusPoint`. `/v1/state` then reports `focus_mode: "point"` |
| `focus_distance` | `value=<float diopters>` | Set manual focus distance |
| `ae_comp` | `value=<int steps>` | Set exposure compensation, in the lens's AE-compensation steps (see `aeCompStep`) |
| `nr_mode` | `value=<int 0-4>` | Set noise reduction mode (desktop UI only offers 0/1/2 = Off/Fast/High Quality) |
| `edge_mode` | `value=<int 0-3>` | Set sharpening/edge mode (desktop UI only offers 0/1/2 = Off/Fast/High Quality) |
| `black_level_lock` | `value=1\|0` | Toggle black level lock |
| `torch` | `value=1\|0` | Toggle flash/torch |
| `jpeg_quality` | `value=<int 1-100>` | Set JPEG quality on the phone |
| `bitrate` | `value=<int bits/s>` | Set the H.264 bitrate (clamped to 1-30 Mbps); `0` sizes it from resolution and fps |
| `fps_target` | `value=<int 1-120>` | Set capture FPS on the phone (desktop UI restricts to 5-60) |

All responses: `{"ok": true}` or `{"ok": false, "error": "..."}`.

> **Manual exposure note:** `CONTROL_AE_MODE_OFF` only activates when *both* ISO and shutter are set and the selected camera reports `supportsManualSensor` - `CONTROL_MODE` itself stays `CONTROL_MODE_AUTO` throughout, so autofocus keeps running independently of manual exposure. The desktop app sends both ISO and shutter simultaneously when switching to manual mode.

### `GET /v1/hello`

On port 8766, and the one request without auth: it only says which phone this is, so the desktop can tell its phone from any other one before trusting a USB forward or an address.

```json
{ "protocol": 2, "phoneId": "3f9c…", "phoneName": "Pixel 8 Pro" }
```

`phoneId` is random, made once per install, and is what the desktop stores the phone by.

### `GET /v1/ping`

Served on port 8766 by `SessionServer` - unlike the endpoints above, it exists whether or not a stream is running (while the app's main screen is up, or while the camera service is running, or both). Returns `200` if the token belongs to a paired computer, `401` if not.

```json
{
  "protocol": 2,
  "streaming": false,
  "busy": false,
  "localOnly": true,
  "phoneId": "3f9c…",
  "phoneName": "Pixel 8 Pro"
}
```

`streaming` is a live stream, `busy` is a start in flight (camera opening, session configuring), and `localOnly` mirrors the app's **Local only - USB** setting, so the desktop can name that mismatch instead of timing out against an address nothing is listening on.

### `POST /v1/session`

Also on 8766. JSON body `{"action": "start"}` or `{"action": "stop"}`; same auth and the same `{"ok": true}` / `{"ok": false, "error": "..."}` responses as `/v1/control`. This is what makes the desktop's Start button sufficient on its own.

| `action` | effect |
|---|---|
| `start` | Start the camera service, reproducing the camera/resolution/OIS selection last used on the phone. `{"ok": true}` if a stream is already running. |
| `stop` | Stop the camera service. `{"ok": true}` if nothing was running. |

Refusal reasons, all reported with HTTP `200` and `"ok": false` (the request was fine, the camera wouldn't open): `no_camera_permission`, `busy` (a start is already in flight), `start_refused` (Android declined the foreground-service start).

A start is only accepted while `SessionServer` is bound at all, i.e. the app's main screen is up or the camera service is already running - so this cannot open the camera on a phone that is both backgrounded and idle.

The desktop polls `/v1/ping` after a start until `streaming` goes true (12s budget), because the service answers as soon as the start is accepted, well before the capture session is configured.

### `POST /v1/unpair`

Also on 8766, body `{}`. Removes the computer whose token made the request, and only that one - a computer can't unpair others. The desktop calls it when you remove a phone, so the phone stops accepting a computer that has forgotten it. Answers `{"ok": true}`, or `401` for an unknown token.

### QR pairing payload

Generated by the desktop (`telescope/pairing.py`), rendered as the QR code, and pushed verbatim (base64-encoded) over `adb` for USB pairing. Both sides speak version `3` only; a mismatch is reported as "update both apps" rather than "invalid code", since desktop and APK ship together.

```json
{
  "version": 3,
  "port": 8765,
  "candidates": [
    { "ip": "192.168.1.42",  "interface": "Wi-Fi",      "kind": "lan" },
    { "ip": "100.90.12.34",  "interface": "tailscale0", "kind": "tailscale" }
  ],
  "nonce": "...",
  "token": "...",
  "computer_id": "8d1e…",
  "computer_name": "desk"
}
```

`kind` is one of `lan`, `tailscale`, `other`, and candidates are ordered best-first. The phone rejects the payload outright if any candidate carries a malformed IPv4 literal or an unrecognised `kind`, if the list is empty, or if the port/nonce/token are unusable. `interface` is the desktop-side adapter name, carried for diagnostics. The copy pushed over adb advertises a single candidate - `127.0.0.1`, kind `other` - reached through the `adb reverse` tunnel.

`computer_id` is made once per desktop install and is what the phone files the token under, so pairing again from the same computer replaces its token instead of adding a second entry. `computer_name` is what the phone's list shows (the desktop's host name unless renamed in **Your phones**).

The phone then `POST`s to `http://<ip>:<port>/pair/<nonce>` with `{"name": ..., "phone_id": ..., "ips": [...], "token": ...}`; the echoed token confirms the request came from a device that actually read the current code, on top of the one-shot nonce in the path.

The desktop also notes the source address that request arrived from. That address is, by construction, one of the phone's *and* reachable from this machine over whatever path the phone found, so it becomes the phone's first active address.

</details>

<details>
<summary>⚖️ <b>License</b></summary>

## License

Telescope is licensed under the [GNU Affero General Public License v3.0](https://www.gnu.org/licenses/agpl-3.0.html) - see [LICENSE](LICENSE) for the full text.

    Copyright (C) 2026 LunarKittyy

    This program is free software: you can redistribute it and/or modify
    it under the terms of the GNU Affero General Public License as published
    by the Free Software Foundation, either version 3 of the License, or
    (at your option) any later version.

    This program is distributed in the hope that it will be useful,
    but WITHOUT ANY WARRANTY; without even the implied warranty of
    MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
    GNU Affero General Public License for more details.

You are free to use, modify, and redistribute it, including for commercial purposes, provided derivative works remain under the AGPL-3.0 and you make the corresponding source available - including to users who interact with a modified version over a network. AGPL-3.0 is also compatible with the GPL v3 licensing of the bundled PyQt6 dependency.

## Third-party components

Full notices (bundled binaries and Python runtime dependencies) are in [`desktop/THIRD_PARTY_NOTICES.txt`](desktop/THIRD_PARTY_NOTICES.txt), which ships inside both the Windows zip and the Linux tarball. Summary:

**UnityCapture** (`desktop/unitycapture/`) - DirectShow virtual camera filter for Windows.
Copyright (c) 2018 Bernhard Schelling. MIT License. See `desktop/unitycapture/LICENSE`.
Source: https://github.com/schellingb/UnityCapture

**Android SDK Platform Tools** (`desktop/platform-tools/`) - includes `adb.exe` for USB mode.
Copyright (c) Google LLC. Android Software Development Kit License Agreement.
See `desktop/platform-tools/NOTICE` and https://developer.android.com/studio/terms

**Python runtime dependencies** (PyQt6, opencv-python, numpy, pyvirtualcam, qrcode, ifaddr, zeroconf) - installed from PyPI; exact pinned versions are in `desktop/constraints.txt`. PyQt6 in particular is GPL v3-licensed (a commercial Riverbank Computing license also exists but isn't what this project uses).

</details>

<details>
<summary>⚙️ <b>CI / GitHub Actions</b></summary>

## CI / GitHub Actions

### Versions

Both apps share one version, the `VERSION` file at the repo root. The build number is the commit count on `master`, so it only grows; it's the Android `versionCode` and what the update check compares. A build is stable (`0.5.0`), nightly (`0.5.0-nightly.123`) or a source checkout (`0.5.0 dev`). The version shows at the bottom of the Advanced dialog on the desktop and under Copy diagnostics on the phone.

### `release.yml` - every push to `master`, and every `v*` tag

Builds everything from one commit, then publishes it together:

- a push to `master` replaces the rolling **`nightly`** pre-release (deleted and recreated, so it stays at the top of the Releases page)
- a tag `vX.Y.Z` creates the stable release `Telescope X.Y.Z`; the tag must match `VERSION`

Each release holds `Telescope.apk`, `Telescope-windows.zip`, `Telescope-linux.tar.gz` (both desktop bundles include the APK, for Install over USB) and `manifest.json`: version, build number, channel, commit, session protocol, and each file's URL, size and SHA-256. The apps' update check reads the manifest: the phone compares `android.versionCode` with its own, the desktop compares `build`.

The APK is signed with the release key from the repository secrets `TELESCOPE_KEYSTORE` (the keystore, base64), `TELESCOPE_KEYSTORE_PASSWORD`, `TELESCOPE_KEY_ALIAS` and `TELESCOPE_KEY_PASSWORD`. A release fails rather than publish an APK signed with a debug key, because Android only installs an update signed with the same key as the installed app.

The three build workflows below also run on pull requests, without publishing.

### `build-apk.yml` - pull requests touching `android/**`

1. JDK 21 (Temurin) + Gradle cache
2. Android SDK (android-34, build-tools;34.0.0)
3. `./gradlew lintDebug testDebugUnitTest`, then `./gradlew assembleRelease -PbuildNumber=N -Pchannel=nightly|stable` (debug-signed on pull requests)
4. In a release: checks the APK isn't debug-signed

### `build-windows.yml` - pull requests touching `desktop/**`

1. Python 3.11 + pip cache
2. `pip install -r requirements-dev.txt -c constraints.txt`; runs `pytest`
3. `pip install -r requirements.txt pyinstaller -c constraints.txt`
4. In a release: `scripts/write_build_info.py` stamps the version into `telescope/_build.py`
5. `python scripts/smoke_check.py` - packaging smoke checks (see below)
6. `pyinstaller telescope.spec`
7. Assembles the bundle: EXE + `THIRD_PARTY_NOTICES.txt` + `platform-tools/` + `unitycapture/`, and checks nothing is missing

`telescope.spec` uses `collect_all('PyQt6')` to include Qt platform plugins that PyInstaller's default analysis misses. Expected EXE size: 60-80 MB.

### `build-linux.yml` - pull requests touching `desktop/**`

1. Python 3.11 + pip cache; apt-installs `libegl1 libgl1 libxkbcommon0 libdbus-1-3` (PyQt6 needs these even in headless/offscreen test mode); installs `requirements-dev.txt` via `constraints.txt`; runs `pytest`
2. In a release: stamps the version
3. `python3 scripts/smoke_check.py` - packaging smoke checks
4. Assembles the bundle: `main.py` + `telescope/` package + `requirements.txt` + `constraints.txt` + `start.sh` + `THIRD_PARTY_NOTICES.txt`

No compiled build step - the Linux bundle is the Python source and launcher script, which creates its own venv on first run (see `start.sh`).

### `desktop/scripts/smoke_check.py`

Run in both desktop CI workflows before assembling the bundle: constructs the full app and registers every plugin, exercises ADB discovery and virtual-camera-availability detection without crashing, and drives a real authenticated MJPEG round-trip (auth header, multipart framing, JPEG decode, and that an unauthenticated request is actually rejected) against a local test server. It isn't a substitute for testing against a real phone - see the manual [release checklist](docs/release-checklist.md) and [device-compatibility matrix](docs/device-compatibility.md) for that.

</details>

<details>
<summary>🛠️ <b>Troubleshooting</b></summary>

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| Only 2 cameras visible | Physical sub-cameras hidden behind logical camera | Already handled via `physicalCameraIds`; if still missing, device may restrict access |
| Manual exposure greyed out | Camera doesn't report `MANUAL_SENSOR` capability | Some front cameras and telephoto lenses don't support it; use Auto |
| `/dev/video11` gone after reboot | v4l2loopback isn't loaded at boot | Start Streaming loads it. Leave **Also switch it on at every startup** ticked (or tick **Load at boot** in Advanced) and it won't ask again |
| pyvirtualcam fails to open (Linux) | Module not loaded, or not installed | Install `v4l2loopback-dkms` (Debian/Ubuntu/Arch) or `v4l2loopback` (Fedora/Nobara, via RPM Fusion), then click Start again. **Advanced** can load it by hand |
| pyvirtualcam fails to open (Windows) | UnityCapture not registered | Open **Advanced** from the settings menu and reinstall the driver |
| "Virtual camera is set up differently" banner when starting | Some other app (OBS's own virtual camera, a previous session, etc.) already has the module loaded with different settings | Close that app, or run `sudo modprobe -r v4l2loopback` yourself, then click Start again |
| Canvas restart fails with "module in use" | OBS or another app still holds the device | Close all apps using the virtual camera, then retry |
| Camera control panel never appears | Phone HTTP server slow to start | App retries 3x over 6s; check the phone still shows the stream running |
| WB slider has no effect | Camera doesn't support `MANUAL_POST_PROCESSING` | Falls back gracefully; auto AWB still works |
| ISO/shutter change has no effect | Only one of the two was sent | Switch to Manual - desktop sends both simultaneously |
| High latency over Wi-Fi | MJPEG is per-frame JPEG, higher bandwidth than H.264 | Switch Format to H.264, use USB mode, lower JPEG quality, or reduce phone FPS |
| Second launch does nothing | Single-instance enforcement | The existing window is brought to the front |
| QR pairing fails ("Could not reach the desktop") | Phone and desktop not on the same network, or desktop firewall blocking port 8765 | The failure dialog on the phone lists every address it tried and how each failed. Make sure both are on the same Wi-Fi and the Add phone dialog is still open (the pairing server only runs while it is), or plug the phone in and pair over USB |
| QR pairing fails on a guest/public Wi-Fi | Client isolation - the access point blocks device-to-device traffic entirely | Nothing on either device can work around this; use USB pairing, or a network you control |
| Plugged in, but USB pairing doesn't happen | The Add phone dialog's USB line says which: adb isn't installed, USB debugging hasn't been allowed on the phone, or the app isn't on screen | Install `adb` (Linux; it's bundled on Windows), accept the USB debugging prompt on the phone, and keep Telescope open on it - the offer is re-sent every few seconds |
| Streams over Wi-Fi although the phone is plugged in | The Connection panel says why under **Using**: Telescope isn't open on the phone, USB debugging isn't allowed yet, or the cable has a different phone on it | Fix what it names and the next connect uses USB (mid-stream, **Switch to USB** appears). To never use Wi-Fi, set **Connect via** to USB only |
| "Can't reach the phone" | The app isn't open on the phone, or the phone is on another network and not plugged in | Open Telescope on the phone and keep it on screen; the status updates by itself |
| "Needs pairing again" | The phone removed this computer, or the app was reinstalled | Click **Add phone** and pair it again; its settings on this computer are kept |
| QR pairing fails while a VPN is active | The VPN is blocking local-network traffic outright. (A VPN that *allows* LAN access is handled: the desktop advertises its real interface addresses rather than whatever owns the default route, and the phone sends LAN attempts over its Wi-Fi interface rather than the tunnel) | Turn on the VPN's "allow local network access"/"LAN access" option, pause the VPN while pairing, or use USB pairing. Once paired, streaming has the same requirement |
| QR scanner opens in landscape | Manifest override not applied | The app overrides ZXing's default orientation to portrait; rebuild if you see this on an old build |

</details>
