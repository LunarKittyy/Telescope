<!-- covers: tools/device_row.py .github/DISCUSSION_TEMPLATE/device-compatibility.yml; checked: a604783e3 -->
# Device compatibility

Phones Telescope has been tried on and what each one can do with it. Android 8 or newer is needed. The table covers what changes from one phone to the next. Pairing, reconnecting and starting or stopping the camera work the same on every phone.

Tried Telescope on a phone that isn't here? [Tell me how it went](https://github.com/LunarKittyy/Telescope/discussions/new?category=device-compatibility). Paste the phone's diagnostics there (in the phone app, open About and tap **Copy diagnostics**) and most of the row can be filled in from that alone. Even one stream helps.

✅ works · ⚠️ works with caveats (see [notes](#notes)) · ❌ doesn't work · ➖ not known yet · n/a the phone has no lens to switch to

| Phone | Android | Tested with | Lenses Telescope can use | Auto zoom | Frame rate | Manual control | Light (H.264) | Photos | Microphone |
|---|---|---|---|---|---|---|---|---|---|
| Samsung Galaxy S10+ (SM-G975F) | 12 | 3.1.0-nightly.374 | ⚠️ main, ultrawide, 2 front | ❌ | ➖ | ✅ | ✅ | ➖ | ✅ |
| Samsung Galaxy S25 Ultra (SM-S938U) | 17 | ➖ | ultrawide, main, 2.8x, 5.1x, 2 front | ✅ | 60 fps | ✅ | ✅ | 12 MP | ➖ |
| vivo V2413 | 16 | 3.1.0-nightly.374 | all of them | ✅ | ➖ | ✅ | ✅ | ➖ | ✅ |
| Xiaomi Poco M6 Pro (2312FPCA6G) | 16 | 3.4.0 | ⚠️ main, front | n/a | 30 fps | ✅ | ✅ | 8 MP, RAW | ✅ |
| OnePlus 7 (GM1903) | 12 | not streamed yet | main, front | n/a | 30 fps | ✅ | ➖ | 12 MP | ➖ |

## What the columns mean

- **Lenses Telescope can use** are the ones that show up in the lens picker. Phone makers decide which lenses other apps get. A lens the phone has but Telescope can't use is marked ⚠️ with a note
- **Auto zoom** is whether zooming in on the Auto lens moves to the telephoto for a sharper picture
- **Frame rate** is the highest rate the phone offers Telescope on the main lens (Telescope goes up to 60)
- **Manual control** is manual exposure (ISO and shutter), white balance and focus on the main lens. Front cameras often have no focus to set
- **Light (H.264)** is whether the phone's video encoder works for the Light format. Without it Telescope streams Heavy (MJPEG), which needs more bandwidth
- **Photos** is the size of a normal photo from the main lens with the [Photos](features/effects.md#photos) card and whether the phone offers RAW. Full resolution mode can go bigger on some phones
- **Microphone** is whether the phone's mic works as the Telescope microphone on the computer

## Notes

- **Samsung Galaxy S10+:** Auto can't switch to the 2x telephoto because the phone doesn't give it to other apps. Only the main, ultrawide and both front lenses show up and all four work
- **Samsung Galaxy S25 Ultra:** Auto covers the main camera plus both telephotos (they kick in at about 3x and 5x). The ultrawide and both front cameras show up as their own lenses
- **Xiaomi Poco M6 Pro:** the ultrawide and macro lenses don't show up because the phone only gives other apps the main back camera and the front camera. Xiaomi's own camera app records 1080p at 60 fps but other apps get 30 at most, since the phone offers 60 fps and up only as 720p slow-motion mode, which Telescope doesn't use
- **OnePlus 7:** filled in from its diagnostics only, it hasn't streamed yet. Like on the Poco, the 60 fps and up modes are slow-motion only (1080p up to 240 fps), which Telescope doesn't use
- Some phone makers close apps in the background more eagerly than others. It isn't in the table because it changes with the phone's settings and how long it's been idle as much as with the model. What helps is under "The stream stops after a while with the phone's screen off" in [Troubleshooting](troubleshooting.md#pairing-and-connecting)

<details>
<summary><b>For testers: how to fill a row</b></summary>

### From the diagnostics

In the phone app open About and tap **Copy diagnostics**, then run the text through the script in `tools/`:

```sh
python3 tools/device_row.py diagnostics.txt
```

It prints a draft row with what the phone reports to Telescope (the phone, Android version, lenses, frame rate, manual control and photo size). Pasting the desktop's diagnostics into the same file adds whether the phone offers H.264. A phone can report something that then fails in a stream. The checks below turn a draft into ✅ or ❌.

### What to check

| Column | It passes when |
|---|---|
| Lenses Telescope can use | Every lens in the picker changes the picture when picked, not just the zoom. Compare with the lenses the phone has and mark ⚠️ with a note if any are missing |
| Auto zoom | Zooming past the dots on the zoom slider switches to the telephoto (the dot next to the zoom turns lavender) and the picture gets sharper. n/a when the phone gives Telescope one back lens only |
| Frame rate | The highest FPS that isn't greyed out streams at that rate |
| Manual control | The ISO and shutter sliders change the exposure, the Kelvin slider shifts the colour and manual focus and clicking the preview both change what's sharp. Note anything only some lenses can do |
| Light (H.264) | Switching Format to Light streams and the Mbps readout drops compared to Heavy |
| Photos | The Photos card saves a photo at the size shown. Note whether RAW is in the Format list |
| Microphone | With the Microphone card on, other apps record the phone's mic |

### Process

1. Install the release candidate APK and desktop bundle and pair the phone (reset the phone's pairing first if it was paired to a different build)
2. Draft the row from the diagnostics, then stream and work through each column
3. Put the exact app version under "Tested with". A row from diagnostics alone gets "not streamed yet" there
4. File an issue for any ❌ or notable ⚠️ before ticking it off in the [release checklist](development/release-checklist.md)

Pairing, reconnecting, stop and start and the battery and temperature readouts belong to the release checklist rather than this table, since they don't depend on the phone.
</details>
