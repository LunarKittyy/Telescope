# Building

## Build the Android app

Needs JDK 21 and the Android SDK with `platform-tools`, `platforms;android-34` and `build-tools;34.0.0`.

```bash
cd android
echo "sdk.dir=$ANDROID_SDK_ROOT" > local.properties
./gradlew assembleDebug
# output: app/build/outputs/apk/debug/app-debug.apk
adb install app/build/outputs/apk/debug/app-debug.apk
```

This is a self-signed debug build for development.

## Run the desktop app from source

The desktop app is a Rust workspace in `rust/`. You need a recent stable Rust (from [rustup](https://rustup.rs)). On Linux it also wants FFmpeg's headers (bindgen reads them, so `clang` too), fontconfig and a few fonts. On Debian and Ubuntu:

```bash
sudo apt-get install clang pkg-config libavcodec-dev libavformat-dev libavutil-dev libswscale-dev libswresample-dev \
  libavfilter-dev libavdevice-dev libfontconfig1-dev libxkbcommon-dev fonts-inter fonts-noto-core fonts-noto-cjk libegl1 libgl1 libdbus-1-3
```

Then, from the repository root:

```bash
cd rust
cargo run -p telescope-app                      # the app (a build from source keeps its settings in a profile of its own)
cargo nextest run --workspace --locked          # the tests, run side by side as CI does (cargo install cargo-nextest)
python tools/i18n.py extract                    # after adding or changing a text, see translating.md
```

Windows needs MSVC and an FFmpeg build, see [Package the app](#package-the-app). The tests need `RUST_MIN_STACK=8388608`, since building a window this size can overflow the 2 MB a test thread gets. Add `CARGO_BUILD_WARNINGS=deny` to build the way CI does, where a compiler warning fails the check. Some tests start a real copy of the app. `a_new_copy_cleans_up_once_it_has_the_lock` fails while another copy of Telescope is running on the machine.

The page of the Browser camera is checked in Node (`rust/crates/link/tests/page_harness.js`), so the tests want `node` on the path. Without it they skip, and on CI (where `CI` is set) they fail.

Linux still needs the [virtual camera setup](setup.md) for a real stream. Plugins are Lua, see [Writing a plugin](writing-plugins.md).

## The Windows 11 camera

The camera Telescope installs on Windows 11 is a small C++ DLL in `rust/windows/virtualcamera/` (how it works is in [Architecture](architecture.md#the-windows-11-camera)). Builds need Visual Studio 2022 or its Build Tools with the C++ workload, a Windows 11 SDK and CMake (the C++ workload ships one). The DLL has the camera's wait screen built in, which the app draws itself, so `rust/tools/build_windows.ps1` first runs `telescope-rs camera-wait-screen` and hands the file to CMake. To build the DLL alone from `rust/` in a Developer PowerShell:

```powershell
cargo run -p telescope-app -- camera-wait-screen target\wait_screen.bin
cmake -S windows\virtualcamera -B target\camera -A x64 -DTELESCOPE_WAIT_SCREEN=target\wait_screen.bin
cmake --build target\camera --config Release
```

That writes `target\camera\Release\TelescopeCamera.dll`. **Install driver** copies it into Program Files and registers it, from the `virtualcamera\` folder next to `TelescopeDesktop.exe`.

Change the DLL in a way an installed copy has to be replaced for (anything but comments) and raise `kCameraVersion` in `camera.h` and `MF_VERSION` in `crates/outputs/src/wincam.rs` together. Settings then offers **Update** to people who have the old one.

## Try a change without touching your real install

A dev phone app and a dev desktop run next to the real ones and pair only with each other.

```bash
(cd android && ./gradlew installDev)                 # "Telescope (dev)", package com.telescope.dev
(cd rust && cargo run -p telescope-app -- dev --keep)
```

Run both from the repository root.

- The dev app has its own data, pairings and phone identity. The dev desktop keeps its config and log in a profile folder in your temp dir, runs alongside the real desktop, never writes menu or sign-in entries and uses its own virtual mic ("Telescope Microphone (dev)")
- **Install over USB** installs the dev APK once `assembleDev` or `installDev` has built it
- Without `--keep` the profile is fresh on every run, so you pair again each time. With it the profile stays (in `telescope-dev-profile-<user>` under your temp dir) until `telescope-rs dev --reset`
- `--plugin FOLDER` loads a plugin you're writing straight from its folder and restarts the dev desktop on every save. See [Writing a plugin](writing-plugins.md#checking-your-plugin)

`rust/tools/measure.py` times the first painted window and the CPU, memory and delivered frame rate of a streaming desktop, and `rust/tools/stress/` runs it beside game-like loads. `rust/tools/fake_phone.py` is a stand-in phone that serves a recorded clip, for measuring without a phone.

To try the Webcam plugin without a webcam, add a spare v4l2loopback device, feed it a test picture and start the dev desktop with `TELESCOPE_WEBCAM_LOOPBACK=1`, which lists loopback cameras. Telescope's own and ones named as virtual cameras (like OBS Virtual Camera on `/dev/video10`) stay out, so don't use those:

```bash
sudo v4l2loopback-ctl add -n "Test Webcam" -x 1 /dev/video20
```

```bash
ffmpeg -re -f lavfi -i testsrc2=size=1280x720:rate=30 -pix_fmt yuyv422 -f v4l2 /dev/video20
```

`-c:v mjpeg` instead of `-pix_fmt yuyv422` makes it an MJPEG camera, and stopping ffmpeg unplugs it.

Some things are shared because there's only one of them:

- The phone apps use the same ports, so only one can be open at a time. The one you open takes the ports over within a few seconds of the other closing
- Both desktops stream into the same virtual camera, so only one can stream at a time
- Settings actions that change the system (loading v4l2loopback, installing the Windows driver) affect both

## Versions

Both apps share one version, the `VERSION` file at the repo root. The build number is the commit count on `master`, so it only grows. It's the Android `versionCode` and what the update check compares.

A build is stable (`3.0.0`), nightly (`3.0.0-nightly.123`) or a source checkout (`3.0.0 dev`). The version shows under Updates & About in Settings and in both apps' Copy diagnostics.

## Package the app

Moved to [Packaging and CI](packaging.md#package-the-app).

### The vision pack

Moved to [Packaging and CI](packaging.md#the-vision-pack).

### The matting pack

Moved to [Packaging and CI](packaging.md#the-matting-pack).

## CI

Moved to [Packaging and CI](packaging.md#ci).

### `release.yml`

Moved to [Packaging and CI](packaging.md#releaseyml).

### The workflows for pull requests

Moved to [Packaging and CI](packaging.md#the-workflows-for-pull-requests).

### `pages.yml`

Moved to [Packaging and CI](packaging.md#pagesyml).
