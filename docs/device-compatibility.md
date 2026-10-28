# Device compatibility

Phones Telescope has been tested on, and what worked on each. Android 8 or newer is needed.

Tried Telescope on a phone that isn't here? [Tell me how it went](https://github.com/LunarKittyy/Telescope/discussions/new?category=device-compatibility). Even one stream helps.

✅ works · ⚠️ works with caveats (see [notes](#notes)) · ❌ doesn't work · ➖ not tested

| Device | Android | App build | USB pairing | Wi-Fi pairing | Lens selection | Zoom on Auto | Manual exposure | Manual WB | Focus | OIS toggle | H.264 | Microphone | Reconnect | Battery/temp | Stop/start |
|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|
| Samsung Galaxy S10+ (SM-G975F) | 12 | 3.1.0-nightly.374 | ✅ | ✅ | ⚠️ | ➖ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Samsung Galaxy S25 Ultra (SM-S938U) | 17 | ➖ | ✅ | ➖ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ➖ | ➖ | ✅ | ✅ |
| vivo V2413 | 16 | 3.1.0-nightly.374 | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |
| Xiaomi Poco M6 Pro (2312FPCA6G) | 16 | 3.4.0 | ✅ | ✅ | ⚠️ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ |

## Notes

- **Samsung Galaxy S10+:** the 2x telephoto isn't exposed through Camera2, so only the main, ultrawide and both front lenses show up. All four work
- **Samsung Galaxy S25 Ultra:** Auto covers the main camera plus both telephotos (they kick in at about 3x and 5x). The ultrawide and both front cameras show up as their own lenses. Tested over USB only
- **Xiaomi Poco M6 Pro:** the ultrawide and macro lenses don't show up because Camera2 only exposes the main back camera and the front camera. Xiaomi's own camera app records 1080p at 60 fps but other apps get 30 at most, since Android offers 60 fps and up only as 720p high-speed mode, which Telescope doesn't use

<details>
<summary><b>For testers: what each column means and how to fill a row</b></summary>

### What to check

| Column | It passes when |
|---|---|
| USB pairing | With Add phone open, plugging in and clicking **Pair** pairs the phone on USB only and an authenticated stream works |
| Wi-Fi pairing | QR pairing works over Wi-Fi without USB |
| Lens selection | Every physical lens (wide, main, telephoto) shows up and switching changes the video feed, not just digital zoom |
| Zoom on Auto | Zooming past the lens marks switches to the telephoto (the dot next to the zoom turns lavender) and the picture gets sharper |
| Manual exposure | The ISO and shutter sliders change the exposure on the device |
| Manual WB | The Kelvin slider visibly shifts colour temperature. Record exactly what happens since it differs across devices and lenses |
| Focus | Manual focus distance and clicking the preview both change what's sharp |
| OIS toggle | Visible effect on lenses that report OIS |
| H.264 | Switching Format to Light streams and the Mbps readout drops compared to Heavy |
| Microphone | With the Microphone card on, other apps record the phone's mic |
| Reconnect | Kill Wi-Fi or unplug USB mid-stream and it reconnects when connectivity returns, with no full restart |
| Battery/temp | The Monitoring values update and alert thresholds fire |
| Stop/start | Start and Stop control the camera, including with the screen dark, and 5+ cycles don't break the phone's service or the virtual camera |

### Process

1. Install the release candidate APK and desktop bundle
2. Pair fresh (reset the phone's pairing first if it was paired to a different build) over USB and Wi-Fi
3. Work each column and note the exact build in "App build"
4. File an issue for any ❌ or notable ⚠️ before ticking it off in the [release checklist](release-checklist.md)

The matrix is maintained by hand. Update it after testing that exact device and build.
</details>
