# Release testing

Tools and notes for going through [the release checklist](../../docs/release-checklist.md) against a nightly without touching your own install: an isolated Linux profile the scripts here can drive, a Windows 11 VM and the Android emulator. They don't replace the real phone and desktop part of the checklist (see the end of this page).

`fetch_nightly.sh [tag]` downloads a release's assets (the nightly by default) with `gh` into `downloads/n<build>` and checks each one against the manifest's sha256. Set `NIGHTLY_DIR` to put them somewhere else, such as a folder your Windows VM can see. `downloads/` is ignored by git.

## Linux

Everything runs in its own profile (`linux/.profile`, with its own HOME and XDG folders) on an offscreen Qt to leave your real install and desktop alone. The log and the single-instance lock are still shared with a real install (`/tmp/telescope-$USER`), so keep sessions short and don't run both at once. `linux/.profile` is ignored by git.

```bash
linux/install.sh downloads/n810/Telescope-linux.tar.gz   # unpack and build the venv through start.sh
linux/run_app.sh                       # start it under drive.py in the background (the first launch takes about 20s)
linux/drv.sh 'print(shot("main.png", main_win()))' 200
linux/stop_app.sh
```

`drv.sh` runs Python inside the app and prints what it printed. The number is how long to wait for an answer, in tenths of a second. Helpers from `drive.py`: `main_win()`, `wins()`, `shot(path, widget)`, `buttons(root, text)`, `click(text, root, n)`, plus `QTest`, `Qt` and `QPoint`. Each snippet runs as its own Qt event, so a snippet that opens a modal dialog returns and the next one can click inside it. Point `RT_APP` at another unpacked copy to test a moved folder or an older version.

Things that tripped me up:
- File dialogs can't be driven offscreen. To install a plugin zip, send the plugins panel a simulated drop event instead.
- Corrupt config: write junk to `linux/.profile/home/.config/telescope/telescope_config.json` and start. Expect a `.invalid-<time>` copy and the banner.
- Updates: install an older build, set the channel to Nightly in Settings, then Update and restart. It comes back with `--after-update`.

## Windows

A Windows 11 VM with a clean snapshot to go back to is enough. Keep UAC at its default (prompting for consent), which is what real users have.

- Start things the way a user would: unelevated, through Explorer. Anything started elevated (as many VM tools' "run a command" features do) hides UAC bugs.
- UAC prompts show up on screenshots and take clicks. The driver install and the camera removal on uninstall both show "Windows PowerShell" as the app asking.
- Installed copy: `%LOCALAPPDATA%\Programs\Telescope`, with the version under `HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*` (`DisplayVersion`). Settings live in `%APPDATA%\Telescope`, adb in `%LOCALAPPDATA%\Telescope\platform-tools`.
- DirectShow camera check: the DLLs go in `C:\Program Files\Telescope`, owned by Administrators.
- Opening the exe from inside the zip fails in the PyInstaller loader ("Failed to load Python DLL") on current Windows 11, in 3.5.0 too. Explorer only extracts the exe into `%TEMP%\<guid>_<zip>.<n>`, so our own "extract first" warning never runs.

## Android (emulator)

- An Android 14 emulator does fine. After boot it needs `adb shell svc wifi enable; adb shell svc data enable` before it has internet.
- A leftover `com.telescope` with another signature blocks installs. `adb uninstall com.telescope` first.
- `adb install -r Telescope.apk` over the previous release checks the upgrade path and keeps pairings.
- To see the Update card without waiting for a newer nightly, install a dev build with a lower build number: `./gradlew installDev -Pchannel=nightly -PbuildNumber=800` in `android/`. It installs next to the real app as `com.telescope.dev`.
- Skip the "allow installs" settings screen with `adb shell appops set com.telescope.dev REQUEST_INSTALL_PACKAGES allow`.
- Screenshots: `adb exec-out screencap -p > shot.png`. Taps are in device pixels.
- To talk to the phone's session server (port 8766) from the host: `adb forward tcp:48766 tcp:8766`, then `curl -H "Authorization: Bearer <token>" localhost:48766/v1/ping`. A 401 means the token was revoked. Remove the forward afterwards.
- USB pairing and Install over USB with the Linux desktop work through the emulator's adb.

## What these can't cover

A real camera and GPU (hardware encoding, Auto framing and Background quality), OBS, Zoom or Discord picking up the camera, VPN and Tailscale routing, and the phone-outdated banner (that needs a protocol change between versions).
