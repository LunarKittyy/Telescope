Rolling pre-release, rebuilt from every push to `master`. Stable releases are the ones tagged `v*`.

| File | What it is |
|------|------------|
| `Telescope-windows.zip` | Windows. Extract and run `TelescopeDesktop.exe`. Includes adb, UnityCapture and the phone app. |
| `Telescope-linux.tar.gz` | Linux. Extract and run `./start.sh`, which installs the Python dependencies and launches. Includes the phone app. |
| `Telescope.apk` | Android app. |
| `manifest.json` | Version and checksums, read by the apps' update check. |
