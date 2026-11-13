<!-- covers: .github/workflows .github/check_release.py rust/tools installer rust/windows rust/crates/system/src/update_guard.rs rust/crates/vision/src/pack.rs rust/crates/vision/vision_pack.json rust/crates/vision/matting_pack.json; checked: b7f3c283e -->
# Packaging and CI

How the desktop app is packaged and what each workflow does. [Building](building.md) covers building and trying a change.

## Package the app

The desktop app packages into the shape the old Python release had, which lets the Python app's updater install it ([the update path](#the-update-path)). `release.yml` runs everything below through `rust-package.yml`.

FFmpeg comes first. `rust/tools/build_ffmpeg.sh` makes a small LGPL build with only the decoders the app opens (about 5 MB instead of about 170 MB) and it wants Ubuntu 22.04 for both targets, Windows' through mingw:

```bash
docker run --rm -v "$PWD:/src" -w /src ubuntu:22.04 bash -c '
  apt-get update && apt-get install -y --no-install-recommends build-essential curl ca-certificates xz-utils nasm patchelf pkg-config \
    libva-dev libdrm-dev zlib1g-dev gcc-mingw-w64-x86-64-win32 libz-mingw-w64-dev
  bash rust/tools/build_ffmpeg.sh linux rust/target/ffmpeg-linux
  bash rust/tools/build_ffmpeg.sh windows rust/target/ffmpeg-windows'
```

The GPL build with libx264 is only for the tests, which need an H.264 encoder.

On Linux `rust/tools/package_linux.sh --ffmpeg DIR --channel nightly --build 123 --commit <sha>` writes the folder that becomes `Telescope-linux.tar.gz`. Build it on Ubuntu 22.04 as well, because the machine it's built on sets the oldest Linux it runs on. Besides the program it holds a `start.sh`, a `main.py` that only hands over to `start.sh` and a `telescope/` folder with the stamp, because the Python updater turns down a tarball without the last two. FFmpeg is in `lib/`. `lib/fallback/` has libva for a system that has none (`start.sh` only uses it then, because a system's own libva already knows where its video drivers are). `lib/fallback-fonts/` does the same for fontconfig and what it needs, in a folder of its own so that a missing libva can't put those ahead of the system's.

`rust/tools/package_appimage.sh BUNDLE OUT` turns that folder into `Telescope-x86_64.AppImage`. It needs `squashfs-tools` 4.5 or newer and `curl` for the runtime, which is the pinned static type 2 runtime from `AppImage/type2-runtime` with its checksum in the script. `APPIMAGE_RUNTIME=<file>` uses a copy you already have. The static runtime doesn't need `libfuse2`, only a `fusermount` or `fusermount3` on the system. Without one, `--appimage-extract-and-run` still starts the program. The AppImage holds the folder without `start.sh`, `main.py` and `telescope/` and adds `rust/tools/AppRun` as its entry point, a `.desktop` file and the icon from `telescope-rs app-icon`. Change the runtime release and `RUNTIME_SHA256` together.

The mount is read-only and the program writes nothing inside it. Settings, plugins, vision packs and logs live in the user's folders as they do for the tarball. Its first lines in `main` note the AppImage and clear `APPIMAGE`, `APPDIR`, `ARGV0`, `OWD` and `AppRun`'s folders in `LD_LIBRARY_PATH` from the process, which covers every program it starts from then on (other apps, adb, pkexec, browsers). `FONTCONFIG_FILE` is the exception, because fontconfig loads it late and `AppRun` only sets it on a system with no fontconfig of its own. The mount goes when the process quits. Anything that starts Telescope again or writes a menu, sign-in or shortcut entry has to use the file's path and not `current_exe` (`telescope_core::system::start_command`).

An update replaces the file beside itself in one rename and leaves its permissions as they were. The journal, `.previous` and `.update-failed` are the tarball's and sit in the file's folder. `.previous` is gone once the new version has started fine. Before the swap the new file has to answer `build-info`, since a file whose runtime or `AppRun` is broken would have nothing left to roll back from. After the swap `update_guard` and `AppRun` undo a start that fails, the same way as `start.sh`.

Whatever it's built on sets the oldest Linux it runs on. Ubuntu 22.04 has glibc 2.35, which covers Debian 12, Fedora 36 and anything newer.

On Windows `rust/tools/build_windows.ps1 -Channel nightly -Build 123 -Commit <sha>` with `FFMPEG_DIR` set writes the folder that becomes `Telescope-windows-portable.zip`: `TelescopeDesktop.exe`, `ui-kit`, FFmpeg's and RNNoise's DLLs and both cameras. Because the C runtime is linked into the program, it starts on a Windows without the VC++ redistributable. The camera DLL's wait screen comes from `telescope-rs camera-wait-screen`. `installer/build.ps1 -Bundle <folder> -Out setup -Rust` packs that folder into the setup.

A stamped build (channel `stable` or `nightly`) uses the real settings folder, where a `dev` build uses a profile of its own.

`python rust/tools/check_package.py --version 4.2.0 --build 123 --channel nightly <folder, archive or AppImage>` is the package's smoke check.

The packaging scripts use Python 3 for two small jobs, `tools/fetch_rnnoise.py` (RNNoise out of a pinned wheel) and `tools/write_notices.py` (the notices file).

### The vision pack

MediaPipe and its two models come as a separate download that the optional plugins switch on (Auto framing, Background, Expose for my face). `python tools/build_vision_pack.py --check --out ../vision` from `rust/` builds the pack for both systems (pip fetches the other one's wheels) and checks it against `crates/vision/vision_pack.json`, which the app is built with. After changing a pin in `tools/vision-pack-requirements.txt`, run it with `--write` and commit the result. It also prints how big each pack is unpacked, which has to stay under `MAX_UNPACKED` in `crates/vision/src/pack.rs` (160 MiB, with the Linux pack at 135 MB), because the app stops unpacking a download at that size. Raise it if a pin bump makes the pack grow past it. `release.yml` publishes the zips next to the other files.

### The matting pack

Background's **High quality** option runs Robust Video Matting (the model, GPL-3.0) with ONNX Runtime (MIT) from a second download, kept in a `matting` folder next to the vision packs. `python tools/build_matting_pack.py --check --out ../vision` from `rust/` builds the pack for both systems from pinned downloads (every file is checked against a sha256 in the script) and checks it against `crates/vision/matting_pack.json`. After changing a pin, run it with `--write` and commit the result. The app loads the library by path from the pack with the `ort` crate's `load-dynamic` feature, so nothing is linked or bundled: the library is `libonnxruntime.so` on Linux and `onnxruntime.dll` on Windows (CPU only for now). `release.yml` builds it in the same job as the vision packs and publishes `Telescope-matting-<system>-<id>.zip` next to the other files, where nightly builds find it under the `nightly` tag.

## The update path

The Python updater doesn't need to know about Rust. On Windows it swaps `TelescopeDesktop.exe` and starts it. On Linux it swaps the tarball's top-level entries and runs `start.sh --after-update`, and it only accepts a tarball with `main.py` and a `telescope/` folder in it. So a Rust package has the same shape: `TelescopeDesktop.exe` on Windows, and on Linux a `start.sh` that starts the binary, a `main.py` that hands over to `start.sh` and a `telescope/_build.py` stamp (`tools/package_linux.sh` and `tools/build_windows.ps1` make them and `tools/check_package.py` checks them).

`crates/system/src/update_guard.rs` does what the Python `update_guard.py` did, with the same journal: finish a cut-short swap, confirm a trial once the window is up and roll back one that never starts. Its tests use the journal states a Python update leaves behind. The first Rust start also cleans up what the Python build left behind (the `lib-<build>` folders, `.previous/` and the old requirements and guard files).

## CI

### `release.yml`

Runs on every push to `master` that changes more than docs or the website, and on every `v*` tag. It builds everything from one commit and publishes it together:

- a push to `master` replaces the rolling **`nightly`** pre-release
- a tag `vX.Y.Z` creates the stable release `Telescope X.Y.Z` (the tag must match `VERSION`)

Each release holds `Telescope.apk`, `Telescope-windows-portable.zip`, `TelescopeSetup.exe`, `Telescope-linux.tar.gz`, `Telescope-x86_64.AppImage` and `manifest.json` (version, build number, channel, commit, session protocol, and each file's URL, size and SHA-256). The desktop bundles include the APK for Install over USB. The zip is also listed as `Telescope-windows.zip`, its old name, which older copies still look for when they update. The apps' update check reads the manifest: the phone compares `android.versionCode` and the desktop compares `build`. Every copy looks its own asset up by name. The Python app and older Rust copies pass over the AppImage and keep updating from the tarball.

The APK is signed with the release key from the repository secrets `TELESCOPE_KEYSTORE` (base64), `TELESCOPE_KEYSTORE_PASSWORD`, `TELESCOPE_KEY_ALIAS` and `TELESCOPE_KEY_PASSWORD`. A release fails rather than publish a debug-signed APK, because Android only installs an update signed with the same key as the installed app.

Before publishing, `.github/check_release.py` checks the files it's about to upload: the APK's certificate is the release key's (pinned in the script), the APK and the Linux bundle's stamp carry this version and build, both bundles hold the notices and the APK, the AppImage is there and listed, and `manifest.json`'s checksums match. `rust/tools/check_package.py --no-run` then looks through both bundles and the AppImage once more. That one is checked where the program can run, in `rust-package.yml`, because the Windows stamp is compiled in.

### The workflows for pull requests

These run on pull requests without publishing. `release.yml` calls them for the real thing. The checks called `android`, `windows` and `linux` have to pass before `master` takes a pull request (the maintainer can push past it). `build-apk.yml` and `rust.yml` each have a small `changes` job that skips the check when nothing it covers changed, which GitHub counts as passing. A docs-only pull request then never waits on a check that doesn't run. A new push cancels checks still running.

| Workflow | Runs for | What it does |
|---|---|---|
| `build-apk.yml` | `android/**` | `./gradlew lintDebug testDebugUnitTest`, then `assembleRelease`. In a release it checks the APK isn't debug-signed |
| `rust.yml` | `rust/**` | The `linux` and `windows` checks: builds the workspace and runs its tests with `cargo nextest`, and any compiler warning fails them. Its `changes` job also checks `VERSION` and that the release checklist names every file. On `master` it runs inside `release.yml` and doesn't start a second time |
| `rust-package.yml` | `rust/tools/**`, `rust/windows/**`, `installer/**` | Builds both FFmpegs and both packages and an AppImage and runs `check_package.py` on them. On pull requests it also builds a quick `TelescopeSetup.exe` and checks the installer, update and uninstall paths and an unzipped copy moving to the installed one. A release does those on the setup it publishes, in `release.yml` |

The Windows setup (`installer/Telescope.iss`) is per-user: it installs to `%LOCALAPPDATA%\Programs\Telescope` without admin, because the app updates itself in place and needs its folder writable. It doesn't touch the camera driver (the first-run checklist installs that). Uninstalling closes a running Telescope, deletes the log, the downloaded adb and the vision download, asks before deleting settings and pairings, and offers to remove the camera driver and firewall rules (one UAC prompt). Keep its `AppId` as it is since Windows finds an existing install by it.

### `pages.yml`

Publishes `site/`, the landing page, to GitHub Pages on pushes to `master` that touch `site/**`. There's no build step. The repository's Pages source has to be set to **GitHub Actions**. The page is served at `telescope.webcam`, set under Settings → Pages (not a `CNAME` file, which Actions deploys ignore), with DNS in Cloudflare as an unproxied CNAME to `lunarkittyy.github.io`.
