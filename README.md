# PhoneCam

Stream your Android phone's camera - including telephoto and wide-angle lenses - to a virtual webcam on Linux or Windows. Camera controls (ISO, shutter, white balance, lens selection) are exposed over a local HTTP API so the desktop app can drive them live.

---

## Quick Start

### 1. Install the Android app

Download `PhoneCam.apk` from the [latest release](../../releases/tag/latest) and install it:

```bash
adb install PhoneCam.apk
```

Or sideload it from your phone's file manager if you have "Install unknown apps" enabled.

### 2. Set up and run the desktop app

**Linux** - download `PhoneCam-linux.tar.gz` from the [latest release](../../releases/tag/latest), extract it, and run:
```bash
./start.sh
```

That script installs Python dependencies automatically. The first launch will prompt you to load the v4l2loopback kernel module if it isn't already active - you can also do that from inside the app with the **Load module** button.

For USB mode you also need `adb` on your PATH (`sudo apt install adb`, `sudo dnf install android-tools`, or `sudo pacman -S android-tools`).

**Windows** - download `PhoneCam-windows.zip` from the [latest release](../../releases/tag/latest) and extract it anywhere. The zip contains everything needed:

```
PhoneCam-windows/
  PhoneCamDesktop.exe          <- self-contained, no Python needed
  start.bat                    <- alternative launcher if you have Python
  platform-tools/
    adb.exe                    <- used automatically for USB mode
    ...
  unitycapture/
    UnityCaptureFilter32.dll   <- registered as a virtual camera by the app on first run
    UnityCaptureFilter64.dll
```

Run `start.bat` (installs Python deps and launches the app) or `PhoneCamDesktop.exe` directly (no Python needed). The app will detect and register the virtual camera driver on first launch.

### 3. Connect your phone

1. Open the PhoneCam app on your phone, pick a camera and resolution, tap **Start**.
2. On the desktop app, select **USB** or **Wi-Fi** mode and press **Start Streaming**.
3. The camera control panel (lens picker, ISO, shutter, white balance, OIS) will populate within ~2 seconds of connecting.
4. In OBS (or any other app), select **Phone Camera** (Linux) or **Unity Video Capture** (Windows) as your webcam source.

---

## Features

**Camera control**
- Lens picker: switches between wide, main, and telephoto sensors (physical sub-cameras, not digital zoom)
- Manual ISO and shutter speed with log-scale sliders and direct numeric entry; range updates per-lens
- Manual white balance (2000-8000 K Kelvin slider) with named presets (Daylight, Incandescent, etc.) - *partially working: applies inconsistently depending on device/lens, results may not match expectations*
- OIS toggle
- Controls are greyed out per-lens if the camera hardware reports it doesn't support them

**Stream transforms (applied on the desktop, no phone restart needed)**
- Horizontal and vertical flip
- Rotation: 90 CW, 180, 90 CCW
- Software zoom 1-5x with pan X/Y sliders (center crop + resize)
- Output resolution downscale: pass-through, 1080p, 720p, 480p, 360p
- Virtual camera FPS (1-120)

**Bandwidth controls**
- JPEG quality slider (50-100%) - controls compression on the phone
- Phone FPS target (5-60 fps) - controls capture rate on the phone
- Both settings take effect immediately without restarting the stream

**Monitoring**
- Live FPS display in the footer while streaming
- Battery level and phone temperature polled every 15 seconds while streaming, shown in the footer with color coding
- Configurable battery alert threshold (default 20%) - fires a tray/desktop notification when the phone is discharging below it
- Configurable temperature alert threshold (default 45 C) - fires a notification when exceeded

**Multi-device and config persistence**
- Named device list in Wi-Fi mode: add/remove devices by name and IP, switch between them with a dropdown
- All settings (resolution, fps, flip, rotation, exposure, zoom, quality, alert thresholds, etc.) are saved per device to `phonecam_config.json` and restored on next launch
- Settings from the old single-device config format are migrated automatically

**System integration**
- Minimizes to system tray on close; streaming continues in the background
- Right-click the tray icon to quit, or click it to show/hide the window
- Launching a second instance brings the existing window to the front instead of opening a duplicate
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
      |  WiFi: direct HTTP
      v
phonecam_desktop.py  (Python, PyQt6)
      |  reads MJPEG with cv2.VideoCapture
      |  pyvirtualcam -> virtual camera device
      v
Any app that reads a webcam
```

On **Linux**, two `v4l2loopback` devices are created (`/dev/video10` and `/dev/video11`). PhoneCam writes to `video11`; `video10` is intentionally left free for other software (e.g. OBS Virtual Camera) so the two don't conflict.

On **Windows**, the virtual camera is [UnityCapture](https://github.com/schellingb/UnityCapture) - a standalone DirectShow filter, no OBS required.

---

## Repository layout

```
phonecam/
|-- .github/
|   +-- workflows/
|       |-- build-apk.yml        # CI: debug APK on ubuntu-latest
|       |-- build-windows.yml    # CI: Windows bundle (EXE + adb + UnityCapture)
|       +-- build-linux.yml      # CI: Linux bundle (Python script + start.sh)
|
|-- android/                     # Gradle project
|   +-- app/
|       |-- build.gradle         # compileSdk 34, minSdk 26, AGP 8.3.2, Kotlin 1.9.24
|       +-- src/main/
|           |-- AndroidManifest.xml
|           +-- kotlin/com/phonecam/
|               |-- MainActivity.kt          # UI: enumerate cameras, start/stop service
|               |-- CameraStreamService.kt   # Foreground service: Camera2 + HTTP control
|               +-- MjpegServer.kt           # HTTP: /video  /cameras  /control
|
+-- desktop/
    |-- phonecam_desktop.py      # PyQt6 app
    |-- requirements.txt
    |-- phonecam.spec            # PyInstaller spec for Windows EXE
    |-- start.sh                 # Linux launcher (auto-installs deps)
    |-- start.bat                # Windows launcher (auto-installs deps + UnityCapture)
    |-- platform-tools/          # Bundled adb for Windows (Google Android SDK Platform Tools)
    |   |-- adb.exe
    |   |-- AdbWinApi.dll
    |   |-- AdbWinUsbApi.dll
    |   |-- libwinpthread-1.dll
    |   +-- NOTICE
    +-- unitycapture/            # Bundled virtual camera driver (MIT)
        |-- UnityCaptureFilter32.dll
        |-- UnityCaptureFilter64.dll
        +-- LICENSE
```

---

## Android app

### What it does

Runs a **foreground service** (declared type `camera`, required on Android 14+) that owns a Camera2 session and an HTTP server on port 8080. Three endpoints:

- `GET /video` - MJPEG stream (`multipart/x-mixed-replace`)
- `GET /cameras` - JSON of all detected cameras + current exposure/WB/battery state
- `GET /control?action=...` - live camera control

The app enumerates **physical sub-cameras** of logical multi-camera groups via `CameraCharacteristics.physicalCameraIds` (API 28+). On many modern phones the logical back camera (ID `0`) hides individual wide/main/telephoto sensors behind it; this app surfaces all of them and lets you pick. Physical cameras are opened via `OutputConfiguration.setPhysicalCameraId()` within a session on the logical parent.

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
| Theming | qt-material |
| MJPEG decode | opencv-python (`cv2.VideoCapture`) |
| Virtual camera output | pyvirtualcam |
| Frame buffers | numpy |

### One-time setup (detailed)

**Linux:**

The `start.sh` script handles pip dependencies automatically. For the virtual camera driver:

```bash
# Load v4l2loopback (the app's Load Module button does this for you, or run manually)
sudo modprobe v4l2loopback devices=2 video_nr=10,11 \
  card_label="OBS Virtual Camera,Phone Camera" exclusive_caps=1
```

PhoneCam uses `/dev/video11`; `/dev/video10` is left free for other tools (e.g. OBS Virtual Camera).

Persist across reboots (Fedora/Nobara/any `dracut` distro):
```bash
echo 'options v4l2loopback devices=2 video_nr=10,11 card_label="OBS Virtual Camera,Phone Camera" exclusive_caps=1' \
  | sudo tee /etc/modprobe.d/98-v4l2loopback.conf

# Remove any conflicting file (kmod package ships one at the same path)
sudo rm -f /etc/modprobe.d/v4l2loopback.conf
echo "v4l2loopback" | sudo tee /etc/modules-load.d/v4l2loopback.conf
sudo dracut --force
```

If OBS is installed as Flatpak, grant it device access:
```bash
flatpak override --user --device=all com.obsproject.Studio
```

**Windows:**

`start.bat` installs pip dependencies and the UnityCapture DLLs (already bundled in `desktop/unitycapture/`) on first run. The app registers the virtual camera driver automatically - just click the button when prompted.

For the `PhoneCamDesktop.exe` path: `adb.exe` and the UnityCapture DLLs are already bundled in the repo under `desktop/platform-tools/` and `desktop/unitycapture/`. The app looks for them next to itself, so as long as those folders are in the same directory as the EXE, everything is found automatically. No extra installs needed.

### Key implementation notes (for contributors)

**Live transform:** `StreamWorker.flip_h`, `flip_v`, `rotation`, `zoom`, `pan_x`, `pan_y` are plain instance attributes. Python's GIL makes bool/`None`/float writes atomic, so the UI thread sets them while the worker reads them each frame - no lock needed.

**Live FPS/resolution:** Changing these requires recreating the `pyvirtualcam.Camera` context (it is constructed with fixed dimensions). The worker holds a `threading.Event` (`_restart_vcam`). When set, the inner loop breaks, the pyvirtualcam context closes, and the outer loop re-enters with new parameters. Interruption is typically under one second.

**Software zoom/pan:** `apply_zoom()` center-crops the frame to `(w/zoom, h/zoom)` then resizes back to the original dimensions using `cv2.INTER_LINEAR`. Pan offsets shift the crop center within bounds. Applied after resize (to preserve aspect ratio through rotation) and before rotation.

**Auto-reconnect:** If `cap.read()` fails, the stream reader calls `_reconnect_cap()`, which loops with a 3-second delay until the stream comes back. The pyvirtualcam context stays open during reconnect so the virtual camera device doesn't disappear from OBS.

**Control client:** `PhoneControlClient` sends `GET /control?...` in a daemon thread per request, fire-and-forget. Failures are silently dropped - a missed control command is non-critical; the next interaction resyncs state.

**ISO/shutter sliders:** log scale over 2000 steps across the range the phone reports per camera. Range updates when switching lenses. Spinbox allows direct numeric entry. Shutter spinbox shows milliseconds (scale factor 1e-6) while the API and internal state use nanoseconds.

**White balance:** linear Kelvin slider 2000-8000 K. Translates to `RggbChannelVector` using the Tanner Helland K->RGB algorithm and sets `COLOR_CORRECTION_GAINS`. Reverting to auto restores `CONTROL_AWB_MODE_AUTO`.

**Camera capability gating:** When a lens is selected, `supportsManualSensor` and `supportsManualWB` from `/cameras` determine whether the manual exposure and WB controls are enabled. If the camera doesn't support a capability the corresponding radio button is disabled and a tooltip explains why.

**Per-device config:** All UI settings are serialized to `phonecam_config.json` (next to the script/EXE) with a 500ms debounce on any change. The `devices` dict is keyed by device name; switching devices in the combo saves the current device's settings before loading the new one's. Old single-device flat configs are migrated automatically on first load.

**Single-instance:** `acquire_single_instance()` tries to bind a local TCP socket on port 47823. If already bound, it connects and sends `b"raise"` to signal the running instance to restore its window, then exits. A background thread in the running instance listens for these signals.

**Battery/temperature polling:** A `QTimer` fires every 15 seconds while streaming and calls `GET /cameras` for the `battery`, `charging`, and `battery_temp_c` fields. Notifications fire once per threshold crossing and reset with a 5-degree/5-percent hysteresis to avoid repeated alerts.

**adb lookup:** `adb_exe()` checks `desktop/platform-tools/adb[.exe]` first, then falls back to `adb` on PATH.

**UnityCapture:** DLLs are bundled in `desktop/unitycapture/`. `register_unitycapture()` calls `regsvr32` to register them as a DirectShow filter. The app can also download fresh copies from GitHub if the local ones are missing.

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
    },
    {
      "id": "3",
      "logicalId": "0",
      "label": "Back ~89mm [phys]",
      "current": true,
      "isoMin": 50,
      "isoMax": 6400,
      "shutterMinNs": 200000,
      "shutterMaxNs": 500000000,
      "supportsManualSensor": true,
      "supportsManualWB": false,
      "hasOis": false,
      "hwLevel": "LIMITED"
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
| `camera` | `id=<id>` | Switch camera. Closes current session, reopens with correct physical-stream config. |
| `auto` | - | Restore auto exposure (AE mode ON, AF continuous video). |
| `iso` | `value=<int>` | Set ISO. Switches AE to OFF. See note below. |
| `shutter` | `value=<long ns>` | Set shutter in nanoseconds. Switches AE to OFF. See note below. |
| `wb_auto` | - | Restore auto white balance. |
| `wb_kelvin` | `value=<int>` | Set color temperature 1000-40000 K via `COLOR_CORRECTION_GAINS`. |
| `ois` | `value=1\|0` | Toggle OIS (only applied if camera reports support). |
| `jpeg_quality` | `value=<int 50-100>` | Set JPEG compression quality on the phone. Lower values reduce bandwidth. |
| `fps_target` | `value=<int 5-60>` | Set the camera capture FPS on the phone. |

All responses: `{"ok": true}` or `{"ok": false, "error": "..."}`.

> **Manual exposure note:** `CONTROL_MODE_OFF` only activates when *both* ISO and shutter are set. Sending just one has no visible effect until the other is also provided. The desktop app sends both simultaneously when switching to manual mode.

---

## License

This project is licensed under [CC BY-NC 4.0](https://creativecommons.org/licenses/by-nc/4.0/) - see [LICENSE](LICENSE).
You are free to use, modify, and share it with attribution, but not for commercial purposes or monetary gain.

## Third-party components

Bundled third-party components retain their own licenses.

**UnityCapture** (`desktop/unitycapture/`) - DirectShow virtual camera filter for Windows.
Copyright (c) 2018 Bernhard Schelling. MIT License. See `desktop/unitycapture/LICENSE`.
Source: https://github.com/schellingb/UnityCapture

**Android SDK Platform Tools** (`desktop/platform-tools/`) - includes `adb.exe` for USB mode.
Copyright (c) Google LLC. Distributed under the Android Software Development Kit License Agreement.
See `desktop/platform-tools/NOTICE` and https://developer.android.com/studio/terms

---

## CI / GitHub Actions

All three workflows publish to a rolling **`latest` release** on every push to `master`.

### `build-apk.yml` - triggered on changes to `android/**`

1. JDK 21 (Temurin) + Gradle cache via `actions/setup-java@v4`
2. Android SDK via `android-actions/setup-android@v3` (platform-tools, android-34, build-tools;34.0.0)
3. Generate `local.properties` from `$ANDROID_SDK_ROOT`
4. `./gradlew assembleDebug --no-daemon`
5. Publishes `PhoneCam.apk` to the `latest` release (and as a 30-day artifact)

### `build-windows.yml` - triggered on changes to `desktop/**`

1. Python 3.11 + pip cache via `actions/setup-python@v5`
2. `pip install -r requirements.txt pyinstaller`
3. `pyinstaller phonecam.spec`
4. Assembles `PhoneCam-windows.zip`: EXE + `start.bat` + `platform-tools/` + `unitycapture/`
5. Publishes the zip to the `latest` release (and as a 30-day artifact)

`phonecam.spec` uses `collect_all('PyQt6')` to include Qt platform plugins (`qwindows.dll` etc.) that PyInstaller's default analysis misses. UnityCapture is a system COM filter and does not need bundling. Expected EXE size: 60-80 MB.

### `build-linux.yml` - triggered on changes to `desktop/**`

1. Assembles `PhoneCam-linux.tar.gz`: `phonecam_desktop.py` + `requirements.txt` + `start.sh`
2. Publishes the tarball to the `latest` release (and as a 30-day artifact)

No build step needed - the Linux bundle is just the Python source and the launcher script.

All workflows also expose `workflow_dispatch` for manual triggering.

---

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| Only 2 cameras visible in app | Physical sub-cameras hidden behind logical camera | Already handled via `physicalCameraIds` (API 28+); if still missing, device may restrict access |
| Manual exposure greyed out | Camera doesn't report `MANUAL_SENSOR` capability | Some front cameras and telephoto lenses don't support it; use Auto |
| `/dev/video11` gone after reboot | v4l2loopback not persistent | Follow the `dracut` / `modules-load.d` steps above |
| pyvirtualcam fails to open device (Linux) | Module not loaded | App has a **Load module** button, or run `sudo modprobe v4l2loopback ...` |
| pyvirtualcam fails to open device (Windows) | UnityCapture not registered | Use the app's setup button, or re-run `start.bat` |
| Camera control panel never appears | Phone HTTP server slow to start | App retries 3x over 6 s after connecting; check USB debugging is active |
| WB slider has no effect | Camera doesn't support `MANUAL_POST_PROCESSING` | Falls back gracefully; auto AWB still works |
| ISO/shutter change has no effect | Only one of the two was sent | Switch to Manual - desktop sends both simultaneously |
| High latency over Wi-Fi | MJPEG is per-frame JPEG, higher bandwidth than H.264 | Use USB mode, lower JPEG quality, or reduce phone FPS |
| Second launch does nothing | Single-instance enforcement | The existing window is brought to the front; only one instance runs at a time |
