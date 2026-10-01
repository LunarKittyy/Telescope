# Building and CI

## Build the Android app

Requires JDK 21 and Android SDK with `platform-tools`, `platforms;android-34`, `build-tools;34.0.0`.

```bash
cd android
echo "sdk.dir=$ANDROID_SDK_ROOT" > local.properties
./gradlew assembleDebug
# output: app/build/outputs/apk/debug/app-debug.apk
```

Install it over ADB:
```bash
adb install app/build/outputs/apk/debug/app-debug.apk
```

This is a debug build - self-signed, for personal/development use.

## CI / GitHub Actions

### Versions

Both apps share one version, the `VERSION` file at the repo root. The build number is the commit count on `master`, so it only grows; it's the Android `versionCode` and what the update check compares. A build is stable (`3.0.0`), nightly (`3.0.0-nightly.123`) or a source checkout (`3.0.0 dev`). The version shows at the bottom of the Advanced dialog on the desktop and under Copy diagnostics on the phone, and both apps' Copy diagnostics include it.

### `release.yml` - every push to `master` that changes more than docs or the website, and every `v*` tag

Builds everything from one commit, then publishes it together:

- a push to `master` replaces the rolling **`nightly`** pre-release (deleted and recreated, so it stays at the top of the Releases page)
- a tag `vX.Y.Z` creates the stable release `Telescope X.Y.Z`; the tag must match `VERSION`

Each release holds `Telescope.apk`, `Telescope-windows.zip`, `Telescope-linux.tar.gz` (both desktop bundles include the APK, for Install over USB) and `manifest.json`: version, build number, channel, commit, session protocol, and each file's URL, size and SHA-256. The apps' update check reads the manifest: the phone compares `android.versionCode` with its own, the desktop compares `build`.

The APK is signed with the release key from the repository secrets `TELESCOPE_KEYSTORE` (the keystore, base64), `TELESCOPE_KEYSTORE_PASSWORD`, `TELESCOPE_KEY_ALIAS` and `TELESCOPE_KEY_PASSWORD`. A release fails rather than publish an APK signed with a debug key, because Android only installs an update signed with the same key as the installed app.

The three build workflows below also run on pull requests, without publishing. A new push to a pull request cancels its checks that are still running.

### `build-apk.yml` - pull requests touching `android/**`, `VERSION` or the workflow itself

1. JDK 21 (Temurin) + Gradle cache (dependencies and build outputs, written only by `master` runs)
2. Android SDK (android-34, build-tools;34.0.0)
3. `./gradlew lintDebug testDebugUnitTest`, then `./gradlew assembleRelease -PbuildNumber=N -Pchannel=nightly|stable` (debug-signed on pull requests)
4. In a release: checks the APK isn't debug-signed

### `build-windows.yml` - pull requests touching `desktop/**`, `VERSION` or the workflow itself

1. Python 3.11 + pip cache
2. `pip install -r requirements-dev.txt pyinstaller -c constraints.txt`; runs `pytest`
3. In a release: `scripts/write_build_info.py` stamps the version into `telescope/_build.py`
4. `python scripts/smoke_check.py` - packaging smoke checks (see below)
5. Registers UnityCapture on the runner and checks it's listed as **Telescope** and pyvirtualcam opens it
6. `pyinstaller telescope.spec` - a folder build: `TelescopeDesktop.exe` next to `lib-<build>/`
7. Assembles the bundle: the app folder + `THIRD_PARTY_NOTICES.txt` + `platform-tools/` + `unitycapture/`, and checks nothing is missing, including the Qt plugins it can't start or draw without

`telescope.spec` takes Qt through PyInstaller's own hooks (only the modules Telescope imports) and skips UPX.

### `build-linux.yml` - pull requests touching `desktop/**`, `VERSION` or the workflow itself

1. Python 3.11 + pip cache; apt-installs `libegl1 libgl1 libxkbcommon0 libdbus-1-3` (PyQt6 needs these even in headless/offscreen test mode); installs `requirements-dev.txt` via `constraints.txt`; runs `pytest`
2. In a release: stamps the version
3. `python3 scripts/smoke_check.py` - packaging smoke checks
4. Assembles the bundle: `main.py` + `update_guard.py` + `telescope/` package + `requirements.txt` + `constraints.txt` + `start.sh` + `THIRD_PARTY_NOTICES.txt`

No compiled build step - the Linux bundle is the Python source and launcher script, which creates its own venv on first run (see `start.sh`).

### `pages.yml` - pushes to `master` touching `site/**` or the workflow itself

Publishes `site/`, the landing page, to GitHub Pages as it is; there's no build step. The page loads three.js from jsDelivr and falls back to a still version without WebGL or with reduced motion. The repository's Pages source has to be set to **GitHub Actions**.

### `desktop/scripts/smoke_check.py`

Run in both desktop CI workflows before assembling the bundle: constructs the full app and registers every plugin, exercises ADB discovery and virtual-camera-availability detection without crashing, and drives a real authenticated MJPEG round-trip (auth header, multipart framing, JPEG decode, and that an unauthenticated request is actually rejected) against a local test server. It isn't a substitute for testing against a real phone - see the manual [release checklist](release-checklist.md) and [device-compatibility matrix](device-compatibility.md) for that.
