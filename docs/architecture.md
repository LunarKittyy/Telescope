# Architecture

How Telescope works inside, for contributors. The protocol itself is in [protocol.md](protocol.md), and [desktop/MODULES.md](../desktop/MODULES.md) goes through the desktop app module by module.

## Overview

```
Android device  (Telescope app, port 8080)
      |
      |  USB: adb forward tcp:0 tcp:8080 (adb picks the local port)
      |  Wi-Fi: direct HTTP, address found via mDNS or stored
      v
desktop/main.py  (Python, PyQt6)
      |
      |-- telescope/stream.py       StreamWorker (QThread)
      |     reads authenticated MJPEG (mjpeg_reader.py) or H.264 (h264_reader.py)
      |     runs frames through plugin pipeline
      |     _fit_frame() letterboxes to canvas size
      |     pyvirtualcam -> virtual camera device
      |
      +-- telescope/plugins/        one plugin per UI card
            setup                   Advanced dialog: virtual camera module, canvas, APK over USB
            connection              phones, Add phone dialog (pairing.py, port 8765), route choice
                                    (phones.py, discovery.py), session channel (port 8766): status, start/stop
            onboarding              first-run checklist on the video stage
            camera_control          lens, exposure, WB, focus, OIS
            stream_output           resolution, FPS, format, JPEG quality or bitrate
            transforms              flip, rotation, zoom, pan
            preview                 in-card and pop-out video preview
            monitoring              battery, temperature alerts
            wait_screen             holds the camera while idle (vcam.py), notices apps reading it
```

(The desktop also has `presets`, `microphone` (audio.py), `updates` and `startup` plugins; see [desktop/MODULES.md](../desktop/MODULES.md) for all of them.)

A second responder on the phone (`SessionServer`, port 8766) runs independently of the streaming server, so the desktop can reach the phone in exactly the state the streaming server doesn't exist in: idle. It answers `GET /v1/hello` (which phone this is, no auth), `GET /v1/ping` (pairing status plus what the phone is currently doing), `POST /v1/session` (start or stop the camera from the desktop) and `POST /v1/unpair` (forget the calling computer).

That second endpoint is why the desktop's Start button is the only one anyone has to press. Hitting Start asks the phone to bring its camera up, waits for it, then connects; hitting Stop takes the phone's camera back down. Starting on the phone still works exactly as before, and the desktop leaves a stream it finds already running alone.

`SessionServer` stays bound while **either** `MainActivity` is on screen **or** `CameraStreamService` is running (see `SessionEndpoint`'s refcount). Those two owners are the safety boundary: a fully backgrounded, non-streaming app cannot be told to open the camera - which is also what keeps the start legal, since Android 12+ blocks starting a `camera`-type foreground service from the background. Covering the streaming case as well is what lets you stop and restart a session from the desktop after the phone's screen has gone dark.

On **Linux**, two `v4l2loopback` devices are created (`/dev/video10` and `/dev/video11`). Telescope writes to `video11`; `video10` is intentionally left free for other software (e.g. OBS Virtual Camera).

On **Windows**, the virtual camera is [UnityCapture](https://github.com/schellingb/UnityCapture) - a standalone DirectShow filter, no OBS required.

## Repository layout

```
telescope/
|-- VERSION                    # The version both apps ship as
|-- .github/
|   |-- write_manifest.py        # Writes a release's manifest.json
|   |-- NIGHTLY_NOTES.md         # Body of the nightly release
|   |-- DISCUSSION_TEMPLATE/     # Device compatibility report form
|   |-- ISSUE_TEMPLATE/          # Bug form, links to Discussions
|   +-- workflows/
|       |-- release.yml          # Builds all three and publishes nightly or a stable release
|       |-- build-apk.yml        # APK (signed with the release key in a release)
|       |-- build-windows.yml    # Windows bundle (app folder + adb + UnityCapture)
|       +-- build-linux.yml      # Linux bundle (source + start.sh)
|
|-- docs/                       # Everything past the README's Quick Start; README.md is the index
|
|-- android/                     # Gradle project
|   +-- app/src/main/kotlin/com/telescope/
|       |-- MainActivity.kt      # UI: setup, pairing, Local only, Wait for my computer, Stop while streaming, diagnostics, updates
|       |-- PreviewActivity.kt   # Fullscreen live preview, standalone or attached to a running stream
|       |-- CameraStreamService.kt  # Foreground service: Camera2 + HTTP control
|       |-- WaitingService.kt    # "Wait for my computer": keeps the session port up; streams run under it
|       |-- CameraSessionController.kt  # Owns the live Camera2 session and capture-request state
|       |-- CameraCatalog.kt     # Enumerates cameras, incl. physical sub-cameras of logical multi-cams
|       |-- StreamStateMachine.kt   # Idle/StartingServer/.../Streaming/Failed state + history
|       |-- Protocol.kt          # kotlinx.serialization models for the v1 API
|       |-- SetupSteps.kt       # Get set up card rules: ask, or send to settings (JVM-tested)
|       |-- Pairing.kt           # QR payload (v3) parsing/validation, attempt ordering, failure text
|       |-- PairedComputers.kt   # Paired computers (one token each), this phone's id and name
|       |-- MjpegServer.kt       # Authenticated HTTPS: /v1/video(.h264)  /v1/state  /v1/control
|       |-- H264Encoder.kt       # MediaCodec H.264 from the camera Surface
|       |-- H264Stream.kt        # Per-viewer H.264 queue (and its link measurements), bitrate defaults
|       |-- DynamicBitrate.kt    # Dynamic bitrate: follows the link from the queue's measurements (JVM-tested)
|       |-- AudioStreamer.kt     # Microphone recording while someone listens
|       |-- AudioStream.kt       # PCM format, per-listener queue
|       |-- SessionServer.kt     # Out-of-band responder (port 8766): /v1/hello, /v1/ping, /v1/session, /v1/unpair
|       |-- TlsIdentity.kt       # The phone's TLS key and self-signed certificate, and its fingerprint
|       |-- PhoneTls.kt          # Makes and keeps the TLS identity in no-backup storage
|       |-- PendingLimiter.kt    # Caps connections still sending their request, per address and in total
|       |-- SessionEndpoint.kt   # Refcounted owner of SessionServer + the commands it runs
|       |-- StreamLauncher.kt    # Single place CameraStreamService is started from
|       |-- StreamPrefs.kt       # Last camera/resolution selection, for desktop-initiated starts
|       |-- Updater.kt           # Self-update: manifest check, download, verify, PackageInstaller
|       |-- UpdateLogic.kt       # Manifest parsing and version rules (JVM-tested)
|       |-- RecentRuns.kt        # The last 3 streams' reports, kept for Copy diagnostics
|       |-- LanAnnouncer.kt      # mDNS announcement (_telescope._tcp) so desktops find the phone's address
|       +-- HttpWire.kt          # The HTTP/1.1 subset MjpegServer and SessionServer share
|
+-- desktop/
    |-- main.py                  # Entry point: registers plugins, restores config
    |-- update_guard.py          # Finishes or rolls back an update before the app imports anything
    |-- requirements.txt         # Readable ">=" lower bounds
    |-- requirements-dev.txt     # requirements.txt + pytest, used by CI
    |-- constraints.txt          # Exact pinned versions for CI/release installs
    |-- scripts/smoke_check.py   # Packaging smoke checks (see building.md)
    |-- scripts/write_build_info.py  # Stamps a release build's version into telescope/_build.py
    |-- scripts/write_icon.py    # Writes resources/telescope.ico, the Windows exe's icon
    |-- tests/                   # pytest suite (desktop only; Android has its own JVM unit tests)
    |-- THIRD_PARTY_NOTICES.txt  # Bundled into both release archives
    |-- telescope.spec           # PyInstaller spec for the Windows app folder
    |-- start.sh                 # Linux launcher (creates/reuses a Telescope-owned venv)
    |-- start.bat                # Windows source-checkout launcher (auto-installs deps); not in the release zip, the EXE needs neither
    |-- platform-tools/          # Bundled adb for Windows
    |-- unitycapture/            # Bundled UnityCapture DLLs (MIT)
    +-- telescope/
        |-- version.py           # This build's version, build number and channel
        |-- diagnostics.py       # Sanitized log in the temp folder, Copy diagnostics report
        |-- updates.py           # Qt-free update check, download, verify and install
        |-- app.py               # TelescopeWindow: plugin host, responsive shell, stream lifecycle
        |-- theme.py             # Palette tokens + the app stylesheet
        |-- stream.py            # StreamWorker: MJPEG -> pipeline -> pyvirtualcam
        |-- mjpeg_reader.py      # Authenticated multipart-MJPEG reader (replaces cv2.VideoCapture)
        |-- h264_reader.py       # Authenticated H.264 reader (PyAV), same interface
        |-- audio.py             # Phone mic -> jitter buffer -> virtual mic
        |-- session.py           # StreamSession: owns worker/client for one connect-to-disconnect lifecycle
        |-- vcam.py              # Opens the virtual camera, wait screen, whether an app reads it
        |-- plugin.py            # TelescopePlugin base class, EventBus, HostServices protocol
        |-- config.py            # Versioned JSON config (v3) with per-section validation
        |-- models.py            # Typed contracts: PhoneState, CameraCapabilities (parsed from /v1/state)
        |-- phone_client.py      # Authenticated HTTP client for /v1/state and /v1/control (port 8080)
        |-- session_client.py    # HTTPS client for /v1/hello, /v1/ping, /v1/session, /v1/unpair (port 8766)
        |-- pinned_https.py      # PhoneAuth: HTTPS pinned to the phone's certificate fingerprint
        |-- pairing.py           # PairingServer: Qt-free pairing HTTP handshake (nonce, token proof, certificate pin)
        |-- phones.py            # Phone model, RouteResolver (USB or Wi-Fi, and why), refcounted adb forwards
        |-- discovery.py         # Finds phones on the LAN via mDNS (zeroconf)
        |-- ip_utils.py          # Desktop address discovery for the pairing code, address ranking
        |-- platform/
        |   |-- autostart.py     # Open at sign-in (XDG autostart / HKCU Run)
        |   |-- linux.py         # v4l2loopback helpers (load, unload, reload)
        |   |-- virtual_mic.py   # Telescope Microphone (pactl) and finding VB-Cable
        |   |-- windows.py       # UnityCapture helpers, zip detection
        |   +-- winjob.py        # Job object so the bundled adb server dies with Telescope
        |-- plugins/
        |   |-- setup.py
        |   |-- connection.py
        |   |-- camera_control.py
        |   |-- stream_output.py
        |   |-- transforms.py
        |   |-- presets.py       # Saved camera/output/transform settings
        |   |-- microphone.py    # The Microphone card
        |   |-- preview.py
        |   |-- onboarding.py    # First-run checklist
        |   |-- updates.py       # Update button and dialog
        |   |-- startup.py       # Automatic streaming (start and stop), open at sign-in
        |   |-- wait_screen.py   # Wait screen and its dialog
        |   +-- monitoring.py
        +-- widgets/
            |-- banner.py        # In-window problem banners
            |-- common.py        # NoScroll*, LogSliderRow, rows, segmented toggles, icons
            |-- qr.py            # QR code widget
            +-- lens_panel.py    # Lens picker widget
```

## Android app

### What it does

On first launch the top card is **Get set up**: camera access, notifications and the battery exemption, in that order, each with its reason and an Allow button (plus Microphone, once a desktop has asked for it). The app asks for nothing on its own, and the card goes once everything is allowed. If Android stops showing a permission prompt (denied twice), the button becomes Open settings.

Runs a **foreground service** (type `camera`, required on Android 14+, plus `microphone` once that's allowed) that owns a Camera2 session and an HTTPS server on port 8080 (TLS 1.2 or 1.3, with the certificate from `PhoneTls`). All endpoints require a bearer token issued during pairing:

- `GET /v1/video` - MJPEG stream (`multipart/x-mixed-replace`)
- `GET /v1/video.h264` - H.264 stream (`video/h264`: Annex-B, Baseline, a keyframe every second). A new viewer starts with the codec config and the next keyframe. Each frame is followed by an access unit delimiter, so a decoder can show it without waiting for the next one. `ffplay` plays it given the bearer header. Opening either video route switches the phone to that format. A viewer that gets nothing for 10 seconds is closed, so a computer that left can't hold a stream slot.
- `GET /v1/audio` - the microphone, raw PCM (`audio/pcm; rate=48000; channels=1; format=s16le`, 10 ms chunks). Recording starts with the first listener and stops with the last. `403` with the reason in `error` when it can't record (no permission yet, or the mic is busy)
- `GET /v1/state` - JSON of all detected cameras + current exposure/WB/battery state
- `POST /v1/control` - live camera control, JSON body

A separate HTTPS responder (`SessionServer`, port 8766, same certificate) runs independently of the streaming service. `GET /v1/hello` says which phone this is, without auth, so the desktop can tell its phone from any other one on a USB cable. `GET /v1/ping` checks the request's bearer token against the paired computers' tokens, returning 200 or 401 plus a small JSON body saying whether the phone is streaming, mid-start, or bound local-only. `POST /v1/unpair` removes the calling computer. `POST /v1/session` starts or stops the camera on the desktop's behalf, opening the lens, resolution and OIS the desktop last chose (the service saves each `camera`, `resolution` and `ois` control to `StreamPrefs`).

Its lifetime is refcounted by `SessionEndpoint` across three owners: `MainActivity` while it is started, `CameraStreamService` while it is running, and `WaitingService` while **Wait for my computer** is on. So the desktop can confirm pairing before any stream exists, start one, and stop or restart it later even if the phone's screen has since gone dark - but with Wait for my computer off (the default), an app that is both backgrounded and idle is unreachable, and a remote start in that state is impossible by construction. `WaitingService` is a camera/microphone foreground service that holds that port and opens neither; it's only started from a visible `MainActivity` and isn't sticky. Android 14+ refuses a camera foreground service started from the background, so while it waits `StreamLauncher` starts `CameraStreamService` as a plain service that runs under it, and the waiting notification says the camera is streaming. If waiting is turned off mid-stream, the stream takes its own foreground service if the app is on screen, and stops otherwise. If a start fails anyway, `CameraStreamService` marks it failed and stops, and the session ping reports busy for a moment first, so the desktop says so straight away instead of waiting out its timeout.

`CameraStreamService` stops itself after 60 seconds with no authorized request from the desktop (a state poll, a control command, or a fresh `/v1/video` connection) and no viewer taking video, so a crashed or disconnected desktop doesn't leave the camera running and draining the battery. The desktop already polls `/v1/state` every 15 seconds while streaming, well inside that margin. The watchdog is exempted while `PreviewActivity`'s local preview surface is attached, since that path never touches HTTP. The desktop drives the lens, capture resolution and OIS live; the phone app has no pickers of its own, only a Stop button while streaming, Local only, and the preview.

The app enumerates **physical sub-cameras** of logical multi-camera groups via `CameraCharacteristics.physicalCameraIds` (API 28+). On many modern phones the logical back camera (ID `0`) hides individual wide/main/telephoto sensors behind it; this app surfaces all of them and lets you pick.

The main screen's pairing card lists the computers this phone is paired with (each with a **Remove** that asks first) and has a **Scan pairing code** button that opens a ZXing barcode scanner (portrait, via `journeyapps:zxing-android-embedded`). Scanning the code in the desktop's Add phone dialog sends the phone's id, name, IPv4 addresses and certificate fingerprint to the desktop over HTTP, with a proof that it read the code. The pairing POST requires `android:usesCleartextTraffic="true"` since the desktop's pairing server runs plain HTTP; nothing in it is secret.

Both of the phone's servers speak TLS with a self-signed P-256 certificate the app makes on first run (`TlsIdentity`, `PhoneTls`) and keeps in no-backup storage, so a restored or reinstalled phone gets a new one and pairs again. Each server lets at most 4 connections per address, and 32 in total, sit sending their request, and a request has 5 seconds in total to arrive, so a device on the LAN can't tie the servers up.

The QR code carries a list of desktop address *candidates* (see [QR pairing payload](protocol.md#qr-pairing-payload)), and the phone works through them in a deliberate order: LAN candidates first, sent over the phone's actual Wi-Fi network via `Network.openConnection()` rather than whatever holds the default route, then every candidate again over the default network. That first pass is what makes pairing work with a VPN running on the phone - a VPN owns the default route, so a LAN address goes nowhere through it, while the Wi-Fi interface underneath still reaches the desktop as long as the VPN permits local-network traffic. Only the pairing request is bound this way; the process is never pinned to Wi-Fi. Attempts are capped at 2s each and 12s in total, so a full candidate list can't leave the user watching nothing happen for half a minute; anything not reached by then is reported as untried rather than silently dropped. If nothing answers, a dialog lists each address tried and how it failed, and names the two situations the phone can't work around: a VPN that blocks LAN traffic outright, and client-isolated guest Wi-Fi - both of which leave USB pairing as the way through. Pairing logic that doesn't need Android (payload parsing/validation, attempt ordering, the failure text) lives in `Pairing.kt` and is unit-tested.

Over USB there's no scan at all: while the Add phone dialog is open, the desktop pushes the same payload via `adb shell am broadcast` to a dedicated intent, registered exported but gated on the `DUMP` permission - held by `adb shell` by default, but not obtainable by ordinary third-party apps, so only adb (not another app on the phone) can trigger it. It re-sends every few seconds, since the broadcast only lands while the app is on screen.

Each paired computer has its own token, stored by the id the desktop sends in the pairing code. Pairing again from the same computer replaces only that computer's token; other computers stay paired. The phone checks tokens per request, so pairing another computer doesn't disturb a running stream.

While its session port is up (and **Local only** is off), the app announces itself on the LAN as `_telescope._tcp` with its phone id in the TXT record, via `NsdManager`. The desktop uses that to find the phone's current address; the announcement proves nothing by itself, since every connection still authenticates.

A **Copy diagnostics** button copies app version, device info, current stream state, recent state transitions/errors and what each camera's hardware supports to the clipboard, for pasting into a bug report. It also includes the last 3 streams' reports, kept in a small file so they survive the app being closed. Never includes the pairing token, a URL, or raw config.

### Permissions

| Permission | Reason |
|---|---|
| `CAMERA` | Open Camera2 device |
| `FOREGROUND_SERVICE` | Run foreground service |
| `FOREGROUND_SERVICE_CAMERA` | Required on Android 14+ for camera-type service, including **Wait for my computer** |
| `RECORD_AUDIO` | The desktop's microphone option; asked for only after a desktop wants it |
| `FOREGROUND_SERVICE_MICROPHONE` | Recording the mic while the screen is off |
| `INTERNET` | HTTP server on 0.0.0.0:8080 |
| `WAKE_LOCK` | Keep CPU active with screen off |
| `POST_NOTIFICATIONS` | Persistent streaming notification |
| `ACCESS_NETWORK_STATE` | Show device IP in UI |
| `REQUEST_IGNORE_BATTERY_OPTIMIZATIONS` | The Battery step of Get set up (exempts the app from battery restrictions) |
| `REQUEST_INSTALL_PACKAGES` | Installing its own updates; Android asks the first time |

## Desktop app

### Stack

| Component | Library |
|---|---|
| UI | PyQt6 |
| MJPEG decode | opencv-python-headless (`cv2.imdecode`), read via `telescope/mjpeg_reader.py`'s authenticated reader - not `cv2.VideoCapture`, which has no way to attach the bearer token |
| H.264 decode | PyAV (`av`, bundling FFmpeg), via `telescope/h264_reader.py`. Optional: without it only MJPEG is offered |
| Virtual camera output | pyvirtualcam |
| Frame processing | numpy |
| QR code generation | qrcode (rendered via QPainter, no Pillow) |
| Finding phones and addresses | zeroconf (mDNS), ifaddr (network adapters) |
| Microphone output (Windows) | sounddevice, into VB-Cable. Linux uses `pactl` |

### Implementation notes

**Plugin system:** The app is built around `TelescopePlugin` - a base class with hooks for `setup()`, `create_panel()`, `process_frame()`, `on_stream_starting/start/stop()`, `on_phone_state()`, `get/set_config()`, `apply_preset()`, `diagnostics()` and `shutdown()`. Plugins are registered in `main.py` in order; each creates one UI card. An `EventBus` (QObject with Qt signals) handles cross-plugin communication.

**Window layout:** A plugin declares a `panel_region` (`"left"`, `"right"` or `"center"`) and the window routes its panel there, so no plugin knows where it physically lands. Wide windows get three columns - the desktop-side cards (connection, output, transforms) on the left and the phone-side cards (camera, monitoring) on the right, both the same width so the video stage stays centred; below ~1300px the rails fold together, and below ~900px everything stacks into one scrolling column. A plugin can also contribute a header control via `create_header_widget()` (the Connection plugin puts the phone picker there) or entries in the header's settings menu via `create_menu_actions()` (how Advanced is reached, since nothing in it is adjusted mid-stream). Plugins don't call each other: the first-run checklist hears about paired phones through `EventBus.phones_changed`, asks for pairing with `add_phone_requested`, and tells the video stage to make room with `setup_needed`.

**Theming:** `telescope/theme.py` owns the entire look - palette constants, a dark `QPalette` so Qt-drawn chrome matches, and one stylesheet, applied over Fusion by `apply_theme()`. There are no image assets: icons are drawn procedurally with `QPainter` (`create_vector_icon`), and segmented toggles are ordinary radios/checkboxes carrying a `segmented` property the stylesheet picks up, so exclusivity and signal wiring stay plain Qt.

**Frame pipeline:** `StreamWorker` holds a list of `process_frame` callables (one per plugin). Each frame passes through the full pipeline on a decoder thread, in BGR. `_fit_frame()` then letterboxes/pillarboxes the result to the fixed vcam canvas size, preserving aspect ratio with black bars.

**Canvas size:** The vcam canvas (`pyvirtualcam.Camera` dimensions) is set at stream start from `SetupPlugin.get_canvas_dims()`. It's independent of the phone feed decode resolution. Changing it requires restarting the stream (and reloading v4l2loopback on Linux). `_fit_frame()` handles any mismatch between the processed frame size and the canvas.

**Clean stop/restart:** `_stop()` disconnects the worker's status signal before requesting stop, preventing the old worker's eventual `"idle"` emission from clobbering the new worker's state after a canvas restart. Both Linux and Windows `restart_vcam_canvas()` wait for the old `QThread` to fully exit (via `QThread.wait()`) before starting the new one, avoiding pyvirtualcam slot conflicts.

**Linux root commands:** every v4l2loopback operation (load, load-and-persist, unload, reload, the boot config) is one `pkexec sh -c "..."` call, so one password prompt. If pkexec is missing or has no agent, `sudo -n` covers cached credentials; `sudo` is never run where it could wait for a password, since a GUI app has no terminal to type it in. Otherwise the same steps come back as a pasteable `sudo` / `sudo tee` command.

**Live transform:** Plugin attributes like `flip_h`, `rotation` and the desktop's share of the zoom crop are plain Python instance attributes updated by the UI thread and read each frame by the worker thread. Python's GIL makes bool/float writes atomic at this granularity, so no lock is needed.

**Live FPS change:** Changing FPS requires recreating the `pyvirtualcam.Camera` context (constructed with fixed fps). The worker holds a `threading.Event` (`_restart_vcam`). When set, the vcam loop breaks, the context closes, and `_run_vcam()` opens a new one at the new rate. The phone connection and reader thread stay up throughout.

**Live resolution change:** Unlike FPS, mid-stream resolution changes don't require a vcam restart. The reader thread reads `self._width`/`self._height` dynamically each frame, and `_fit_frame()` adapts the output to the fixed canvas dimensions.

**Auto-reconnect:** If `cap.read_packet()` fails, the stream reader calls `_reconnect_cap()`, which loops with a 3-second delay until the stream comes back. Meanwhile the window resolves the route again every 3 seconds and, when the phone answers another way (Wi-Fi after the cable was pulled, or a fresh adb forward after it's plugged back in), points the reader there. The pyvirtualcam context stays open during reconnect so the virtual camera doesn't disappear from OBS. Every plugin's current settings (ISO, WB, JPEG quality, etc.) are resent to the phone right after a successful reconnect, since the phone has no way to know its control state might be stale.

**Genuine-connection signal:** `EventBus.stream_connected` fires only when `StreamWorker` reports its first `"ok"` status (an actual frame decoded), not merely when a worker object exists. The Connection card shows **Connecting…** until it fires, so a worker quietly retrying against a phone that isn't answering never reads as a healthy stream.

**Desktop address discovery:** `ip_utils.get_pairing_addresses()` enumerates the machine's real network adapters (via `ifaddr`) instead of asking the routing table where a public address would go. The old UDP "route probe" reported whichever interface owns the default route - which under a VPN is the VPN's, so the physical LAN address the phone can actually reach went missing from the QR code exactly when it mattered. Loopback, link-local (`169.254/16`) and IPv6 addresses are dropped, as are container/VM-only adapters (`docker*`, `br-*`, `veth<hex>`, `virbr*`, `vboxnet*`, `vmnet*`, VirtualBox/VMware host-only) - though Windows' `vEthernet (...)` is kept, since Hyper-V bridges the host's real LAN through it. What's left is classified `lan` (RFC 1918), `tailscale` (`100.64/10`) or `other`, and ordered that way: the physical LAN path first, Tailscale as the cross-network fallback. Recognisable tunnel adapters (`tun*`, `wg*`, `utun*`, …) are still advertised but sorted behind physical ones in the same class, so a desktop VPN handing out a `10.x` address doesn't push the real LAN address down the phone's list. The whole thing is capped at 8 candidates with interface names trimmed to 32 characters, since every byte adds modules to a code someone has to scan with a phone camera.

**How the phone is reached:** `phones.RouteResolver` decides on every connect and every status check, starting from scratch each time rather than remembering a mode. USB comes first: for each device adb reports as usable, it opens a temporary `adb forward tcp:0` to the session port and asks `/v1/hello` which phone it is; only a matching id counts, then `/v1/ping` confirms the token. Anything else becomes the reason shown on the card (`no_adb`, `no_cable`, `unauthorized`, `other_phone`, `app_closed`). Wi-Fi then probes, in parallel, the addresses mDNS currently reports for this phone id, the address that answered last time, and the rest of the phone's stored addresses ranked Tailscale first. Whichever answers is remembered as the phone's active address. With **Connect via** set to USB only, a failed USB check is reported as such instead of falling back. adb forwards are refcounted per (device, port) in `UsbTunnels`, so the status probe, a start and a stop that overlap don't tear down each other's tunnel.

**First address after pairing:** a phone reports every IPv4 address it has, and ranking alone gets it wrong in both directions - a phone on a tailnet this desktop isn't on has its unreachable Tailscale address preferred over its Wi-Fi one, while two devices sharing only a tailnet need exactly the opposite. `PairingResult.source_ip` settles it: the address the pairing POST arrived from is one of the phone's *and* demonstrably reachable from here, so it becomes the phone's `active_ip`. It's ignored when it isn't one of the addresses the phone reported (USB pairing arrives through the `adb reverse` tunnel, so its source is this machine's loopback).

**Re-pair mid-stream:** pairing the phone you're streaming from again replaces this computer's token on the phone, so the desktop reconnects the stream with the new one.

**Control client:** `PhoneControlClient` runs a single background worker thread that POSTs each command as a JSON body to `/v1/control`, in the order it was queued. Requests that share the same `action` are coalesced to just the latest value while still waiting to be sent - a burst of slider drags can't have an older request's response arrive after a newer one - except camera switches, which are always sent individually and in order. A request that doesn't get through (a stalling link) is sent again, waiting a little longer each time, for up to 15 s, unless a newer value for the same action is already waiting or the phone answered with an error. Every action sets a value, so one that arrived and only lost its reply is harmless sent twice; the phone ignores a switch to the lens it's already on.

**ISO/shutter sliders:** Log scale over 2000 steps across the range the phone reports per camera. Range updates when switching lenses. Shutter spinbox shows milliseconds while the API uses nanoseconds.

**White balance:** Linear Kelvin slider 2000-10000 K plus a green-magenta tint slider (-150..+150). `_kelvin_to_rggb()` converts both to Camera2 RGGB channel gains with an exponential model centred at ~5500 K (not a lookup table), sent via the `wb_gains` action and applied with `COLOR_CORRECTION_MODE_TRANSFORM_MATRIX` / `COLOR_CORRECTION_GAINS`. Reverting to auto restores `CONTROL_AWB_MODE_AUTO`.

**Per-phone config:** Camera, stream-output, transform, and monitoring settings serialize to `telescope_config.json` with a 500ms debounce. Connection settings and virtual-camera canvas settings are global. The `devices` dict is keyed by phone id; switching phones saves the current phone's settings before loading the new one's, and removing a phone deletes its entry. A version 2 config is upgraded by dropping its pairings and per-phone settings (keyed by name back then) and keeping the rest; anything older is backed up as `telescope_config.json.invalid-<timestamp>` and replaced with defaults.

**Single-instance:** `acquire_single_instance()` tries to bind a local TCP socket on port 47823. If already bound, it signals the running instance to restore its window and exits.

**Zoom split:** the phone does as much of the crop as its lens allows, straight from the full-resolution sensor (on the **Auto** lens, switching to the telephoto when the framing fits in it), and the desktop crops whatever is left, so the framing is the same either way. While you zoom or pan, the preview briefly outlines what each longer lens sees; the outlines are preview-only and approximate, since the lenses sit a little apart. The zoom slider sticks to the dots where the phone switches to a longer lens (the sharpest view that lens gets), the pan sliders and preview drag stick at the centre, and compensation sticks at 0. The sticking is a few pixels wide and the arrow keys never stick. The lens dot is lavender when the phone switched to a longer lens and red when it stayed on the main one although the zoom would get the telephoto (panned outside it, or the phone chose to, usually in low light or up close).

**Light/Heavy bitrate:** Auto is about 8 Mbps at 1080p30, scaled by size and fps, capped at 30 Mbps (the phone's encoder may cap lower). Dynamic goes up to about 2.5 times Auto's sizing (20 Mbps at 1080p30, 80 at 4K30, 100 at 4K60), lowers it within a second or two when the queue shows the link can't keep up, ignores short Wi-Fi hiccups, and climbs back quickly at first, then carefully near where it last ran into trouble. A phone app without Dynamic uses Auto. An encoder that fails mid-stream falls back to Heavy and says so; a size the encoder can't do (4:3 4K on most phones) stops instead, since Heavy at that size can be hundreds of Mbps.

**Can't keep up:** throughput goes amber when frames arrive under 85% of the target for two 2 s windows in a row (a burst counts every frame in it). After a Start, a lens switch or a new resolution or FPS, it waits until frames are coming again plus 2.5 s, 10 s at most. If the phone's own `camera_fps` shows the camera is the slow part (dim light), it's not counted as the link: no amber, only a tooltip on the FPS readout.

**Automatic streaming holds:** after a manual Stop, "when the phone is ready" waits until the phone goes away and comes back, and "when an app opens the camera" waits until that app lets go. A stream Telescope stopped by itself, or a battery or heat stop from Monitoring, holds the same way. Its own preview doesn't count as an app reading the camera. On Linux, if an app already holds the camera at some size, the stream and wait screen open at that size.

**Log:** capped at about 1 MB plus the one before it, and a repeated line is counted instead of written again. On Linux, `telescope.log` next to `start.sh` links to it.

**Config migration:** config from the previous major version keeps its global settings but drops pairings and per-phone settings. Anything older or malformed is backed up next to the real file, and each section is validated on its own, so one bad section resets without discarding the rest.

**Battery/temperature polling:** A `QTimer` fires every 15 seconds while streaming. Notifications fire once per threshold crossing with 5-degree/5-percent hysteresis to avoid repeated alerts; a threshold's Stop streaming acts on every reading past it. A reading that arrives after its stream stopped is dropped.
