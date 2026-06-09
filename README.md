# PhoneCam

Stream your Android phone's camera — including telephoto and wide-angle lenses — to a virtual webcam on Linux or Windows. Camera controls (ISO, shutter, white balance, lens selection) are exposed over a local HTTP API so the desktop app can drive them live.

---

## Why

Most Android camera streaming solutions either lock you to a specific app ecosystem, use ADB screen mirroring which blocks the back camera on some devices, or route through OBS to create the virtual camera — which is a problem if you need OBS free for its own output. PhoneCam runs as a self-contained foreground service that serves MJPEG directly and exposes camera controls as a simple REST API, leaving OBS (or any other capture tool) completely unencumbered.

---

## Architecture

```
Android device  (PhoneCam app, port 8080)
      │
      │  USB: adb forward tcp:8080 tcp:8080
      │  WiFi: direct HTTP
      ▼
phonecam_desktop.py  (Python, PyQt6)
      │  reads MJPEG with cv2.VideoCapture
      │  pyvirtualcam → virtual camera device
      ▼
Any app that reads a webcam
```

On **Linux**, two `v4l2loopback` devices are created (`/dev/video10` and `/dev/video11`). PhoneCam writes to `video10`; `video11` is intentionally left free for other software (e.g. OBS Virtual Camera) so the two don't conflict.

On **Windows**, the virtual camera is [UnityCapture](https://github.com/schellingb/UnityCapture) — a standalone DirectShow filter, no OBS required.

---

## Repository layout

```
phonecam/
├── .github/
│   └── workflows/
│       ├── build-apk.yml        # CI: debug APK on ubuntu-latest
│       └── build-windows.yml    # CI: Windows EXE on windows-latest
│
├── android/                     # Gradle project
│   ├── app/
│   │   ├── build.gradle         # compileSdk 34, minSdk 26, AGP 8.3.2, Kotlin 1.9.24
│   │   └── src/main/
│   │       ├── AndroidManifest.xml
│   │       └── kotlin/com/phonecam/
│   │           ├── MainActivity.kt          # UI: enumerate cameras, start/stop service
│   │           ├── CameraStreamService.kt   # Foreground service: Camera2 + HTTP control
│   │           └── MjpegServer.kt           # HTTP: /video  /cameras  /control
│   ├── build.gradle
│   ├── settings.gradle
│   ├── gradle.properties        # android.useAndroidX=true
│   └── gradle/wrapper/          # Gradle 8.6
│
├── desktop/
│   ├── phonecam_desktop.py      # PyQt6 app
│   ├── requirements.txt
│   └── phonecam.spec            # PyInstaller spec for Windows EXE
│
└── README.md
```

---

## Android app

### What it does

Runs a **foreground service** (declared type `camera`, required on Android 14+) that owns a Camera2 session and an HTTP server on port 8080. Three endpoints:

- `GET /video` — MJPEG stream (`multipart/x-mixed-replace`)
- `GET /cameras` — JSON of all detected cameras + current exposure/WB state
- `GET /control?action=...` — live camera control

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

This is a debug build — self-signed, for personal/development use.

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
| Frame buffers | numpy |

### One-time setup

**Linux:**
```bash
# Two loopback devices — one for this app, one free for other tools
sudo modprobe v4l2loopback devices=2 video_nr=10,11 \
  card_label="Phone Camera,Virtual Camera 2" exclusive_caps=1

pip install -r desktop/requirements.txt
```

Persist across reboots (Fedora/Nobara/any `dracut` distro):
```bash
echo 'options v4l2loopback devices=2 video_nr=10,11 card_label="Phone Camera,Virtual Camera 2" exclusive_caps=1' \
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
1. Install [UnityCapture](https://github.com/schellingb/UnityCapture): run `Install.bat` as Administrator. "Unity Video Capture" appears as a camera device in all apps.
2. For USB mode: install [Android SDK Platform Tools](https://developer.android.com/tools/releases/platform-tools), add `adb.exe` to PATH.
3. `pip install -r desktop/requirements.txt`

### Run

```bash
python desktop/phonecam_desktop.py
```

1. Start the PhoneCam app on your phone, pick a camera and resolution, tap **Start**.
2. On desktop, select USB or Wi-Fi mode and press **Start Streaming**.
3. The camera control panel (lens picker, ISO, shutter, WB, OIS) populates within ~2 s of connecting.

### Key implementation notes (for contributors / AI agents)

**Live transform:** `StreamWorker.flip_h`, `flip_v`, `rotation` are plain instance attributes. Python's GIL makes bool/`None` writes atomic, so the UI thread sets them while the worker reads them each frame — no lock needed.

**Live FPS/resolution:** Changing these requires recreating the `pyvirtualcam.Camera` context (it is constructed with fixed dimensions). The worker holds a `threading.Event` (`_restart_vcam`). When set, the inner loop breaks, the pyvirtualcam context closes, and the outer loop re-enters with new parameters. Interruption is typically under one second.

**Control client:** `PhoneControlClient` sends `GET /control?...` in a daemon thread per request, fire-and-forget. Failures are silently dropped — a missed control command is non-critical; the next interaction resyncs state.

**ISO/shutter sliders:** log scale over 2000 steps across the range the phone reports per camera. Range updates when switching lenses. Spinbox allows direct numeric entry.

**White balance:** linear Kelvin slider 2000–8000 K. Translates to `RggbChannelVector` using the Tanner Helland K→RGB algorithm and sets `COLOR_CORRECTION_GAINS`. Reverting to auto restores `CONTROL_AWB_MODE_AUTO`.

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
      "shutterMaxNs": 1000000000
    },
    {
      "id": "3",
      "logicalId": "0",
      "label": "Back ~89mm [phys]",
      "current": true,
      "isoMin": 50,
      "isoMax": 6400,
      "shutterMinNs": 200000,
      "shutterMaxNs": 500000000
    }
  ],
  "auto": true,
  "iso": null,
  "shutter_ns": null,
  "wb_kelvin": null,
  "ois": true
}
```

### `GET /control?action=<action>[&param=value]`

| `action` | extra params | effect |
|---|---|---|
| `camera` | `id=<id>` | Switch camera. Closes current session, reopens with correct physical-stream config. |
| `auto` | — | Restore auto exposure (AE mode ON, AF continuous video). |
| `iso` | `value=<int>` | Set ISO. Switches AE to OFF. See note below. |
| `shutter` | `value=<long ns>` | Set shutter in nanoseconds. Switches AE to OFF. See note below. |
| `wb_auto` | — | Restore auto white balance. |
| `wb_kelvin` | `value=<int>` | Set color temperature 1000–40000 K via `COLOR_CORRECTION_GAINS`. |
| `ois` | `value=1\|0` | Toggle OIS (only applied if camera reports support). |

All responses: `{"ok": true}` or `{"ok": false, "error": "..."}`.

> **Manual exposure note:** `CONTROL_MODE_OFF` only activates when *both* ISO and shutter are set. Sending just one has no visible effect until the other is also provided. The desktop app sends both simultaneously when switching to manual mode.

---

## CI / GitHub Actions

### `build-apk.yml` — triggered on changes to `android/**`

1. JDK 21 (Temurin) + Gradle cache via `actions/setup-java@v4`
2. Android SDK via `android-actions/setup-android@v3` (platform-tools, android-34, build-tools;34.0.0)
3. Generate `local.properties` from `$ANDROID_SDK_ROOT`
4. `./gradlew assembleDebug --no-daemon`
5. Upload `app-debug.apk` as artifact (30-day retention)

### `build-windows.yml` — triggered on changes to `desktop/**`

1. Python 3.11 + pip cache via `actions/setup-python@v5`
2. `pip install -r requirements.txt pyinstaller`
3. `pyinstaller phonecam.spec`
4. Upload `PhoneCamDesktop.exe` as artifact (30-day retention)

`phonecam.spec` uses `collect_all('PyQt6')` to include Qt platform plugins (`qwindows.dll` etc.) that PyInstaller's default analysis misses. UnityCapture is a system COM filter and does not need bundling. Expected EXE size: 60–80 MB.

Both workflows also expose `workflow_dispatch` for manual triggering.

---

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| Only 2 cameras visible in app | Physical sub-cameras hidden behind logical camera | Already handled via `physicalCameraIds` (API 28+); if still missing, device may restrict access |
| `/dev/video10` gone after reboot | v4l2loopback not persistent | Follow the `dracut` / `modules-load.d` steps above |
| pyvirtualcam fails to open device (Linux) | Module not loaded | App has a **Load module** button, or run `sudo modprobe v4l2loopback ...` |
| pyvirtualcam fails to open device (Windows) | UnityCapture not installed | Run `Install.bat` as Administrator |
| Camera control panel never appears | Phone HTTP server slow to start | App retries 6× over 12 s; check USB debugging is active |
| WB slider has no effect | Camera doesn't expose `COLOR_CORRECTION_GAINS` | Falls back gracefully; auto AWB still works |
| ISO/shutter change has no effect | Only one of the two was sent | Switch to Manual — desktop sends both simultaneously |
| High latency over Wi-Fi | MJPEG is per-frame JPEG, higher bandwidth than H.264 | Use USB mode, or lower resolution in phone app |
