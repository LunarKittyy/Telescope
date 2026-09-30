# Device compatibility matrix

Manually maintained - update after testing that exact device/build combo. "OK" means the feature worked as documented in [features.md](features.md); put caveats in the notes under the table instead of just checking off.

Tried Telescope on a phone? Share how it went in [Device compatibility](https://github.com/LunarKittyy/Telescope/discussions/new?category=device-compatibility).

Legend: `OK` tested and working · `PARTIAL` works with caveats (see [notes](#notes)) ·
`FAIL` doesn't work · `-` not tested yet.

| Device | Android version | App build | USB pairing | Wi-Fi pairing | Lens selection | Zoom on Auto | Manual exposure | Manual WB | Focus | OIS toggle | H.264 | Microphone | Reconnect after drop | Battery/temp reporting | Stop/start |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Pixel-like (e.g. Pixel 6/7/8) | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - |
| Samsung Galaxy S10+ (SM-G975F) | 12 | 3.1.0-nightly.374 | OK | OK | PARTIAL | - | OK | OK | OK | OK | OK | OK | OK | OK | OK |
| Samsung Galaxy A-series | - | - | - | - | - | - | - | - | - | - | - | - | - | - | - |
| vivo V2413 | 16 | b1819a6 | OK | OK | OK | OK | OK | OK | OK | OK | OK | OK | OK | OK | OK |

## Notes

- **Samsung Galaxy S10+**: the 2x telephoto isn't exposed through Camera2, so only the main, ultrawide and both front lenses show up. All four of those work.

## What to check per row

- **USB pairing**: with Add phone open, plugging in pairs the phone on USB only (no Wi-Fi/LAN), pairing server reached via `adb reverse`, then `adb forward` + authenticated stream works.
- **Wi-Fi pairing**: QR code pairing works over Wi-Fi without USB.
- **Lens selection**: all physical lenses (wide/main/telephoto) enumerate; switching changes video feed, not just digital zoom.
- **Zoom on Auto**: on the Auto lens, zooming in past the lens marks switches to the telephoto (the dot next to the zoom turns lavender) and the picture gets sharper.
- **Manual exposure**: ISO and shutter sliders change on-device exposure (not just toggle correctly).
- **Manual WB**: Kelvin slider visibly shifts color temperature (features.md already notes this is inconsistent across devices/lenses - record exactly what happens, not just pass/fail).
- **Focus**: manual focus distance and clicking the preview to focus both change what's sharp.
- **OIS toggle**: visible effect on lenses reporting `hasOis: true`.
- **H.264**: switching Format to H.264 streams, and the Mbps readout drops compared to MJPEG.
- **Microphone**: with the Microphone card on, other apps record the phone's mic.
- **Reconnect after drop**: kill Wi-Fi or unplug USB mid-stream; confirm auto-reconnect within `RECONNECT_DELAY` when connectivity returns (no full restart needed).
- **Battery/temp reporting**: Monitoring-panel values update and alert thresholds fire correctly.
- **Stop/start**: the desktop's Start/Stop controls the phone camera, including with the phone screen dark, and 5+ cycles don't break the phone's foreground service or the desktop virtual camera.

## Process

1. Install release-candidate APK and desktop bundle.
2. Pair fresh (reset phone pairing first if previously paired to different build) via USB and Wi-Fi.
3. Work each column, note exact build/commit in "App build" column.
4. File issue for any FAIL or notable PARTIAL before checking off in [release-checklist.md](release-checklist.md).
