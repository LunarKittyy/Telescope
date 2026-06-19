# PhoneCam

Stream your Android phone's camera - including telephoto and wide-angle lenses - to a virtual webcam on Linux or Windows. Camera controls (ISO, shutter, white balance, lens selection) are exposed over a local HTTP API so the desktop app can drive them live.

---

## Quick Start

### 1. Install the Android app

**Easiest:** connect your phone via USB with [USB debugging enabled](https://developer.android.com/studio/debug/dev-options), set up the desktop app first (step 2), then use **System Setup → Install Phone App** — it runs `adb install` for you automatically if `PhoneCam.apk` is next to the desktop app.

**Manually:** download `PhoneCam.apk` from the [latest release](../../releases/tag/latest) and either:

```bash
adb install PhoneCam.apk
```

Or sideload it from your phone's file manager with "Install unknown apps" enabled.

### 2. Set up and run the desktop app

**Linux** - download `PhoneCam-linux.tar.gz` from the [latest release](../../releases/tag/latest), extract it, and run:
```bash
./start.sh
```

The script installs Python dependencies automatically. On first launch, open **System Setup** to load the v4l2loopback kernel module if it isn't already active.

For USB mode you also need `adb` on your PATH (`sudo apt install adb`, `sudo dnf install android-tools`, or `sudo pacman -S android-tools`), and **USB debugging must be enabled** on your phone — see [Configure on-device developer options](https://developer.android.com/studio/debug/dev-options).

**Windows** - download `PhoneCam-windows.zip` from the [latest release](../../releases/tag/latest) and extract it anywhere:

```
PhoneCam-windows/
  PhoneCamDesktop.exe          <- self-contained, no Python needed
  start.bat                    <- alternative launcher if you have Python
  platform-tools/
    adb.exe                    <- used automatically for USB mode
    ...
  unitycapture/
    UnityCaptureFilter32.dll   <- registered as a virtual camera by the app
    UnityCaptureFilter64.dll
```

Run `start.bat` (installs Python deps and launches the app) or `PhoneCamDesktop.exe` directly. The app will detect and register the virtual camera driver on first launch via the System Setup dialog.

### 3. Connect your phone

1. Open the PhoneCam app on your phone, pick a camera and resolution, tap **Start Streaming**.
   - On first launch Android will prompt to disable battery optimization. Allow it so the service isn't killed in the background.
   - Once streaming, the status card shows your WiFi and USB URLs. Tap either one to copy it.
2. On the desktop app, select **USB** or **Wi-Fi** mode and press **Start Streaming**.
3. The camera control panel (lens picker, ISO, shutter, white balance, OIS) will populate within ~2 seconds of connecting.
4. In OBS (or any other app), select **Phone Camera** (Linux) or **Unity Video Capture** (Windows) as your webcam source.

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
- Named device list in Wi-Fi mode: add/remove devices by name and IP, switch between them with a dropdown
- All settings (resolution, fps, flip, rotation, exposure, zoom, quality, alert thresholds, canvas size, etc.) are saved per device to `phonecam_config.json` and restored on next launch
- Settings from older config formats are migrated automatically

**System integration**
- Minimizes to system tray on close; streaming continues in the background
- Right-click the tray icon to quit, or click it to show/hide the window
- Launching a second instance brings the existing window to the front
- Battery/temperature notifications use `notify-send` on Linux (if available) or the system tray on Windows

---

## Why

Most Android camera streaming solutions either lock you to a specific app ecosystem, use ADB screen mirroring which blocks the back camera on some devices, or route through OBS to create the virtual camera - which is a problem if you need OBS free for its own output. PhoneCam runs as a self-contained foreground service that serves MJPEG directly and exposes camera controls as a simple REST API, leaving OBS (or any other capture tool) completely unencumbered.

---

## Architecture

```
Android device  (PhoneCam app, port 8080)
      |
      |  USB: adb forward tcp:8080 tcp:8080
      |  Wi-Fi: direct HTTP
      v
desktop/main.py  (Python, PyQt6)
      |
      |-- phonecam/stream.py       StreamWorker (QThread)
      |     reads MJPEG via cv2.VideoCapture
      |     runs frames through plugin pipeline
      |     _fit_frame() letterboxes to canvas size
      |     pyvirtualcam -> virtual camera device
      |
      +-- phonecam/plugins/        one plugin per UI card
            connection             IP/USB device selection
            camera_control         lens, ISO, shutter, WB, OIS
            stream_output          resolution, FPS, JPEG quality
            transforms             flip, rotation, zoom, pan
            monitoring             battery, temperature alerts
            setup                  driver setup, canvas settings
```

On **Linux**, two `v4l2loopback` devices are created (`/dev/video10` and `/dev/video11`). PhoneCam writes to `video11`; `video10` is intentionally left free for other software (e.g. OBS Virtual Camera).

On **Windows**, the virtual camera is [UnityCapture](https://github.com/schellingb/UnityCapture) - a standalone DirectShow filter, no OBS required.

---

## Repository layout

```
phonecam/
|-- .github/workflows/
|   |-- build-apk.yml            # CI: debug APK on ubuntu-latest
|   |-- build-windows.yml        # CI: Windows bundle (EXE + adb + UnityCapture)
|   +-- build-linux.yml          # CI: Linux bundle (source + start.sh)
|
|-- android/                     # Gradle project
|   +-- app/src/main/kotlin/com/phonecam/
|       |-- MainActivity.kt      # UI: enumerate cameras, start/stop service
|       |-- CameraStreamService.kt  # Foreground service: Camera2 + HTTP control
|       +-- MjpegServer.kt       # HTTP: /video  /cameras  /control
|
+-- desktop/
    |-- main.py                  # Entry point: registers plugins, restores config
    |-- requirements.txt
    |-- phonecam.spec            # PyInstaller spec for Windows EXE
    |-- start.sh                 # Linux launcher (auto-installs deps)
    |-- start.bat                # Windows launcher (auto-installs deps)
    |-- platform-tools/          # Bundled adb for Windows
    |-- unitycapture/            # Bundled UnityCapture DLLs (MIT)
    +-- phonecam/
        |-- app.py               # PhoneCamWindow: plugin host, stream lifecycle
        |-- stream.py            # StreamWorker: MJPEG -> pipeline -> pyvirtualcam
        |-- plugin.py            # PhoneCamPlugin base class + EventBus
        |-- config.py            # Versioned JSON config (v2) with migration
        |-- phone_client.py      # HTTP client for /video and /cameras
        |-- platform/
        |   |-- linux.py         # v4l2loopback helpers (load, unload, reload)
        |   +-- windows.py       # UnityCapture helpers
        |-- plugins/
        |   |-- connection.py
        |   |-- camera_control.py
        |   |-- stream_output.py
        |   |-- transforms.py
        |   |-- monitoring.py
        |   +-- setup.py
        +-- widgets/
            |-- common.py        # NoScroll*, LogSliderRow, separators, icons
            +-- lens_panel.py    # Lens picker widget
```

---

## Android app

### What it does

Runs a **foreground service** (declared type `camera`, required on Android 14+) that owns a Camera2 session and an HTTP server on port 8080. Three endpoints:

- `GET /video` - MJPEG stream (`multipart/x-mixed-replace`)
- `GET /cameras` - JSON of all detected cameras + current exposure/WB/battery state
- `GET /control?action=...` - live camera control

The app enumerates **physical sub-cameras** of logical multi-camera groups via `CameraCharacteristics.physicalCameraIds` (API 28+). On many modern phones the logical back camera (ID `0`) hides individual wide/main/telephoto sensors behind it; this app surfaces all of them and lets you pick.

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

---

## Desktop app

### Stack

| Component | Library |
|---|---|
| UI | PyQt6 |
| MJPEG decode | opencv-python (`cv2.VideoCapture`) |
| Virtual camera output | pyvirtualcam |
| Frame processing | numpy |

### One-time setup (detailed)

**Linux:**

The `start.sh` script handles pip dependencies automatically. For the virtual camera driver, use the **System Setup** dialog's Load Module button, or run manually:

```bash
sudo modprobe v4l2loopback devices=2 video_nr=10,11 \
  card_label="OBS Virtual Camera,Phone Camera" exclusive_caps=1
```

Persist across reboots (Fedora/Nobara/any `dracut` distro):
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

**Plugin system:** The app is built around `PhoneCamPlugin` - a base class with hooks for `setup()`, `create_panel()`, `process_frame()`, `on_stream_start/stop()`, `on_phone_state()`, and `get/set_config()`. Plugins are registered in `main.py` in order; each creates one UI card. An `EventBus` (QObject with Qt signals) handles cross-plugin communication.

**Frame pipeline:** `StreamWorker` holds a list of `process_frame` callables (one per plugin). Each frame passes through the full pipeline on the reader thread. `_fit_frame()` then letterboxes/pillarboxes the result to the fixed vcam canvas size, preserving aspect ratio with black bars.

**Canvas size:** The vcam canvas (`pyvirtualcam.Camera` dimensions) is set at stream start from `SetupPlugin.get_canvas_dims()`. It's independent of the phone feed decode resolution. Changing it requires restarting the stream (and reloading v4l2loopback on Linux). `_fit_frame()` handles any mismatch between the processed frame size and the canvas.

**Clean stop/restart:** `_stop()` disconnects the worker's status signal before requesting stop, preventing the old worker's eventual `"idle"` emission from clobbering the new worker's state after a canvas restart. Both Linux and Windows `restart_vcam_canvas()` wait for the old `QThread` to fully exit (via `QThread.wait()`) before starting the new one, avoiding pyvirtualcam slot conflicts.

**Linux loopback reload:** `v4l2_reload()` runs `modprobe -r v4l2loopback && sleep 0.5 && modprobe v4l2loopback ...` as a single `pkexec sh -c "..."` invocation so there is only one password prompt for the full unload+reload cycle.

**Live transform:** Plugin attributes like `flip_h`, `rotation`, `zoom` are plain Python instance attributes updated by the UI thread and read each frame by the worker thread. Python's GIL makes bool/float writes atomic at this granularity, so no lock is needed.

**Live FPS change:** Changing FPS requires recreating the `pyvirtualcam.Camera` context (constructed with fixed fps). The worker holds a `threading.Event` (`_restart_vcam`). When set, the inner vcam loop breaks, the context closes, and the outer loop re-enters with new parameters.

**Live resolution change:** Unlike FPS, mid-stream resolution changes don't require a vcam restart. The reader thread reads `self._width`/`self._height` dynamically each frame, and `_fit_frame()` adapts the output to the fixed canvas dimensions.

**Auto-reconnect:** If `cap.read()` fails, the stream reader calls `_reconnect_cap()`, which loops with a 3-second delay until the stream comes back. The pyvirtualcam context stays open during reconnect so the virtual camera doesn't disappear from OBS.

**Control client:** `PhoneControlClient` sends `GET /control?...` in a daemon thread per request, fire-and-forget. Failures are silently dropped - a missed control command is non-critical.

**ISO/shutter sliders:** Log scale over 2000 steps across the range the phone reports per camera. Range updates when switching lenses. Shutter spinbox shows milliseconds while the API uses nanoseconds.

**White balance:** Linear Kelvin slider 2000-8000 K. Translates to `RggbChannelVector` using the Tanner Helland K->RGB algorithm and sets `COLOR_CORRECTION_GAINS`. Reverting to auto restores `CONTROL_AWB_MODE_AUTO`.

**Per-device config:** All UI settings serialize to `phonecam_config.json` with a 500ms debounce. The `devices` dict is keyed by device name; switching devices saves the current device's settings before loading the new one's. Config v0/v1 formats are migrated to v2 automatically on first load.

**Single-instance:** `acquire_single_instance()` tries to bind a local TCP socket on port 47823. If already bound, it signals the running instance to restore its window and exits.

**Battery/temperature polling:** A `QTimer` fires every 15 seconds while streaming. Notifications fire once per threshold crossing with 5-degree/5-percent hysteresis to avoid repeated alerts.

---

## Control API reference

All requests are plain `GET`. Server is on the phone at port 8080.

### `GET /cameras`

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

### `GET /control?action=<action>[&param=value]`

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

---

## License

This project is licensed under [CC BY-NC 4.0](https://creativecommons.org/licenses/by-nc/4.0/) - see [LICENSE](LICENSE).
You are free to use, modify, and share it with attribution, but not for commercial purposes or monetary gain.

## Third-party components

**UnityCapture** (`desktop/unitycapture/`) - DirectShow virtual camera filter for Windows.
Copyright (c) 2018 Bernhard Schelling. MIT License. See `desktop/unitycapture/LICENSE`.
Source: https://github.com/schellingb/UnityCapture

**Android SDK Platform Tools** (`desktop/platform-tools/`) - includes `adb.exe` for USB mode.
Copyright (c) Google LLC. Android Software Development Kit License Agreement.
See `desktop/platform-tools/NOTICE` and https://developer.android.com/studio/terms

---

## CI / GitHub Actions

All three workflows publish to a rolling **`latest` release** on every push to `master`.

### `build-apk.yml` - triggered on changes to `android/**`

1. JDK 21 (Temurin) + Gradle cache
2. Android SDK (android-34, build-tools;34.0.0)
3. `./gradlew assembleDebug --no-daemon`
4. Publishes `PhoneCam.apk` to the `latest` release

### `build-windows.yml` - triggered on changes to `desktop/**`

1. Python 3.11 + pip cache
2. `pip install -r requirements.txt pyinstaller`
3. `pyinstaller phonecam.spec`
4. Assembles `PhoneCam-windows.zip`: EXE + `start.bat` + `platform-tools/` + `unitycapture/`
5. Publishes the zip to the `latest` release

`phonecam.spec` uses `collect_all('PyQt6')` to include Qt platform plugins that PyInstaller's default analysis misses. Expected EXE size: 60-80 MB.

### `build-linux.yml` - triggered on changes to `desktop/**`

1. Assembles `PhoneCam-linux.tar.gz`: `main.py` + `phonecam/` package + `requirements.txt` + `start.sh`
2. Publishes the tarball to the `latest` release

No build step needed - the Linux bundle is the Python source and launcher script.

---

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| Only 2 cameras visible | Physical sub-cameras hidden behind logical camera | Already handled via `physicalCameraIds`; if still missing, device may restrict access |
| Manual exposure greyed out | Camera doesn't report `MANUAL_SENSOR` capability | Some front cameras and telephoto lenses don't support it; use Auto |
| `/dev/video11` gone after reboot | v4l2loopback not persistent | Follow the `dracut` / `modules-load.d` steps above |
| pyvirtualcam fails to open (Linux) | Module not loaded | Use System Setup -> Load Module |
| pyvirtualcam fails to open (Windows) | UnityCapture not registered | Use System Setup -> Install Driver |
| Canvas restart fails with "module in use" | OBS or another app still holds the device | Close all apps using the virtual camera, then retry |
| Camera control panel never appears | Phone HTTP server slow to start | App retries 3x over 6s; check USB debugging is active |
| WB slider has no effect | Camera doesn't support `MANUAL_POST_PROCESSING` | Falls back gracefully; auto AWB still works |
| ISO/shutter change has no effect | Only one of the two was sent | Switch to Manual - desktop sends both simultaneously |
| High latency over Wi-Fi | MJPEG is per-frame JPEG, higher bandwidth than H.264 | Use USB mode, lower JPEG quality, or reduce phone FPS |
| Second launch does nothing | Single-instance enforcement | The existing window is brought to the front |
