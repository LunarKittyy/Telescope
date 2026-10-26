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
```

For normal use from source, Linux can run `./start.sh` and Windows `start.bat`. Those launchers create their own environment for running the app; they don't install the test dependencies or activate it in your terminal. Linux still needs the [virtual camera setup](setup.md) for a real stream.

## The Windows 11 camera

The camera Telescope installs on Windows 11 is a small C++ DLL in `desktop/virtualcamera/` (how it works is in [Architecture](architecture.md#the-windows-11-camera)). Builds need Visual Studio 2022 or its Build Tools with the C++ workload, a Windows 11 SDK and CMake (the C++ workload ships one). From `desktop/` in a Developer PowerShell:

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

Some things are shared because there's only one of them:

- The phone apps use the same ports, so only one can be open at a time. The one you open takes the ports over within a few seconds of the other closing
- Both desktops stream into the same virtual camera, so only one can stream at a time
- Settings actions that change the system (loading v4l2loopback, installing the Windows driver) affect both

## Versions

Both apps share one version, the `VERSION` file at the repo root. The build number is the commit count on `master`, so it only grows. It's the Android `versionCode` and what the update check compares.

A build is stable (`3.0.0`), nightly (`3.0.0-nightly.123`) or a source checkout (`3.0.0 dev`). The version shows under Updates & About in Settings and in both apps' Copy diagnostics.

## CI

### `release.yml`

Runs on every push to `master` that changes more than docs or the website, and on every `v*` tag. It builds everything from one commit and publishes it together:

- a push to `master` replaces the rolling **`nightly`** pre-release
- a tag `vX.Y.Z` creates the stable release `Telescope X.Y.Z` (the tag must match `VERSION`)

Each release holds `Telescope.apk`, `Telescope-windows-portable.zip`, `TelescopeSetup.exe`, `Telescope-linux.tar.gz` and `manifest.json` (version, build number, channel, commit, session protocol, and each file's URL, size and SHA-256). The desktop bundles include the APK for Install over USB. The zip is also listed as `Telescope-windows.zip`, its old name, so older copies still find their update. The apps' update check reads the manifest: the phone compares `android.versionCode` and the desktop compares `build`.

The APK is signed with the release key from the repository secrets `TELESCOPE_KEYSTORE` (base64), `TELESCOPE_KEYSTORE_PASSWORD`, `TELESCOPE_KEY_ALIAS` and `TELESCOPE_KEY_PASSWORD`. A release fails rather than publish a debug-signed APK, because Android only installs an update signed with the same key as the installed app.

Before publishing, `.github/check_release.py` checks the files it's about to upload: the APK's certificate is the release key's (pinned in the script), the APK, the Linux bundle's stamp and the Windows bundle's `lib-<build>` folder all carry this version and build, both bundles hold the notices and the APK, and `manifest.json`'s checksums match.

### The three build workflows

They also run on every pull request without publishing. Their checks are called `android`, `windows` and `linux`, and `master` only takes a pull request once all three pass (the maintainer can push past it). Each has a small `changes` job that skips the check when nothing it covers changed, which GitHub counts as passing, so a docs-only pull request never waits on a check that doesn't run. A new push cancels checks still running.

| Workflow | Runs for | What it does |
|---|---|---|
| `build-apk.yml` | `android/**`, `VERSION` | `./gradlew lintDebug testDebugUnitTest`, then `assembleRelease`. In a release it checks the APK isn't debug-signed |
| `build-windows.yml` | `desktop/**`, `installer/**`, `VERSION` | Builds the Windows 11 camera's DLL, installs dependencies, runs `pytest` and `smoke_check.py`, checks the UnityCapture install for real on the runner (its Windows Server is older than Windows 11, so the Windows 11 camera can't be registered there), builds the app folder with PyInstaller (`telescope.spec`), and on pull requests builds a quick `TelescopeSetup.exe` with Inno Setup and checks the installer, update and uninstall paths. A release does those last two in `release.yml`, on the setup it publishes |
| `build-linux.yml` | `desktop/**`, `VERSION` | Runs `pytest` (with the Inter and Noto fonts installed, which `test_text_fit.py` measures with) and `smoke_check.py`, then assembles the source bundle: `main.py`, `update_guard.py`, the `telescope/` package, the requirements files, `start.sh` and the notices. There's no compiled build step since `start.sh` makes its own venv on first run |

The Windows setup (`installer/Telescope.iss`) is per-user: it installs to `%LOCALAPPDATA%\Programs\Telescope` without admin, because the app updates itself in place and needs its folder writable. It doesn't touch the camera driver (the first-run checklist installs that). Uninstalling closes a running Telescope, deletes the log and the downloaded adb, asks before deleting settings and pairings, and offers to remove the camera driver and firewall rules (one UAC prompt). Keep its `AppId` as it is since Windows finds an existing install by it.

### `pages.yml`

Publishes `site/`, the landing page, to GitHub Pages on pushes to `master` that touch `site/**`. There's no build step. The repository's Pages source has to be set to **GitHub Actions**. The page is served at `telescope.webcam`, set under Settings → Pages (not a `CNAME` file, which Actions deploys ignore), with DNS in Cloudflare as an unproxied CNAME to `lunarkittyy.github.io`.

### `desktop/scripts/smoke_check.py`

Runs in both desktop workflows before the bundle is assembled. It builds the full app with every plugin, exercises adb discovery and virtual camera detection, does a real authenticated MJPEG round trip against a local test server (including that an unauthenticated request is rejected), and serves a Browser camera page over TLS with an aiortc peer sending video through the WebSocket. A bundle missing `cryptography`, aiortc or the `web/` files fails here. It's no substitute for testing on a real phone, see the [release checklist](release-checklist.md) and [device compatibility](device-compatibility.md).
