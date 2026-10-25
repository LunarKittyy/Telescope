# Release checklist

Everything that has to hold before a version gets tagged. Most of it is checked by something other than a person now:

- **The Release workflow** runs the desktop tests, the Android unit tests, the smoke checks, the installer checks (install, update, uninstall, an unzipped copy moving to the installed one, the driver) and `.github/check_release.py` (the APK's signing key, the version in every file, what the bundles carry and the manifest's checksums). It won't publish when one of them fails.
- **`automation/release-testing/emulator_pass.py`** goes through what an Android emulator and an isolated Linux profile can do against the nightly: a fresh phone install and Get set up, USB pairing, streaming with the phone's Status card and Live Update, transforms and the wait screen in the virtual camera, the remote start toast, re-pairing and removing from either side, the Start failure banner, the heat step, plugins (install, the warning, black and white output, changed files, a plugin that crashes, a built-in switched off, Remove), a broken config, single instance, the Linux menu entry and sign-in entry, Copy diagnostics and updating an older phone app over USB. Its [README](../automation/release-testing/README.md) has the setup.
- **`.github/checklist_due.py`** lists which of the sections below the changes since the last release can reach.

That leaves a short pass on a real phone every release plus whatever sections are due.

## Cutting a stable release

1. Set `VERSION` to the new version (e.g. `3.1.0`) and merge that to `master`.
2. Against that commit's nightly: run `emulator_pass.py`, go through [Every release](#every-release), then run `python3 .github/checklist_due.py` and go through each section it lists.
3. Tag it and push the tag: `git tag v3.1.0 <commit> && git push origin v3.1.0`.
4. `release.yml` builds everything and publishes the release. It refuses a tag that doesn't match `VERSION`.
5. Merge the draft PRs held for the release: README and website screenshots or text that show what's new. Until the release is out they would advertise the nightly.
6. Rename `docs/roadmap/next.md` after the release (for example `3.6.md`), add it to the list in `docs/roadmap/README.md` and start a fresh `next.md` with whatever didn't make it. Patch releases go into the file of the release they patch.

The APK signing key lives in the repository secrets (see [Building and CI](building.md#ci)). Keep a backup of the keystore outside GitHub. Without it no future APK can install as an update over the current one.

## Every release

- [ ] `emulator_pass.py` passes against the nightly (a skip needs a reason that holds, like an emulator camera without 1080p).
- [ ] `python scripts/phone_check.py --wifi-drop 5 --handover` in `desktop/` passes against the dev app on a real phone (it exits with 1 when something didn't work).
- [ ] What this release's notes promise works on a real phone and on both desktops.
- [ ] Camera controls (lens, exposure, WB, OIS) apply live on a real phone and match what the desktop shows.
- [ ] A call or OBS on Linux and on Windows picks up the Telescope camera and the Telescope microphone and still has them after a Stop and Start.
- [ ] `desktop/constraints.txt` hasn't drifted out of date and for every dependency bumped since the last release its entries in [Upstream bugs](upstream-bugs.md) are checked (fixed ones have their workaround dropped).
- [ ] Device-compatibility matrix updated with test results.
- [ ] Release notes written in `.github/release-notes/<VERSION>.md` (the release uses them; without one it falls back to generated notes).

## Only when due

`checklist_due.py` reads the comment under each heading below: the files and packages that section depends on. A section is due when one of them changed since the last release. Code that everything leans on (the main window, the stream service, the protocol) is listed as shared and a change there brings up every section as worth a look along with how big the change is. How far it reaches is for a person to judge. Any other file that's in no section makes the script fail until someone puts it in one, which keeps the lists from going stale. When unsure where a file goes, put it in more than one section.

<!-- shared: desktop/main.py desktop/telescope/app.py desktop/telescope/models.py desktop/telescope/plugin.py desktop/telescope/session.py desktop/telescope/theme.py desktop/telescope/i18n.py desktop/telescope/version.py desktop/telescope/__init__.py desktop/telescope/plugins/__init__.py desktop/telescope/widgets/__init__.py desktop/telescope/widgets/common.py desktop/telescope/widgets/tiles.py desktop/telescope/widgets/banner.py desktop/telescope/widgets/line_icons.py desktop/requirements.txt desktop/constraints.txt android/app/src/main/kotlin/com/telescope/MainActivity.kt android/app/src/main/kotlin/com/telescope/CameraStreamService.kt android/app/src/main/kotlin/com/telescope/Protocol.kt android/app/src/main/kotlin/com/telescope/Strings.kt android/app/src/main/kotlin/com/telescope/StreamPrefs.kt android/app/src/main/kotlin/com/telescope/CardHeader.kt android/app/src/main/kotlin/com/telescope/SystemBars.kt android/app/src/main/AndroidManifest.xml android/app/src/main/res/layout/** android/app/src/main/res/values/** android/app/src/main/res/xml/** android/app/src/main/res/menu/** android/app/src/main/res/resources.properties android/app/build.gradle android/build.gradle android/settings.gradle android/gradle.properties android/gradle/** android/gradlew android/gradlew.bat .github/workflows/build-apk.yml VERSION ; packages: pyqt6 pyqt6-qt6 pyqt6-sip -->

<!-- ignore: desktop/tests/** desktop/scripts/check_plugin.py desktop/scripts/dev_desktop.py desktop/scripts/i18n.py desktop/scripts/translation_*.py desktop/scripts/translation_preview/** desktop/translation-state/** desktop/scripts/phone_check.py desktop/scripts/smoke_check.py desktop/scripts/text_fit.py desktop/scripts/windows_move_check.py desktop/scripts/write_build_info.py desktop/examples/** desktop/telescope/locale/** desktop/MODULES.md desktop/ARCHITECTURE.md desktop/pytest.ini desktop/requirements-dev.txt android/app/src/test/** android/app/proguard-rules.pro docs/** site/** automation/** README.md CONTRIBUTING.md CLAUDE.md LICENSE .gitignore .github/release-notes/** .github/pull_request_template.md .github/ISSUE_TEMPLATE/** .github/NIGHTLY_NOTES.md .github/dependabot.yml .github/check_release.py .github/checklist_due.py .github/workflows/pages.yml .github/workflows/dependency-graph.yml .github/DISCUSSION_TEMPLATE/** .github/FUNDING.yml .github/python_dependency_snapshot.py desktop/THIRD_PARTY_NOTICES.txt android/app/src/main/res/drawable*/** android/app/src/main/res/mipmap*/** android/app/src/main/res/color/** android/app/src/main/res/values-*/** ; packages: pytest pytest-xdist execnet iniconfig pluggy pygments -->

### Windows packaging

<!-- due: installer/** desktop/telescope.spec desktop/resources/** desktop/unitycapture/** desktop/start.bat desktop/update_guard.py desktop/telescope/splash.py desktop/telescope/platform/windows.py desktop/telescope/platform/winjob.py desktop/telescope/platform/__init__.py desktop/telescope/platform/adb_download.py desktop/telescope/vcam.py desktop/telescope/single_instance.py desktop/telescope/widgets/adb.py desktop/scripts/write_splash.py desktop/scripts/write_icon.py desktop/scripts/windows_driver_check.ps1 .github/workflows/build-windows.yml .github/workflows/release.yml ; packages: pyinstaller pyinstaller-hooks-contrib altgraph packaging setuptools jinja2 markupsafe pywin32 -->

- [ ] The first start after installing shows the splash within a few seconds, the window never shows Not Responding (open Add phone and Settings during the first minute) and it takes about as long as the last release did or less.
- [ ] `TelescopeDesktop.exe` has the Telescope icon, the first-run checklist's Install driver registers UnityCapture (one UAC prompt naming Windows PowerShell), **Get adb** downloads adb and USB pairing then works and no `adb.exe` is left running after quitting.
- [ ] Settings, Add phone, Your phones and the shortcut editor open centred with no stray small windows flashing up.
- [ ] `TelescopeSetup.exe` started from Explorer installs without a UAC prompt and the Start menu entry opens Telescope. Uninstalling from Settings → Apps asks before deleting settings and after one UAC prompt takes the camera out of the camera list.
- [ ] An unzipped copy of the previous release: Updates says updating installs it, **Update and restart** comes back as the installed copy with settings and phones kept and the unzipped folder goes. **Keep this copy where it is** updates in place instead. Windows asks about the firewall once more and Add phone over Wi-Fi works after allowing it. A copy on a USB stick doesn't offer the move.
- [ ] Opening `TelescopeDesktop.exe` from inside the zip without extracting says to extract it first.
- [ ] OBS and Zoom list the camera as **Telescope**. On a machine registered by an older version, Virtual camera & phone offers Rename (or **Reinstall for a security fix** for a driver registered from the app folder) and streaming works before and after.

### Linux system setup

<!-- due: desktop/start.sh desktop/telescope/platform/linux.py desktop/telescope/platform/autostart.py desktop/telescope/vcam.py .github/workflows/build-linux.yml ; packages: pyvirtualcam jeepney -->

- [ ] Module not loaded: Start asks once with the startup box ticked and after a reboot Start doesn't ask. Unticked, it asks again after a reboot.
- [ ] Without pkexec (or with no polkit agent): Start shows the command banner, Copy command works and running it then Start streams.
- [ ] Open Telescope when I sign in (Linux and Windows): after a real sign-out and sign-in Telescope is in the tray.

### Updates and settings from older versions

<!-- due: desktop/telescope/updates.py desktop/update_guard.py desktop/telescope/plugins/updates.py desktop/telescope/config.py android/app/src/main/kotlin/com/telescope/Updater.kt android/app/src/main/kotlin/com/telescope/UpdateLogic.kt android/app/src/main/kotlin/com/telescope/UpdateHttp.kt .github/write_manifest.py .github/workflows/release.yml installer/** -->

- [ ] Desktop on the previous nightly (Windows and Linux): the Update button appears, Update and restart replaces the app and it comes back on the new version with the stream working.
- [ ] Update is refused while streaming, on both apps.
- [ ] Phone on the previous nightly: the update card offers the new build, asks once to allow installs from Telescope and the installed app shows the new version.
- [ ] Phone app older than the desktop with a different session protocol, plugged in: the Connection panel says so and Update over USB installs the bundled APK.
- [ ] Windows: an update cut short (kill the app mid-install) gets finished on the next launch, or comes back on the old version.
- [ ] Settings stay across a restart, including each phone's own after switching between phones. A config from the previous version keeps the global settings and asks to pair again.

### Pairing and networks

QR code advertises desktop addresses, phone sends LAN attempts on Wi-Fi.

<!-- due: desktop/telescope/pairing.py desktop/telescope/discovery.py desktop/telescope/ip_utils.py desktop/telescope/pinned_https.py desktop/telescope/session_client.py desktop/telescope/phones.py desktop/telescope/phone_client.py desktop/telescope/dev_profile.py desktop/telescope/plugins/connection.py desktop/telescope/plugins/setup.py desktop/telescope/plugins/onboarding.py desktop/telescope/platform/adb_download.py desktop/telescope/widgets/adb.py desktop/telescope/widgets/qr.py android/app/src/main/kotlin/com/telescope/Pairing.kt android/app/src/main/kotlin/com/telescope/PairedComputers.kt android/app/src/main/kotlin/com/telescope/SessionServer.kt android/app/src/main/kotlin/com/telescope/SessionEndpoint.kt android/app/src/main/kotlin/com/telescope/LanAnnouncer.kt android/app/src/main/kotlin/com/telescope/WifiLink.kt android/app/src/main/kotlin/com/telescope/TlsIdentity.kt android/app/src/main/kotlin/com/telescope/PhoneTls.kt android/app/src/main/kotlin/com/telescope/ScanActivity.kt android/app/src/main/kotlin/com/telescope/SetupSteps.kt android/app/src/main/kotlin/com/telescope/WaitingService.kt android/app/src/main/kotlin/com/telescope/RemoteStartNotice.kt ; packages: zeroconf ifaddr cryptography cffi pycparser pyopenssl truststore certifi qrcode -->

- [ ] Add phone by QR over Wi-Fi on Linux and on Windows and by plugging in over USB on Windows.
- [ ] The setup guide ticks each step as it's done and the video stage replaces it after the first stream. The APK installs through its QR download link and through its own Install over USB button.
- [ ] USB with the debugging prompt not yet accepted: Add phone says to allow it and pairs once it's accepted.
- [ ] One phone paired with two computers: both stream (one at a time) and removing one computer on the phone leaves the other working.
- [ ] Offline router (no WAN uplink at all): pairing still works and the QR code lists the LAN address.
- [ ] Desktop VPN only (LAN access permitted): pairing works over the LAN address (QR lists it, not just the VPN).
- [ ] Phone VPN only (LAN access permitted): pairing works (tests the Wi-Fi-bound first attempt).
- [ ] Both devices on a VPN (both LAN-accessible): pairing works.
- [ ] Tailscale on both (no shared LAN): pairing works over `100.64/10`.
- [ ] VPN blocking local-network traffic: pairing fails (the phone's dialog lists each address) and USB pairing works.
- [ ] Desktop with Docker, libvirt or VirtualBox installed: the QR code doesn't advertise `docker0`/`virbr0`/`vboxnet0` addresses.
- [ ] Phone on a tailnet the desktop isn't on: after Wi-Fi pairing the line under the Connection status shows the phone's Wi-Fi address (not `100.64/10`) and the stream works.
- [ ] Automatic route: plugged in uses USB, unplugged uses Wi-Fi. Plugged in with the app closed, or with a different phone on the cable, uses Wi-Fi and the card says why.
- [ ] Pulling the cable mid-stream on USB carries on over Wi-Fi and plugging it back in can use USB again.
- [ ] Connect via USB only / Wi-Fi only is honoured, including reporting a missing cable instead of falling back (the banner's Switch to Automatic works).
- [ ] Local only on, no cable: the desktop says the phone accepts USB only and Start doesn't open the camera. Plug in and it streams over USB. A second machine on the network can't reach the phone.
- [ ] Phone gets a new IP from the router: the desktop finds it again (mDNS) without re-pairing.
- [ ] USB-only viewers hold no Wi-Fi performance lock.
- [ ] Close the window to the tray with a phone waiting and Automatic streaming off: the phone stops getting checked (no new `/v1/ping` in its logs) and opening the window checks it straight away.

### Streaming

<!-- due: desktop/telescope/stream.py desktop/telescope/stream_flow.py desktop/telescope/sources.py desktop/telescope/h264_reader.py desktop/telescope/mjpeg_reader.py desktop/telescope/udp_reader.py desktop/telescope/jpeg_guard.py desktop/telescope/pyav_vaapi.py desktop/telescope/widgets/vaapi_setup.py desktop/telescope/plugins/stream_output.py desktop/telescope/plugins/transforms.py desktop/telescope/plugins/monitoring.py desktop/telescope/diagnostics.py android/app/src/main/kotlin/com/telescope/CameraSessionController.kt android/app/src/main/kotlin/com/telescope/StreamStateMachine.kt android/app/src/main/kotlin/com/telescope/StreamLauncher.kt android/app/src/main/kotlin/com/telescope/H264Encoder.kt android/app/src/main/kotlin/com/telescope/H264Stream.kt android/app/src/main/kotlin/com/telescope/CodecFailure.kt android/app/src/main/kotlin/com/telescope/MjpegServer.kt android/app/src/main/kotlin/com/telescope/UdpVideo.kt android/app/src/main/kotlin/com/telescope/HttpWire.kt android/app/src/main/kotlin/com/telescope/DynamicBitrate.kt android/app/src/main/kotlin/com/telescope/DynamicFrameRate.kt android/app/src/main/kotlin/com/telescope/FrameRate.kt android/app/src/main/kotlin/com/telescope/FrameGate.kt android/app/src/main/kotlin/com/telescope/PendingLimiter.kt android/app/src/main/kotlin/com/telescope/RecentRuns.kt ; packages: av numpy opencv-python-headless pyee -->

- [ ] H.264 on a real phone: a fresh phone starts on Light and switching Format between Light and Heavy reconnects and streams on Wi-Fi and USB. Compare latency (a clock on screen) and the Mbps readout with MJPEG at the same size. A 30-minute run stays smooth, a reconnect (unplug, Wi-Fi off and on) recovers and changing resolution or lens mid-stream keeps working.
- [ ] H.264 fallback: a size the encoder refuses (4:3 4K on most phones) stops with a banner to try a lower resolution or FPS and its Switch to Heavy button streams on Heavy. A phone without an encoder switches to Heavy automatically, with no banner.
- [ ] FPS: the dropdown greys out rates the camera doesn't list. On a phone that lists 60, picking 60 shows about 60 in the footer and Copy diagnostics shows what the camera actually did.
- [ ] Plugging in mid-stream over Wi-Fi (Connect via Automatic) moves the stream to USB a few seconds later without a gap in the video and the mic moves with it.
- [ ] UDP, mic off: the phone says Streaming. Removing the computer on the phone stops its video within a second and a lost desktop releases its UDP session within 5 seconds.
- [ ] UDP to Dynamic and the UDP switch both reconnect onto the right transport and changing to Heavy leaves no UDP session behind.
- [ ] Linux, AMD or Intel card: **Set up VA-API…** under Video uses the distro's PyAV or builds one and after **Restart now** Hardware acceleration says it found VA-API.
- [ ] Hardware acceleration: a driver failure resumes in software without missing setup headers and Copy diagnostics reflects the decoder still running if the preference changes mid-stream.
- [ ] On a phone that runs hot (Android 11+) stream for 15 minutes or more. The frame rate drops to 24 fps shortly before Android itself reports severe heat, the FPS readout's tooltip says the phone got hot and it goes back up once the phone cools.
- [ ] Battery and temperature alerts fire once per threshold cross, not repeatedly.
- [ ] Canvas size change (Linux and Windows) restarts cleanly.

### Camera controls and preview

<!-- due: desktop/telescope/plugins/camera_control.py desktop/telescope/plugins/preview.py desktop/telescope/plugins/presets.py desktop/telescope/view.py desktop/telescope/view_controller.py desktop/telescope/widgets/lens_panel.py desktop/telescope/widgets/controls.py android/app/src/main/kotlin/com/telescope/CameraCatalog.kt android/app/src/main/kotlin/com/telescope/LensLabel.kt android/app/src/main/kotlin/com/telescope/PreviewActivity.kt -->

- [ ] Phone in the foreground: the desktop's Start and Stop control the camera, also with the screen dark. A lens, size and OIS setting picked on the desktop come back after Stop and Start and the phone's Stop streaming stops the stream.
- [ ] Zoom on the Auto lens: the lens marks on the zoom slider switch to the telephoto (the dot turns lavender) and panning out of its view falls back (red dot). Sliding the zoom back and forth quickly on the telephoto never turns the dot red. Scrolling the preview zooms around the mouse, dragging pans and the lens outlines show and fade.
- [ ] Zoom and Brightness sliders and the pan pad stick at their neutral spots and marks when dragged, never with the arrow keys and Temperature and Tint never stick. A double-click resets them all.
- [ ] Point focus: clicking a near and a far object in the preview focuses each (with and without zoom, flip and rotation), exposure follows the point in auto and Auto returns to continuous. The pop-out works the same.
- [ ] Presets: switching between two presets while streaming changes lens, exposure, WB, zoom and fps together. A second phone doesn't see the first one's presets.

### Microphone

<!-- due: desktop/telescope/audio.py desktop/telescope/plugins/microphone.py desktop/telescope/platform/virtual_mic.py desktop/telescope/widgets/level_meter.py android/app/src/main/kotlin/com/telescope/AudioStream.kt android/app/src/main/kotlin/com/telescope/AudioStreamer.kt android/app/src/main/kotlin/com/telescope/MicProblem.kt ; packages: sounddevice -->

- [ ] Linux (PipeWire): switching it on while streaming makes "Telescope Microphone" appear, a call records the phone and switching off or quitting removes it. Lip-sync looks right by eye. A 30-minute run doesn't drift and unplugging and replugging recovers.
- [ ] Windows: without VB-Cable the card offers Install VB-Cable. It asks first (naming VB-Audio), opens VB-Audio's setup through UAC and after a restart and an off/on apps record from CABLE Output.
- [ ] The meter moves with your voice even when no app is recording and its dot goes yellow on a loud clap (red with the limiter off under Audio). Mute (card and tray) gives silence without the mic disappearing from the call app. Gain changes and typed values are audible without crackle and Max gain changes the slider's range. On Windows too.
- [ ] Permission: the first request makes the phone's Get set up card show Microphone and allowing it there starts the audio within a few seconds without restarting the stream. Recording keeps going with the phone's screen off.
- [ ] Automatic routing, mic only: plugging in USB moves the microphone too and rapid mic off and on leaves no recording held.
- [ ] Auto level: beginning in silence or recovering from a gap doesn't turn up steady room noise.

### Starting, stopping and the camera button

<!-- due: desktop/telescope/plugins/startup.py desktop/telescope/plugins/wait_screen.py desktop/telescope/vcam.py desktop/telescope/single_instance.py desktop/telescope/plugins/microphone.py -->

- [ ] Start set to "When the phone is ready": opening the phone app (or plugging it in) starts the stream, Stop keeps it stopped until the phone leaves and comes back and closing the window keeps it in the tray.
- [ ] Start set to "When an app opens the camera" and Stop to "When no app uses the camera, if it started the stream" (Linux and Windows): opening the camera in OBS or a call starts the stream and closing it stops it 15 s later.
- [ ] Windows, Stop set to "when no app uses the camera, even if you started it": a stream started by hand before any app opened the camera stops after the wait.
- [ ] Camera off: in a call using both Telescope devices, press the camera button under the preview. The call shows the wait screen and still hears the mic, the phone's camera light goes out and the tray icon loses its red dot. Press it again and the picture comes back.
- [ ] Mic only from the start: with the mic on, press the camera button, then **Start mic only**. The phone opens no camera and the call hears the mic.
- [ ] With the mic on and Stop set to "when no app uses the camera": turn the camera off in the call. After the wait only the camera turns off and turning it back on in the call brings the picture back.
- [ ] Camera off on a phone in the background (screen off, **Wait for my computer** on): turn it off and on again. It comes back and if another app holds the camera it stays off with a banner saying so.
- [ ] Wait screen: a chosen image and a GIF reach apps and Mirror flips them. Starting and stopping a stream while a call has the camera open keeps the picture in the call.
- [ ] Tray minimise and restore work.

### Browser camera

<!-- due: desktop/telescope/browser_rtc.py desktop/telescope/browser_server.py desktop/telescope/web.py desktop/telescope/web/** desktop/telescope/plugins/browser_camera.py desktop/requirements-nodeps.txt ; packages: aiortc aioice av pylibsrtp pyee google-crc32c dnspython typing-extensions cryptography pyopenssl -->

- [ ] iPhone in Safari: scan the code, tap through the certificate warning, Start. Video and mic reach the virtual camera and mic, the screen stays on and after locking the phone and coming back to the page the stream picks up again. Flip camera flips lenses.
- [ ] Black screen (portrait and landscape): the page goes black and, on Android, full screen without the status and nav bars. One tap shows the hint and fades again, a double tap brings the page back and the stream keeps going the whole time.
- [ ] Black screen on iPhone: a black video opens in the system player and hides the status bar, the stream keeps going behind it and closing the player brings the page back.
- [ ] Android in Chrome and a second computer's browser: the same. Resolution and Frame rate on the card change what arrives. New link drops the browser and the old code stops working. Opening the link in a second tab takes over from the first.
- [ ] Windows: the firewall prompt (or a rule) lets port 8767 through and picking a phone again stops the server.

### Settings, keyboard and folding

<!-- due: desktop/telescope/widgets/settings_window.py desktop/telescope/widgets/settings_search.py desktop/telescope/widgets/collapse.py desktop/telescope/fuzzy.py desktop/telescope/shortcuts.py desktop/telescope/plugins/shortcuts.py desktop/telescope/platform/hotkeys.py desktop/telescope/platform/hotkeys_portal.py desktop/telescope/platform/hotkeys_windows.py desktop/telescope/phones.py ; packages: jeepney -->

- [ ] Every Settings tab opens and the settings button brings the window back on the tab it was left on.
- [ ] Folding: clicking a card title and a section title folds and unfolds them, Tab reaches the titles and Space or Enter toggles them. A shortcut for a control in a folded card still works. Folds come back after a restart.
- [ ] Tab through the tiles and switches, use rotation and pan shortcuts saved before this release, pan with arrow keys and Home and inspect labels with a screen reader.
- [ ] Your phones: Rename works, Change name changes what a newly paired phone shows and saved browsers are listed with the phones.

### Vision effects

<!-- due: desktop/telescope/vision.py desktop/telescope/pose.py desktop/telescope/vision_pack.py desktop/telescope/vision_pack.json desktop/telescope/plugins/background.py desktop/telescope/plugins/framing.py desktop/telescope/widgets/vision_download.py desktop/telescope/widgets/vision_status.py desktop/scripts/build_vision_pack.py desktop/requirements-vision.txt ; packages: mediapipe opencv-python-headless numpy -->

- [ ] Auto framing and Background (Linux and Windows): switching one on under **Optional** asks before downloading, shows the progress and after **Restart now** its card shows. Auto framing follows you while streaming from a real phone, widens when you hold a hand out and zooms back out when you leave, also at 90, 180 and 270 degrees and flipped. Zoom and Pan in Transforms grey out while it's on. Background blurs and puts a picture behind you. With both off **Remove download** deletes the download.

### Plugins on Windows

<!-- due: desktop/telescope/plugin_loader.py desktop/telescope/widgets/plugins_panel.py desktop/telescope.spec -->

- [ ] The Windows app installs a plugin `.zip` with **Install** and by dragging it from Explorer and after switching it on and **Restart now** its card shows. Make the zip from `desktop/examples/plugins/black_and_white` with the folder itself inside it (`emulator_pass.py` does the rest of the plugin checks on Linux).
