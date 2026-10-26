# Control API reference

Server is on the phone at port 8080 for `/v1/video`, `/v1/video.h264`, `/v1/video.udp`, `/v1/audio`, `/v1/sensors`, `/v1/state` and `/v1/control` (all only exist while actively streaming); a separate responder on port 8766 serves `/v1/hello`, `/v1/ping`, `/v1/session` and `/v1/unpair`. Every request below except `/v1/hello` requires an `Authorization: Bearer <token>` header carrying the token this computer got when it paired; missing or unknown tokens get `401`. With **Local only** on, port 8766 answers anything that didn't arrive over USB (the phone's loopback, where `adb forward` lands) with `403`, except `/v1/ping` and `/v1/unpair`. The desktop reads a `403` on `/v1/hello` as "this phone is in Local only" and checks with `/v1/ping`.

## `GET /v1/state`

A short example is below. [`state.json`](state.json) has every field, and both apps' tests check it (`StateExampleTest.kt` that the phone still sends it, `test_state_example.py` that the desktop still reads it), so a field renamed on one side fails a test instead of quietly breaking a control. After changing `V1State`, run the phone's tests with `UPDATE_STATE_EXAMPLE=1` to rewrite it.

```json
{
  "cameras": [
    {
      "id": "0",
      "logicalId": null,
      "label": "Back ~24mm OIS",
      "current": false,
      "hasOis": true,
      "isoMin": 50,
      "isoMax": 12800,
      "shutterMinNs": 100000,
      "shutterMaxNs": 1000000000,
      "supportsManualSensor": true,
      "supportsManualWB": true,
      "supportsManualFocus": true,
      "supportsFocusPoint": true,
      "supportsMeterPoint": true,
      "minFocusDistance": 8.3,
      "aeCompMin": -8,
      "aeCompMax": 8,
      "aeCompStep": 0.167,
      "supportsFlash": true,
      "hwLevel": "FULL",
      "supportedSizes": [
        { "width": 4032, "height": 3024 },
        { "width": 1920, "height": 1080 }
      ],
      "zoomRatioMax": 10.0,
      "cropZoomMax": 10.0,
      "freeformCrop": true,
      "lensZooms": [3.0],
      "maxFps": 60
    }
  ],
  "auto": true,
  "iso": null,
  "shutter_ns": null,
  "wb_manual": false,
  "wb_r": null,
  "wb_ge": null,
  "wb_go": null,
  "wb_b": null,
  "ois": true,
  "focus_mode": "continuous",
  "focus_distance": 0.0,
  "nr_mode": 1,
  "edge_mode": 1,
  "ae_comp": 0,
  "black_level_lock": false,
  "torch": false,
  "jpeg_quality": 85,
  "phone_fps": 30,
  "camera_fps": 29.8,
  "codecs": ["mjpeg", "h264"],
  "codec": "mjpeg",
  "bitrate": 8087040,
  "active_lens": "2",
  "stream_width": 1920,
  "stream_height": 1080,
  "battery": 87,
  "charging": false,
  "battery_temp_c": 32.5
}
```

`minFocusDistance`, `aeCompMin`/`aeCompMax`/`aeCompStep` are per-lens, reported by Camera2 (`aeCompStep` is typically `0.167` = 1/6 EV). `wb_r`/`wb_ge`/`wb_go`/`wb_b` are the current RGGB channel gains when `wb_manual` is true, `null` otherwise. `supportedSizes` is the lens's actual list of capture sizes, which the desktop uses to populate its resolution dropdown instead of a fixed list. `h264Sizes` is the part of that list the phone's H.264 encoder takes (`MediaCodecInfo.VideoCapabilities.isSizeSupported`), so on Light the desktop leaves the rest out; it's missing on a phone without an encoder and on an older app, and the desktop then lists every size. `stream_width`/`stream_height` are the size the desktop asked for on the current lens. `codecs` lists what the phone can send (`h264` only with a hardware encoder), `codec` is what it's sending, and `bitrate` is the H.264 target in bits per second (on Dynamic, where it is right now). `dynamic_bitrate` is true when the phone takes `bitrate` `-1`. `dynamic_fps` is the rate Dynamic stepped the camera down to because the link is too slow for the one asked for (`phone_fps` stays what was asked for); it's left out at the rate asked for and by older phones. `heat_fps` is the rate the phone capped itself at because Android reports it's running hot (24 fps at the severe thermal status, 15 at critical, Android 10 and newer; from Android 11 also at 24 when `getThermalHeadroom` says severe is 30 s away); it's left out while it isn't capped. `heat_width`/`heat_height` are the size the phone dropped to at the critical thermal status (the largest one of the same shape whose short side is 720 or less), while `stream_width`/`stream_height` stay the size asked for so the desktop keeps that pick; they're left out at the size asked for and by older phones. `h264_profile` is what the encoder was set up with while H.264 runs: `high` (asked for without B-frames, and dropped for the rest of the run if the encoder puts frames out of order anyway), `baseline` where the encoder refuses High, or `auto` where it refuses both. `intra_refresh` is true when the encoder refreshes the picture over about a second of frames instead of sending a keyframe every second, so there are no keyframe bursts on the link (keyframes then only come when a viewer joins or skips ahead, and every 10 s); it's left out where the encoder doesn't offer it. `wifi_band` (`2.4`, `5` or `6` GHz), `wifi_rssi` (dBm) and `wifi_mbps` (link speed) describe the Wi-Fi the phone is on, which the desktop uses to say why a Wi-Fi stream can't keep up; they're left out on mobile data, before Android 10 and by older phones. `codec_error` appears when H.264 failed and the phone went back to MJPEG, and `codec_unsupported` is true when that's because it can't do H.264 at this size or rate (not a crash), which the desktop asks about instead of switching to Heavy. `zoomRatioMax` is how far the lens zooms on the sensor (1 = it can't), `cropZoomMax` how far the crop can go, `freeformCrop` whether the crop can sit off-centre, and `lensZooms` the ratios where a multi-lens camera switches to a longer lens. `maxFps` is the fastest rate in the lens's AE target FPS ranges, which the desktop uses to gray out faster FPS choices. `supportsFocusPoint` is true on a lens that takes `focus_point` and `supportsMeterPoint` on one that takes `meter_point` (it has AE regions); a fixed-focus front camera often has the second without the first, and an older phone leaves `supportsMeterPoint` out. `camera_fps` is how many frames per second the phone actually made for the stream over the last couple of seconds (under `phone_fps` in dim light), which the desktop compares with what arrives before calling the connection slow. `active_lens` is the physical lens a multi-lens camera is streaming from right now. `low_light` is true while Low light is on, and `low_light_toggle` is true on a phone that takes the `low_light` action (an older one leaves both out). `camera_toggle` is true on a phone that takes `camera_on` and a start with the camera off; an older phone leaves it out. `camera_off` is true while the stream is just the mic, and `camera_error` says why the camera didn't come back on (it then stays off). `camera_taken` is true while another app on the phone has the camera: the stream (and the mic) keeps going without video, and the phone opens the camera again as soon as Android says it's free. Only a camera taken after the stream's first frame waits like this; one that's in use at the start still fails the start. `takes_notes` is true on a phone that takes the notes controls below (an older phone leaves it out and the desktop asks for an update instead), `notes` is true while it holds notes from the desktop and `notes_shown` while they're on its screen. `notes_closed` is true once they were closed on the phone (Back or the close button): it doesn't open them again until new `notes` come. `notes_preview` is true while the phone's Preview is open and holds the notes back, since opening them would restart the camera session; they open once Preview closes. Older phones leave both out. `sensors` lists what `/v1/sensors` can stream on this phone (it's there even when empty); an older phone leaves it out and has no `/v1/sensors`. Fields at their default value are left out, so read them with a default.

## Texts the desktop words itself

The desktop is translated and the phone's English isn't, so where the phone has something to say it sends a fixed code and the numbers the sentence needs, and the desktop words it in its own language. The English text goes along next to them: an older desktop shows that, and a desktop shows it for a code it doesn't know. A phone that sends no code is read the same way, so either side can be a version behind. Neither breaks, it just shows English.

| Where | English text | Code and fields | Codes |
|---|---|---|---|
| `/v1/state` `cameras[]` | `label`, like `Back ~24mm OIS [auto]` | `facing`, `focalMm` (35 mm equivalent, left out when not known) and `lensKind` (left out for a camera that is just one lens). `hasOis` is the OIS part | `facing`: `back`, `front`, `external`. `lensKind`: `physical` (one lens of a multi-lens camera), `auto` (the multi-lens camera itself) |
| `/v1/state` | `codec_error` | `codec_error_code`, and `codec_error_width` and `codec_error_height` for the size it is about | `h264_unavailable`, `camera_cant_feed`, `camera_stopped` (all three: this size or rate won't work here, which `codec_unsupported` says too), `encoder_stopped` (a crash) |
| `/v1/state` | `camera_error` | `camera_error_code` | `camera_in_use` |
| `/v1/audio` refusal | `error` | `code`, in the same body: `{"error": "The phone's microphone is in use", "code": "mic_in_use"}` | `mic_none`, `mic_not_streaming`, `mic_permission`, `mic_open_app`, `mic_in_use` |

The desktop shortens a lens to the name on its button ("Auto ~24mm OIS", "F/~25mm") from these fields, or by cutting the words out of `label` when `facing` is missing. That name is also what a saved shortcut and the "which lens is live" comparison go by, so it stays in English; only the words shown are translated.

## `POST /v1/control`

JSON body `{"action": "<action>", ...params}`.

| `action` | extra params | effect |
|---|---|---|
| `camera` | `id=<id>` | Switch camera |
| `resolution` | `width=<int> height=<int>` | Set the capture resolution to one of the lens's reported supported sizes |
| `auto` | - | Restore auto exposure |
| `iso` | `value=<int>` | Set ISO, fitted to the lens's `isoMin`-`isoMax` (0 or less is refused); switches AE to OFF (once shutter is also set) |
| `shutter` | `value=<long ns>` | Set shutter in nanoseconds, fitted to `shutterMinNs`-`shutterMaxNs` (0 or less is refused); switches AE to OFF (once ISO is also set) |
| `wb_auto` | - | Restore auto white balance |
| `wb_gains` | `r=<float> ge=<float> go=<float> b=<float>` | Set manual white balance via `COLOR_CORRECTION_GAINS` RGGB channel gains, each above 0 and at most 16 (anything else is refused) |
| `ois` | `value=1\|0` | Toggle OIS |
| `focus_mode` | `value=continuous\|manual` | Switch autofocus / manual focus (also ends point focus) |
| `focus_point` | `x=<0..1> y=<0..1> [size=<0..1>]` | Focus on a point of the stream frame, and meter exposure there while it's automatic. `size` is the region's side as a fraction of the frame's shorter side (default 0.1). Refused on a lens without `supportsFocusPoint`. `/v1/state` then reports `focus_mode: "point"` |
| `meter_point` | `x=<0..1> y=<0..1> [size=<0..1>]` | Meter exposure on a point of the stream frame while it's automatic and leave focus as it is, for a lens that can't focus on a point. Same `size` as `focus_point`. Refused on a lens without `supportsMeterPoint`. Ends with `focus_point`, `focus_mode` or a lens switch |
| `focus_distance` | `value=<float diopters>` | Set manual focus distance, fitted to 0-`minFocusDistance` |
| `zoom` | `ratio=<float> crop=<float> x=<0..1> y=<0..1>` | Zoom on the phone: `ratio` is the centred zoom (a multi-lens camera switches lens on it), `crop` extra zoom inside that, centred on `x`, `y` of the view |
| `ae_comp` | `value=<int steps>` | Set exposure compensation, in the lens's AE-compensation steps (see `aeCompStep`), fitted to `aeCompMin`-`aeCompMax` |
| `nr_mode` | `value=<int 0-4>` | Set noise reduction mode (desktop UI only offers 0/1/2 = Off/Fast/High Quality) |
| `edge_mode` | `value=<int 0-3>` | Set sharpening/edge mode (desktop UI only offers 0/1/2 = Off/Fast/High Quality) |
| `black_level_lock` | `value=1\|0` | Toggle black level lock |
| `low_light` | `value=1\|0` | Let auto exposure slow the camera down in the dark (to 10 fps where the phone has a range for it, else the nearest below), with high quality noise reduction |
| `torch` | `value=1\|0` | Toggle flash/torch |
| `jpeg_quality` | `value=<int 1-100>` | Set JPEG quality on the phone |
| `bitrate` | `value=<int bits/s>` | Set the H.264 bitrate (clamped to 1-100 Mbps and to what the phone's encoder takes); `0` sizes it from resolution and fps; `-1` is Dynamic (the phone follows the link, see below). A phone without Dynamic treats `-1` as `0` |
| `camera_on` | `value=1\|0` | Turn the camera off (the server and `/v1/audio` keep going, every camera setting is kept, and controls sent meanwhile apply when it's back) or on again. A camera that won't reopen stays off and `/v1/state` reports `camera_error` instead of the stream ending |
| `keyframe` | - | Ask the H.264 encoder for a keyframe now. The desktop sends it when a plugin starts or stops changing the video before decoding it, so the change starts from a whole picture and the picture recovers afterwards without waiting for the next keyframe. Does nothing on Heavy |
| `fps_target` | `value=<int 1-120>` | Set capture FPS on the phone (the desktop's dropdown offers 15, 24, 25, 30, 48 and 60). A new rate rebuilds the capture session, since the rate is also set up with it; expect a short hitch |
| `notes` | `text=<string>` | Show the notes on the phone's screen next to the front camera, scrolled to the top (an empty `text` takes them down). They go when the stream stops. The phone opens them right away while the app is on screen, otherwise next time it is |
| `notes_style` | `size=<int sp 16-120> speed=<int 0-10> playing=1\|0` | How the notes look and scroll, each optional: `speed` 1 to 10 is 0.12 to 1.2 lines a second and 0 only scrolls by hand, and they scroll while `playing` is 1 |
| `notes_scroll` | `by=<int>` or `to=top` | Move the notes by that many typed lines (negative goes back, and the first step back from the middle of a line goes to its start), or back to the top |

On Dynamic the phone measures its own H.264 sending: every 250 ms, the shortest time a packet waited to go out (a queue that stays, not a keyframe's burst) and how much the socket took. A queue that stays for half a second drops the bitrate to just under what got through; otherwise it climbs, first to just under where the link filled up last time, then more carefully past it. It also keeps the socket's send buffer to about 100 ms of video and skips ahead to the next keyframe when video has waited over a second, so a full link shows up as a queue right away instead of as lag. Over UDP the queue comes from the desktop's reports instead (see [`/v1/video.udp`](#get-v1videoudp-experimental)). `DynamicBitrate.kt` has the rules.

A control body can be up to 64 KiB (for the notes); `/v1/session` and the rest still stop at 4 KiB.

All responses: `{"ok": true}` or `{"ok": false, "error": "..."}`.

The desktop checks each `/v1/state` it reads against the settings it last sent (exposure, white balance mode, focus, `ae_comp`, `fps_target`, `jpeg_quality`, OIS, noise reduction, sharpening, black level lock, and Low light on a phone that takes it) and sends one that doesn't show again. It does that once per value: a phone that fitted a value to the lens keeps reporting the fitted one, and that's left alone.

> **Manual exposure note:** `CONTROL_AE_MODE_OFF` only activates when *both* ISO and shutter are set and the selected camera reports `supportsManualSensor` - `CONTROL_MODE` itself stays `CONTROL_MODE_AUTO` throughout, so autofocus keeps running independently of manual exposure. The desktop app sends both ISO and shutter simultaneously when switching to manual mode.

## Photos: `/v1/still`

A real photo from the phone's camera at full quality, not a frame of the video. A phone that takes these sends `stills: 1` in `/v1/state` (an older one leaves it out and the desktop then offers no photos). Each lens that can take them has a `stills` object in `cameras[]`:

| Field | Meaning |
|---|---|
| `formats` | `jpeg` always, then `heic` (Android 10), `ultrahdr` (Ultra HDR JPEG, Android 14) and `raw` (a DNG) where the lens lists them |
| `width`, `height` | The largest JPEG, which is what a photo is taken at |
| `maxResWidth`, `maxResHeight` | The JPEG from the maximum resolution sensor mode (Android 12), only when it's bigger. `maxResRaw` is true when RAW works in that mode too |
| `extensions` | The maker's own modes the lens has through Camera2 extensions (Android 12): `night`, `hdr`, `bokeh` and `auto` |
| `extensionSizes` | The size each of those modes takes its photos at, like `{"night": {"width": 3840, "height": 2160}}`. Often smaller than `width` and `height` |
| `exposureMaxNs` | The longest exposure the sensor and its frame duration allow, for long exposures. 0 on a lens without manual exposure |
| `bracketing`, `focusBracketing` | Exposure bracketing (needs manual exposure) and focus bracketing (needs manual focus) |
| `noiseReductionHq`, `sharpeningHq`, `hotPixelHq`, `distortionCorrection` | The high quality noise reduction, sharpening and hot pixel modes, and whether distortion correction can be switched |
| `burstMax` | The most pictures one request takes, counting every burst, bracket and focus step |

A lens of a multi-lens camera has no full resolution mode or maker modes (those belong to the camera as a whole) and its RAW needs Android 12. The phone reads all this once per lens when the stream starts, which is why `stills` can be missing from the first state or two.

### Taking one

`POST /v1/still` with `{"action": "take", ...}` and any of these, all as strings or numbers like `/v1/control`'s params:

| Param | Effect |
|---|---|
| `format` | `jpeg`, `heic`, `ultrahdr`, or `none` for RAW only |
| `raw` | `1` for a DNG as well (from the same exposure as the JPEG) |
| `quality` | 1-100 for JPEG, HEIC and Ultra HDR, 95 when left out |
| `max_res` | `1` for the maximum resolution sensor mode (JPEG and RAW only) |
| `extension` | `night`, `hdr`, `bokeh` or `auto`. JPEG only, and the maker's processing sets its own exposure and focus, so it can't go with RAW, bracketing or a manual exposure |
| `exposure_ns`, `iso` | A manual exposure, fitted to `exposureMaxNs` and the lens's ISO range. Either one alone takes the other from the stream (as set by hand, or what auto exposure picked) |
| `bracket_ev` | Exposure steps in EV from that exposure, like `-2,0,2`: up to 9, each between -6 and 6. The exposure time moves first and ISO only where the time runs out |
| `focus_steps`, `focus_from`, `focus_to` | Focus bracketing: 2-30 pictures from `focus_from` to `focus_to` in diopters (the nearest focus to infinity when left out) |
| `count` | A burst: everything above taken this many times |
| `timer_s` | Wait 0-30 seconds before shooting |
| `rotation` | 0, 90, 180 or 270, written into the JPEG and DNG orientation |
| `nr_hq`, `edge_hq`, `hot_pixel_hq` | `1` for the high quality mode, ignored by a lens without it |
| `distortion` | `1` or `0` to switch distortion correction on or off. Left out, the camera does what it does anyway |

Everything else comes from the stream, meaning the lens, zoom and crop, manual exposure, white balance, focus, exposure compensation, OIS and the torch.

The answer is `{"ok": true, "id": "<id>"}`, or `{"ok": false, "error": "...", "code": "..."}` with one of these codes: `unsupported` (the lens can't do what was asked, or can't take photos at all), `bad_option`, `not_ready` (the phone hasn't read the lens yet, or bracketing has no light reading to start from), `busy` (another photo is still being taken, as only one goes at a time) and `camera_off`.

A plain photo goes into the stream's own camera session next to the video and the video doesn't stop for it. Only the first photo makes a hitch of a fraction of a second, while the session is set up again with the photo's output, and the next ones reuse it. When the phone won't take that output next to the stream's, and always for the maker's modes, the photo gets a session of its own and the video pauses for a second or two, plus the exposure. The phone checks with `isSessionConfigurationSupported` where it can (Android 10) and remembers what didn't fit until the stream ends. While a photo is being taken the phone doesn't drop a viewer for sending it no frames.

### Getting the files

`GET /v1/still?id=<id>` says how it's going:

```json
{"id": "61155b2c22c2ab28", "state": "capturing", "shots": 3, "taken": 1, "paused": false,
 "files": [{"name": "IMG_20261026_142501_01.dng", "size": 48711980, "type": "image/x-adobe-dng"}]}
```

`state` goes from `waiting` (the timer, with `countdown_ms` left) to `capturing` and ends at `done` or `failed` (with `error` and `code`: one of the codes above, or `interrupted` when the lens changed, the camera reopened (a new size or codec) or turned off, or the stream stopped, `timeout` and `failed`). A setting that only rebuilds the stream's session (the frame rate, low light, the phone's preview) doesn't end a photo: it carries on, and asks once more for anything the rebuild dropped. Changes the phone makes by itself, like Dynamic's frame rate step or the heat, wait until the photo is done. Every photo ends: it gets 10 s plus 5 s a picture plus its exposures after the timer, and a photo that shot and then hears nothing from the camera for 6 s (plus its longest exposure) has stalled. A stalled photo with any of the best quality modes (`nr_hq`, `edge_hq`, `hot_pixel_hq`) goes again once without them, and photos of that format on that stream leave them out from then on; otherwise it ends with `timeout`. Should the camera thread itself get stuck in the phone's camera service, the phone still reports `timeout` a few seconds after that limit (twice it, with the best quality modes). The best quality modes keep a photo out of the video, which skips that frame. `paused` is true while the video is paused for it, which also covers manual exposures next to the stream that add up to half a second or more, since those frames stay out of the video. `files` lists every file written so far in the order they were taken, and a desktop can start fetching them before the last one is done.

`GET /v1/still/file?id=<id>&n=<index>` sends file `n` (from 0) with its `Content-Length` and `Content-Type`, streamed in pieces since a RAW file is often 50 MB or more. It takes one of the stream slots while it sends, and a download that stops moving for 30 seconds is cut off. Both `GET` requests answer `404` for an `id` or file the phone doesn't have (any more).

`POST /v1/still` with `{"action": "forget", "id": "<id>"}` deletes the files once the desktop has them. It always answers ok, also for an `id` that's already gone. Otherwise the phone holds on to them in its cache for ten minutes (at most the last four requests and 1 GB) and deletes all of them when the stream stops.

The desktop gives up on a photo whose status hasn't moved for 45 s plus its exposures (`timeout`, and it tells the phone to forget it), and ends one as `interrupted` as soon as its own stream to that phone stops or restarts, rather than waiting to lose the phone.

## `GET /v1/video.udp` (experimental)

The same H.264 as `/v1/video.h264`, but the frames come over UDP, so one lost packet on a busy Wi-Fi doesn't hold up everything behind it the way TCP does. The desktop only asks for it on a Wi-Fi route with **UDP on Wi-Fi** checked and the Light format. With Dynamic it also needs `udpDynamic` in the phone's `/v1/ping` (an older phone gets Dynamic over TCP as before). Changing the switch, or entering Dynamic with an older phone, reconnects onto the appropriate transport.

- The desktop opens a UDP socket and asks `GET /v1/video.udp?port=<its port>`. Over the phone's loopback (USB) the answer is `409`, since there's nothing to gain there.
- The reply is `application/x-telescope-udp`, and its first line is JSON: `{"key": <32 random bytes, base64>, "port": <the phone's UDP port>}`. The request then stays open as the session: the phone writes `\n` every second, and when either side closes it, the UDP stream ends too. The key comes over the pinned HTTPS connection, so it's as private as the token.
- Every datagram is an 8-byte big-endian counter followed by AES-256-GCM of the plaintext, with the nonce being a direction byte (`1` phone to desktop, `2` desktop to phone), three zero bytes and the counter. Each side drops anything that doesn't decrypt or that it has already seen (a 4096-datagram window).
- Phone to desktop, each plaintext is a piece of one frame: frame number (u32), piece index (u16), piece count (u16), flags (u8, `1` for a keyframe), then up to 1100 bytes of Annex-B. Keyframes start with the encoder's SPS/PPS, so the desktop can start at any of them, and every frame ends with an access unit delimiter.
- Desktop to phone, the first byte says what it is: `1` NACK (frame number u32, then the missing piece indexes as u16, or none for the whole frame), `2` keyframe please, `3` hello, `4` report. The desktop sends a hello every half second, which also keeps its firewall open for the phone's datagrams; the phone sends to wherever the last valid one came from. The desktop likewise answers whichever port the phone's datagrams come from, the way back through a NAT in between.
- The report goes out every 100 ms whatever the bitrate, all big-endian: datagrams it took (u32), the highest datagram counter it saw (u32, its low 32 bits), bytes of video it took (u32), its own clock in ms (u32), the newest frame it has whole (u32, all ones before any), how many ms ago that frame became whole (u16) and frames it gave up on (u16). Every count only grows and wraps around, so a lost report costs nothing. A phone without Dynamic over UDP ignores it.
- The phone checks pairing while the session is open and closes it when that token is revoked, the encoder closes, or no authenticated feedback arrives for 5 seconds. Recent feedback counts as an active viewer.
- The phone keeps the last 90 frames for resending and gives at most one keyframe per 300 ms however many are asked for. When its queue to that desktop backs up, it drops what's queued and waits for the next keyframe.
- The desktop asks again for a frame that's missing pieces after 15 ms (60 ms for the newest one, whose pieces may still be arriving), every 50 ms after that, and gives up after 250 ms: it then freezes on the last picture and asks for a keyframe rather than decode on a broken reference. A partial keyframe, including the first one, is repaired too; its timeout starts over while more pieces arrive. A newer keyframe can replace a full backlog.
- On Dynamic the phone goes by the reports (`UdpLink` in `UdpVideo.kt`). Datagrams missing between the counters are the share lost, counted once at least 50 went out (at the lowest bitrate one lost in 8 would otherwise look like 12%), and samples in between keep the last share for up to 2 seconds. Under 3% over the last 2 seconds counts as nothing because the resends cover that much. From 8% it counts at once, like a queue of 60 ms growing to 400 ms at 25%. A frame given up on counts as 10% at least, and half a second without a report counts as a stall, the same as nothing going out over TCP. How long a frame takes to go out, reach the desktop and show up in a report, beyond the quickest trip of the last 10 seconds, is a queue on the way. Frames waiting on the phone are one too. `DynamicBitrate` goes by the longest of those and takes the bytes that reached the desktop as what got through. A cut also aims low enough to drain a queue on the way within about 2 seconds (the phone can't skip ahead past one sitting in a router).
- On Dynamic the phone also spreads each frame's datagrams out at 2.5 times the video's rate or the bitrate Dynamic set (whichever is higher, at least 2 Mbps), instead of sending a keyframe in one burst that a thin router would drop. Once Dynamic has cut for a full link, it paces at 1.1 times the fastest the reports showed video arriving while frames queued on the way (but never under 1.2 times the video), so a big keyframe fits a router that buffers about 100 ms. Its socket's send buffer stays at about 100 ms of video like on TCP.
- When no datagram arrives within 3 seconds (usually a firewall), none of them makes a frame within 10 (a link too thin for even a keyframe), or the phone refuses, the desktop switches to `/v1/video.h264` and doesn't try UDP with that phone again until it restarts.

## `GET /v1/sensors`

The phone's motion and light sensors, for as long as the request stays open. The phone only turns a sensor on while some computer has it open, to save battery. Sensor events arrive on a thread of their own and never touch the camera.

- Ask with `?kinds=orientation,light` (any of `orientation`, `accelerometer`, `gyroscope`, `magnetometer`, `gravity`, `rotation`, `light`). Kinds this phone doesn't have are skipped and leaving `kinds` out asks for all of them. `/v1/state`'s `sensors` says which exist here.
- None of the kinds asked for exists: `409` with `{"error": "This phone has none of those sensors", "code": "sensors_none"}`. A phone from before this endpoint answers `404`.
- The reply is `application/x-ndjson`, one JSON object per line. The first line is `{"version": 1, "sensors": [...], "alive_ms": 15000}` with the kinds actually streamed. After that each line has `t` (the phone's clock in ms) and only the kinds that changed since the line before, about 30 lines a second at most: `{"t": 81234, "orientation": {"roll": -2.4, "pitch": 6.1, "camera": "0"}, "light": 212.0}`. Light comes twice a second at most. When nothing changed for a second the phone sends just `{"t": ...}`, which lets a phone notice a computer that's gone (and lets the computer notice a phone that is).
- `orientation` is only offered while one of the phone's own cameras streams (not an external one, and not while the camera is still starting). It's worked out on the phone for the picture the stream shows. `roll` is how many degrees the horizon is turned clockwise in the picture as it comes off the phone (before the desktop's flip and rotation), from -180 to 180; turning the picture anticlockwise by that much levels it. `pitch` is how far above the horizon the camera looks (negative below). `camera` is the id of the camera that's streaming, since a camera switch changes the answer. It's computed from Android's gravity sensor (the accelerometer, smoothed, on phones without one), the camera's `SENSOR_ORIENTATION` and which way it faces. The stream itself is never turned to match the screen, so the way the phone is held is already all in the gravity reading. `roll` is `null` while the camera points within about 10 degrees of straight up or down (where it means nothing). An external camera gets no `orientation` at all because nobody knows how it's mounted.
- The other kinds are Android's own readings in the phone's axes (x right, y up, z out of the screen, in portrait): `accelerometer` and `gravity` in m/s², `gyroscope` in rad/s, `magnetometer` in µT, each `[x, y, z]`; `rotation` is the game rotation vector (no compass) as a quaternion `[x, y, z, w]`; `light` is lux.
- Each sensor is registered only for the kinds some open request asked for, about 30 times a second for motion and twice a second for light, and Android may batch the events by up to 0.1 s (1 s for light) where the chip can. It's unregistered as soon as the last request that needs it ends.
- The phone closes the stream when the computer's token is removed, when the stream ends, when a write fails, when its writes have been stuck for 5 seconds, or when that computer made no request at all for `alive_ms`. A computer that vanished without closing the connection (a laptop going to sleep) is noticed either way. While it reads, the desktop asks `GET /v1/sensors/alive` (it answers `{"ok": true}`) every third of `alive_ms`. It takes one of the 16 stream slots like video and audio do.

## `GET /v1/hello`

On port 8766, and the one request without auth: it only says which phone this is, so the desktop can tell its phone from any other one before trusting a USB forward or an address.

```json
{ "protocol": 2, "phoneId": "3f9c…", "phoneName": "Pixel 8 Pro", "appVersion": "3.0.0", "build": 236 }
```

`phoneId` is random, made once per install, and is what the desktop stores the phone by. `appVersion` and `build` let the desktop say which app is out of date when the protocols differ.

## `GET /v1/ping`

Served on port 8766 by `SessionServer` - unlike the endpoints above, it exists whether or not a stream is running (while the app's main screen is up, while the camera service is running, while **Wait for my computer** is on, and for 6 s after a stream stops - in any combination). Returns `200` if the token belongs to a paired computer, `401` if not.

```json
{
  "protocol": 2,
  "streaming": false,
  "busy": false,
  "localOnly": true,
  "phoneId": "3f9c…",
  "phoneName": "Pixel 8 Pro",
  "udpDynamic": true
}
```

`udpDynamic` is true on a phone that runs Dynamic over UDP from the desktop's reports (an older phone leaves it out). `streaming` is a live stream, `busy` is a start in flight (camera opening, session configuring), and `localOnly` mirrors the app's **Local only - USB** setting, so the desktop can name that mismatch instead of timing out against an address nothing is listening on. `streamingFor` is the name of another paired computer that stream or start belongs to (see [One computer per stream](#one-computer-per-stream)); it's left out when the stream is the asking computer's, or nobody's. An older phone never sends it.

## `POST /v1/session`

Also on 8766. JSON body `{"action": "start"}` or `{"action": "stop"}`; same auth and the same `{"ok": true}` / `{"ok": false, "error": "..."}` responses as `/v1/control`. This is what makes the desktop's Start button sufficient on its own.

| `action` | effect |
|---|---|
| `start` | Start the camera service, reproducing the camera/resolution/OIS selection last used on the phone. Optional `width` and `height` (both or neither) and `fps` open it at that size and rate instead, so a size that just failed isn't what opens again; the size is fitted to the lens like a `resolution` control. `camera: "off"` starts with the camera off, just the mic, until a `camera_on` control. `{"ok": true}` if a stream is already running. |
| `stop` | Stop the camera service. `{"ok": true}` if nothing was running. |

Refusal reasons, all reported with HTTP `200` and `"ok": false` (the request was fine, the camera wouldn't open): `no_camera_permission`, `busy` (a start is already in flight), `start_refused` (Android declined the foreground-service start), `busy_other` (the stream belongs to another paired computer, named in `computer`: `{"ok": false, "error": "busy_other", "computer": "desk"}`). A `stop` from a computer the stream doesn't belong to is refused with `busy_other` too, and the stream carries on.

### One computer per stream

A stream belongs to the paired computer whose `start` opened it, or, for one started on the phone, to the first computer that sends `start` or opens `/v1/video` or `/v1/video.h264` on it. While it runs, another computer's `start` and `stop` get `busy_other`, its `/v1/ping` carries `streamingFor`, and its video request doesn't switch the codec (it gets the stream as it is). The desktop says "The phone is streaming to <name>" instead of joining in. The owner itself can always reconnect, after a network blip or a restart, since it's matched by its pairing, not by its connection. Ownership ends with the stream: an owner that stops polling lets the idle watchdog stop it as before (60 s), and an owner that's unpaired on the phone no longer counts. `/v1/state`, `/v1/control` and `/v1/audio` aren't checked, so an older desktop that joins a running stream still works as it did.

A start is only accepted while `SessionServer` is bound at all, i.e. the app's main screen is up, the camera service is already running, or **Wait for my computer** is on. With that setting off (the default), this cannot open the camera on a phone that is both backgrounded and idle. With it on, `WaitingService` holds the port from the foreground, and a start from the desktop runs the camera under that service even while the app is in the background. It can still come back `start_refused` if Android declines it.

The desktop polls `/v1/ping` after a start until `streaming` goes true (12s budget), because the service answers as soon as the start is accepted, well before the capture session is configured.

## `POST /v1/unpair`

Also on 8766, body `{}`. Removes the computer whose token made the request, and only that one - a computer can't unpair others. The desktop calls it when you remove a phone, so the phone stops accepting a computer that has forgotten it. Answers `{"ok": true}`, or `401` for an unknown token.

## QR pairing payload

Generated by the desktop (`telescope/pairing.py`), rendered as the QR code, and pushed verbatim (base64-encoded) over `adb` for USB pairing. Both sides speak version `4` only; a mismatch is reported as "update both apps" rather than "invalid code", since desktop and APK ship together.

```json
{
  "version": 4,
  "port": 8765,
  "candidates": [
    { "ip": "192.168.1.42",  "interface": "Wi-Fi",      "kind": "lan" },
    { "ip": "100.90.12.34",  "interface": "tailscale0", "kind": "tailscale" }
  ],
  "nonce": "...",
  "token": "...",
  "computer_id": "8d1e…",
  "computer_name": "desk"
}
```

`kind` is one of `lan`, `tailscale`, `other`, and candidates are ordered best-first. The phone rejects the payload outright if any candidate carries a malformed IPv4 literal or an unrecognised `kind`, if the list is empty, or if the port/nonce/token are unusable. `interface` is the desktop-side adapter name, carried for diagnostics. The copy pushed over adb advertises a single candidate - `127.0.0.1`, kind `other` - reached through the `adb reverse` tunnel.

`computer_id` is made once per desktop install and is what the phone files the token under, so pairing again from the same computer replaces its token instead of adding a second entry. `computer_name` is what the phone's list shows (the desktop's host name unless renamed in **Your phones**).

The phone then `POST`s to `http://<ip>:<port>/pair/<nonce>` with `{"name": ..., "phone_id": ..., "ips": [...], "cert_sha256": ..., "proof": ...}`. `cert_sha256` is the SHA-256 of the phone's TLS certificate, which the desktop pins from then on. `proof` is HMAC-SHA256 keyed with the token over `telescope-pair-v4\n<nonce>\n<phone_id>\n<cert_sha256>`, in lowercase hex: it shows the phone read the current code without sending the token, and ties the fingerprint to it, so someone watching the network can neither reuse the token nor swap in their own certificate. The pairing server takes at most 32 connections at once, 4 per address, and drops any connection still sending after 15 seconds.

The desktop also notes the source address that request arrived from. That address is, by construction, one of the phone's *and* reachable from this machine over whatever path the phone found, so it becomes the phone's first active address.

One code pairs one phone: once a phone has paired from it, a POST from a different `phone_id` gets `409` (the same phone retrying still gets `200`).

## Browser camera

Not the phone: the desktop serves this itself (`telescope/browser_server.py`) on port 8767 (any free port if that's taken) while Browser camera is picked, over HTTPS with a self-signed certificate it keeps next to the config. The QR code is `https://<desktop>:8767/#<token>`. The token is after `#`, so loading the page (`/`, `/app.js`) never sends it.

The camera and mic go over WebRTC (`telescope/browser_rtc.py`, using aiortc). A WebSocket on `/ws?token=<token>&id=<browser id>` carries everything else: the offer and answer, the settings and the page's stats. It's also how the desktop knows the browser is there. A wrong token gets `403`. A second connection from the same browser id takes over and the first is closed with code `4000`. A browser that would need a virtual camera when none is free is closed with `4001`. **New link** closes every browser with `4002`, which tells the page to stop instead of retrying a code that no longer works, and switching Browser camera off closes them with `1001`. The desktop pings every 3 seconds (browsers answer those on their own) and a connection it hears nothing from for 10 seconds is closed.

Page to desktop, all text:

- `{"type": "hello", "device": "Safari on iPhone", "mic_error": ""}` on connect and when the mic changes. A non-empty `mic_error` says why there's no mic, and the Microphone card shows it
- `{"type": "offer", "sdp": "..."}`: a complete offer (every address gathered first, there's no trickle ICE) with one send-only video and one send-only audio transceiver. The page asks for H.264 Constrained Baseline or Baseline with `packetization-mode=1` first (what the device's hardware encoder usually does), then VP8, the two codecs the desktop decodes. It sends a new offer after the connection fails, or stays disconnected for 5 seconds
- `{"type": "stats", "width": 1280, "height": 720, "fps": 30, "limit": ""}` every 2 seconds: what the browser is really sending, and `cpu` or `bandwidth` when it says that's holding it back

Desktop to page:

- `{"type": "config", "width": 1280, "height": 720, "fps": 30, "audio": false, "max_kbps": 4147, "camera": "Telescope #2"}` once on connect and whenever something changes. `camera` (the virtual camera it streams to) is left out while there's none
- `{"type": "answer", "sdp": "..."}` for each offer, or `{"type": "rtc_error", "message": "..."}` when it couldn't answer

The answer carries no STUN or TURN server, so only the two devices' own addresses are tried and nothing outside the local network is contacted. Its video codecs get `x-google-start-bitrate` and `x-google-max-bitrate`: without the first Chrome starts at 300 kbps and takes about half a minute to get sharp. `max_kbps` is width × height × fps × 0.15 bits, between 1500 and 12000 kbps.

The page asks the camera for that size and rate, sets the sender's `maxBitrate`, `maxFramerate` and, for a camera that delivers more than asked, `scaleResolutionDownBy`. The mic's sender only has a track while `audio` is true (the Microphone card is on and streaming). The mic is opened raw, without echo cancellation, noise suppression or automatic gain, since the app on the computer does its own. Messages over 64 KB end the connection and binary ones are ignored.
