# Release testing

Tools and notes for going through [the release checklist](../../docs/release-checklist.md) against a nightly without touching your own install: an isolated Linux profile the scripts here can drive, a Windows 11 VM and the Android emulator. They don't replace the real phone and desktop part of the checklist (see the end of this page).

`fetch_nightly.sh [tag]` downloads a release's assets (the nightly by default) with `gh` into `downloads/n<build>` and checks each one against the manifest's sha256. Set `NIGHTLY_DIR` to put them somewhere else, such as a folder your Windows VM can see. `downloads/` is ignored by git.

## The emulator pass

`emulator_pass.py` goes through what the release checklist leaves to an Android emulator and the Linux profile below, and prints PASS, FAIL or SKIP with a reason for each check. `--list` shows them. A full run takes about 15 minutes.

```bash
./fetch_nightly.sh                                   # the release to check
./fetch_nightly.sh v4.1.0                            # and the last one, for the upgrade check
linux/install.sh downloads/n932/Telescope-linux.tar.gz
linux/.profile/home/.local/share/Telescope/venv/bin/python3 emulator_pass.py downloads/n932 --previous downloads/n838
```

It runs with the profile's own Python because the frame checks need OpenCV and NumPy and that one has both. It also needs `ffmpeg`, `v4l2-ctl`, `curl` and v4l2loopback loaded, and an emulator that's already running (adb has to see exactly one device). It uninstalls the phone app and starts the profile's settings fresh.

Which emulator to use:
- Run the whole pass on an Android 14 image with button navigation, the steadiest one. Its camera only goes up to 720p and the heat check gets skipped there.
- An Android 17 image is the one with the Local network row and a 1080p camera. With SwiftShader rendering its System UI crashes on going Home or taking a screenshot (`hasReadColorBufferDma`). Give it only the checks that stay inside the app: `--only phone_fresh_install phone_allow_everything desktop_fresh_start usb_pairing stream_and_status heat_step`.
- Turn button navigation on with `adb shell cmd overlay enable com.android.internal.systemui.navbar.threebutton`.

## The Windows pass

`windows_pass.py` does the Windows part of the checklist on a Windows 11 VM it reaches over SSH. It runs in the signed-in user's session, unelevated like anything started from Explorer, finds Telescope's controls through UI Automation and clicks them with the real mouse (so leave the VM's mouse alone while it runs). `--list` shows the checks and a full run takes about 20 minutes.

```bash
./fetch_nightly.sh
./fetch_nightly.sh v4.1.0
python3 windows_pass.py downloads/n932 --previous downloads/n838 --ssh "-p 2222 -i ~/.ssh/vm_key user@127.0.0.1"
```

What the VM needs:
- OpenSSH server with key login for an administrator account that's also signed in on the desktop (the checks run as a scheduled task in that session).
- A snapshot to go back to. The run uninstalls Telescope, deletes its settings, registers and removes the camera and creates a share called TelescopeTest.
- UAC at its default. For the run the script switches the prompt off (the prompts sit on the secure desktop where nothing can press them) and counts elevations from the security log instead, then puts both settings back.

Things to know:
- First start times only mean something on a fresh snapshot. Defender scans files the first time they run, which is most of the slowness the first start check is about. A VM that already ran nearly the same files (any other Telescope build) starts it in a couple of seconds. That's why the first start comes right after the install, before the update checks bring in other builds.
- On a freshly restored snapshot the first start check sometimes never gets past finding Telescope's window, though the window is up and responding (4 of 6 tries on 4.3.0 nightlies). The desktop job is stuck in UI Automation, not Telescope. Run `--only clean_install first_start` again.
- The update checks land on whatever the channel's newest build is, which isn't necessarily the build being checked once newer nightlies are out. A run that overlaps a nightly being published can fail them; run those checks again with `--only`.
- On Windows 11 the camera checks expect Telescope's own camera (`desktop/virtualcamera`), not UnityCapture. The older-driver check registers UnityCapture the way older versions did and expects Update to replace it. On Windows 10 they check UnityCapture as before.
- **Get adb** stays skipped unless you pass `--agree-adb-licence`, since it means accepting Google's SDK licence in the app.
- Windows' firewall question can't be reached through UI Automation and it swallows clicks elsewhere. The script closes it whenever it comes up, like dismissing it would.

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
