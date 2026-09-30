# Control API reference

Server is on the phone at port 8080 for `/v1/video`, `/v1/video.h264`, `/v1/audio`, `/v1/state` and `/v1/control` (all only exist while actively streaming); a separate responder on port 8766 serves `/v1/hello`, `/v1/ping`, `/v1/session` and `/v1/unpair`. Every request below except `/v1/hello` requires an `Authorization: Bearer <token>` header carrying the token this computer got when it paired; missing or unknown tokens get `401`.

## `GET /v1/state`

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
      "lensZooms": [3.0]
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

`minFocusDistance`, `aeCompMin`/`aeCompMax`/`aeCompStep` are per-lens, reported by Camera2 (`aeCompStep` is typically `0.167` = 1/6 EV). `wb_r`/`wb_ge`/`wb_go`/`wb_b` are the current RGGB channel gains when `wb_manual` is true, `null` otherwise. `supportedSizes` is the lens's actual list of capture sizes, which the desktop uses to populate its resolution dropdown instead of a fixed list. `stream_width`/`stream_height` are the current lens's live capture size. `codecs` lists what the phone can send (`h264` only with a hardware encoder), `codec` is what it's sending, and `bitrate` is the H.264 target in bits per second (on Dynamic, where it is right now). `dynamic_bitrate` is true when the phone takes `bitrate` `-1`. `codec_error` appears when H.264 failed and the phone went back to MJPEG, and `codec_unsupported` is true when that's because it can't do H.264 at this size or rate (not a crash), which the desktop asks about instead of switching to Heavy. `zoomRatioMax` is how far the lens zooms on the sensor (1 = it can't), `cropZoomMax` how far the crop can go, `freeformCrop` whether the crop can sit off-centre, and `lensZooms` the ratios where a multi-lens camera switches to a longer lens. `camera_fps` is how many frames per second the phone actually made for the stream over the last couple of seconds (under `phone_fps` in dim light), which the desktop compares with what arrives before calling the connection slow. `active_lens` is the physical lens a multi-lens camera is streaming from right now. Fields at their default value are left out, so read them with a default.

## `POST /v1/control`

JSON body `{"action": "<action>", ...params}`.

| `action` | extra params | effect |
|---|---|---|
| `camera` | `id=<id>` | Switch camera |
| `resolution` | `width=<int> height=<int>` | Set the capture resolution to one of the lens's reported supported sizes |
| `auto` | - | Restore auto exposure |
| `iso` | `value=<int>` | Set ISO; switches AE to OFF (once shutter is also set) |
| `shutter` | `value=<long ns>` | Set shutter in nanoseconds; switches AE to OFF (once ISO is also set) |
| `wb_auto` | - | Restore auto white balance |
| `wb_gains` | `r=<float> ge=<float> go=<float> b=<float>` | Set manual white balance via `COLOR_CORRECTION_GAINS` RGGB channel gains |
| `ois` | `value=1\|0` | Toggle OIS |
| `focus_mode` | `value=continuous\|manual` | Switch autofocus / manual focus (also ends point focus) |
| `focus_point` | `x=<0..1> y=<0..1> [size=<0..1>]` | Focus on a point of the stream frame, and meter exposure there while it's automatic. `size` is the region's side as a fraction of the frame's shorter side (default 0.1). Refused on a lens without `supportsFocusPoint`. `/v1/state` then reports `focus_mode: "point"` |
| `focus_distance` | `value=<float diopters>` | Set manual focus distance |
| `zoom` | `ratio=<float> crop=<float> x=<0..1> y=<0..1>` | Zoom on the phone: `ratio` is the centred zoom (a multi-lens camera switches lens on it), `crop` extra zoom inside that, centred on `x`, `y` of the view |
| `ae_comp` | `value=<int steps>` | Set exposure compensation, in the lens's AE-compensation steps (see `aeCompStep`) |
| `nr_mode` | `value=<int 0-4>` | Set noise reduction mode (desktop UI only offers 0/1/2 = Off/Fast/High Quality) |
| `edge_mode` | `value=<int 0-3>` | Set sharpening/edge mode (desktop UI only offers 0/1/2 = Off/Fast/High Quality) |
| `black_level_lock` | `value=1\|0` | Toggle black level lock |
| `torch` | `value=1\|0` | Toggle flash/torch |
| `jpeg_quality` | `value=<int 1-100>` | Set JPEG quality on the phone |
| `bitrate` | `value=<int bits/s>` | Set the H.264 bitrate (clamped to 1-100 Mbps and to what the phone's encoder takes); `0` sizes it from resolution and fps; `-1` is Dynamic (the phone follows the link, see below). A phone without Dynamic treats `-1` as `0` |
| `fps_target` | `value=<int 1-120>` | Set capture FPS on the phone (desktop UI restricts to 5-60). A new rate rebuilds the capture session, since the rate is also set up with it; expect a short hitch |

On Dynamic the phone measures its own H.264 sending: every 250 ms, the shortest time a packet waited to go out (a queue that stays, not a keyframe's burst) and how much the socket took. A queue that stays for half a second drops the bitrate to just under what got through; otherwise it climbs, first to just under where the link filled up last time, then more carefully past it. It also keeps the socket's send buffer to about 100 ms of video and skips ahead to the next keyframe when video has waited over a second, so a full link shows up as a queue right away instead of as lag. `DynamicBitrate.kt` has the rules.

All responses: `{"ok": true}` or `{"ok": false, "error": "..."}`.

> **Manual exposure note:** `CONTROL_AE_MODE_OFF` only activates when *both* ISO and shutter are set and the selected camera reports `supportsManualSensor` - `CONTROL_MODE` itself stays `CONTROL_MODE_AUTO` throughout, so autofocus keeps running independently of manual exposure. The desktop app sends both ISO and shutter simultaneously when switching to manual mode.

## `GET /v1/hello`

On port 8766, and the one request without auth: it only says which phone this is, so the desktop can tell its phone from any other one before trusting a USB forward or an address.

```json
{ "protocol": 2, "phoneId": "3f9c…", "phoneName": "Pixel 8 Pro", "appVersion": "3.0.0", "build": 236 }
```

`phoneId` is random, made once per install, and is what the desktop stores the phone by. `appVersion` and `build` let the desktop say which app is out of date when the protocols differ.

## `GET /v1/ping`

Served on port 8766 by `SessionServer` - unlike the endpoints above, it exists whether or not a stream is running (while the app's main screen is up, or while the camera service is running, or both). Returns `200` if the token belongs to a paired computer, `401` if not.

```json
{
  "protocol": 2,
  "streaming": false,
  "busy": false,
  "localOnly": true,
  "phoneId": "3f9c…",
  "phoneName": "Pixel 8 Pro"
}
```

`streaming` is a live stream, `busy` is a start in flight (camera opening, session configuring), and `localOnly` mirrors the app's **Local only - USB** setting, so the desktop can name that mismatch instead of timing out against an address nothing is listening on.

## `POST /v1/session`

Also on 8766. JSON body `{"action": "start"}` or `{"action": "stop"}`; same auth and the same `{"ok": true}` / `{"ok": false, "error": "..."}` responses as `/v1/control`. This is what makes the desktop's Start button sufficient on its own.

| `action` | effect |
|---|---|
| `start` | Start the camera service, reproducing the camera/resolution/OIS selection last used on the phone. Optional `width` and `height` (both or neither) and `fps` open it at that size and rate instead, so a size that just failed isn't what opens again; the size is fitted to the lens like a `resolution` control. `{"ok": true}` if a stream is already running. |
| `stop` | Stop the camera service. `{"ok": true}` if nothing was running. |

Refusal reasons, all reported with HTTP `200` and `"ok": false` (the request was fine, the camera wouldn't open): `no_camera_permission`, `busy` (a start is already in flight), `start_refused` (Android declined the foreground-service start).

A start is only accepted while `SessionServer` is bound at all, i.e. the app's main screen is up or the camera service is already running - so this cannot open the camera on a phone that is both backgrounded and idle.

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

The phone then `POST`s to `http://<ip>:<port>/pair/<nonce>` with `{"name": ..., "phone_id": ..., "ips": [...], "cert_sha256": ..., "proof": ...}`. `cert_sha256` is the SHA-256 of the phone's TLS certificate, which the desktop pins from then on. `proof` is HMAC-SHA256 keyed with the token over `telescope-pair-v4\n<nonce>\n<phone_id>\n<cert_sha256>`, in lowercase hex: it shows the phone read the current code without sending the token, and ties the fingerprint to it, so someone watching the network can neither reuse the token nor swap in their own certificate.

The desktop also notes the source address that request arrived from. That address is, by construction, one of the phone's *and* reachable from this machine over whatever path the phone found, so it becomes the phone's first active address.

One code pairs one phone: once a phone has paired from it, a POST from a different `phone_id` gets `409` (the same phone retrying still gets `200`).
