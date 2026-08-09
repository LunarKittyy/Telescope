# Device compatibility matrix

Manually maintained - update after testing that exact device/build combo. "OK" means the feature worked as documented in README; note caveats instead of just checking off.

Legend: `OK` tested and working · `PARTIAL` works with caveats (see notes) ·
`FAIL` doesn't work · `-` not tested yet.

| Device | Android version | App build | USB pairing | Wi-Fi pairing | Lens selection | Manual exposure | Manual WB | OIS toggle | Reconnect after drop | Battery/temp reporting | Stop/start | Remote start/stop | Notes |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Pixel-like (e.g. Pixel 6/7/8) | - | - | - | - | - | - | - | - | - | - | - | - | |
| Samsung Galaxy S-series | - | - | - | - | - | - | - | - | - | - | - | - | |
| Samsung Galaxy A-series | - | - | - | - | - | - | - | - | - | - | - | - | |
| vivo V2413 | (see build) | b1819a6 | OK | OK | OK | OK | OK | OK | OK | - | OK | - | Two defects found on 56bdafe are fixed as of 448fa13/b1819a6: reconnect after drop now resends the last-applied control settings (exposure/WB/etc.) instead of leaving the phone on defaults, and the paired device now survives a desktop app restart while in USB mode instead of losing its selection. |

## What to check per row

- **USB pairing**: Pair via ADB completes with phone on USB only (no Wi-Fi/LAN), pairing server reached via `adb reverse`, then `adb forward` + authenticated stream works.
- **Wi-Fi pairing**: QR code pairing works over Wi-Fi without USB.
- **Lens selection**: all physical lenses (wide/main/telephoto) enumerate; switching changes video feed, not just digital zoom.
- **Manual exposure**: ISO and shutter sliders change on-device exposure (not just toggle correctly).
- **Manual WB**: Kelvin slider visibly shifts color temperature (README already notes this is inconsistent across devices/lenses - record exactly what happens, not just pass/fail).
- **OIS toggle**: visible effect on lenses reporting `hasOis: true`.
- **Reconnect after drop**: kill Wi-Fi or unplug USB mid-stream; confirm auto-reconnect within `RECONNECT_DELAY` when connectivity returns (no full restart needed).
- **Battery/temp reporting**: Monitoring-panel values update and alert thresholds fire correctly.
- **Stop/start**: 5+ stop/start cycles don't break phone foreground service or desktop virtual camera.
- **Remote start/stop**: Telescope app foreground - desktop Start/Stop control phone camera; test with screen dark too.

## Process

1. Install release-candidate APK and desktop bundle.
2. Pair fresh (reset phone pairing first if previously paired to different build) via USB and Wi-Fi.
3. Work each column, note exact build/commit in "App build" column.
4. File issue for any FAIL or notable PARTIAL before checking off in [release-checklist.md](release-checklist.md).
