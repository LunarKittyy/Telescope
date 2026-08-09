# Telescope

Stream your Android phone's camera - including telephoto and wide-angle lenses - to a virtual webcam on Linux or Windows. Camera controls (ISO, shutter, white balance, lens selection) are exposed over a local HTTP API so the desktop app can drive them live.

---

## Quick Start

You'll need an Android phone and a PC running Linux or Windows.

### 1. 📱 Install the phone app

Open the [latest release](../../releases) page, expand **Assets**, and tap `Telescope.apk`. (Assets is just a plain list of downloadable files - ignore everything else on that page.)

**Easiest:** open that link on your phone's own browser and tap the APK to install it. Your phone will ask to allow "install from this source" the first time - allow it.

(Downloaded it on your PC instead of your phone? That's fine too - the desktop app you're about to install can put it on your phone for you.)

### 2. 🖥️ Run the desktop app

**🪟 Windows**

Download `Telescope-windows.zip` from the same [releases page](../../releases), extract it, and run `TelescopeDesktop.exe`.

**🐧 Linux**

Download `Telescope-linux.tar.gz` from the [releases page](../../releases), extract it, and run `./start.sh`. You'll also need a couple of things from your package manager:

- **`v4l2loopback`** - what the virtual camera runs on. Telescope can turn it on and off but not install it.
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

- 💡 **`adb`** *(optional)* - only needed for pairing your phone over USB, or if you want the desktop app to install the phone app for you instead of doing it on your phone yourself.
  - Debian/Ubuntu: `sudo apt install adb`
  - Fedora/Nobara: `sudo dnf install android-tools`
  - Arch: `sudo pacman -S android-tools`

**Both platforms:** on first launch, click the gear icon in the top right (next to the **Start Streaming** button) and choose **Setup Drivers & APK**. It sets up the virtual camera and can install the phone app for you (you'll be asked to pick the APK you downloaded). You only need to open this dialog once - if it already says everything's ready, there's nothing left to do here.

### 3. 🔗 Pair your phone

Open Telescope on your phone and leave it on screen. On the desktop app, click **Pair Device**, then pick one:

- **Wi-Fi:** scan the QR code with your phone's scan button.
- **USB:** click **Pair via ADB** (needs `adb` on your PATH - bundled on Windows; on Linux, install it via your package manager: `adb` on Debian/Ubuntu, `android-tools` on Fedora/Nobara and Arch).

### 4. ▶️ Start streaming

Pick a camera and resolution on the phone, then hit **Start Streaming** on the desktop - it starts the phone's camera for you. In OBS (or anywhere else), select **Phone Camera** (Linux) or **Unity Video Capture** (Windows) as your webcam.

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
- OIS toggle
- Noise reduction and sharpening (edge mode): Off / Fast / High Quality
- Black level lock toggle
- Torch/flash toggle, on lenses that report a flash unit
- Controls are greyed out per-lens if the camera hardware reports it doesn't support them

**Stream transforms** (applied on the desktop, no phone restart needed)
- Horizontal and vertical flip
- Rotation: 90 CW, 180, 90 CCW
- Software zoom 1-5x with pan X/Y sliders (center crop + resize)

**Canvas size control** (Advanced, in System Setup)
- Set the virtual camera canvas independently of the phone feed resolution
- Presets: 720p/1080p/4K in 16:9 landscape and portrait, XGA and UXGA in 4:3, or fully custom
- On Linux: reloads v4l2loopback in a single elevated prompt (close OBS first); stream restarts automatically
- On Windows: stops and restarts the stream with the new canvas size

**Resolution and FPS**
- Resolution dropdown is populated from the current lens's actual supported capture sizes (read from the phone), not a fixed list - picking one sends a live `resolution` control to the phone instead of resizing after decode
- The readout goes amber while a resolution change is in flight and clears once the stream confirms the new size, or turns red if it never does
- One FPS spinner (5-60) drives both the phone's capture rate and the virtual camera's playback rate - there's no separate "phone" and "playback" rate to keep in sync

**Bandwidth controls**
- JPEG quality slider (50-100%) - controls compression on the phone
- Both take effect immediately without restarting the stream

**Monitoring**
- Live FPS and a "LIVE THROUGHPUT" Mbps readout in the footer while streaming, colored amber if the real decode rate is sustained-struggling against the target
- A dropped stream shows an animated "Stream dropped - reconnecting..." status instead of a static line
- Battery level and phone temperature polled every 15 seconds, shown in the Monitoring panel with color coding
- Configurable battery alert threshold (default 20%) - fires a tray/desktop notification when discharging below it
- Configurable temperature alert threshold (default 45 C) - fires a notification when exceeded

**Multi-device and config persistence**
- USB mode targets a specific ADB serial: if exactly one authorized device/emulator is connected it's picked automatically, if more than one is connected you're prompted to choose which one (avoids `adb: more than one device/emulator` failures on forward/install)
- Named device list in Wi-Fi mode: add/remove/edit devices via the gear button popup; switch between them with a dropdown
- Each device stores multiple IPs; a second dropdown selects the active IP. Tailscale IPs (100.64.0.0/10) are ranked first, LAN IPs second
- Pairing: click **Pair Device** on the desktop to open the pairing dialog - a scannable QR code in Wi-Fi mode, or a **Pair via ADB** button in USB mode that pushes the request over adb instead. Either way the phone is registered automatically with all its IPs. A status label next to the Pair Device button shows live reachability (Paired / Not paired / Unreachable / Checking...), not just whether a token happens to be saved
- Camera, stream-output, transform, and monitoring settings (resolution, FPS, flip, rotation, exposure, zoom, quality, alert thresholds, etc.) are saved per device to `telescope_config.json`; connection settings and the virtual-camera canvas are global
- The config format is not migrated across versions: an unsupported or malformed config is backed up alongside the real one and replaced with defaults rather than carrying compatibility code for old formats. Each section (connection/plugin settings, per-device settings, selected device) is validated independently, so one malformed section resets to defaults without discarding the rest

**Privacy**
- Network authentication is meant to stop accidental or opportunistic access from other devices on the LAN, not an active attacker or network observer: traffic is unencrypted, and possession/interception of the bearer token is enough for access
- For an actual security boundary, use **Local only - USB**, which keeps the camera service off the network entirely
- Local only mode: binds the server to `127.0.0.1` so the stream is unreachable from the network; only USB works in this mode
- Toggle in the Android app restarts the stream automatically to apply the change
- Switching between USB and Wi-Fi mode on the desktop also restarts the stream automatically

**System integration**
- Minimizes to system tray on close only when streaming; otherwise quits
- Right-click the tray icon to quit, or click it to show/hide the window
- Launching a second instance brings the existing window to the front
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
      |  USB: adb forward tcp:8080 tcp:8080
      |  Wi-Fi: direct HTTP
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
            setup                   driver setup, canvas settings
            connection              device list, IP dropdown, pairing dialog (pairing.py, port 8765) + session channel (port 8766): pair-status probe, remote start/stop
            camera_control          lens, ISO, shutter, WB, OIS
            stream_output           resolution, FPS, JPEG quality
            transforms              flip, rotation, zoom, pan
            preview                 in-card and pop-out video preview
            monitoring              battery, temperature alerts
```

A second responder on the phone (`SessionServer`, port 8766) runs independently of the streaming server, so the desktop can reach the phone in exactly the state the streaming server doesn't exist in: idle. It answers `GET /v1/ping` (pairing status plus what the phone is currently doing) and `POST /v1/session` (start or stop the camera from the desktop).

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
|-- .github/workflows/
|   |-- build-apk.yml            # CI: debug APK on ubuntu-latest
|   |-- build-windows.yml        # CI: Windows bundle (EXE + adb + UnityCapture)
|   +-- build-linux.yml          # CI: Linux bundle (source + start.sh)
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
|       |-- Pairing.kt           # QR payload (v2) parsing/validation, attempt ordering, failure text
|       |-- MjpegServer.kt       # Authenticated HTTP: /v1/video  /v1/state  /v1/control
|       |-- SessionServer.kt     # Out-of-band responder (port 8766): GET /v1/ping, POST /v1/session
|       |-- SessionEndpoint.kt   # Refcounted owner of SessionServer + the commands it runs
|       |-- StreamLauncher.kt    # Single place CameraStreamService is started from
|       |-- StreamPrefs.kt       # Last camera/resolution selection, for desktop-initiated starts
|       |-- HttpWire.kt          # The HTTP/1.1 subset MjpegServer and SessionServer share
|       +-- TokenStore.kt        # Persists the single active pairing bearer token
|
+-- desktop/
    |-- main.py                  # Entry point: registers plugins, restores config
    |-- requirements.txt         # Readable ">=" lower bounds
    |-- requirements-dev.txt     # requirements.txt + pytest, used by CI
    |-- constraints.txt          # Exact pinned versions for CI/release installs
    |-- scripts/smoke_check.py   # Packaging smoke checks (see CI section below)
    |-- tests/                   # pytest suite (desktop only; Android has its own JVM unit tests)
    |-- THIRD_PARTY_NOTICES.txt  # Bundled into both release archives
    |-- telescope.spec            # PyInstaller spec for Windows EXE
    |-- start.sh                 # Linux launcher (creates/reuses a Telescope-owned venv)
    |-- start.bat                # Windows source-checkout launcher (auto-installs deps); not in the release zip, the EXE needs neither
    |-- platform-tools/          # Bundled adb for Windows
    |-- unitycapture/            # Bundled UnityCapture DLLs (MIT)
    +-- telescope/
        |-- app.py               # TelescopeWindow: plugin host, responsive shell, stream lifecycle
        |-- theme.py             # Palette tokens + the app stylesheet
        |-- stream.py            # StreamWorker: MJPEG -> pipeline -> pyvirtualcam
        |-- mjpeg_reader.py      # Authenticated multipart-MJPEG reader (replaces cv2.VideoCapture)
        |-- session.py           # StreamSession: owns worker/client for one connect-to-disconnect lifecycle
        |-- plugin.py            # TelescopePlugin base class, EventBus, HostServices protocol
        |-- config.py            # Versioned JSON config (v2) with per-section validation
        |-- models.py            # Typed contracts: PhoneState, CameraCapabilities, DeviceProfile, StreamSettings
        |-- phone_client.py      # Authenticated HTTP client for /v1/state and /v1/control (port 8080)
        |-- session_client.py    # Authenticated HTTP client for /v1/ping and /v1/session (port 8766)
        |-- pairing.py           # PairingServer: Qt-free pairing HTTP handshake (nonce/token, no PyQt import)
        |-- platform/
        |   |-- linux.py         # v4l2loopback helpers (load, unload, reload)
        |   +-- windows.py       # UnityCapture helpers
        |-- plugins/
        |   |-- setup.py
        |   |-- connection.py
        |   |-- camera_control.py
        |   |-- stream_output.py
        |   |-- transforms.py
        |   |-- preview.py
        |   +-- monitoring.py
        +-- widgets/
            |-- common.py        # NoScroll*, LogSliderRow, rows, segmented toggles, icons
            +-- lens_panel.py    # Lens picker widget
```

</details>

<details>
<summary>📱 <b>Android app</b></summary>

## Android app

### What it does

Runs a **foreground service** (declared type `camera`, required on Android 14+) that owns a Camera2 session and an HTTP server on port 8080. Three endpoints, all requiring a bearer token issued during pairing:

- `GET /v1/video` - MJPEG stream (`multipart/x-mixed-replace`)
- `GET /v1/state` - JSON of all detected cameras + current exposure/WB/battery state
- `POST /v1/control` - live camera control, JSON body

A separate HTTP responder (`SessionServer`, port 8766) runs independently of the streaming service. `GET /v1/ping` checks the request's bearer token against the currently stored pairing token, returning 200 or 401 plus a small JSON body saying whether the phone is streaming, mid-start, or bound local-only. `POST /v1/session` starts or stops the camera on the desktop's behalf, reproducing the camera and resolution last chosen on the phone (persisted by `StreamPrefs`, since the spinners may not exist when the request arrives).

Its lifetime is refcounted by `SessionEndpoint` across two owners: `MainActivity` while it is started, and `CameraStreamService` while it is running. So the desktop can confirm pairing before any stream exists, start one, and stop or restart it later even if the phone's screen has since gone dark - but an app that is both backgrounded and idle is unreachable, and a remote start in that state is impossible by construction.

`CameraStreamService` stops itself after 60 seconds with no authorized request from the desktop (a state poll, a control command, or a fresh `/v1/video` connection), so a crashed or disconnected desktop doesn't leave the camera running and draining the battery. The desktop already polls `/v1/state` every 15 seconds while streaming, well inside that margin. The watchdog is exempted while `PreviewActivity`'s local preview surface is attached, since that path never touches HTTP. The desktop can also drive the phone's capture resolution live (the `resolution` control), and `MainActivity` mirrors whatever camera/resolution/OIS selection is actually live into its own spinners while streaming, so they don't fall out of sync with a desktop-initiated change.

The app enumerates **physical sub-cameras** of logical multi-camera groups via `CameraCharacteristics.physicalCameraIds` (API 28+). On many modern phones the logical back camera (ID `0`) hides individual wide/main/telephoto sensors behind it; this app surfaces all of them and lets you pick.

A **scan button** in the top-right corner of the main screen opens a ZXing barcode scanner (portrait, via `journeyapps:zxing-android-embedded`). Scanning the QR code shown by the desktop app sends the phone's name and all its IPv4 addresses to the desktop over HTTP, which adds it as a named device automatically. The pairing POST requires `android:usesCleartextTraffic="true"` since the desktop's pairing server runs plain HTTP.

The QR code carries a list of desktop address *candidates* (see [QR pairing payload](#qr-pairing-payload)), and the phone works through them in a deliberate order: LAN candidates first, sent over the phone's actual Wi-Fi network via `Network.openConnection()` rather than whatever holds the default route, then every candidate again over the default network. That first pass is what makes pairing work with a VPN running on the phone - a VPN owns the default route, so a LAN address goes nowhere through it, while the Wi-Fi interface underneath still reaches the desktop as long as the VPN permits local-network traffic. Only the pairing request is bound this way; the process is never pinned to Wi-Fi. Attempts are capped at 2s each and 12s in total, so a full candidate list can't leave the user watching nothing happen for half a minute; anything not reached by then is reported as untried rather than silently dropped. If nothing answers, a dialog lists each address tried and how it failed, and names the two situations the phone can't work around: a VPN that blocks LAN traffic outright, and client-isolated guest Wi-Fi - both of which leave USB pairing as the way through. Pairing logic that doesn't need Android (payload parsing/validation, attempt ordering, the failure text) lives in `Pairing.kt` and is unit-tested.

In USB mode the desktop can pair without a QR scan at all: it pushes the same payload via `adb shell am broadcast` to a dedicated intent, registered exported but gated on the `DUMP` permission - held by `adb shell` by default, but not obtainable by ordinary third-party apps, so only adb (not another app on the phone) can trigger it. Either pairing path rotates the token, revoking whatever was paired before, and stops an in-progress stream rather than leaving it enforcing a token that's no longer valid. Unpairing from the phone now asks for confirmation first rather than clearing the token on a single tap.

A **Copy Diagnostics** button copies app version, device info, current stream state, and recent state transitions/errors to the clipboard, for pasting into a bug report. Never includes the pairing token, a URL, or raw config.

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
| `REQUEST_IGNORE_BATTERY_OPTIMIZATIONS` | Prompt to exempt app from battery restrictions on first launch |

</details>

<details>
<summary>🖥️ <b>Desktop app</b></summary>

## Desktop app

### Stack

| Component | Library |
|---|---|
| UI | PyQt6 |
| MJPEG decode | opencv-python (`cv2.imdecode`), read via `telescope/mjpeg_reader.py`'s authenticated reader - not `cv2.VideoCapture`, which has no way to attach the bearer token |
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

The `start.sh` script handles pip dependencies automatically. Once the package above is installed, use the **System Setup** dialog's Load Module button, or run manually:

```bash
sudo modprobe v4l2loopback devices=2 video_nr=10,11 \
  card_label="OBS Virtual Camera,Phone Camera" exclusive_caps=1
```

To persist across reboots, tick **Keep this config after reboot** in the System Setup dialog -
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

The release zip bundles the UnityCapture DLLs already; the app registers them from the System Setup dialog on first run. Running from a source checkout instead (contributors), `start.bat` installs pip dependencies and downloads+registers the DLLs on first run - it isn't part of the release zip, since the packaged EXE needs neither step.

### Key implementation notes (for contributors)

**Plugin system:** The app is built around `TelescopePlugin` - a base class with hooks for `setup()`, `create_panel()`, `process_frame()`, `on_stream_start/stop()`, `on_phone_state()`, and `get/set_config()`. Plugins are registered in `main.py` in order; each creates one UI card. An `EventBus` (QObject with Qt signals) handles cross-plugin communication.

**Window layout:** A plugin declares a `panel_region` (`"left"`, `"right"` or `"center"`) and the window routes its panel there, so no plugin knows where it physically lands. Wide windows get three columns - setup rails either side of the video stage; below ~1300px the rails fold together, and below ~900px everything stacks into one scrolling column. A plugin can also contribute a header control via `create_header_widget()` (the Connection plugin puts the device picker there) or entries in the header's settings menu via `create_menu_actions()` (how System Setup and the Quick Start Guide are reached, since neither is something you adjust mid-stream).

**Theming:** `telescope/theme.py` owns the entire look - palette constants, a dark `QPalette` so Qt-drawn chrome matches, and one stylesheet, applied over Fusion by `apply_theme()`. There are no image assets: icons are drawn procedurally with `QPainter` (`create_vector_icon`), and segmented toggles are ordinary radios/checkboxes carrying a `segmented` property the stylesheet picks up, so exclusivity and signal wiring stay plain Qt.

**Frame pipeline:** `StreamWorker` holds a list of `process_frame` callables (one per plugin). Each frame passes through the full pipeline on the reader thread. `_fit_frame()` then letterboxes/pillarboxes the result to the fixed vcam canvas size, preserving aspect ratio with black bars.

**Canvas size:** The vcam canvas (`pyvirtualcam.Camera` dimensions) is set at stream start from `SetupPlugin.get_canvas_dims()`. It's independent of the phone feed decode resolution. Changing it requires restarting the stream (and reloading v4l2loopback on Linux). `_fit_frame()` handles any mismatch between the processed frame size and the canvas.

**Clean stop/restart:** `_stop()` disconnects the worker's status signal before requesting stop, preventing the old worker's eventual `"idle"` emission from clobbering the new worker's state after a canvas restart. Both Linux and Windows `restart_vcam_canvas()` wait for the old `QThread` to fully exit (via `QThread.wait()`) before starting the new one, avoiding pyvirtualcam slot conflicts.

**Linux loopback reload:** `v4l2_reload()` runs `modprobe -r v4l2loopback && sleep 0.5 && modprobe v4l2loopback ...` as a single `pkexec sh -c "..."` invocation so there is only one password prompt for the full unload+reload cycle.

**Live transform:** Plugin attributes like `flip_h`, `rotation`, `zoom` are plain Python instance attributes updated by the UI thread and read each frame by the worker thread. Python's GIL makes bool/float writes atomic at this granularity, so no lock is needed.

**Live FPS change:** Changing FPS requires recreating the `pyvirtualcam.Camera` context (constructed with fixed fps). The worker holds a `threading.Event` (`_restart_vcam`). When set, the inner vcam loop breaks, the context closes, and the outer loop re-enters with new parameters.

**Live resolution change:** Unlike FPS, mid-stream resolution changes don't require a vcam restart. The reader thread reads `self._width`/`self._height` dynamically each frame, and `_fit_frame()` adapts the output to the fixed canvas dimensions.

**Auto-reconnect:** If `cap.read()` fails, the stream reader calls `_reconnect_cap()`, which loops with a 3-second delay until the stream comes back. The pyvirtualcam context stays open during reconnect so the virtual camera doesn't disappear from OBS. Every plugin's current settings (ISO, WB, JPEG quality, etc.) are resent to the phone right after a successful reconnect, since the phone has no way to know its control state might be stale.

**Genuine-connection signal:** `EventBus.stream_connected` fires only when `StreamWorker` reports its first `"ok"` status (an actual frame decoded), not merely when a worker object exists. `ConnectionPlugin` uses it to tell "worker started" apart from "phone actually responded" for its pair-status indicator, so a stale token doesn't get shown as a healthy pairing while the worker silently retries forever.

**Desktop address discovery:** `ip_utils.get_pairing_addresses()` enumerates the machine's real network adapters (via `ifaddr`) instead of asking the routing table where a public address would go. The old UDP "route probe" reported whichever interface owns the default route - which under a VPN is the VPN's, so the physical LAN address the phone can actually reach went missing from the QR code exactly when it mattered. Loopback, link-local (`169.254/16`) and IPv6 addresses are dropped, as are container/VM-only adapters (`docker*`, `br-*`, `veth<hex>`, `virbr*`, `vboxnet*`, `vmnet*`, VirtualBox/VMware host-only) - though Windows' `vEthernet (...)` is kept, since Hyper-V bridges the host's real LAN through it. What's left is classified `lan` (RFC 1918), `tailscale` (`100.64/10`) or `other`, and ordered that way: the physical LAN path first, Tailscale as the cross-network fallback. Recognisable tunnel adapters (`tun*`, `wg*`, `utun*`, …) are still advertised but sorted behind physical ones in the same class, so a desktop VPN handing out a `10.x` address doesn't push the real LAN address down the phone's list. The whole thing is capped at 8 candidates with interface names trimmed to 32 characters, since every byte adds modules to a code someone has to scan with a phone camera. The pairing dialog lists what it's advertising and stays open after a failed attempt so the list can be compared against the phone's.

**Which phone address to stream to:** a phone reports every IPv4 address it has, and picking from that list by rank alone gets it wrong in both directions - a phone on a tailnet this desktop isn't on has its (unreachable) Tailscale address preferred over its Wi-Fi one, while two devices sharing only a tailnet need exactly the opposite. `PairingResult.source_ip` settles it without guessing: the address the pairing POST arrived from is one of the phone's *and* demonstrably reachable from here, right now, over whatever path the phone found. `_on_device_paired()` pins it as the device's `active_ip`, overriding both the rank-based default and a stale choice saved from an earlier pairing. It's ignored when it isn't one of the addresses the phone reported - USB pairing arrives through the `adb reverse` tunnel, so the source is this machine's own loopback - and the other addresses stay in the dropdown to switch to by hand.

**Re-pair mid-stream:** pairing a device rotates its bearer token, which the phone's already-running server would otherwise keep rejecting since it read the old token once at startup. `_on_device_paired` stops an active desktop stream first when this happens (matched on the Android side: a successful re-pair also stops the phone's own running stream).

**Control client:** `PhoneControlClient` runs a single background worker thread that POSTs each command as a JSON body to `/control`, in the order it was queued. Requests that share the same `action` are coalesced to just the latest value while still waiting to be sent - a burst of slider drags can't have an older request's response arrive after a newer one - except camera switches, which are always sent individually and in order. Failures are silently dropped - a missed control command is non-critical.

**ISO/shutter sliders:** Log scale over 2000 steps across the range the phone reports per camera. Range updates when switching lenses. Shutter spinbox shows milliseconds while the API uses nanoseconds.

**White balance:** Linear Kelvin slider 2000-10000 K plus a green-magenta tint slider (-150..+150). `_kelvin_to_rggb()` converts both to Camera2 RGGB channel gains with an exponential model centred at ~5500 K (not a lookup table), sent via the `wb_gains` action and applied with `COLOR_CORRECTION_MODE_TRANSFORM_MATRIX` / `COLOR_CORRECTION_GAINS`. Reverting to auto restores `CONTROL_AWB_MODE_AUTO`.

**Per-device config:** Camera, stream-output, transform, and monitoring settings serialize to `telescope_config.json` with a 500ms debounce. Connection settings and virtual-camera canvas settings are global. The `devices` dict is keyed by device name; switching devices saves the current device's settings before loading the new one's. There is no cross-version migration - a config from an older format is backed up as `telescope_config.json.invalid-<timestamp>` and replaced with defaults on next load.

**Single-instance:** `acquire_single_instance()` tries to bind a local TCP socket on port 47823. If already bound, it signals the running instance to restore its window and exits.

**Battery/temperature polling:** A `QTimer` fires every 15 seconds while streaming. Notifications fire once per threshold crossing with 5-degree/5-percent hysteresis to avoid repeated alerts.

</details>

<details>
<summary>📡 <b>Control API reference</b></summary>

## Control API reference

Server is on the phone at port 8080 for `/v1/video`, `/v1/state`, and `/v1/control` (all only exist while actively streaming); a separate responder on port 8766 serves `/v1/ping` and `/v1/session`. Every request below requires an `Authorization: Bearer <token>` header carrying the token issued during pairing; missing or mismatched tokens get `401`.

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
  "stream_width": 1920,
  "stream_height": 1080,
  "battery": 87,
  "charging": false,
  "battery_temp_c": 32.5
}
```

`minFocusDistance`, `aeCompMin`/`aeCompMax`/`aeCompStep` are per-lens, reported by Camera2 (`aeCompStep` is typically `0.167` = 1/6 EV). `wb_r`/`wb_ge`/`wb_go`/`wb_b` are the current RGGB channel gains when `wb_manual` is true, `null` otherwise. `supportedSizes` is the lens's actual list of capture sizes, which the desktop uses to populate its resolution dropdown instead of a fixed list. `stream_width`/`stream_height` are the current lens's live capture size.

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
| `focus_mode` | `value=continuous\|manual` | Switch autofocus / manual focus |
| `focus_distance` | `value=<float diopters>` | Set manual focus distance |
| `ae_comp` | `value=<int steps>` | Set exposure compensation, in the lens's AE-compensation steps (see `aeCompStep`) |
| `nr_mode` | `value=<int 0-4>` | Set noise reduction mode (desktop UI only offers 0/1/2 = Off/Fast/High Quality) |
| `edge_mode` | `value=<int 0-3>` | Set sharpening/edge mode (desktop UI only offers 0/1/2 = Off/Fast/High Quality) |
| `black_level_lock` | `value=1\|0` | Toggle black level lock |
| `torch` | `value=1\|0` | Toggle flash/torch |
| `jpeg_quality` | `value=<int 1-100>` | Set JPEG quality on the phone (desktop UI restricts to 50-100) |
| `fps_target` | `value=<int 1-120>` | Set capture FPS on the phone (desktop UI restricts to 5-60) |

All responses: `{"ok": true}` or `{"ok": false, "error": "..."}`.

> **Manual exposure note:** `CONTROL_AE_MODE_OFF` only activates when *both* ISO and shutter are set and the selected camera reports `supportsManualSensor` - `CONTROL_MODE` itself stays `CONTROL_MODE_AUTO` throughout, so autofocus keeps running independently of manual exposure. The desktop app sends both ISO and shutter simultaneously when switching to manual mode.

### `GET /v1/ping`

Served on a separate port, 8766, by `SessionServer` - unlike the three endpoints above, it exists whether or not a stream is running (while the app's main screen is up, or while the camera service is running, or both). Same bearer-token auth. Returns `200` if the token matches, `401` if it doesn't.

```json
{
  "protocol": 1,
  "streaming": false,
  "busy": false,
  "localOnly": true
}
```

The status code alone still carries the pairing verdict, so a desktop that only reads it keeps working. The body tells a newer one what the phone is actually doing: `streaming` is a live stream, `busy` is a start in flight (camera opening, session configuring), and `localOnly` mirrors the app's **Local only - USB** setting, so the desktop can name that mismatch instead of timing out against an address nothing is listening on.

### `POST /v1/session`

Also on 8766. JSON body `{"action": "start"}` or `{"action": "stop"}`; same auth and the same `{"ok": true}` / `{"ok": false, "error": "..."}` responses as `/v1/control`. This is what makes the desktop's Start button sufficient on its own.

| `action` | effect |
|---|---|
| `start` | Start the camera service, reproducing the camera/resolution/OIS selection last used on the phone. `{"ok": true}` if a stream is already running. |
| `stop` | Stop the camera service. `{"ok": true}` if nothing was running. |

Refusal reasons, all reported with HTTP `200` and `"ok": false` (the request was fine, the camera wouldn't open): `no_camera_permission`, `busy` (a start is already in flight), `start_refused` (Android declined the foreground-service start).

A start is only accepted while `SessionServer` is bound at all, i.e. the app's main screen is up or the camera service is already running - so this cannot open the camera on a phone that is both backgrounded and idle.

The desktop polls `/v1/ping` after a start until `streaming` goes true (12s budget), because the service answers as soon as the start is accepted, well before the capture session is configured. A `404` here means an APK predating this endpoint: the desktop falls back to connecting to a stream started by hand, exactly as it did before.

### QR pairing payload

Generated by the desktop (`telescope/pairing.py`), rendered as the QR code, and pushed verbatim (base64-encoded) over `adb` for USB pairing. The desktop emits version `2` only, and the app accepts version `2` only - a mismatch is reported as "update both apps" rather than "invalid code", since desktop and APK ship together.

```json
{
  "version": 2,
  "port": 8765,
  "candidates": [
    { "ip": "192.168.1.42",  "interface": "Wi-Fi",      "kind": "lan" },
    { "ip": "100.90.12.34",  "interface": "tailscale0", "kind": "tailscale" }
  ],
  "nonce": "...",
  "token": "..."
}
```

`kind` is one of `lan`, `tailscale`, `other`, and candidates are ordered best-first. The phone rejects the payload outright if any candidate carries a malformed IPv4 literal or an unrecognised `kind`, if the list is empty, or if the port/nonce/token are unusable. `interface` is the desktop-side adapter name, carried for diagnostics. USB pairing advertises a single candidate - `127.0.0.1`, kind `other` - reached through the `adb reverse` tunnel.

The phone then `POST`s to `http://<ip>:<port>/pair/<nonce>` with `{"name": ..., "ips": [...], "token": ...}`; the echoed token confirms the request came from a device that actually read the current code, on top of the one-shot nonce in the path. Both are unchanged from version 1.

The desktop also notes the source address that request arrived from. That address is, by construction, one of the phone's *and* reachable from this machine over whatever path the phone found, so it becomes the device's active address - see the implementation note below.

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

**Python runtime dependencies** (PyQt6, opencv-python, numpy, pyvirtualcam, qrcode) - installed from PyPI; exact pinned versions are in `desktop/constraints.txt`. PyQt6 in particular is GPL v3-licensed (a commercial Riverbank Computing license also exists but isn't what this project uses).

</details>

<details>
<summary>⚙️ <b>CI / GitHub Actions</b></summary>

## CI / GitHub Actions

All three workflows publish to a rolling **`latest` release** on qualifying pushes to `master`.

### `build-apk.yml` - triggered on changes to `android/**`

1. JDK 21 (Temurin) + Gradle cache
2. Android SDK (android-34, build-tools;34.0.0)
3. `./gradlew lintDebug testDebugUnitTest --no-daemon`, then `./gradlew assembleDebug --no-daemon`
4. Publishes `Telescope.apk` to the `latest` release

### `build-windows.yml` - triggered on changes to `desktop/**`

1. Python 3.11 + pip cache
2. `pip install -r requirements-dev.txt -c constraints.txt`; runs `pytest`
3. `pip install -r requirements.txt pyinstaller -c constraints.txt`
4. `python scripts/smoke_check.py` - packaging smoke checks (see below)
5. `pyinstaller telescope.spec`
6. Assembles `Telescope-windows.zip`: EXE + `THIRD_PARTY_NOTICES.txt` + `platform-tools/` + `unitycapture/`, then verifies the bundle contains all required files before publishing
7. Publishes the zip to the `latest` release

`telescope.spec` uses `collect_all('PyQt6')` to include Qt platform plugins that PyInstaller's default analysis misses. Expected EXE size: 60-80 MB.

### `build-linux.yml` - triggered on changes to `desktop/**`

1. Python 3.11 + pip cache; apt-installs `libegl1 libgl1 libxkbcommon0 libdbus-1-3` (PyQt6 needs these even in headless/offscreen test mode); installs `requirements-dev.txt` via `constraints.txt`; runs `pytest`
2. `python3 scripts/smoke_check.py` - packaging smoke checks
3. Assembles `Telescope-linux.tar.gz`: `main.py` + `telescope/` package + `requirements.txt` + `constraints.txt` + `start.sh` + `THIRD_PARTY_NOTICES.txt`
4. Publishes the tarball to the `latest` release

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
| `/dev/video11` gone after reboot | v4l2loopback not persistent | Follow the `dracut` / `modules-load.d` steps above |
| pyvirtualcam fails to open (Linux) | Module not loaded, or not installed | Click **Setup Drivers & APK** -> Load Module. If it says the module isn't installed, install `v4l2loopback-dkms` (Debian/Ubuntu/Arch) or `v4l2loopback` (Fedora/Nobara, via RPM Fusion) first |
| pyvirtualcam fails to open (Windows) | UnityCapture not registered | Click **Setup Drivers & APK** -> Install Driver |
| "v4l2loopback conflict" when starting | Some other app (OBS's own virtual camera, a previous session, etc.) already has the module loaded with different settings | Close that app, or run `sudo modprobe -r v4l2loopback` yourself, then click Start again |
| Canvas restart fails with "module in use" | OBS or another app still holds the device | Close all apps using the virtual camera, then retry |
| Camera control panel never appears | Phone HTTP server slow to start | App retries 3x over 6s; check USB debugging is active |
| WB slider has no effect | Camera doesn't support `MANUAL_POST_PROCESSING` | Falls back gracefully; auto AWB still works |
| ISO/shutter change has no effect | Only one of the two was sent | Switch to Manual - desktop sends both simultaneously |
| High latency over Wi-Fi | MJPEG is per-frame JPEG, higher bandwidth than H.264 | Use USB mode, lower JPEG quality, or reduce phone FPS |
| Second launch does nothing | Single-instance enforcement | The existing window is brought to the front |
| QR pairing fails ("Could not reach the desktop") | Phone and desktop not on the same network, or desktop firewall blocking port 8765 | The failure dialog on the phone lists every address it tried and how each failed; the desktop dialog stays open showing the addresses it's advertising, so the two lists can be compared. Make sure both are on the same Wi-Fi; the pairing server only runs while the QR dialog is open |
| QR pairing fails on a guest/public Wi-Fi | Client isolation - the access point blocks device-to-device traffic entirely | Nothing on either device can work around this; use USB pairing, or a network you control |
| "Pair via ADB" fails or times out | `adb` unavailable, phone app not foregrounded, or the adb reverse tunnel didn't come up | Install Android platform-tools on Linux, or use the bundled Windows release, then retry; make sure the Telescope app is open and in the foreground on the phone before clicking **Pair via ADB** |
| QR pairing fails while a VPN is active | The VPN is blocking local-network traffic outright. (A VPN that *allows* LAN access is handled: the desktop advertises its real interface addresses rather than whatever owns the default route, and the phone sends LAN attempts over its Wi-Fi interface rather than the tunnel) | Turn on the VPN's "allow local network access"/"LAN access" option, pause the VPN while pairing, or use USB pairing. Once paired, streaming has the same requirement |
| QR scanner opens in landscape | Manifest override not applied | The app overrides ZXing's default orientation to portrait; rebuild if you see this on an old build |

</details>
