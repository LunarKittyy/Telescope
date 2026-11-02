# Building and CI

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

Use Python 3.12 or newer. From the repository root, create a development environment:

```bash
cd desktop
python -m venv .venv
```

Activate it with `source .venv/bin/activate` on Linux or `.venv\Scripts\Activate.ps1` in Windows PowerShell. Then install the same requirements as CI:

```bash
python -m pip install -r requirements-dev.txt -c constraints.txt
python -m pip install --no-deps -r requirements-nodeps.txt -c constraints.txt
python scripts/dev_desktop.py --keep
```

The second install handles the video dependency whose declared version range conflicts with the version Telescope uses. Keep both steps. In the same activated environment, run:

```bash
python -m pytest -q              # tests (add -n auto to run them in parallel, as CI does)
python scripts/smoke_check.py    # packaging smoke checks
python scripts/bench_frames.py   # what each plugin costs a frame
```

A test fails at teardown when it leaves a thread or an active `QTimer` behind or when an exception happens in a Qt callback during it (without the suite's hook PyQt aborts the whole process for those). Left running they fire into whichever test comes next, and that's where Qt crashes, in a different test every run. Stop what the test started in its fixture (a plugin's `shutdown()` usually does it). Timing checks compare against reference work measured in the same run with the `timing` fixture rather than against milliseconds, because a busy CI runner can easily be several times slower than a desktop.

`bench_frames.py` builds the window offscreen with every plugin and runs synthetic 1080p frames through the real frame path: each plugin idle and at its heaviest settings, a session with several on, the stream worker, the preview, Several cameras in one picture and decoding. It prints each one's time per frame, its share of a 30 and a 60 fps frame and how much memory it allocates. `--profile` lists the costliest calls under each row and `--only fun_pack` keeps to one plugin. The vision model is swapped for a stand-in that costs nothing, which leaves only what Telescope's own code adds (Copy diagnostics has the real model's times).

For normal use from source, Linux can run `./start.sh` and Windows `start.bat`. Those launchers create their own environment for running the app; they don't install the test dependencies or activate it in your terminal. Linux still needs the [virtual camera setup](setup.md) for a real stream.

## The Windows 11 camera

The camera Telescope installs on Windows 11 is a small C++ DLL in `desktop/virtualcamera/` (how it works is in [Architecture](architecture.md#the-windows-11-camera)). Builds need Visual Studio 2022 or its Build Tools with the C++ workload, a Windows 11 SDK, CMake (the C++ workload ships one) and Python with the desktop's requirements, since the build renders the camera's wait screen with the app's own code (`scripts/write_camera_wait_screen.py`). From `desktop/` in a Developer PowerShell:

```powershell
cmake -S virtualcamera -B virtualcamera/build -A x64
cmake --build virtualcamera/build --config Release
```

That writes `virtualcamera/build/Release/TelescopeCamera.dll`, where a source checkout looks for it. **Install driver** then copies it into Program Files and registers it. CI builds the same way and puts the DLL in `virtualcamera\` next to `TelescopeDesktop.exe`.

Change the DLL in a way an installed copy has to be replaced for (anything but comments) and raise `kCameraVersion` in `camera.h` and `MF_VERSION` in `platform/windows.py` together. Settings then offers **Update** to people who have the old one.

## Try a change without touching your real install

A dev phone app and a dev desktop run next to the real ones and pair only with each other.

```bash
(cd android && ./gradlew installDev)      # "Telescope (dev)", package com.telescope.dev
cd desktop && python scripts/dev_desktop.py --keep
```

Run both from the repository root, with the development environment above activated.

- The dev app has its own data, pairings and phone identity. The dev desktop keeps its config and log in a profile folder in your temp dir, runs alongside the real desktop, never writes menu or sign-in entries and uses its own virtual mic ("Telescope Microphone (dev)")
- **Install over USB** installs the dev APK once `assembleDev` or `installDev` has built it
- Without `--keep` the profile is fresh on every run, so you pair again each time. With it the profile stays (in `telescope-dev-profile-<user>` under your temp dir) until `python scripts/dev_desktop.py --reset`
- `--plugin FOLDER` loads a plugin you're writing straight from its folder and restarts the dev desktop on every save. See [Writing a plugin](writing-plugins.md#trying-out-changes)

`python scripts/phone_check.py` runs a quick check against the paired dev phone: how long a start takes and how steady the stream is. `--wifi-drop 5` also turns the phone's Wi-Fi off for 5 seconds and times the recovery, and `--handover` checks the automatic Wi-Fi to USB handover. Both need the cable plugged in. It only prints numbers, so the output is fine to paste into an issue or PR.

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

## Package the Rust app

The desktop app in `rust/` packages into the same shape as the old Python release, which lets the Python app's updater install it. `release.yml` runs everything below through `rust-package.yml`.

FFmpeg comes first. `rust/tools/build_ffmpeg.sh` makes a small LGPL build with only the decoders the app opens (about 5 MB instead of about 170 MB) and it wants Ubuntu 22.04 for both targets, Windows' through mingw:

```bash
docker run --rm -v "$PWD:/src" -w /src ubuntu:22.04 bash -c '
  apt-get update && apt-get install -y --no-install-recommends build-essential curl ca-certificates xz-utils nasm patchelf pkg-config \
    libva-dev libdrm-dev zlib1g-dev gcc-mingw-w64-x86-64-win32 libz-mingw-w64-dev
  bash rust/tools/build_ffmpeg.sh linux rust/target/ffmpeg-linux
  bash rust/tools/build_ffmpeg.sh windows rust/target/ffmpeg-windows'
```

The GPL build with libx264 is only for the tests, which need an H.264 encoder.

On Linux `rust/tools/package_linux.sh --ffmpeg DIR --channel nightly --build 123 --commit <sha>` writes the folder that becomes `Telescope-linux.tar.gz`. Build it on Ubuntu 22.04 as well, because the machine it's built on sets the oldest Linux it runs on. Besides the program it holds a `start.sh`, a `main.py` that only hands over to `start.sh` and a `telescope/` folder with the stamp, because the Python updater turns down a tarball without the last two. FFmpeg is in `lib/`, and `lib/fallback/` has libva for a system that has none (`start.sh` only uses it then, a system's own libva knows where its video drivers are). `lib/fallback-fonts/` does the same for fontconfig and what it needs, in a folder of its own so that a missing libva can't put those ahead of the system's.

`rust/tools/package_appimage.sh BUNDLE OUT` turns that folder into `Telescope-x86_64.AppImage`. It needs `squashfs-tools` 4.5 or newer and `curl` for the runtime, which is the pinned static type 2 runtime from `AppImage/type2-runtime` with its checksum in the script. `APPIMAGE_RUNTIME=<file>` uses a copy you already have. The static runtime has no use for `libfuse2`, it only needs a `fusermount` or `fusermount3` on the system, and without one `--appimage-extract-and-run` still starts the program. The AppImage holds the folder without `start.sh`, `main.py` and `telescope/` and adds `rust/tools/AppRun` as its entry point, a `.desktop` file and the icon from `telescope-rs app-icon`. Change the runtime release and `RUNTIME_SHA256` together.

The mount is read-only and the program writes nothing inside it. Settings, plugins, vision packs and logs live in the user's folders as they do for the tarball. Its first lines in `main` note the AppImage and clear `APPIMAGE`, `APPDIR`, `ARGV0`, `OWD` and `AppRun`'s folders in `LD_LIBRARY_PATH` from the process, which covers every program it starts from then on (other apps, adb, pkexec, browsers). `FONTCONFIG_FILE` is the exception, because fontconfig loads it late and `AppRun` only sets it on a system with no fontconfig of its own. The mount goes when the process quits, so anything that starts Telescope again or writes a menu, sign-in or shortcut entry has to use the file's path and not `current_exe` (`telescope_core::system::start_command`).

An update replaces the file beside itself in one rename and keeps its permissions. The journal, `.previous` and `.update-failed` are the tarball's and sit in the file's folder, and `.previous` is gone once the new version has started fine. Before the swap the new file has to answer `build-info`, since a file whose runtime or `AppRun` is broken would have nothing left to roll back from. After the swap `update_guard` and `AppRun` undo a start that fails, the same way as `start.sh`.

What it's built on sets the oldest Linux it runs on: Ubuntu 22.04's glibc 2.35, so Debian 12, Fedora 36 and anything newer.

On Windows `rust/tools/build_windows.ps1 -Channel nightly -Build 123 -Commit <sha>` with `FFMPEG_DIR` set writes the folder that becomes `Telescope-windows-portable.zip`: `TelescopeDesktop.exe`, `ui-kit`, FFmpeg's and RNNoise's DLLs and both cameras. The C runtime is linked into the program, so it starts on a Windows without the VC++ redistributable. The camera DLL's wait screen comes from `telescope-rs camera-wait-screen`, so no Python app is needed for it. `installer/build.ps1 -Bundle <folder> -Out setup -Rust` packs that folder into the setup.

A stamped build (channel `stable` or `nightly`) uses the real settings folder, like the Python app it replaces, where a `dev` build keeps to a profile of its own.

`python rust/tools/check_package.py --version 4.2.0 --build 123 --channel nightly <folder, archive or AppImage>` is the package's smoke check.

## CI

### `release.yml`

Runs on every push to `master` that changes more than docs or the website, and on every `v*` tag. It builds everything from one commit and publishes it together:

- a push to `master` replaces the rolling **`nightly`** pre-release
- a tag `vX.Y.Z` creates the stable release `Telescope X.Y.Z` (the tag must match `VERSION`)

Each release holds `Telescope.apk`, `Telescope-windows-portable.zip`, `TelescopeSetup.exe`, `Telescope-linux.tar.gz`, `Telescope-x86_64.AppImage` and `manifest.json` (version, build number, channel, commit, session protocol, and each file's URL, size and SHA-256). The desktop bundles include the APK for Install over USB. The zip is also listed as `Telescope-windows.zip`, its old name, so older copies still find their update. The apps' update check reads the manifest: the phone compares `android.versionCode` and the desktop compares `build`. Every copy looks its own asset up by name, so the Python app and older Rust copies pass over the AppImage and keep updating from the tarball.

The APK is signed with the release key from the repository secrets `TELESCOPE_KEYSTORE` (base64), `TELESCOPE_KEYSTORE_PASSWORD`, `TELESCOPE_KEY_ALIAS` and `TELESCOPE_KEY_PASSWORD`. A release fails rather than publish a debug-signed APK, because Android only installs an update signed with the same key as the installed app.

Before publishing, `.github/check_release.py` checks the files it's about to upload: the APK's certificate is the release key's (pinned in the script), the APK and the Linux bundle's stamp carry this version and build, both bundles hold the notices and the APK, the AppImage is there and listed, and `manifest.json`'s checksums match. `rust/tools/check_package.py --no-run` then looks through both bundles and the AppImage once more. The Windows stamp is compiled in, so that one is checked where the program can run, in `rust-package.yml`.

### The workflows for pull requests

These run on pull requests without publishing, and `release.yml` calls them for the real thing. The checks called `android`, `windows` and `linux` have to pass before `master` takes a pull request (the maintainer can push past it). `build-apk.yml` and `rust.yml` each have a small `changes` job that skips the check when nothing it covers changed, which GitHub counts as passing, so a docs-only pull request never waits on a check that doesn't run. A new push cancels checks still running.

| Workflow | Runs for | What it does |
|---|---|---|
| `build-apk.yml` | `android/**` | `./gradlew lintDebug testDebugUnitTest`, then `assembleRelease`. In a release it checks the APK isn't debug-signed |
| `rust.yml` | `rust/**`, `desktop/**` | The `linux` and `windows` checks: builds the workspace and runs its tests, with the fonts and the Python desktop app's dependencies some of them compare against. Its `changes` job also checks `VERSION` and that the release checklist names every file. On `master` it runs inside `release.yml`, so it doesn't start a second time |
| `rust-parity.yml` | `rust/**`, `desktop/**` | The parity tools' tests and the ledger check, on pull requests and on `master` |
| `rust-package.yml` | `rust/tools/**`, `installer/**`, `desktop/virtualcamera/**` | Builds both FFmpegs and both packages and an AppImage and runs `check_package.py` on them. On pull requests it also builds a quick `TelescopeSetup.exe` and checks the installer, update and uninstall paths and an unzipped copy moving to the installed one. A release does those on the setup it publishes, in `release.yml` |

`build-linux.yml` and `build-windows.yml` are the Python desktop app's tests and bundles. They only run by hand now. `desktop/` stays in the repository because the parity tools and some Rust tests read it.

The Windows setup (`installer/Telescope.iss`) is per-user: it installs to `%LOCALAPPDATA%\Programs\Telescope` without admin, because the app updates itself in place and needs its folder writable. It doesn't touch the camera driver (the first-run checklist installs that). Uninstalling closes a running Telescope, deletes the log, the downloaded adb and the vision download, asks before deleting settings and pairings, and offers to remove the camera driver and firewall rules (one UAC prompt). Keep its `AppId` as it is since Windows finds an existing install by it.

### `pages.yml`

Publishes `site/`, the landing page, to GitHub Pages on pushes to `master` that touch `site/**`. There's no build step. The repository's Pages source has to be set to **GitHub Actions**. The page is served at `telescope.webcam`, set under Settings → Pages (not a `CNAME` file, which Actions deploys ignore), with DNS in Cloudflare as an unproxied CNAME to `lunarkittyy.github.io`.

### `desktop/scripts/smoke_check.py`

Runs in the two Python workflows before the bundle is assembled. It builds the full app with every plugin, exercises adb discovery and virtual camera detection, does a real authenticated MJPEG round trip against a local test server (including that an unauthenticated request is rejected), and serves a Browser camera page over TLS with an aiortc peer sending video through the WebSocket. A bundle missing `cryptography`, aiortc or the `web/` files fails here. It's no substitute for testing on a real phone, see the [release checklist](release-checklist.md) and [device compatibility](device-compatibility.md).
