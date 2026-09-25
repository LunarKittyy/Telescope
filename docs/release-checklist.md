# Manual release checklist

Run before tagging. CI covers pytest, Android unit tests, and packaging smoke checks; everything here needs a real phone and desktop.

## Cutting a stable release

1. Set `VERSION` to the new version (e.g. `0.6.0`) and merge that to `master`.
2. Run through this checklist against that commit's nightly build.
3. Tag it and push the tag: `git tag v0.6.0 <commit> && git push origin v0.6.0`.
4. `release.yml` builds everything and publishes the release. It refuses a tag that doesn't match `VERSION`.

The APK signing key lives in the repository secrets (see the README's CI section). Keep a backup of the keystore outside GitHub: without it, no future APK can install as an update over the current one.

## Before starting

- [ ] `desktop` pytest suite passes locally and in CI for the release commit.
- [ ] Android JVM unit tests pass locally and in CI for the release commit.
- [ ] `desktop/scripts/smoke_check.py` passes on both Linux and Windows CI runners.
- [ ] `desktop/constraints.txt` installs cleanly in clean venv and versions haven't drifted out of date.

## Packaging

- [ ] Windows: `TelescopeDesktop.exe` launches, the first-run checklist's Install driver registers UnityCapture, bundled `adb.exe` works for USB.
- [ ] Windows: OBS and Zoom list the camera as **Telescope**. On a machine registered by an older version, Advanced offers Rename, and streaming works before and after.
- [ ] Linux: `start.sh` creates venv at `$XDG_DATA_HOME/Telescope/venv` on clean machine/account and launches successfully.
- [ ] Both bundles contain `THIRD_PARTY_NOTICES.txt` and `Telescope.apk`.
- [ ] `manifest.json` checksums match the downloaded files (`sha256sum`).
- [ ] The new APK installs over the previous release's APK with `adb install -r` (same signing key).
- [ ] Advanced (desktop) and the diagnostics card (phone) show the release's version.
- [ ] APK installs via `adb install`, via the checklist's QR link, and via Install over USB (checklist and Advanced).

## Updates

- [ ] Desktop on the previous nightly (Windows and Linux): the Update button appears, Update and restart replaces the app, and it comes back on the new version with the stream working.
- [ ] Update is refused while streaming, on both apps.
- [ ] Phone on the previous nightly: the update card offers the new build, asks once to allow installs from Telescope, and the installed app shows the new version.
- [ ] Phone app older than the desktop, plugged in: the Connection panel says so and Update over USB installs the bundled APK.
- [ ] Switching the channel to Stable on a nightly build doesn't offer a downgrade.

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
- [ ] Connect via USB only / Wi-Fi only is honoured, including reporting a missing cable instead of falling back.
- [ ] Phone gets a new IP from the router: the desktop finds it again (mDNS) without re-pairing.
- [ ] Phone foreground: desktop Start/Stop controls camera; test with screen dark too.
- [ ] Local-only mode blocks Wi-Fi access (verify from second machine on network).
- [ ] Camera controls (lens, exposure, WB, OIS) apply live and match what's shown on the desktop UI.
- [ ] Stream transforms (flip, rotate, zoom/pan) apply without restart.
- [ ] Point focus: clicking a near and a far object in the preview focuses each (with and without zoom, flip and rotation), exposure follows the point in auto, and Auto returns to continuous. The pop-out works the same.
- [ ] Presets: switching between two presets while streaming changes lens, exposure, WB, zoom and fps together. A second phone doesn't see the first one's presets.
- [ ] Canvas size change (Linux and Windows) restarts cleanly.
- [ ] Linux, module not loaded: Start asks once, with the startup box ticked; after a reboot Start doesn't ask. Unticked: it asks again after a reboot.
- [ ] Linux without pkexec (or with no polkit agent): Start shows the command banner, Copy command works, and running it then Start streams.
- [ ] Each Start failure shows a banner with a working button: no phone (Add phone), phone app closed (Try again), USB only without a cable (Switch to Automatic), app versions differ (Update).
- [ ] Battery/temp alerts fire once per threshold cross, not repeatedly.
- [ ] Config persists across app restart, including per-phone settings after switching. A config from the previous version keeps global settings and asks to pair again.
- [ ] Corrupted `telescope_config.json` backs up (`.invalid-<timestamp>`) and app starts with defaults.
- [ ] Tray minimize/restore and single-instance behavior both work.
- [ ] Start streaming when the phone is ready: opening the phone app (or plugging it in) starts the stream; Stop keeps it stopped until the phone leaves and comes back; closing the window keeps it in the tray.
- [ ] Open Telescope when I sign in (Linux and Windows): after signing out and in, Telescope is in the tray; unticking removes the entry.

## Sign-off

- [ ] Device-compatibility matrix updated with test results.
- [ ] CHANGELOG/release notes drafted.
- [ ] Tag pushed; the Release workflow published the assets.
