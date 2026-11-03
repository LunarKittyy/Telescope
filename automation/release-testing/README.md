# Release testing

Tools and notes for going through [the release checklist](../../docs/release-checklist.md) against a nightly without touching your own install: a Windows 11 VM and the Android emulator. They don't replace the real phone and desktop part of the checklist (see the end of this page).

`fetch_nightly.sh [tag]` downloads a release's assets (the nightly by default) with `gh` into `downloads/n<build>` and checks each one against the manifest's sha256. Set `NIGHTLY_DIR` to put them somewhere else, such as a folder your Windows VM can see. `downloads/` is ignored by git.

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
- On a freshly restored snapshot the first start check sometimes never gets past finding Telescope's window, though the window is up and responding (more often than not). The desktop job is stuck in UI Automation, not Telescope. Run `--only clean_install first_start` again.
- The update checks land on whatever the channel's newest build is, which isn't necessarily the build being checked once newer nightlies are out. A run that overlaps a nightly being published can fail them; run those checks again with `--only`.
- On Windows 11 the camera checks expect Telescope's own camera (`rust/windows/virtualcamera`), not UnityCapture. The older-driver check registers UnityCapture the way older versions did and expects Update to replace it. On Windows 10 they check UnityCapture as before.
- The checks move the main window inside the screen before clicking it, after `window_fits_screen` has read where it opened. Controls scrolled out of view aren't in UI Automation's tree at all, so the checks turn the mouse wheel until they appear.
- **Get adb** stays skipped unless you pass `--agree-adb-licence`, since it means accepting Google's SDK licence in the app.
- Windows' firewall question can't be reached through UI Automation and it swallows clicks elsewhere. The script closes it whenever it comes up, like dismissing it would.

## Windows

A Windows 11 VM with a clean snapshot to go back to is enough. Keep UAC at its default (prompting for consent), which is what real users have.

- Start things the way a user would: unelevated, through Explorer. Anything started elevated (as many VM tools' "run a command" features do) hides UAC bugs.
- UAC prompts show up on screenshots and take clicks. The driver install and the camera removal on uninstall both show "Windows PowerShell" as the app asking.
- Installed copy: `%LOCALAPPDATA%\Programs\Telescope`, with the version under `HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*` (`DisplayVersion`). Settings live in `%APPDATA%\Telescope`, adb in `%LOCALAPPDATA%\Telescope\platform-tools`.
- DirectShow camera check: the DLLs go in `C:\Program Files\Telescope`, owned by Administrators.

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
