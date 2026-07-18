# Telescope

Stream your Android phone's camera - including telephoto and wide-angle lenses - to a virtual webcam on Linux or Windows. Camera controls (ISO, shutter, white balance, lens selection) are exposed over a local HTTP API so the desktop app can drive them live.

---

## Quick Start

### 1. Install the Android app

**Easiest:** connect your phone via USB with [USB debugging enabled](https://developer.android.com/studio/debug/dev-options), set up the desktop app first (step 2), then click **Setup Drivers & APK -> Install**. It runs `adb install` for you automatically if `Telescope.apk` is next to the desktop app.

**Manually:** download `Telescope.apk` from the [latest release](../../releases) and either:

```bash
adb install Telescope.apk
```

Or sideload it from your phone's file manager with "Install unknown apps" enabled.

### 2. Set up and run the desktop app

**Linux** - download `Telescope-linux.tar.gz` from the [latest release](../../releases), extract it, and run:
```bash
./start.sh
```

The script installs Python dependencies automatically. On first launch, open **System Setup** to load the v4l2loopback kernel module if it isn't already active.

For USB mode you also need `adb` on your PATH (`sudo apt install adb`, `sudo dnf install android-tools`, or `sudo pacman -S android-tools`). **USB debugging must also be enabled** on your phone - see [Configure on-device developer options](https://developer.android.com/studio/debug/dev-options).

**Windows** - download `Telescope-windows.zip` from the [latest release](../../releases) and extract it anywhere:

```
Telescope-windows/
  TelescopeDesktop.exe          <- self-contained, no Python needed
  start.bat                    <- alternative launcher if you have Python
  platform-tools/
    adb.exe                    <- used automatically for USB mode
    ...
  unitycapture/
    UnityCaptureFilter32.dll   <- registered as a virtual camera by the app
    UnityCaptureFilter64.dll
```

Run `start.bat` (installs Python deps and launches the app) or `TelescopeDesktop.exe` directly. The app will detect and register the virtual camera driver on first launch via the System Setup dialog.

### 3. Connect your phone

**Wi-Fi mode - QR pairing:**

1. On the desktop app, select **Wi-Fi** mode and click the Pair button next to the device selector.
2. A QR code appears. In the Telescope app on your phone, tap the scan button in the top-right corner and scan it.
3. The phone is added to your device list automatically. Close the pairing dialog.

**USB mode - Pair via ADB:**

1. On the desktop app, select **USB** mode and click the Pair button next to the device selector (it switches to a USB icon and opens the same dialog).
2. Click **Pair via ADB** in the dialog. Instead of scanning a code, this pushes the pairing request to the phone over `adb shell am broadcast`.
3. If the phone doesn't respond within 8 seconds, the dialog explains why and re-enables the button - make sure the Telescope app is open and in the foreground on the phone, then click **Pair via ADB** again.

The gear button next to the device selector opens the device list; its **Pair...** button opens this same pairing dialog to add another device - there's no separate manual name/IP entry, since a device is only usable once it actually has a token. A status label next to the Pair button (**Paired** / **Not paired** / **Unreachable** / **Checking...**) shows whether the phone actually accepts the currently stored token right now, polling every few seconds, and pins to Paired once a stream is confirmed connected. Re-pairing a device rotates its token; if a stream is currently running against that device, re-pairing stops it rather than leaving it silently broken.

**Then:**

1. Open the Telescope app on your phone, pick a camera and resolution, tap **Start Streaming**.
   - Android will prompt to disable battery optimization if not already exempted. Allow it so the service isn't killed in the background.
   - Once streaming, the status card shows your Wi-Fi and USB URLs. Tap either one to copy it.
2. On the desktop app, select your device and connection mode, then press **Start Streaming**.
3. The camera control panel (lens picker, ISO, shutter, white balance, OIS) will populate within ~2 seconds of connecting.
4. In OBS (or any other app), select **Phone Camera** (Linux) or **Unity Video Capture** (Windows) as your webcam source.

> [!NOTE]
> Pairing is required before a remote stream can be used: the phone's `/v1/state`, `/v1/video`, `/v1/control`, and `/v1/ping` endpoints all require a bearer token that's only ever handed to a phone via a scanned QR code (Wi-Fi) or an adb broadcast restricted to the Telescope app (USB), so an unpaired device on the same network can't view the stream or send controls. The connection itself is still plain HTTP, not HTTPS - the token stops casual unauthorized access but doesn't provide confidentiality against a network observer. On public or shared networks, enable **Local only - USB** in the Android app to also bind the server to localhost only, so the stream and controls are reachable via USB alone.

---

## Features

**Camera control**
- Lens picker: switches between wide, main, and telephoto sensors (physical sub-cameras, not digital zoom)
- Manual ISO and shutter speed with log-scale sliders and direct numeric entry; range updates per-lens
- Manual white balance (2000-8000 K slider) with named presets (Daylight, Incandescent, etc.) - *partially working: applies inconsistently depending on device/lens*
- OIS toggle
- Controls are greyed out per-lens if the camera hardware reports it doesn't support them

**Stream transforms** (applied on the desktop, no phone restart needed)
- Horizontal and vertical flip
- Rotation: 90 CW, 180, 90 CCW
- Software zoom 1-5x with pan X/Y sliders (center crop + resize)
- Output resolution downscale: pass-through, 1080p, 720p, 480p, 360p
- Virtual camera FPS (1-120)

**Canvas size control** (Advanced, in System Setup)
- Set the virtual camera canvas independently of the phone feed resolution
- Presets: 720p/1080p/4K in 16:9 landscape and portrait, XGA and UXGA in 4:3, or fully custom
- On Linux: reloads v4l2loopback in a single elevated prompt (close OBS first); stream restarts automatically
- On Windows: stops and restarts the stream with the new canvas size

**Bandwidth controls**
- JPEG quality slider (50-100%) - controls compression on the phone
- Phone FPS target (5-60 fps) - controls capture rate on the phone
- Both take effect immediately without restarting the stream

**Monitoring**
- Live FPS display in the footer while streaming
- Battery level and phone temperature polled every 15 seconds, shown in the footer with color coding
- Configurable battery alert threshold (default 20%) - fires a tray/desktop notification when discharging below it
- Configurable temperature alert threshold (default 45 C) - fires a notification when exceeded

**Multi-device and config persistence**
- USB mode targets a specific ADB serial: if exactly one authorized device/emulator is connected it's picked automatically, if more than one is connected you're prompted to choose which one (avoids `adb: more than one device/emulator` failures on forward/install)
- Named device list in Wi-Fi mode: add/remove/edit devices via the gear button popup; switch between them with a dropdown
- Each device stores multiple IPs; a second dropdown selects the active IP. Tailscale IPs (100.64.0.0/10) are ranked first, LAN IPs second
- Pairing: click the Pair button on the desktop to open the pairing dialog - a scannable QR code in Wi-Fi mode, or a "Pair via ADB" button in USB mode that pushes the request over adb instead. Either way the phone is registered automatically with all its IPs. A status label next to the Pair button shows live reachability (Paired / Not paired / Unreachable / Checking...), not just whether a token happens to be saved
- All settings (resolution, fps, flip, rotation, exposure, zoom, quality, alert thresholds, canvas size, etc.) are saved per device to `telescope_config.json` and restored on next launch
- The config format is not migrated across versions: an unsupported or malformed config is backed up alongside the real one and replaced with defaults rather than carrying compatibility code for old formats. Each section (connection/plugin settings, per-device settings, selected device) is validated independently, so one malformed section resets to defaults without discarding the rest

**Privacy**
- Local only mode: binds the server to `127.0.0.1` so the stream is unreachable from the network; only USB works in this mode
- Toggle in the Android app restarts the stream automatically to apply the change
- Switching between USB and Wi-Fi mode on the desktop also restarts the stream automatically

**System integration**
- Minimizes to system tray on close only when streaming; otherwise quits
- Right-click the tray icon to quit, or click it to show/hide the window
- Launching a second instance brings the existing window to the front
- Battery/temperature notifications use `notify-send` on Linux (if available) or the system tray on Windows

---

## Why

Most Android camera streaming solutions either lock you to a specific app ecosystem, use ADB screen mirroring which blocks the back camera on some devices, or route through OBS to create the virtual camera - which is a problem if you need OBS free for its own output. Telescope runs as a self-contained foreground service that serves MJPEG directly and exposes camera controls as a simple REST API, leaving OBS (or any other capture tool) completely unencumbered.

---

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
            connection              device list, IP dropdown, pairing dialog (pairing.py, port 8765) + pair-status probe (port 8766)
            camera_control          lens, ISO, shutter, WB, OIS
            stream_output           resolution, FPS, JPEG quality
            transforms              flip, rotation, zoom, pan
            preview                 in-card and pop-out video preview
            monitoring              battery, temperature alerts
```

A second, always-on responder on the phone (`PingServer`, port 8766) answers `GET /v1/ping` independently of the streaming server, so the desktop can confirm pairing status without a stream running.

On **Linux**, two `v4l2loopback` devices are created (`/dev/video10` and `/dev/video11`). Telescope writes to `video11`; `video10` is intentionally left free for other software (e.g. OBS Virtual Camera).

On **Windows**, the virtual camera is [UnityCapture](https://github.com/schellingb/UnityCapture) - a standalone DirectShow filter, no OBS required.

---

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
|       |-- MjpegServer.kt       # Authenticated HTTP: /v1/video  /v1/state  /v1/control
|       |-- PingServer.kt        # Always-on pairing-status responder: GET /v1/ping (port 8766)
|       +-- TokenStore.kt        # Persists the single active pairing bearer token
|
+-- desktop/
    |-- main.py                  # Entry point: registers plugins, restores config
    |-- requirements.txt         # Readable ">=" lower bounds
    |-- constraints.txt          # Exact pinned versions for CI/release installs
    |-- scripts/smoke_check.py   # Packaging smoke checks (see CI section below)
    |-- THIRD_PARTY_NOTICES.txt  # Bundled into both release archives
    |-- telescope.spec            # PyInstaller spec for Windows EXE
    |-- start.sh                 # Linux launcher (creates/reuses a Telescope-owned venv)
    |-- start.bat                # Windows launcher (auto-installs deps)
    |-- platform-tools/          # Bundled adb for Windows
    |-- unitycapture/            # Bundled UnityCapture DLLs (MIT)
    +-- telescope/
        |-- app.py               # TelescopeWindow: plugin host, stream lifecycle
        |-- stream.py            # StreamWorker: MJPEG -> pipeline -> pyvirtualcam
        |-- mjpeg_reader.py      # Authenticated multipart-MJPEG reader (replaces cv2.VideoCapture)
        |-- session.py           # StreamSession: owns worker/client for one connect-to-disconnect lifecycle
        |-- plugin.py            # TelescopePlugin base class, EventBus, HostServices protocol
        |-- config.py            # Versioned JSON config (v2) with per-section validation
        |-- models.py            # Typed contracts: PhoneState, CameraCapabilities, DeviceProfile, StreamSettings
        |-- phone_client.py      # Authenticated HTTP client for /v1/state and /v1/control
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
            |-- common.py        # NoScroll*, LogSliderRow, separators, icons
            +-- lens_panel.py    # Lens picker widget
```

---

## Android app

### What it does

Runs a **foreground service** (declared type `camera`, required on Android 14+) that owns a Camera2 session and an HTTP server on port 8080. Three endpoints, all requiring a bearer token issued during pairing:

- `GET /v1/video` - MJPEG stream (`multipart/x-mixed-replace`)
- `GET /v1/state` - JSON of all detected cameras + current exposure/WB/battery state
- `POST /v1/control` - live camera control, JSON body

A separate, always-on HTTP responder (`PingServer`, port 8766) runs independently of the streaming service, tied to the main screen's own lifecycle rather than the foreground service - `GET /v1/ping` checks the request's bearer token against the currently stored pairing token and returns 200 or 401. This lets the desktop confirm the phone is reachable and correctly paired before a stream is ever started, not just whether a token happens to be saved locally.

The app enumerates **physical sub-cameras** of logical multi-camera groups via `CameraCharacteristics.physicalCameraIds` (API 28+). On many modern phones the logical back camera (ID `0`) hides individual wide/main/telephoto sensors behind it; this app surfaces all of them and lets you pick.

A **scan button** in the top-right corner of the main screen opens a ZXing barcode scanner (portrait, via `journeyapps:zxing-android-embedded`). Scanning the QR code shown by the desktop app sends the phone's name and all its IPv4 addresses to the desktop over HTTP, which adds it as a named device automatically. The pairing POST requires `android:usesCleartextTraffic="true"` since the desktop's pairing server runs plain HTTP.

In USB mode the desktop can pair without a QR scan at all: it pushes the same payload via `adb shell am broadcast` to a dedicated intent, registered exported but gated on the `DUMP` permission - held by `adb shell` by default, but not obtainable by ordinary third-party apps, so only adb (not another app on the phone) can trigger it. Either pairing path rotates the token, revoking whatever was paired before, and stops an in-progress stream rather than leaving it enforcing a token that's no longer valid. Unpairing from the phone now asks for confirmation first rather than clearing the token on a single tap.

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

---

## Desktop app

### Stack

| Component | Library |
|---|---|
| UI | PyQt6 |
| MJPEG decode | opencv-python (`cv2.VideoCapture`) |
| Virtual camera output | pyvirtualcam |
| Frame processing | numpy |
| QR code generation | qrcode (rendered via QPainter, no Pillow) |

### One-time setup (detailed)

**Linux:**

The `start.sh` script handles pip dependencies automatically. For the virtual camera driver, use the **System Setup** dialog's Load Module button, or run manually:

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

`start.bat` installs pip dependencies and registers the UnityCapture DLLs (bundled in `desktop/unitycapture/`) on first run. The app can also do this from the System Setup dialog.

### Key implementation notes (for contributors)

**Plugin system:** The app is built around `TelescopePlugin` - a base class with hooks for `setup()`, `create_panel()`, `process_frame()`, `on_stream_start/stop()`, `on_phone_state()`, and `get/set_config()`. Plugins are registered in `main.py` in order; each creates one UI card. An `EventBus` (QObject with Qt signals) handles cross-plugin communication.

**Frame pipeline:** `StreamWorker` holds a list of `process_frame` callables (one per plugin). Each frame passes through the full pipeline on the reader thread. `_fit_frame()` then letterboxes/pillarboxes the result to the fixed vcam canvas size, preserving aspect ratio with black bars.

**Canvas size:** The vcam canvas (`pyvirtualcam.Camera` dimensions) is set at stream start from `SetupPlugin.get_canvas_dims()`. It's independent of the phone feed decode resolution. Changing it requires restarting the stream (and reloading v4l2loopback on Linux). `_fit_frame()` handles any mismatch between the processed frame size and the canvas.

**Clean stop/restart:** `_stop()` disconnects the worker's status signal before requesting stop, preventing the old worker's eventual `"idle"` emission from clobbering the new worker's state after a canvas restart. Both Linux and Windows `restart_vcam_canvas()` wait for the old `QThread` to fully exit (via `QThread.wait()`) before starting the new one, avoiding pyvirtualcam slot conflicts.

**Linux loopback reload:** `v4l2_reload()` runs `modprobe -r v4l2loopback && sleep 0.5 && modprobe v4l2loopback ...` as a single `pkexec sh -c "..."` invocation so there is only one password prompt for the full unload+reload cycle.

**Live transform:** Plugin attributes like `flip_h`, `rotation`, `zoom` are plain Python instance attributes updated by the UI thread and read each frame by the worker thread. Python's GIL makes bool/float writes atomic at this granularity, so no lock is needed.

**Live FPS change:** Changing FPS requires recreating the `pyvirtualcam.Camera` context (constructed with fixed fps). The worker holds a `threading.Event` (`_restart_vcam`). When set, the inner vcam loop breaks, the context closes, and the outer loop re-enters with new parameters.

**Live resolution change:** Unlike FPS, mid-stream resolution changes don't require a vcam restart. The reader thread reads `self._width`/`self._height` dynamically each frame, and `_fit_frame()` adapts the output to the fixed canvas dimensions.

**Auto-reconnect:** If `cap.read()` fails, the stream reader calls `_reconnect_cap()`, which loops with a 3-second delay until the stream comes back. The pyvirtualcam context stays open during reconnect so the virtual camera doesn't disappear from OBS.

**Genuine-connection signal:** `EventBus.stream_connected` fires only when `StreamWorker` reports its first `"ok"` status (an actual frame decoded), not merely when a worker object exists. `ConnectionPlugin` uses it to tell "worker started" apart from "phone actually responded" for its pair-status indicator, so a stale token doesn't get shown as a healthy pairing while the worker silently retries forever.

**Re-pair mid-stream:** pairing a device rotates its bearer token, which the phone's already-running server would otherwise keep rejecting since it read the old token once at startup. `_on_device_paired` stops an active desktop stream first when this happens (matched on the Android side: a successful re-pair also stops the phone's own running stream).

**Control client:** `PhoneControlClient` sends `GET /control?...` in a daemon thread per request, fire-and-forget. Failures are silently dropped - a missed control command is non-critical.

**ISO/shutter sliders:** Log scale over 2000 steps across the range the phone reports per camera. Range updates when switching lenses. Shutter spinbox shows milliseconds while the API uses nanoseconds.

**White balance:** Linear Kelvin slider 2000-8000 K. Translates to `RggbChannelVector` using the Tanner Helland K->RGB algorithm and sets `COLOR_CORRECTION_GAINS`. Reverting to auto restores `CONTROL_AWB_MODE_AUTO`.

**Per-device config:** All UI settings serialize to `telescope_config.json` with a 500ms debounce. The `devices` dict is keyed by device name; switching devices saves the current device's settings before loading the new one's. There is no cross-version migration - a config from an older format is backed up as `telescope_config.json.invalid-<timestamp>` and replaced with defaults on next load.

**Single-instance:** `acquire_single_instance()` tries to bind a local TCP socket on port 47823. If already bound, it signals the running instance to restore its window and exits.

**Battery/temperature polling:** A `QTimer` fires every 15 seconds while streaming. Notifications fire once per threshold crossing with 5-degree/5-percent hysteresis to avoid repeated alerts.

---

## Control API reference

Server is on the phone at port 8080 for `/v1/video`, `/v1/state`, and `/v1/control` (all only exist while actively streaming); a separate always-on responder on port 8766 serves `/v1/ping`. Every request below requires an `Authorization: Bearer <token>` header carrying the token issued during pairing; missing or mismatched tokens get `401`.

### `GET /v1/state`

```json
{
  "cameras": [
    {
      "id": "0",
      "logicalId": null,
      "label": "Back ~24mm OIS",
      "current": false,
      "isoMin": 50,
      "isoMax": 12800,
      "shutterMinNs": 100000,
      "shutterMaxNs": 1000000000,
      "supportsManualSensor": true,
      "supportsManualWB": true,
      "hasOis": true,
      "hwLevel": "FULL"
    }
  ],
  "auto": true,
  "iso": null,
  "shutter_ns": null,
  "wb_kelvin": null,
  "ois": true,
  "battery": 87,
  "charging": false,
  "battery_temp_c": 32.5
}
```

### `POST /v1/control`

JSON body `{"action": "<action>", ...params}`.

| `action` | extra params | effect |
|---|---|---|
| `camera` | `id=<id>` | Switch camera |
| `auto` | - | Restore auto exposure |
| `iso` | `value=<int>` | Set ISO; switches AE to OFF |
| `shutter` | `value=<long ns>` | Set shutter in nanoseconds; switches AE to OFF |
| `wb_auto` | - | Restore auto white balance |
| `wb_kelvin` | `value=<int>` | Set color temperature via `COLOR_CORRECTION_GAINS` |
| `ois` | `value=1\|0` | Toggle OIS |
| `jpeg_quality` | `value=<int 50-100>` | Set JPEG quality on the phone |
| `fps_target` | `value=<int 5-60>` | Set capture FPS on the phone |

All responses: `{"ok": true}` or `{"ok": false, "error": "..."}`.

> **Manual exposure note:** `CONTROL_MODE_OFF` only activates when *both* ISO and shutter are set. The desktop app sends both simultaneously when switching to manual mode.

### `GET /v1/ping`

Served on a separate port, 8766, by an always-on responder independent of the streaming service - unlike the three endpoints above, it exists whether or not a stream is running. Same bearer-token auth. Returns `200` if the token matches, `401` if it doesn't, no body either way - used by the desktop to check pairing status before starting a stream.

---

## License

This project is licensed under [CC BY-NC 4.0](https://creativecommons.org/licenses/by-nc/4.0/) - see [LICENSE](LICENSE).
You are free to use, modify, and share it with attribution, but not for commercial purposes or monetary gain.

## Third-party components

Full notices (bundled binaries and Python runtime dependencies) are in [`desktop/THIRD_PARTY_NOTICES.txt`](desktop/THIRD_PARTY_NOTICES.txt), which ships inside both the Windows zip and the Linux tarball. Summary:

**UnityCapture** (`desktop/unitycapture/`) - DirectShow virtual camera filter for Windows.
Copyright (c) 2018 Bernhard Schelling. MIT License. See `desktop/unitycapture/LICENSE`.
Source: https://github.com/schellingb/UnityCapture

**Android SDK Platform Tools** (`desktop/platform-tools/`) - includes `adb.exe` for USB mode.
Copyright (c) Google LLC. Android Software Development Kit License Agreement.
See `desktop/platform-tools/NOTICE` and https://developer.android.com/studio/terms

**Python runtime dependencies** (PyQt6, opencv-python, numpy, pyvirtualcam, qt-material, qrcode) - installed from PyPI; exact pinned versions are in `desktop/constraints.txt`. PyQt6 in particular is GPL v3-licensed (a commercial Riverbank Computing license also exists but isn't what this project uses).

---

## CI / GitHub Actions

All three workflows publish to a rolling **`latest` release** on every push to `master`.

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
6. Assembles `Telescope-windows.zip`: EXE + `start.bat` + `THIRD_PARTY_NOTICES.txt` + `platform-tools/` + `unitycapture/`, then verifies the bundle contains all required files before publishing
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

---

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| Only 2 cameras visible | Physical sub-cameras hidden behind logical camera | Already handled via `physicalCameraIds`; if still missing, device may restrict access |
| Manual exposure greyed out | Camera doesn't report `MANUAL_SENSOR` capability | Some front cameras and telephoto lenses don't support it; use Auto |
| `/dev/video11` gone after reboot | v4l2loopback not persistent | Follow the `dracut` / `modules-load.d` steps above |
| pyvirtualcam fails to open (Linux) | Module not loaded | Click **Setup Drivers & APK** -> Load Module |
| pyvirtualcam fails to open (Windows) | UnityCapture not registered | Click **Setup Drivers & APK** -> Install Driver |
| Canvas restart fails with "module in use" | OBS or another app still holds the device | Close all apps using the virtual camera, then retry |
| Camera control panel never appears | Phone HTTP server slow to start | App retries 3x over 6s; check USB debugging is active |
| WB slider has no effect | Camera doesn't support `MANUAL_POST_PROCESSING` | Falls back gracefully; auto AWB still works |
| ISO/shutter change has no effect | Only one of the two was sent | Switch to Manual - desktop sends both simultaneously |
| High latency over Wi-Fi | MJPEG is per-frame JPEG, higher bandwidth than H.264 | Use USB mode, lower JPEG quality, or reduce phone FPS |
| Second launch does nothing | Single-instance enforcement | The existing window is brought to the front |
| QR pairing fails ("Could not reach desktop") | Phone and desktop not on the same network, or desktop firewall blocking port 8765 | Make sure both are on the same Wi-Fi; the pairing server only runs while the QR dialog is open |
| "Pair via ADB" fails or times out | `adb` not on PATH, phone app not foregrounded, or the adb reverse tunnel didn't come up | Install adb (see step 2 above) and retry; make sure the Telescope app is open and in the foreground on the phone before clicking **Pair via ADB** |
| QR pairing fails while a VPN is active | A VPN on the phone and/or desktop can route traffic off the local Wi-Fi network, or advertise an IP the other device can't actually reach | Temporarily disconnect the VPN on both devices while pairing, or pair first and reconnect the VPN afterward |
| QR scanner opens in landscape | Manifest override not applied | The app overrides ZXing's default orientation to portrait; rebuild if you see this on an old build |
