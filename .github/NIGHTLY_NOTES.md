Rolling pre-release, rebuilt from every push to `master`. Stable releases are the ones tagged `v*`.

| File | What it is |
|------|------------|
| `TelescopeSetup.exe` | Windows installer. Installs for your user only (no admin), adds a Start menu entry and an uninstaller. Same files as the zip. |
| `Telescope-windows.zip` | Windows. Extract and run `TelescopeDesktop.exe`, keeping it in its folder. Includes UnityCapture and the phone app. adb for USB is downloaded from Google when you ask for it. |
| `Telescope-linux.tar.gz` | Linux. Extract and run `./start.sh`, which installs the Python dependencies and launches. Includes the phone app. |
| `Telescope.apk` | Android app. |
| `manifest.json` | Version and checksums, read by the apps' update check. |
