# Device compatibility matrix

Manually maintained. Update a row after actually testing that device/build
combination - don't assume a device works because a similar one does.
"OK" means the feature worked as documented in the README; note anything
that only partially worked instead of just checking it off.

Legend: `OK` tested and working · `PARTIAL` works with caveats (see notes) ·
`FAIL` doesn't work · `-` not tested yet.

| Device | Android version | App build | USB pairing | Wi-Fi pairing | Lens selection | Manual exposure | Manual WB | OIS toggle | Reconnect after drop | Battery/temp reporting | Stop/start | Notes |
|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Pixel-like (e.g. Pixel 6/7/8) | - | - | - | - | - | - | - | - | - | - | - | |
| Samsung Galaxy S-series | - | - | - | - | - | - | - | - | - | - | - | |
| Samsung Galaxy A-series | - | - | - | - | - | - | - | - | - | - | - | |
| vivo | - | - | - | - | - | - | - | - | - | - | - | |

## What to check per row

- **USB pairing**: QR pairing completes while the phone is connected via USB (still requires Wi-Fi reachability for the pairing HTTP handshake - see the README's QR pairing section); `adb forward` + authenticated stream works after.
- **Wi-Fi pairing**: QR pairing completes over Wi-Fi; authenticated stream works without USB connected.
- **Lens selection**: all physical sub-cameras (wide/main/telephoto) enumerate and switching between them actually changes the video feed, not just digital zoom.
- **Manual exposure**: ISO and shutter sliders actually change exposure on-device (not just greyed-in/out correctly).
- **Manual WB**: Kelvin slider visibly shifts color temperature (README already notes this is inconsistent across devices/lenses - record exactly what happens, not just pass/fail).
- **OIS toggle**: has a visible effect on lenses that report `hasOis: true`.
- **Reconnect after drop**: kill Wi-Fi or unplug USB mid-stream, confirm the desktop app reconnects automatically within `RECONNECT_DELAY` once connectivity returns, without needing a full stream restart.
- **Battery/temp reporting**: footer values update and alert thresholds fire correctly.
- **Stop/start**: repeated stop/start cycles (at least 5 in a row) don't leave the phone's foreground service or the desktop's virtual camera in a broken state.

## Process

1. Install the release candidate APK and desktop bundle for the platform under test.
2. Pair fresh (reset pairing on the phone first if it was previously paired to a different desktop build) via both USB and Wi-Fi.
3. Work through each column, noting the exact app build/commit tested in the "App build" column.
4. File an issue for any `FAIL` or notable `PARTIAL` before checking a release off in [release-checklist.md](release-checklist.md).
