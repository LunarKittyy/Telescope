# Manual release checklist

Run before tagging. CI covers pytest, Android unit tests, and packaging smoke checks; everything here needs a real phone and desktop.

## Before starting

- [ ] `desktop` pytest suite passes locally and in CI for the release commit.
- [ ] Android JVM unit tests pass locally and in CI for the release commit.
- [ ] `desktop/scripts/smoke_check.py` passes on both Linux and Windows CI runners.
- [ ] `desktop/constraints.txt` installs cleanly in clean venv and versions haven't drifted out of date.

## Packaging

- [ ] Windows: `TelescopeDesktop.exe` launches, registers UnityCapture on first run via System Setup, bundled `adb.exe` works for USB.
- [ ] Linux: `start.sh` creates venv at `$XDG_DATA_HOME/Telescope/venv` on clean machine/account and launches successfully.
- [ ] Both bundles contain `THIRD_PARTY_NOTICES.txt`.
- [ ] APK installs via `adb install` and via the desktop app's Setup Drivers & APK button.

## Functional pass (see [device-compatibility.md](device-compatibility.md) for the per-device matrix)

- [ ] Wi-Fi QR and USB Pair via ADB work end-to-end on at least one device per platform (Linux + Windows).
- [ ] Re-pairing rotates token (old token gets 401; verify with curl).

### Pairing across networks and VPNs

QR code advertises desktop addresses, phone sends LAN attempts on Wi-Fi. Re-check when network setup changes:

- [ ] Offline router (no WAN uplink at all): pairing still works, and the QR code lists the LAN address.
- [ ] Desktop VPN only (LAN access permitted): pairing works over LAN address (QR lists it, not just VPN).
- [ ] Phone VPN only (LAN access permitted): pairing works (tests Wi-Fi-bound first attempt).
- [ ] Both devices VPN (both LAN-accessible): pairing works.
- [ ] Tailscale on both (no shared LAN): pairing works over `100.64/10`.
- [ ] VPN blocking local-network traffic: pairing fails (dialog lists each address), desktop shows advertised addrs. USB pairing works.
- [ ] Desktop with Docker/libvirt/VirtualBox installed: the QR code does not advertise `docker0`/`virbr0`/`vboxnet0` addresses.
- [ ] Phone on tailnet desktop is not on: after Wi-Fi pairing, address dropdown stays on phone Wi-Fi (not `100.64/10`), stream works.
- [ ] Phone "Reset pairing" revokes access (requests get 401 until re-paired).
- [ ] USB and Wi-Fi streaming work, switch between them without restarting.
- [ ] Phone foreground: desktop Start/Stop controls camera; test with screen dark too.
- [ ] Local-only mode blocks Wi-Fi access (verify from second machine on network).
- [ ] Camera controls (lens, exposure, WB, OIS) apply live and match what's shown on the desktop UI.
- [ ] Stream transforms (flip, rotate, zoom/pan) apply without restart.
- [ ] Canvas size change (Linux and Windows) restarts cleanly.
- [ ] Battery/temp alerts fire once per threshold cross, not repeatedly.
- [ ] Config persists across app restart, including per-device settings after switching.
- [ ] Corrupted `telescope_config.json` backs up (`.invalid-<timestamp>`) and app starts with defaults.
- [ ] Tray minimize/restore and single-instance behavior both work.

## Sign-off

- [ ] Device-compatibility matrix updated with test results.
- [ ] CHANGELOG/release notes drafted.
- [ ] Tag pushed; CI publishes assets.
