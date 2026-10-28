# Next release

Planned in October 2026 after 4.3.1. It's going to be 4.4 since webcam support is too big for a patch release. PyAV 19 is left over from 4.3.

- [x] [Translations for the 4.3 plugins](#translations-for-the-43-plugins)
- [ ] [Webcams and capture cards](#webcams-and-capture-cards)
- [ ] [UDP on Wi-Fi moves to Settings](#udp-on-wi-fi-moves-to-settings)
- [ ] [Black screen on the phone](#black-screen-on-the-phone)
- [ ] [PyAV 19](#pyav-19)

## Translations for the 4.3 plugins

The 4.3 batch added 545 texts that none of the six languages has yet. Until they do the new plugins show in English everywhere. The translation site didn't get them either. Once they're translated the site gets the new texts and a new build to show the new plugins in its previews.

## Webcams and capture cards

A webcam plugged into the computer shows up in the phone picker under its own name and streams like a phone would, with every effect and a virtual camera of its own. Cameras with a USB webcam mode and anything plugged into an HDMI capture card count as webcams too, which covers most of what people mean by DSLR support. Capture goes through OpenCV (V4L2 on Linux, Media Foundation on Windows) and Telescope's own virtual cameras never show up in the list.

The Webcam card offers the camera's own resolutions and frame rates. A controls panel is being added on top, built from whatever the camera reports (focus, exposure time, gain, white balance, anti-flicker and the like). On Windows it's best effort and any control that doesn't work reliably there gets hidden.

Before release it needs a pass on real cameras (a Logitech C920 and an HP 320 FHD on Linux so far) and a full run on Windows. The draft PR is #381.

## UDP on Wi-Fi moves to Settings

The switch leaves the Connection card for the Streaming tab of the Settings window and stays off by default. On real Wi-Fi it stutters more often than TCP does and it shouldn't sit up front looking like a headline feature. Anyone who switched it on keeps it on. The draft PR is #382.

## Black screen on the phone

A button next to Preview in the phone app turns the screen black while the camera keeps going, like the one on Browser camera's page. Double tap wakes it. Some phones' power saving still kills the stream now and then even with battery optimisation off. An app that stays on screen should get killed less often (the screen being black saves battery on OLED phones too). The draft PR is #383.

## PyAV 19

Needs Python 3.12, which 4.2 brought in. aiortc 1.15 still declares `av<18` which we get around with `--no-deps`. aiortc's main branch already allows `av<20` but there's no release with it yet. Wait for that release, then drop the `--no-deps` workaround if it's no longer needed. PyAV 19 has API changes too. Check browser video and audio and hardware decoding on a real device. The draft PR is #273.
