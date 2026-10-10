# Manual release checklist

Run before tagging. CI covers pytest, Android unit tests, and packaging smoke checks; everything here needs a real phone and desktop.

## Cutting a stable release

1. Set `VERSION` to the new version (e.g. `3.1.0`) and merge that to `master`.
2. Run through this checklist against that commit's nightly build.
3. Tag it and push the tag: `git tag v3.1.0 <commit> && git push origin v3.1.0`.
4. `release.yml` builds everything and publishes the release. It refuses a tag that doesn't match `VERSION`.

The APK signing key lives in the repository secrets (see [Building and CI](building.md#ci--github-actions)). Keep a backup of the keystore outside GitHub: without it, no future APK can install as an update over the current one.

## Before starting

- [ ] `desktop` pytest suite passes locally and in CI for the release commit.
- [ ] Android JVM unit tests pass locally and in CI for the release commit.
- [ ] `desktop/scripts/smoke_check.py` passes on both Linux and Windows CI runners.
- [ ] `desktop/constraints.txt` installs cleanly in clean venv and versions haven't drifted out of date.

## Packaging

- [ ] Windows: `TelescopeDesktop.exe` launches with the Telescope icon, the first-run checklist's Install driver registers UnityCapture, bundled `adb.exe` works for USB, and no `adb.exe` is left running after quitting.
- [ ] Windows, upgrading from a version that registered the driver from the app folder: **Advanced** shows **Reinstall for a security fix**, the camera still works until then, and Reinstall puts the DLLs in `C:\Program Files\Telescope\UnityCapture` and shows Ready.
- [ ] Windows: `TelescopeSetup.exe` installs without a UAC prompt, the Start menu entry opens Telescope, Update and restart works on the installed copy, and uninstalling (from Settings → Apps) removes it and asks before deleting settings.
- [ ] Windows, an unzipped copy of the previous release: Updates says updating installs it, **Update and restart** comes back as the installed copy (Start menu entry there, settings and phones kept), and the unzipped folder is gone a few seconds later. With **Keep this copy where it is** ticked it updates in place instead.
- [ ] Windows: opening `TelescopeDesktop.exe` from inside the zip without extracting says to extract it first.
- [ ] Linux: Telescope shows up in the app menu after the first launch, and still opens after moving the folder and launching once from the new place.
- [ ] Windows: OBS and Zoom list the camera as **Telescope**. On a machine registered by an older version, Advanced offers Rename, and streaming works before and after.
- [ ] Linux: `start.sh` creates venv at `$XDG_DATA_HOME/Telescope/venv` on clean machine/account and launches successfully.
- [ ] Both bundles contain `THIRD_PARTY_NOTICES.txt` and `Telescope.apk`.
- [ ] `manifest.json` checksums match the downloaded files (`sha256sum`).
- [ ] The new APK installs over the previous release's APK with `adb install -r` (same signing key).
- [ ] Advanced (desktop) and the About card (phone) show the release's version. Copy diagnostics works on both.
- [ ] APK installs via `adb install`, via the checklist's QR link, and via Install over USB (checklist and Advanced).

## Updates

- [ ] Desktop on the previous nightly (Windows and Linux): the Update button appears, Update and restart replaces the app, and it comes back on the new version with the stream working.
- [ ] Update is refused while streaming, on both apps.
- [ ] Phone on the previous nightly: the update card offers the new build, asks once to allow installs from Telescope, and the installed app shows the new version.
- [ ] Phone app older than the desktop, plugged in: the Connection panel says so and Update over USB installs the bundled APK.
- [ ] Switching the channel to Stable on a nightly build doesn't offer a downgrade.
- [ ] Desktop update cut short (kill the app mid-install): the next launch finishes it, or comes back on the old version.

## Functional pass (see [device-compatibility.md](device-compatibility.md) for the per-device matrix)

- [ ] Phone fresh install: nothing is asked on launch; Get set up allows camera, notifications and battery in order; the card goes when all are allowed. Deny camera twice: its button becomes Open settings.
- [ ] Fresh install: the checklist shows, each step ticks as it's done, and the video stage replaces it after the first stream.
- [ ] Add phone works by QR over Wi-Fi and by plugging in over USB (no scan), on at least one device per platform (Linux + Windows).
- [ ] USB with the debugging prompt not yet accepted: Add phone says to allow it, and pairs once it's accepted.
- [ ] Re-pairing from the same computer replaces its token (old token gets 401; verify with curl) and doesn't add a second entry on the phone.
- [ ] One phone paired with two computers: both stream (one at a time); removing one computer on the phone leaves the other working.
- [ ] Removing the phone on the desktop unpairs it on the phone too (it disappears from the phone's list).

### Pairing across networks and VPNs

QR code advertises desktop addresses, phone sends LAN attempts on Wi-Fi. Re-check when network setup changes:

- [ ] Offline router (no WAN uplink at all): pairing still works, and the QR code lists the LAN address.
- [ ] Desktop VPN only (LAN access permitted): pairing works over LAN address (QR lists it, not just VPN).
- [ ] Phone VPN only (LAN access permitted): pairing works (tests Wi-Fi-bound first attempt).
- [ ] Both devices VPN (both LAN-accessible): pairing works.
- [ ] Tailscale on both (no shared LAN): pairing works over `100.64/10`.
- [ ] VPN blocking local-network traffic: pairing fails (phone dialog lists each address). USB pairing works.
- [ ] Desktop with Docker/libvirt/VirtualBox installed: the QR code does not advertise `docker0`/`virbr0`/`vboxnet0` addresses.
- [ ] Phone on tailnet desktop is not on: after Wi-Fi pairing, **Using** shows the phone's Wi-Fi address (not `100.64/10`), stream works.
- [ ] Removing the computer on the phone revokes access (requests get 401; desktop shows Needs pairing again).
- [ ] Automatic route: plugged in uses USB, unplugged uses Wi-Fi. Plugged in with the app closed, or with a different phone on the cable, uses Wi-Fi and the card says why.
- [ ] Plugging in mid-stream over Wi-Fi shows Switch to USB, and it switches without restarting the phone's camera.
- [ ] Pulling the cable mid-stream on USB carries on over Wi-Fi; plugging it back in can use USB again.
- [ ] Connect via USB only / Wi-Fi only is honoured, including reporting a missing cable instead of falling back.
- [ ] Local only on, no cable: the desktop says the phone accepts USB only and Start doesn't open the camera. Plug in and it streams over USB.
- [ ] Phone gets a new IP from the router: the desktop finds it again (mDNS) without re-pairing.
- [ ] Phone foreground: desktop Start/Stop controls camera; test with screen dark too.
- [ ] A lens, size and OIS setting picked on the desktop come back after Stop and Start. The phone shows Stop Streaming only while streaming, and it stops the stream.
- [ ] Local-only mode blocks Wi-Fi access (verify from second machine on network).
- [ ] Camera controls (lens, exposure, WB, OIS) apply live and match what's shown on the desktop UI.
- [ ] Stream transforms (flip, rotate, zoom/pan) apply without restart.
- [ ] Zoom on the Auto lens: the lens marks on the zoom slider switch to the telephoto (the dot turns lavender); panning out of its view falls back (red dot). Sliding the zoom back and forth quickly on the telephoto never turns the dot red. Scrolling the preview zooms around the mouse, dragging pans, and the lens outlines show and fade.
- [ ] Zoom, pan and Compensation sliders stick at their neutral spots and marks when dragged, never with the arrow keys; Temperature and Tint never stick. A double-click resets them all.
- [ ] Point focus: clicking a near and a far object in the preview focuses each (with and without zoom, flip and rotation), exposure follows the point in auto, and Auto returns to continuous. The pop-out works the same.
- [ ] Presets: switching between two presets while streaming changes lens, exposure, WB, zoom and fps together. A second phone doesn't see the first one's presets.
- [ ] H.264: on a real phone, a fresh phone starts on Light (H.264), switching Format between Light and Heavy reconnects and streams on Wi-Fi and USB. Compare latency (a clock on screen) and the Mbps readout with MJPEG at the same size. A 30-minute run stays smooth, a reconnect (unplug, Wi-Fi off and on) recovers, and changing resolution or lens mid-stream keeps working.
- [ ] H.264 fallback: a size the encoder refuses (4:3 4K on most phones) stops with a banner to try a lower resolution or FPS, and its Switch to Heavy button streams on Heavy. A phone without an encoder switches to Heavy by itself, with no banner.
- [ ] FPS: the dropdown grays out rates the camera doesn't list (48 and 60 on a 30 fps phone). On a phone that lists 60, picking 60 shows about 60 in the footer; Copy diagnostics shows what the camera actually did.
- [ ] Microphone, Linux (Fedora/Nobara, PipeWire): switching it on while streaming makes "Telescope Microphone" appear; Audacity or a call records the phone; switching off or quitting removes it. Lip-sync looks right by eye. A 30-minute run doesn't drift, and unplugging and replugging recovers.
- [ ] Microphone, Windows: without VB-Cable the card offers Install VB-Cable; it asks first (naming VB-Audio), opens VB-Audio's setup through UAC, and after a restart and an off/on, apps record from CABLE Output.
- [ ] Microphone card: the meter moves with your voice even when no app is recording, and its dot goes yellow on a loud clap (red with the limiter off in Advanced). Mute (card and tray) gives silence without the mic disappearing from the call app. Gain changes and typed values are audible without crackle, and Advanced's Max gain changes the slider's range. On Windows too.
- [ ] Microphone permission: the first request makes the phone's Get set up card show Microphone; allowing it there starts the audio within a few seconds, without restarting the stream. Recording keeps going with the phone's screen off.
- [ ] Canvas size change (Linux and Windows) restarts cleanly.
- [ ] Linux, module not loaded: Start asks once, with the startup box ticked; after a reboot Start doesn't ask. Unticked: it asks again after a reboot.
- [ ] Linux without pkexec (or with no polkit agent): Start shows the command banner, Copy command works, and running it then Start streams.
- [ ] Each Start failure shows a banner with a working button: no phone (Add phone), phone app closed (Try again), USB only without a cable (Switch to Automatic), app versions differ (Update).
- [ ] Battery/temp alerts fire once per threshold cross, not repeatedly.
- [ ] Config persists across app restart, including per-phone settings after switching. A config from the previous version keeps global settings and asks to pair again.
- [ ] Corrupted `telescope_config.json` backs up (`.invalid-<timestamp>`) and app starts with defaults.
- [ ] Tray minimize/restore and single-instance behavior both work.
- [ ] Start streaming when the phone is ready: opening the phone app (or plugging it in) starts the stream; Stop keeps it stopped until the phone leaves and comes back; closing the window keeps it in the tray.
- [ ] Windows, Stop set to "when no app uses the camera, even if you started it": start a stream by hand before any app has opened the camera. It stops after the wait.
- [ ] Camera off: in a call using both Telescope devices, press the camera button under the preview. The call shows the wait screen and still hears the mic, the phone's camera light goes out, and the tray icon loses its red dot. Press it again and the picture comes back.
- [ ] Mic only from the start: with the mic on, press the camera button, then **Start mic only**. The phone opens no camera, and the call hears the mic.
- [ ] With the mic on and Stop set to "when no app uses the camera": turn the camera off in the call. After the wait only the camera turns off. Turn it back on in the call and the picture returns.
- [ ] Camera off on a phone in the background (screen off, **Wait for my computer** on): turn it off and on again. It comes back; if another app holds the camera, it stays off with a banner saying so.
- [ ] Open Telescope when I sign in (Linux and Windows): after signing out and in, Telescope is in the tray; unticking removes the entry.
- [ ] Stream only while an app is using the camera (Linux and Windows): opening the camera in OBS or a call starts the stream, closing it stops 15 s later. Ticking it unticks Start streaming when the phone is ready, and the other way round.
- [ ] Browser camera, iPhone in Safari: scan the code, tap through the certificate warning, Start. Video and mic reach the virtual camera and mic, the screen stays on, and after locking the phone and coming back to the page the stream picks up again. Flip camera flips lenses.
- [ ] Browser camera, Black screen (portrait and landscape): the page goes black and, on Android, full screen without the status and nav bars. One tap shows the hint and fades again, a double tap brings the page back, and the stream keeps going the whole time.
- [ ] Browser camera, Black screen on iPhone: a black video opens in the system player and hides the status bar, the stream keeps going behind it, and closing the player brings the page back.
- [ ] Browser camera, Android in Chrome, and from a second computer's browser: the same. Resolution and Frame rate on the card change what arrives. New link drops the browser and the old code stops working. Opening the link in a second tab takes over from the first.
- [ ] Browser camera, Windows: the firewall prompt (or a rule) lets port 8767 through, and picking a phone again stops the server.
- [ ] Wait screen: with nothing streaming, apps see the default screen, then a chosen image and a GIF; Mirror flips it. Starting and stopping a stream while a call has the camera open keeps the picture in the call.

## Sign-off

- [ ] Device-compatibility matrix updated with test results.
- [ ] Release notes written in `.github/release-notes/<VERSION>.md` (the release uses them; without one it falls back to generated notes).
- [ ] Tag pushed; the Release workflow published the assets.
