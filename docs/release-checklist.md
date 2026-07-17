# Manual release checklist

Run through this before tagging a release. CI (pytest + Android unit tests
+ packaging smoke checks) covers what can be automated; everything here
needs a real phone and a real desktop machine because it can't be.

## Before starting

- [ ] `desktop` pytest suite passes locally and in CI for the release commit.
- [ ] Android JVM unit tests pass locally and in CI for the release commit.
- [ ] `desktop/scripts/smoke_check.py` passes on both Linux and Windows CI runners.
- [ ] `desktop/constraints.txt` still installs cleanly (`pip install -r requirements.txt -c constraints.txt` in a clean venv) and its versions haven't drifted meaningfully out of date.

## Packaging

- [ ] Windows: `TelescopeDesktop.exe` launches without a console window, registers UnityCapture on first run via System Setup, and the bundled `adb.exe` works for USB mode.
- [ ] Linux: `start.sh` creates the Telescope-owned venv under `$XDG_DATA_HOME/Telescope/venv` (or `~/.local/share/Telescope/venv`) on a clean machine/user account with no prior Telescope install, and launches successfully.
- [ ] Both bundles contain `THIRD_PARTY_NOTICES.txt`.
- [ ] APK installs via `adb install` and via the desktop app's Setup Drivers & APK button.

## Functional pass (see [device-compatibility.md](device-compatibility.md) for the per-device matrix)

- [ ] QR pairing works end-to-end on at least one real device per platform (Linux + Windows desktop).
- [ ] Re-pairing an already-paired phone rotates its token (old token stops working - verify with `curl` returning 401).
- [ ] "Reset pairing" on the phone actually revokes access (further requests 401 until re-paired).
- [ ] USB and Wi-Fi streaming both work, including switching between them without restarting the app.
- [ ] Local-only mode actually blocks Wi-Fi access (verify from a second machine on the same network).
- [ ] Camera controls (lens, exposure, WB, OIS) apply live and match what's shown on the desktop UI.
- [ ] Stream transforms (flip, rotate, zoom/pan, resolution downscale) apply without a stream restart.
- [ ] Canvas size change (Linux and Windows) restarts cleanly.
- [ ] Battery/temperature alerts fire once per threshold crossing, not repeatedly.
- [ ] Config persists correctly across an app restart, including per-device settings after switching devices.
- [ ] A deliberately corrupted `telescope_config.json` gets backed up (`.invalid-<timestamp>`) and the app starts with defaults instead of crashing.
- [ ] Tray minimize/restore and single-instance behavior both work.

## Sign-off

- [ ] Device-compatibility matrix updated with this release's actual test results.
- [ ] `CHANGELOG`/release notes drafted.
- [ ] Tag pushed; CI publishes the release assets.
