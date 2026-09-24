# Manual release checklist

Run before tagging. CI covers pytest, Android unit tests, and packaging smoke checks; everything here needs a real phone and desktop.

## Before starting

- [ ] `desktop` pytest suite passes locally and in CI for the release commit.
- [ ] Android JVM unit tests pass locally and in CI for the release commit.
- [ ] `desktop/scripts/smoke_check.py` passes on both Linux and Windows CI runners.
- [ ] `desktop/constraints.txt` installs cleanly in clean venv and versions haven't drifted out of date.

## Packaging

- [ ] Windows: `TelescopeDesktop.exe` launches, the first-run checklist's Install driver registers UnityCapture, bundled `adb.exe` works for USB.
- [ ] Linux: `start.sh` creates venv at `$XDG_DATA_HOME/Telescope/venv` on clean machine/account and launches successfully.
- [ ] Both bundles contain `THIRD_PARTY_NOTICES.txt`.
- [ ] APK installs via `adb install`, via the checklist's QR link, and via Install over USB (checklist and Advanced).

## Functional pass (see [device-compatibility.md](device-compatibility.md) for the per-device matrix)

- [ ] Fresh install: the checklist shows, each step ticks as it's done, and the video stage returns once a phone is paired.
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
- [ ] Canvas size change (Linux and Windows) restarts cleanly.
- [ ] Battery/temp alerts fire once per threshold cross, not repeatedly.
- [ ] Config persists across app restart, including per-phone settings after switching. A config from the previous version keeps global settings and asks to pair again.
- [ ] Corrupted `telescope_config.json` backs up (`.invalid-<timestamp>`) and app starts with defaults.
- [ ] Tray minimize/restore and single-instance behavior both work.

## Sign-off

- [ ] Device-compatibility matrix updated with test results.
- [ ] CHANGELOG/release notes drafted.
- [ ] Tag pushed; CI publishes assets.
