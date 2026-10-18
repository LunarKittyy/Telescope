# Next release

Planned in October 2026, after 3.5. The order is only a rough suggestion. The first three are small, the middle ones need testing on real phones and the last two are bigger.

- [x] [Mic clock drift without clicks](#mic-clock-drift-without-clicks)
- [x] [Wi-Fi latency lock on the phone](#wi-fi-latency-lock-on-the-phone)
- [x] [Phone mic that reopens after an error](#phone-mic-that-reopens-after-an-error)
- [x] [H.264 High profile](#h264-high-profile)
- [x] [Stepping down on Android's thermal status](#stepping-down-on-androids-thermal-status)
- [x] [Frame rate changes without restarting the camera](#frame-rate-changes-without-restarting-the-camera)
- [x] [Moving to USB without a gap](#moving-to-usb-without-a-gap)
- [x] [One shared example of /v1/state for both apps](#one-shared-example-of-v1state-for-both-apps)

Already merged:

- [x] Credit to LunarKittyy next to the version in Advanced
- [x] Auto level for the mic, off by default (noise reduction with RNNoise would need a native library in both builds, so it isn't in)

## Mic clock drift without clicks

The phone's clock and the sound card's never run at exactly the same speed. Right now `JitterBuffer` in `desktop/telescope/audio.py` lets the buffer grow until it hits 150 ms and then cuts it back to 60 ms in one go. When it runs empty the sound stops dead. Both of those can click.

A smoother way is averaging how full the buffer is over about half a second and playing back up to 0.2% faster or slower to hold it on target (with cubic interpolation, since straight-line interpolation dulls "s" and "t" sounds). Gaps should fade out and in over a couple of milliseconds instead of cutting. With that in place the target could also go down from the fixed 60 ms, to something like 20 ms over USB and 40 ms over Wi-Fi, growing when the measured jitter is higher.

Small. It's numpy in the writer loop plus tests next to the existing ones in `test_audio.py`.

## Wi-Fi latency lock on the phone

Phones put Wi-Fi into power saving when the screen is off, which shows up as short latency spikes. Holding a `WifiManager` lock in `WIFI_MODE_FULL_LOW_LATENCY` mode (`WIFI_MODE_FULL_HIGH_PERF` before Android 10) while a stream runs over Wi-Fi stops that. `CameraStreamService` only takes a wake lock for the CPU at the moment.

About 20 lines. Worth comparing latency with the screen off before and after.

Android only honours the low-latency lock with the screen on and Telescope in front, so the phone holds a high-performance lock next to it, which is what covers the screen being off up to Android 13. From Android 14 a high-performance lock turns into a low-latency one, so with the screen off it won't do anything there. To make the most of it the app now keeps the screen on while it's open, and the power button still turns it off.

## Phone mic that reopens after an error

When `AudioRecord.read()` in `AudioStreamer.kt` returns an error (the phone's audio server restarting, for example), the recording thread ends and nothing starts it again, so the mic stays quiet until the stream restarts. It should reopen the recording when that happens, and after half a second with no audio at all. It's still worth checking what the desktop's Microphone card shows in that state.

Small.

## H.264 High profile

Every attempt in `H264Encoder.ATTEMPTS` asks for Baseline first. Asking for High with `KEY_MAX_B_FRAMES` set to 0 keeps frames in order (no extra delay) but gets the better compression tools Baseline doesn't have. If the encoder refuses it can fall back to Constrained Baseline and then to whatever the encoder picks itself. At the same bitrate High usually looks noticeably sharper, which helps Light most on slow Wi-Fi. PyAV decodes it without changes.

Needs trying on a few phones, since some encoders are worse at High than at Baseline. `docs/protocol.md` and `docs/architecture.md` say Baseline and would need updating.

## Stepping down on Android's thermal status

Monitoring compares the battery temperature with the user's thresholds and then notifies or stops the stream. Android 10 and newer also report their own thermal status through `PowerManager.addThermalStatusListener`. The phone could cap the frame rate at 24 fps when that status hits SEVERE and at 15 fps and 720p when it's CRITICAL, using the step-down Dynamic frame rate already has. A hot phone would then keep streaming instead of heating up until an alert stops it.

Medium. The desktop should show when heat was the reason the frame rate dropped, the same way it does for Dynamic.

The 720p step at critical isn't in yet. Changing the size mid-stream reaches a lot further than the frame rate does (the desktop's resolution picker, saved sizes, the encoder) and only the frame rate part has been tried on the emulator.

## Frame rate changes without restarting the camera

Each step Dynamic frame rate takes restarts the phone's camera for a moment. With a GL stage between them the camera could keep running while frames get dropped before the encoder, but Telescope sends the camera straight into the encoder. `KEY_MAX_FPS_TO_ENCODER` (Android 10+) might do it without adding one, though nobody has checked yet whether it can change without setting the encoder up again (which would still be cheaper than restarting the camera).

Needs a look at the APIs first. Lower priority than the rest.

It went in for Android 10 and newer with a safety net. Three seconds after a step the phone counts what the encoder really put out, and an encoder that ignored the cap (the emulator's does) goes back to restarting the camera for the rest of the run.

## Moving to USB without a gap

Plugging in a cable mid-stream over Wi-Fi shows **Switch to USB**, which reconnects the stream and leaves a short gap. Instead the USB stream could open next to the Wi-Fi one and the worker would move over once USB has been healthy for a couple of seconds. Only then would Wi-Fi close. A route that just failed should sit out for about 10 seconds so it can't flap back and forth. With no gap the switch could happen automatically on Automatic routing instead of waiting for a click.

Medium, mostly in `StreamWorker` and the Connection plugin.

## One shared example of /v1/state for both apps

`/v1/state` is described twice, once in `Protocol.kt` on the phone and once in `models.py` on the desktop, and nothing checks that they still agree. One `state.json` example in the repo that both test suites read would catch that. A Kotlin test builds a known state and compares it with the file and a Python test parses the same file.

Small. It's mostly picking where the file lives so both test suites can read it.
