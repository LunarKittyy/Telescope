<!-- covers: .github/checklist_due.py .github/check_release.py rust/app/tests/release_checklist.rs automation/release-testing rust/tools/check_package.py rust/tools/windows_move_check.ps1; checked: b7f3c283e -->
# Release checklist

Everything that has to hold before a version gets tagged. Most of it is checked by something other than a person now:

- **The Release workflow** runs the desktop tests on both systems, the Android unit tests, the package checks, the installer checks (install, update, uninstall, an unzipped copy moving to the installed one) and `.github/check_release.py` (the APK's signing key, the version in every file, what the bundles carry and the manifest's checksums). It won't publish when one of them fails.
- **`automation/release-testing/windows_pass.py`** does the Windows part against the nightly on a Windows 11 VM, driving the app the way a user would. It covers opening the exe from inside the zip, an install without an admin prompt, the first start (splash, never Not Responding), the icon, the window opening inside the screen's usable area, Install driver with one prompt for Windows PowerShell and the camera's name in DirectShow, Settings opening centred over the window and inside the screen with nothing flashing up, the other dialogs appearing inside the window, the security fix and Rename for a driver an older version registered, installing a plugin with Install and by dragging it from Explorer, Remove taking an added plugin out, Get adb and adb gone after quitting, uninstalling, updating the installed previous release, an unzipped copy updating into the installed one or in place, a copy on a network drive (the same rule as a USB stick) and an update cut short.
- **The same script's install checks** cover the setup's mode page and its all-users label, an install for all users (Program Files, a machine uninstall entry), its update through the setup with one admin prompt, its uninstall and an install into a chosen folder. Declining that prompt can't be driven there. The real-prompt item below covers it.
- **`rust/tools/check_package.py`** looks into the desktop packages and checks what the updater and the setup need, FFmpeg's size and licence, and that the program in them runs and knows its stamp. `rust-package.yml` runs it on each package and `release.yml` runs it again on the archives it publishes, next to `installer/check.ps1 -Rust` and `rust/tools/windows_move_check.ps1`.
- **`.github/checklist_due.py`** lists which of the sections below the changes since the last release can reach.

That leaves a short pass on a real phone every release plus whatever sections are due.

## Cutting a stable release

1. Set `VERSION` to the new version (e.g. `3.1.0`) and merge that to `master`.
2. Against that commit's nightly, run `windows_pass.py`, go through [Every release](#every-release), then run `python3 .github/checklist_due.py` and go through each section it lists.
3. Tag it and push the tag: `git tag v3.1.0 <commit> && git push origin v3.1.0`.
4. `release.yml` builds everything and publishes the release. A tag that doesn't match `VERSION` is refused.
5. Merge the draft PRs waiting for the release (README and website screenshots or text that show what's new). Until the release is out they would advertise the nightly.
6. Rename `docs/roadmap/next.md` after the release (for example `3.6.md`), add it to the list in `docs/roadmap/README.md` and start a fresh `next.md` with whatever didn't make it. Patch releases go into the file of the release they patch.

The APK signing key lives in the repository secrets (see [Packaging and CI](packaging.md#ci)). Keep a backup of the keystore outside GitHub. Without it no future APK can install as an update over the current one.

## Every release

- [ ] `windows_pass.py` passes against the nightly on a VM restored to a clean snapshot.
- [ ] With the dev app and `telescope-rs dev --keep` on a real phone, a 5 second Wi-Fi drop while streaming recovers and plugging in the cable moves the stream to USB without a gap.
- [ ] What this release's notes promise works on a real phone and on both desktops.
- [ ] Camera controls (lens, exposure, WB, OIS) apply live on a real phone and match what the desktop shows.
- [ ] A call or OBS on Linux and on Windows picks up the Telescope camera and the Telescope microphone and still has them after a Stop and Start.
- [ ] `rust/Cargo.lock` and the vision pack's pins haven't drifted out of date and for every dependency bumped since the last release its entries in [Upstream bugs](upstream-bugs.md) are checked (fixed ones have their workaround dropped).
- [ ] Device-compatibility matrix updated with test results.
- [ ] `python3 tools/docs_fresh.py` flags no docs, or each flagged doc has been read and bumped (see [Keeping docs fresh](../../CONTRIBUTING.md#keeping-docs-fresh)).
- [ ] Decided whether to translate this release's new texts first (`python tools/i18n.py status` in `rust/` shows what's left).
- [ ] Release notes written in `.github/release-notes/<VERSION>.md` (the release uses them; without one it falls back to generated notes).

## Only when due

`checklist_due.py` reads the comment under each heading below, which lists the files and packages that section depends on. A section is due when one of them changed since the last release. Code that everything leans on (the main window, the stream service, the protocol) is listed as shared and a change there lists every section as worth a look along with how big the change is. How far it reaches is for a person to judge. Any other file that's in no section makes the script fail until someone puts it in one. That stops the lists going stale. Put it in more than one section when unsure where a file goes.

<!-- shared: android/app/src/main/kotlin/com/telescope/MainActivity.kt android/app/src/main/kotlin/com/telescope/CameraStreamService.kt android/app/src/main/kotlin/com/telescope/Protocol.kt android/app/src/main/kotlin/com/telescope/Strings.kt android/app/src/main/kotlin/com/telescope/StreamPrefs.kt android/app/src/main/kotlin/com/telescope/CardHeader.kt android/app/src/main/kotlin/com/telescope/SystemBars.kt android/app/src/main/kotlin/com/telescope/Look.kt android/app/src/main/kotlin/com/telescope/LookButton.kt android/app/src/main/kotlin/com/telescope/LookSegmented.kt android/app/src/main/kotlin/com/telescope/LookSwitch.kt android/app/src/main/kotlin/com/telescope/Springs.kt android/app/src/main/kotlin/com/telescope/Tag.kt android/app/src/main/kotlin/com/telescope/SettingsActivity.kt android/app/src/main/kotlin/com/telescope/ManageActivity.kt android/app/src/main/res/font/** android/app/src/main/assets/IBMPlexSans-OFL.txt android/app/src/main/AndroidManifest.xml android/app/src/main/res/layout/** android/app/src/main/res/values/** android/app/src/main/res/xml/** android/app/src/main/res/menu/** android/app/src/main/res/resources.properties android/app/build.gradle android/build.gradle android/settings.gradle android/gradle.properties android/gradle/** android/gradlew android/gradlew.bat .github/workflows/build-apk.yml VERSION rust/Cargo.toml rust/Cargo.lock rust/app/Cargo.toml rust/app/build.rs rust/app/src/** rust/crates/core/** rust/crates/shell/** rust/crates/ui-kit/** rust/crates/plugin-host/** rust/crates/*/Cargo.toml -->

<!-- ignore: rust/.config/nextest.toml rust/tools/stress/** android/app/src/test/** android/app/proguard-rules.pro android/tools/gen_look.py docs/** site/** automation/** README.md README.es.md README.ru.md CONTRIBUTING.md SECURITY.md CLAUDE.md LICENSE BRANDING.md .gitignore .github/release-notes/** .github/pull_request_template.md .github/ISSUE_TEMPLATE/** .github/NIGHTLY_NOTES.md .github/DOWNLOADS.md .github/dependabot.yml .github/check_release.py .github/checklist_due.py .github/workflows/pages.yml .github/workflows/dependency-graph.yml .github/workflows/codeql.yml .github/codeql/** .github/DISCUSSION_TEMPLATE/** .github/FUNDING.yml .github/python_dependency_snapshot.py android/app/src/main/res/drawable*/** android/app/src/main/res/mipmap*/** android/app/src/main/res/color/** android/app/src/main/res/values-*/** rust/app/tests/** rust/crates/*/tests/** rust/crates/**/*_tests.rs rust/parity/** rust/docs/** rust/tools/fake_*.py rust/tools/measure.py rust/tools/python_probe.py rust/tools/compare_python.py rust/ARCHITECTURE.md rust/.gitignore .github/workflows/rust.yml .github/workflows/rust-speed.yml .github/workflows/rust-arm64.yml .github/workflows/deny.yml rust/deny.toml rust/tools/check_package.py rust/tools/write_notices.py rust/fixtures/** rust/locale/** rust/tools/i18n.py rust/vendor/** tools/docs_fresh.py .github/workflows/docs.yml -->

### Windows packaging

<!-- due: installer/** .github/workflows/release.yml .github/workflows/rust-package.yml rust/tools/build_windows.ps1 rust/tools/build_ffmpeg.sh rust/tools/ffmpeg_libs.ps1 rust/tools/fetch_rnnoise.py rust/tools/windows_move_check.ps1 rust/crates/outputs/src/win*.rs rust/crates/outputs/src/mf.rs rust/crates/outputs/src/unitycapture.rs rust/crates/outputs/src/wait_screen.rs rust/crates/outputs/src/webcams_win.rs rust/crates/system/src/splash*.rs rust/crates/system/src/winapp.rs rust/crates/system/src/registry.rs rust/crates/system/src/single_instance.rs rust/crates/system/src/update_guard.rs rust/crates/system/src/setup_move*.rs rust/crates/system/src/*_windows.rs rust/crates/link/src/winjob.rs rust/crates/link/src/adb_download.rs rust/windows/** rust/app/resources/** -->

`windows_pass.py` checks the rest of Windows packaging. These need a real phone:

- [ ] After **Get adb**, plugging a phone in and clicking its **Pair** button pairs it over USB.
- [ ] An all-users install updating from the previous release with a real UAC prompt: Yes restarts into the new version and the stream works, No leaves the old one running and offers the release page.
- [ ] After an unzipped copy updated into the installed one, Windows asks about the firewall once more and Add phone over Wi-Fi works after allowing it.
- [ ] On a machine an older version registered the driver on, streaming works before and after **Rename** or **Reinstall for a security fix**.
- [ ] Windows 11: after **Install driver** (or **Update** over UnityCapture) the phone's picture shows in the Windows Camera app and in OBS, Automatic streaming starts and stops with the app, and uninstalling removes the camera from both.

### Linux system setup

<!-- due: .github/workflows/rust-package.yml rust/tools/package_linux.sh rust/tools/package_appimage.sh rust/tools/bundle_gphoto2.sh rust/tools/build_libxml2.sh rust/tools/AppRun rust/tools/fonts.conf rust/tools/start.sh rust/tools/main_stub.py rust/tools/build_ffmpeg.sh rust/crates/outputs/src/linux_setup.rs rust/crates/outputs/src/v4l2.rs rust/crates/system/src/autostart.rs rust/crates/system/src/hotkeys_portal.rs -->

- [ ] With the module not loaded, Start shows a prompt once with the startup box ticked and after a reboot Start shows no prompt. Unticked, the prompt returns after a reboot.
- [ ] With the startup box ticked, the extra cameras Telescope #2 to #4 are still there after adding them and rebooting. Removing them and rebooting leaves only Telescope. A setup from before the rename shows Telescope after Unload and Load, and after a reboot too.
- [ ] Without pkexec (or with no polkit agent), Start shows the command banner, Copy command works and running it then Start streams.
- [ ] With Open Telescope when I sign in on, Telescope is in the tray after a real sign-out and sign-in (Linux and Windows).
- [ ] On a real desktop with the AppImage, a double click starts it, the app menu entry and icon work, and the password prompt for the virtual camera appears and loads it.

### Updates and settings from older versions

<!-- due: android/app/src/main/kotlin/com/telescope/Updater.kt android/app/src/main/kotlin/com/telescope/UpdateLogic.kt android/app/src/main/kotlin/com/telescope/UpdateHttp.kt android/app/src/main/kotlin/com/telescope/UpdateInstall.kt .github/write_manifest.py .github/workflows/release.yml installer/** rust/crates/system/src/updates.rs rust/crates/system/src/updates_plugin.rs rust/crates/system/ui/updates.slint rust/crates/system/src/update_guard.rs -->

- [ ] On Linux with the desktop on the previous nightly, the Update button appears, Update and restart replaces the app and it reopens on the new version with the stream working. On Windows `windows_pass.py` does the update and what's left is checking that the stream works after it.
- [ ] With the AppImage on the previous nightly, Update and restart replaces the file in place, it reopens on the new version and the app menu entry still starts it.
- [ ] Update is refused while streaming, on both apps.
- [ ] With the phone on the previous nightly, the update card offers the new build, asks once to allow installs from Telescope and the installed app shows the new version.
- [ ] With a phone app older than the desktop, a different session protocol and the cable plugged in, the Connection panel shows the mismatch and Update over USB installs the bundled APK.
- [ ] Settings are preserved across a restart, including each phone's own after switching between phones. A config from the previous version brings over the global settings and phones need pairing again.

### Pairing and networks

The QR code advertises desktop addresses and the phone sends LAN attempts on Wi-Fi.

<!-- due: android/app/src/main/kotlin/com/telescope/Pairing.kt android/app/src/main/kotlin/com/telescope/PairedComputers.kt android/app/src/main/kotlin/com/telescope/SessionServer.kt android/app/src/main/kotlin/com/telescope/SessionEndpoint.kt android/app/src/main/kotlin/com/telescope/LanAnnouncer.kt android/app/src/main/kotlin/com/telescope/Presence.kt android/app/src/main/kotlin/com/telescope/PresenceWorker.kt android/app/src/main/kotlin/com/telescope/PresenceSockets.kt android/app/src/main/kotlin/com/telescope/WifiLink.kt android/app/src/main/kotlin/com/telescope/TlsIdentity.kt android/app/src/main/kotlin/com/telescope/PhoneTls.kt android/app/src/main/kotlin/com/telescope/ScanActivity.kt android/app/src/main/kotlin/com/telescope/SetupSteps.kt android/app/src/main/kotlin/com/telescope/WaitingService.kt android/app/src/main/kotlin/com/telescope/RemoteStartNotice.kt android/app/src/main/kotlin/com/telescope/OldComputerNotice.kt rust/crates/link/** rust/tools/test_fake_presence.py ; packages: certifi -->

- [ ] Add phone by QR over Wi-Fi on Linux and on Windows and by plugging in over USB on Windows.
- [ ] The setup guide ticks each step as it's done and the video stage replaces it after the first stream. The APK installs through its QR download link and through its own Install over USB button.
- [ ] With USB and the debugging prompt not yet accepted, Add phone asks to allow it and pairs once it's accepted.
- [ ] With one phone paired with two computers, both stream (one at a time) and removing one computer on the phone leaves the other working.
- [ ] With an offline router (no WAN uplink at all), pairing still works and the QR code lists the LAN address.
- [ ] With a VPN on the desktop only (LAN access permitted), pairing works over the LAN address (the QR lists the LAN address along with the VPN one).
- [ ] With a VPN on the phone only (LAN access permitted), pairing works (tests the Wi-Fi-bound first attempt).
- [ ] With both devices on a VPN (both LAN-accessible), pairing works.
- [ ] With Tailscale on both (no shared LAN), pairing works over `100.64/10`.
- [ ] With a VPN blocking local-network traffic, pairing fails (the phone's dialog lists each address) and USB pairing works.
- [ ] With Docker, libvirt or VirtualBox installed on the desktop, the QR code doesn't advertise `docker0`/`virbr0`/`vboxnet0` addresses.
- [ ] With the phone on a tailnet the desktop isn't on, the line under the Connection status shows the phone's Wi-Fi address (not `100.64/10`) after Wi-Fi pairing and the stream works.
- [ ] On the Automatic route, plugged in uses USB and unplugged uses Wi-Fi. Plugged in with the app closed, or with a different phone on the cable, uses Wi-Fi and the card explains why.
- [ ] Pulling the cable mid-stream on USB carries on over Wi-Fi and plugging it back in can use USB again.
- [ ] Connect via USB only / Wi-Fi only is honoured, including reporting a missing cable instead of falling back (the banner's Switch to Automatic works).
- [ ] With Local only on and no cable, the desktop shows that the phone accepts USB only and Start doesn't open the camera. Plug in and it streams over USB. A second machine on the network can't reach the phone.
- [ ] After the phone gets a new IP from the router, the desktop finds it again (mDNS) without re-pairing.
- [ ] With both apps on this build, presence on UDP 7483 finds a phone that gets a new IP again without re-pairing. With UDP 7483 blocked, the stream still starts over HTTPS.
- [ ] USB-only viewers hold no Wi-Fi performance lock.
- [ ] With the window closed to the tray, a phone waiting and Automatic streaming off, the phone stops getting checked (no new `/v1/ping` in its logs) and opening the window checks it straight away.

### Streaming

<!-- due: android/app/src/main/kotlin/com/telescope/CameraSessionController.kt android/app/src/main/kotlin/com/telescope/OpensInFlight.kt android/app/src/main/kotlin/com/telescope/RunGate.kt android/app/src/main/kotlin/com/telescope/StreamStateMachine.kt android/app/src/main/kotlin/com/telescope/StreamLauncher.kt android/app/src/main/kotlin/com/telescope/H264Encoder.kt android/app/src/main/kotlin/com/telescope/H264Stream.kt android/app/src/main/kotlin/com/telescope/CodecFailure.kt android/app/src/main/kotlin/com/telescope/MjpegServer.kt android/app/src/main/kotlin/com/telescope/UdpVideo.kt android/app/src/main/kotlin/com/telescope/HttpWire.kt android/app/src/main/kotlin/com/telescope/IntervalClock.kt android/app/src/main/kotlin/com/telescope/DynamicBitrate.kt android/app/src/main/kotlin/com/telescope/DynamicFrameRate.kt android/app/src/main/kotlin/com/telescope/FrameRate.kt android/app/src/main/kotlin/com/telescope/FrameGate.kt android/app/src/main/kotlin/com/telescope/PendingLimiter.kt android/app/src/main/kotlin/com/telescope/RecentRuns.kt android/app/src/main/kotlin/com/telescope/DiagnosticsExtras.kt android/app/src/main/kotlin/com/telescope/PhoneStats.kt rust/crates/media/** rust/crates/outputs/src/lib.rs rust/crates/outputs/src/convert.rs rust/crates/outputs/src/convert/** rust/crates/core/src/yuv.rs rust/crates/outputs/src/held.rs rust/crates/outputs/src/stream_output.rs rust/crates/outputs/src/stream_output/** rust/crates/outputs/src/ui_check.rs rust/crates/outputs/src/watch.rs rust/crates/outputs/ui/stream_output.slint -->

- [ ] With H.264 on a real phone, a fresh phone starts on Light and switching Format between Light and Heavy reconnects and streams on Wi-Fi and USB. Compare latency (a clock on screen) and the Mbps readout with MJPEG at the same size. A 30-minute run remains smooth, a reconnect (unplug, Wi-Fi off and on) recovers and changing resolution or lens mid-stream still works.
- [ ] With the H.264 fallback, a size the encoder refuses (4:3 4K on most phones) stops with a banner to try a lower resolution or FPS and its Switch to Heavy button streams on Heavy. A phone without an encoder switches to Heavy without a banner.
- [ ] In the FPS dropdown, rates the camera doesn't list are greyed out. On a phone that lists 60, picking 60 shows about 60 in the footer and Copy diagnostics shows what the camera did.
- [ ] Copy diagnostics lists RAW (or no RAW) on each camera line and a Video encoders line with hardware and software H.264 and HEVC.
- [ ] Plugging in mid-stream over Wi-Fi (Connect via Automatic) moves the stream to USB a few seconds later without a gap in the video and the mic moves with it.
- [ ] With UDP and the mic off, the phone shows Streaming. Removing the computer on the phone stops its video within a second and a lost desktop releases its UDP session within 5 seconds.
- [ ] UDP to Dynamic and the UDP switch both reconnect onto the right transport and changing to Heavy leaves no UDP session behind.
- [ ] On Linux with an AMD or Intel card and Hardware acceleration on, the card decodes the stream and Copy diagnostics reports it found VA-API.
- [ ] With Hardware acceleration, a driver failure resumes in software without missing setup headers and Copy diagnostics reflects the decoder still running if the preference changes mid-stream.
- [ ] On a phone that runs hot (Android 11+) stream for 15 minutes or more. The frame rate drops to 24 fps shortly before Android itself reports severe heat, the FPS readout's tooltip shows the phone got hot and it rises again once the phone cools.
- [ ] Battery and temperature alerts fire once per threshold cross and don't repeat.
- [ ] Canvas size change (Linux and Windows) restarts cleanly.

### Camera controls and preview

<!-- due: android/app/src/main/kotlin/com/telescope/CameraCatalog.kt android/app/src/main/kotlin/com/telescope/LensLabel.kt android/app/src/main/kotlin/com/telescope/PreviewActivity.kt android/app/src/main/kotlin/com/telescope/PreviewTransform.kt android/app/src/main/kotlin/com/telescope/PreviewSize.kt android/app/src/main/kotlin/com/telescope/CameraProbeActivity.kt android/app/src/main/kotlin/com/telescope/CameraProbeCache.kt android/app/src/main/kotlin/com/telescope/CameraProbeModel.kt android/app/src/main/kotlin/com/telescope/CameraProbeRunner.kt rust/crates/effects/src/transforms.rs -->

- [ ] With the phone in the foreground, the desktop's Start and Stop control the camera, also with the screen dark. A lens, size and OIS setting picked on the desktop return after Stop and Start and the phone's Stop streaming stops the stream.
- [ ] On the Auto lens, the lens marks on the zoom slider switch to the telephoto (the dot turns lavender) and panning out of its view falls back (red dot). Sliding the zoom back and forth quickly on the telephoto doesn't turn the dot red. Scrolling the preview zooms around the mouse, dragging pans and the lens outlines show and fade.
- [ ] Zoom and Brightness sliders and the pan pad stick at their neutral spots and marks when dragged but not with the arrow keys, and Temperature and Tint don't stick. A double-click resets them all.
- [ ] With Point focus, clicking a near and a far object in the preview focuses each (with and without zoom, flip and rotation), exposure follows the point in auto and Auto returns to continuous. The pop-out works the same.
- [ ] With Presets, switching between two presets while streaming changes lens, exposure, WB, zoom and fps together. A second phone doesn't see the first one's presets.

### Photos

<!-- due: android/app/src/main/kotlin/com/telescope/StillCapture.kt android/app/src/main/kotlin/com/telescope/StillCatalog.kt android/app/src/main/kotlin/com/telescope/StillJobs.kt android/app/src/main/kotlin/com/telescope/StillOptions.kt android/app/src/main/kotlin/com/telescope/StillRebuild.kt android/app/src/main/kotlin/com/telescope/CameraSessionController.kt android/app/src/main/kotlin/com/telescope/PreviewRecovery.kt android/app/src/main/kotlin/com/telescope/MjpegServer.kt rust/crates/effects/src/photos.rs rust/tools/fake_phone_still.jpg rust/tools/test_fake_still.py -->

- [ ] When a JPEG is taken while streaming on Light, the video continues (a short hitch the first time only). On Heavy it pauses for about two seconds and recovers. Either way the photo is the lens's full size, upright with the desktop's rotation.
- [ ] With RAW and JPEG together on a phone that lists RAW, the DNG opens in a raw editor with the right colours and orientation. The same with full resolution on a 50 MP or 200 MP phone, where the file is the sensor's real size.
- [ ] With Night, HDR and Bokeh on a phone whose maker offers them, the video pauses for a few seconds and picks up again without a reconnect, and the photo looks like the phone's own camera app.
- [ ] With a 10 s long exposure in the dark, an exposure bracket and a focus bracket, the brackets really step (brightness, then near to far) and the stream returns after each.
- [ ] When the lens or the size is switched during a timer, or the camera is turned off during one, the photo fails or goes ahead cleanly and the next one works.
- [ ] In the Photos card on a phone and on a DSLR in M mode, ISO, Shutter and How many step with their arrows, the mouse wheel and Left and Right, holding an arrow repeats (in threes on a long list) and stops on release. Clicking the value opens the whole list. The tiles wrap cleanly when the card is narrow and Timer and Save in remain on one line.

### Microphone

<!-- due: android/app/src/main/kotlin/com/telescope/AudioStream.kt android/app/src/main/kotlin/com/telescope/AudioStreamer.kt android/app/src/main/kotlin/com/telescope/MicProblem.kt rust/crates/audio/**  ; packages: pyrnnoise -->

- [ ] On Linux (PipeWire), switching it on while streaming makes "Telescope Microphone" appear, a call records the phone and switching off or quitting removes it. Lip-sync looks right by eye. A 30-minute run doesn't drift and unplugging and replugging recovers.
- [ ] On Windows without VB-Cable the card offers Install VB-Cable. A prompt naming VB-Audio comes first, then it opens VB-Audio's setup through UAC and after a restart and an off/on apps record from CABLE Output.
- [ ] The meter moves with your voice even when no app is recording and its dot goes yellow on a loud clap (red with the limiter off under Audio). Mute (card and tray) gives silence without the mic disappearing from the call app. Gain changes and typed values are audible without crackle and Max gain changes the slider's range. On Windows too.
- [ ] The first permission request makes the phone's Get set up card show Microphone and allowing it there starts the audio within a few seconds without restarting the stream. Recording continues with the phone's screen off.
- [ ] With Automatic routing and mic only, plugging in USB moves the microphone too and rapid mic off and on leaves no recording held.
- [ ] With Auto level, beginning in silence or recovering from a gap doesn't turn up steady room noise.
- [ ] With Noise reduction on Linux and Windows, a fan going and some typing, a call recording loses both while speech still sounds natural and switching it mid-sentence doesn't click.

### Phone sensors

<!-- due: android/app/src/main/kotlin/com/telescope/PhoneSensors.kt android/app/src/main/kotlin/com/telescope/SensorStream.kt -->

- [ ] With a plugin using the phone's sensors, tilting the phone shows on the desktop within a moment with the back and the front camera, held upright and in landscape both ways round. Switching to another phone's stream moves the sensors there and an older phone app is reported as one.
- [ ] The phone's sensors stop once nothing uses them or the stream ends (the `sensors` thread goes away in `adb shell top -H -p $(adb shell pidof com.telescope)`), and the video doesn't stutter while they run.

### Starting, stopping and the camera button

<!-- due: android/app/src/main/kotlin/com/telescope/BlackScreen.kt android/app/src/main/kotlin/com/telescope/BlackScreenState.kt rust/crates/outputs/src/wait_screen*.rs rust/crates/outputs/ui/wait_screen.slint rust/crates/system/src/startup.rs rust/crates/system/ui/startup.slint rust/crates/system/src/single_instance.rs android/app/src/main/kotlin/com/telescope/CameraOwnership.kt -->

- [ ] Start set to "When the phone is ready": opening the phone app (or plugging it in) starts the stream, Stop ends the stream and it doesn't restart until the phone leaves and returns, and closing the window leaves it running in the tray.
- [ ] With Start set to "When an app opens the camera" and Stop to "When no app uses the camera, if it started the stream" on Linux and Windows, opening the camera in OBS or a call starts the stream and closing it stops it 15 s later.
- [ ] Windows, Stop set to "when no app uses the camera, even if you started it": a stream started manually before any app opened the camera stops after the wait.
- [ ] To test Camera off, press the camera button under the preview in a call using both Telescope devices. The call shows the wait screen and still hears the mic, the phone's camera light goes out and the tray icon loses its red dot. Press it again and the picture returns.
- [ ] To start with the mic only, press the camera button with the mic on, then **Start mic only**. The phone opens no camera and the call hears the mic.
- [ ] With the mic on and Stop set to "when no app uses the camera": turn the camera off in the call. After the wait only the camera turns off and turning it back on in the call restores the picture.
- [ ] With a phone in the background (screen off, **Wait for my computer** on), turn the camera off and on again. It resumes and if another app holds the camera it remains off with a banner explaining why.
- [ ] With the wait screen, a chosen image and a GIF reach apps and Mirror flips them. Starting and stopping a stream while a call has the camera open leaves the call showing the picture.
- [ ] Tray minimise and restore work.
- [ ] For the phone app's Black screen, tap it next to Preview while streaming. The screen goes pure black at the lowest brightness without the status and nav bars and the stream doesn't stutter going in or out. One tap shows the hint, a double tap or Back shows the app again and the brightness returns. Stopping the stream from the computer, the power button or the Home button also show the app again.

### Browser camera

<!-- due: rust/crates/link/src/browser/** rust/crates/link/web/** -->

- [ ] On an iPhone in Safari, scan the code, tap through the certificate warning and press Start. Video and mic reach the virtual camera and mic, the screen remains on and after locking the phone and returning to the page the stream picks up again. Flip camera flips lenses.
- [ ] For the Black screen (portrait and landscape), the page goes black and, on Android, full screen without the status and nav bars. One tap shows the hint and fades again, a double tap shows the page again and the stream goes on throughout.
- [ ] On an iPhone the Black screen opens a black video in the system player and hides the status bar, the stream goes on behind it and closing the player shows the page again.
- [ ] Android in Chrome and a second computer's browser behave the same. Resolution and Frame rate on the card change what arrives. New link drops the browser and the old code stops working. Opening the link in a second tab replaces the first.
- [ ] On Windows the firewall prompt (or a rule) lets port 7481 through and picking a phone again stops the server.

### Webcams

<!-- due: rust/crates/outputs/src/webcam*.rs rust/crates/outputs/src/scan_pace.rs rust/crates/outputs/src/webcam/** rust/crates/outputs/src/webcam_capture/** rust/crates/outputs/src/webcams/** rust/crates/outputs/ui/webcam.slint -->

- [ ] A USB webcam shows in the device picker under its own name, and Telescope's own cameras (Telescope, OBS Virtual Camera) don't. Start streams it to the virtual camera and Background, Chroma key and Transforms work on it.
- [ ] Resolution and Frame rate on the Webcam card change what arrives. 1080p at 30 fps on a USB 2 webcam shows MJPEG on the card.
- [ ] With the webcam open in another app (a browser's camera test page will do) Start gives a banner naming the app on Linux. Closing it there lets the stream start without any button press.
- [ ] Unplugging the webcam mid-stream shows the wait screen and plugging it back in picks the stream up again. Plugging one in while Telescope runs adds it to the picker within a few seconds.
- [ ] On Linux, from the AppImage or tarball on a machine without libgphoto2 installed, an older DSLR in live-view mode shows in the picker and streams. When it ends live view after a few minutes the stream continues. With the camera mounted in the file manager you are asked to unmount it.
- [ ] On the same DSLR set to M, the Webcam card has a focus jog with a Focus button beside it, Aperture and Exposure dials and White balance, and each changes the live picture (Aperture on a D5100 about a second after the last click, when live view restarts once). Zooming the lens changes the aperture and the apertures offered, and the card follows within a couple of seconds. Dragging the jog toward Near or Far continues moving the focus (the further out the handle, the bigger the steps), a click beside the handle is one small step, and letting go springs the handle back and stops the focus at once, with no steps left running afterwards. The arrows and the mouse wheel step Aperture and Exposure, and clicking the value opens the whole list. Turning the dial to a scene mode empties the list within a couple of seconds.
- [ ] On Windows with a Nikon and then a Canon in PTP mode over USB (no driver swapped), the camera is in the Webcam picker and shows live view, and focus, Focus, Aperture, Exposure compensation and White balance work. `cargo test -p telescope-outputs a_real_camera_gives_live_view_frames -- --ignored --nocapture` gets a frame first. The Photos card on the same camera takes a JPEG, a RAW and a RAW and JPEG photo (with a timer too), the files land in the Save in folder, live view returns afterwards, and the camera's card holds no new files
- [ ] The Webcam card shows only the settings the camera has. Exposure on Manual makes Exposure time work, the change shows in the picture and after an unplug and replug (or a Stop and Start) the camera has it again. Reset restores the defaults.
- [ ] On Windows the same applies, and a camera with a built-in webcam mode or an HDMI capture card also shows up and streams.

### Spout capture

<!-- due: rust/crates/outputs/src/spout/** rust/crates/outputs/src/spout_shared/** rust/crates/outputs/src/pipewire/** rust/crates/outputs/ui/spout.slint rust/crates/outputs/src/spout_send/** rust/crates/outputs/ui/spout_send.slint -->

- [ ] Spout send (Lab, Streaming and devices). With a stream running, **Share this stream** on the Spout send card makes a sender called Telescope and the phone's name. On Windows OBS's Spout source and a Spout demo receiver show the picture and follow a size change, on Linux OBS's PipeWire capture lists it as a camera. Switching it off or stopping the stream removes it within a few seconds. With Several cameras on, the stream going to the Telescope camera shares the combined picture
- [ ] Spout send's **Transparent background** shares a cut-out stream. A program that lays pictures over others shows the person with nothing behind them. The virtual camera still shows the normal picture. Switching it off returns Background to what it was
- [ ] Windows with Spout capture switched on. A Spout sender (OBS with its Spout output, or Spout2's demo sender) shows in the device picker under its own name. Start streams it at the frame rate on the card with the right colours. Background works on it.
- [ ] Closing the sender mid-stream shows the wait screen and opening it again picks the stream up. Stop is instant.
- [ ] On Linux a Windows game or program sending Spout under Proton with Spout2PW shows in the picker and streams the same way. With PipeWire stopped when Telescope starts and started later, the sender still shows up.
- [ ] Linux, from the AppImage on a machine without libpipewire. Telescope starts and Spout capture lists nothing.

### On Air sign

<!-- due: rust/crates/effects/src/on_air*.rs rust/crates/link/src/web.rs -->

- [ ] With On Air sign switched on, a tablet or phone opens the sign from its code, taps through the certificate warning and goes full screen with the screen remaining on. Joining a call turns it red within about a second and leaving turns it back about six seconds later.
- [ ] A webhook to Home Assistant (or any hub) switches a light on and off with the default POST and with a GET payload. Stopping the hub gives one banner rather than one per change.
- [ ] On Windows the firewall prompt (or a rule) lets port 7482 through. New link makes an open sign show that its link doesn't work any more.

### Settings, keyboard and folding

<!-- due: rust/crates/system/src/shortcuts.rs rust/crates/system/ui/shortcuts.slint rust/crates/system/src/hotkeys*.rs rust/crates/system/src/keys.rs rust/crates/system/src/tray*.rs rust/crates/system/src/icon.rs rust/crates/system/src/memory.rs rust/crates/system/src/lib.rs rust/crates/system/src/version.rs rust/crates/system/src/test_bus.rs rust/crates/setup/** -->

- [ ] Every Settings tab opens and the settings button shows the window again on the tab it was left on.
- [ ] For folding, clicking a card title and a section title folds and unfolds them, Tab reaches the titles and Space or Enter toggles them. A shortcut for a control in a folded card still works. Folds return after a restart.
- [ ] Tab through the tiles and switches, use rotation and pan shortcuts saved before this release, pan with arrow keys and Home on the pan pad, open and pick from a dropdown with the keyboard and inspect labels with a screen reader.
- [ ] In Your devices, Rename works, Change name changes what a newly paired phone shows and saved browsers are listed with the phones.
- [ ] With plugins, installing a newer `.zip` of an added plugin asks, replaces it with its settings preserved and leaves it switched off. One with a GitHub `repo` shows its link and, once a release is newer, a line about it on its tile. **Install updates without asking** asks before it goes on.

### MCP server

<!-- due: rust/crates/system/src/mcp.rs rust/crates/system/src/mcp_bridge.rs rust/crates/system/src/mcp_stats.rs rust/crates/system/src/remote.rs rust/crates/system/ui/mcp.slint rust/crates/shell/src/touched.rs rust/crates/system/src/single_instance.rs rust/crates/core/src/settings.rs docs/features/mcp.md -->

- [ ] With the plugin off, `telescope mcp` started by an AI app still starts and shows where to switch it on. After switching **MCP server** on and restarting, the same app connects without restarting it and Telescope shows a notification for the first connection.
- [ ] In Claude Desktop and one other MCP client (set up as in `docs/features/mcp.md`): `status`, `describe` and `get` answer and `set` with Colour grading's Contrast moves the slider in the card and the picture at once. A wrong value names the problem and changes nothing. `revert` restores the old value and leaves a slider moved manually untouched.
- [ ] With a phone streaming and Colour grading on, `measure` gives numbers while sitting still and explains why when the stream is off. Asking a model to colour grade the picture ends with sensible sliders and nothing outside the ranges.
- [ ] With a phone streaming, `stats` and `watch` give the numbers the Stats panel shows (also with the Stats plugin off), and the stream and phone camera actions are refused.
- [ ] On the **MCP** button in the top bar, the dot shows when an AI app is connected, it lights while the AI reads or changes something, the left side opens the MCP tab and the right side undoes what the AI changed (greyed out with nothing to undo). On the MCP tab, **Read only** makes `set`, `revert` and `run_action` refuse with a line in the activity list marked as refused, and the last 30 calls are listed newest first.
- [ ] When the AI sets Colour grading's Contrast and Temperature, those two sliders get an outline for about a second and a half. With the card folded nothing opens.

### Several cameras in one picture

<!-- due: rust/crates/effects/src/compos*.rs -->

- [ ] With two real phones streaming (Linux and Windows), switching on **Several cameras in one picture** under **Streaming and devices** shows the One picture card. **Start from** Big and small, then drag, resize (plain, Shift to stretch, Alt to crop), zoom and right-drag a stream in **Edit picture…** while the call app shows the picture on the Telescope camera. Borders, corners and a background picture show there too and the second phone's own camera still works.
- [ ] Save two layouts, bind one to a key, save a preset with each and switch between them by key and preset. After stopping the second phone the camera shows the first alone within 3 seconds. The second one is in the picture again once it's restarted. Turning off the first phone's Wi-Fi doesn't freeze the second in the picture. With an upright phone on Telescope and a landscape one on Telescope 2, moving the mic to the second leaves the picture upright on Telescope.

### 3D from two phones

<!-- due: rust/crates/effects/src/stereo*.rs rust/crates/effects/ui/stereo.slint rust/crates/effects/web/headset.* -->

- [ ] With two real phones of the same model, both on the newest phone app, switching on **3D** under **Streaming and devices** shows the 3D card. With both streaming the Telescope camera shows **Side by side** in a call app, the badge shows **In sync** or **Rough sync** and moving a hand quickly in front of both shows little ghosting. **Swap eyes**, **Depth** and **Height** (inside **Fine tune**) change the picture as the card describes and the reset in the card's header restores them all. **Red/cyan** looks right through glasses, with greys remaining grey.
- [ ] Two phones of different models: **Match** puts both on manual exposure with the same shutter (check the Camera card on each), moves the right phone's ISO within a few seconds until the two pictures are about as bright and the badge ends on **Matched** (or **Partly matched** on a scene one phone can't reach). The colours of the two eyes look closer afterwards and **Reset match** takes the colour curve off again. With one phone running the button is greyed out. **Size** starts from the lenses (a phone with a different lens on each side). **Timing** inside **Fine tune** moves the right eye. **Left tilt** and **Right tilt** each turn one eye.
- [ ] Open **Headset** in a VR headset browser on the same Wi-Fi. The link opens after the certificate warning. The flat page shows the picture and reads **Connected**. **Enter VR** puts the left half in the left eye and the right half in the right eye about 2 metres ahead at a comfortable size with no stutter for a minute. Switching **Headset** or 3D off, or **New link**, ends it. Privacy blur turned on shows the same blur on the headset (and the Wait screen shows there too), not the live picture. On Windows the firewall allows the port in the link through (7484 by default).
- [ ] With one phone on an older phone app or a webcam, the badge shows **Not synced** and the picture still goes out. Stopping one phone returns the other to the Telescope camera alone within 3 seconds and switching 3D off does the same. A phone whose picture freezes while it remains connected makes the badge show **Waiting** and the camera goes on moving from the other phone.

### Vision effects

<!-- due: rust/crates/core/src/yuv.rs rust/crates/vision/** rust/crates/effects/src/background.rs rust/crates/effects/src/framing.rs rust/crates/effects/src/fun_*.rs rust/crates/effects/src/face_exposure.rs rust/crates/effects/src/privacy_blur.rs rust/crates/effects/src/vision_row.rs rust/tools/build_vision_pack.py rust/tools/vision-pack-requirements.txt rust/tools/build_matting_pack.py ; packages: mediapipe -->

- [ ] With Auto framing and Background (Linux and Windows), switching one on under **People** asks before downloading, shows the progress and after **Restart now** its card shows. Auto framing follows you while streaming from a real phone, widens when you hold a hand out and zooms back out when you leave, also at 90, 180 and 270 degrees and flipped. Zoom and Pan in Transforms grey out while it's on. Background blurs and puts a picture behind you. With both off **Remove download** deletes the download.
- [ ] Background's **High quality** (Linux and Windows) is off by default. Switching it on offers its own download with a note about the processor, and until it has finished the normal cutout still works. Afterwards hair and edges are cleaner and the mask is steady in a still room. Switching the camera, the resolution or the stream off and on doesn't leave a ghost of the old picture in the cutout. **Remove download** deletes it too.
- [ ] With the Fun pack on a real phone, clones, the cloak and the stick figure follow you, light painting draws with a torch and the retro looks hold 30 fps at 1080p. Datamosh on Light smears when you move, **Clean up** and switching it off restore the picture within a second and on Heavy the card states it needs Light.
- [ ] With Expose for my face on a real phone and a bright window behind you, the face gets lit right within a couple of seconds, a small head move doesn't change the exposure, walking out holds it and a tap on the preview controls it until **Follow my face**.
- [ ] With Privacy blur on a real phone, a second person walking in blurs the whole picture within a second and it stops about 4 seconds after they leave. Try each choice in the dropdown (**Show the wait screen** with your own wait screen image too) and **Blur now** from a keyboard shortcut.

### Preview tools and picture plugins

<!-- due: rust/crates/effects/** -->

- [ ] With Cinema tools on a real phone using manual focus, peaking sits on what's sharp and moves as you turn focus, zebras and false colour show on a bright window and clipped highlights, each scope shows and none of it reaches a call or OBS. Also in the pop-out.
- [ ] With Level on a real phone and both the back and the front camera, tilting it turns the line the same way the picture turns, it sits still on a table and turns green when level, pitch goes up as the camera looks up, and flipping or rotating under Picture leaves it right. An older phone app shows why it can't.
- [ ] With Desk mode on a real phone pointed at a sheet of paper, marking the corners straightens it in a call, a standing page isn't distorted (the pad's width and height fix what Auto gets wrong), Contrast and Black and white make handwriting readable and moving between phones leaves each one's corners as they were.
- [ ] With Chroma key, a real green screen and a real phone, Green keys it cleanly with even light, Picked keys the colour clicked on the preview, Spill takes the green off hair and skin and a picture fills the frame in a call.
- [ ] With QR codes on a real phone, a Wi-Fi code from another phone's share screen puts the password on the clipboard (Linux with the window in front and behind, where Copy has to step in, and Windows) and a printed link code reads from arm's length.
- [ ] During a pick on the preview (Desk mode's corners or Chroma key's colour), the clicks go to the pick instead of focusing or panning, Esc and a right click cancel it, scrolling still zooms and the corners remain on the same spot while zoomed, flipped and rotated.

### Name tag

<!-- due: rust/crates/effects/src/name_tag.rs rust/crates/effects/src/tag.rs -->

- [ ] With Name tag switched on, a name, pronouns and second line show in a call on Linux and Windows in each style, position and size, and a preset restores the tag it saved. Mirror in Transforms flips the picture but not the text.
- [ ] A name in Arabic or Hebrew, one in Chinese or Japanese and one in Hindi show with the right letters on both desktops (no empty boxes), and the right-to-left one starts from the right.

### Colour grading

<!-- due: rust/crates/effects/src/colour_grading.rs rust/crates/effects/src/grade.rs rust/crates/effects/src/cube.rs -->

- [ ] On Linux and Windows, streaming 1080p60 from a real phone, switch Colour grading on under **Look**, drag the wheels and curves and load a `.cube` LUT. The frame rate in the footer remains at 60 and Copy diagnostics shows the colour pipeline's time a frame.
- [ ] Compare shows the ungraded picture left of the line on the preview while a call sees the whole picture graded. The header switch switches the grade off and on again.
- [ ] Export grade… writes a `.cube` that gives the same look in another program (OBS's LUT filter or Resolve). Two phones each have their own grades and a preset restores its grade.

### Filters

<!-- due: rust/crates/effects/src/filters.rs rust/crates/effects/src/filters_cpu.rs rust/crates/effects/src/filter_*.wgsl -->

- [ ] On Linux and Windows, streaming 1080p60 from a real phone in a dim room, Denoise and Over time clean up the noise without a ghost behind a hand waved in front, Sharpen and Clarity make the picture crisper with no white outline, and Vignette, Glow and Film grain show in a call after the grade. The frame rate in the footer remains at 60 and the filters still work with a game in front (the card busy).
- [ ] Skin smoothing with the vision download softens a face and leaves the eyes, brows and hair sharp. Without the download the card explains why it can't. A preset restores the filters and two phones each have their own.

### Notes next to the lens

<!-- due: android/app/src/main/kotlin/com/telescope/Notes.kt android/app/src/main/kotlin/com/telescope/NotesActivity.kt android/app/src/main/kotlin/com/telescope/NotesPlacement.kt rust/crates/effects/src/notes.rs -->

- [ ] Switch Notes on under **On screen**, stream from the front camera and send a few lines. They open on the phone right under the camera (on a notch, a punch hole and a phone without either), Play scrolls them, Next and Previous step a line and a shortcut or presenter clicker bound to Next works. Turn the phone sideways and the text moves beside the lens. The stream doesn't stutter while they scroll.
- [ ] With Telescope in the background on the phone the card prompts you to open it and opening it shows the notes. Back closes them until you send again, and stopping the stream takes them away.
