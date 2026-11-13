<!-- covers: rust/crates/audio/src rust/crates/effects/src/on_air* rust/crates/outputs/src/wait_screen* android/app/src/main/kotlin/com/telescope/AudioStream.kt android/app/src/main/kotlin/com/telescope/AudioStreamer.kt; checked: 1cbb8644b -->
# Sound, sign and monitoring

The microphone, the On Air sign and keeping an eye on the stream.

## Microphone

The phone's mic can be a microphone on your computer while you stream.

- **Linux:** Telescope creates **Telescope Microphone** through PulseAudio or PipeWire. Most desktops need nothing extra
- **Windows:** needs [VB-Audio Virtual Cable](https://vb-audio.com/Cable/) (free). The card's **Install VB-Cable** downloads and opens VB-Audio's setup for you (Windows asks for admin and VB-Audio asks for a restart). Afterwards pick **CABLE Output** as the microphone in other apps and switch the mic off and on again in Telescope
- **Gain** goes from -24 to +12 dB (**Max gain** under Audio in Settings raises the top). The level meter turns yellow while the limiter is holding loud moments down and red when the sound clips. The limiter is on by default
- **Auto level** (off by default) evens out quiet and loud speech and stops adjusting while nobody talks
- **Noise reduction** (off by default) takes typing, fans and room hiss out of the mic with RNNoise running on your computer. It cleans up the same way for every phone and for a browser's mic, which helps most with phones whose own noise suppression is weak or missing. It adds about 20 ms of delay and switches in or out at the next pause in speech. With Discord, Zoom or a similar app turn off the app's own noise suppression while this is on (both at once can make voices sound thin). The level meter shows the mic as it arrives, before noise reduction
- **Mute** sends silence without disconnecting. It's in the card and the tray menu and resets when Telescope restarts
- **Mic only:** the camera button under the preview turns the phone's camera off while the mic stays on. Apps see the wait screen. Press it before starting and Start becomes **Start mic only**
- The tray icon shows what's live: a red dot for the camera and a mic for the mic (crossed out when muted)

## On Air sign

Optional. It lets the people you live with know you're on a call without you having to tell them. Switch it on under **On screen** in the Plugins tab of Settings and it gets an **On Air** tab of its own.

- **On air when** decides what counts. By default it's an app using the virtual camera, even while it only sees the wait screen. It can also count Telescope's mic being live (streaming and not muted), the mic alone or any stream at all
- It waits a second before going on air and six before going off again, because apps like to close and reopen the camera when you change their settings
- **Sign on another device** shows a big On air or Off air page in any browser on your network. Scan the code with an old tablet or phone and tap the page to make it full screen. The screen stays on where the browser allows that. The link stays the same after Telescope restarts until you click **New link** and the page only answers to that link. The browser warns about the certificate the first time, the same as with [Browser camera](sources.md#browser-camera)
- **Webhook** sends a request every time you go on or off air, for Home Assistant or a smart plug or anything else that takes one. POST sends what's in **Going on air** or **Going off air** as the body (`{"on_air": true}` when left empty) and GET adds it to the address instead. **Test on** and **Test off** send one right away
- If the webhook stops answering you get one quiet banner about it and no more of them until it has worked again. Meanwhile Telescope tries the latest state again after 5 seconds, 15 seconds and a minute, then every 5 minutes, which puts the light right after a Wi-Fi blip or a hub restart. It also sends the current state when it starts. Quitting while on air sends the off request first
- While you're on air an **On air** button shows next to the settings button and takes you to the tab

## Monitoring

- FPS and throughput sit in the footer. When the stream can't keep up a note suggests what to try, with a button for it
- A dropped stream reconnects over whichever route works (pull the cable and it carries on over Wi-Fi). After 10 seconds apps get the wait screen instead of a frozen frame and after 30 a banner says the phone can't be reached
- Phone battery and temperature have alerts that can notify you, stop the stream or both. A stop explains why and offers **Start anyway**. A phone that runs hot also lowers its own frame rate first, then its picture size if the frame rate can't go lower, and pauses the camera when it gets very hot until it cools down (Android 10 and newer)
- **Stats** is an optional card (switch it on under **Streaming and devices** in the Plugins tab) with the numbers of the stream: frames in and out, repeated frames, dropped frames with the reason for each, the link and the bitrate against what was asked for. **Nerd** adds how long each step takes, where the frame steps run and why they moved, what each plugin costs and the app's CPU and memory. The delay from the phone's camera and the phone's own numbers need the phone app and show as not available yet
- **Open log**, **Copy diagnostics** and **Report a bug** are under Updates & About in Settings. Report a bug copies the diagnostics and opens the bug form on GitHub for you to paste them in. Addresses, tokens and your home folder are stripped out
