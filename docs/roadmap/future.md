# Some day

Ideas worth keeping that nobody is about to work on. Move one to [next.md](next.md) once it's planned.

## Scenes and a whiteboard

Presets already save Auto framing and Background, so a preset can be a scene: one that frames you while you talk, another with the framing off and the zoom parked on a whiteboard or a desk, each on its own key. A teacher, someone running a workshop or anyone drawing on paper could switch between them mid-call.

The bigger part is the board itself. Telescope would find the board's four corners, straighten it out when the phone looks at it from an angle, even out shadows and glare and make the writing stand out. None of that exists yet, and it's plain OpenCV rather than MediaPipe. Tuning it so it works in real rooms on real boards is most of the work. A Whiteboard source or card could follow the board the way Auto framing follows you.

Big, mostly the corner finding and the clean-up. Worth seeing first whether people use presets as scenes.

## Mic noise reduction

The phone switches on its own noise suppressor and gain control when it has them, but those vary a lot between phones and some have neither. Running RNNoise on the desktop would clean up typing, fans and room hiss the same way on every phone (and for Browser camera's mic). It needs a Python binding or a small native library, and the Microphone card would get a switch for it. **Auto level** is already in, so this is the part that's left.

It would run in the mic's writer loop in `desktop/telescope/audio.py`, which has to keep up in real time (10 ms chunks).

## A Windows 11 camera every app can see

On Windows the virtual camera is UnityCapture, a DirectShow filter. Apps that only look for cameras through Media Foundation never see it. Windows 11 added `MFCreateVirtualCamera`, which registers a camera with the system so every app sees it. That takes a small native DLL that Windows loads, with the app handing it frames through shared memory. A nice side effect is that the camera only opens that shared memory while some app is using it, which would give Automatic streaming a reliable way to tell when the camera is in use.

Big. It means a native DLL next to the Python app, registering it with admin rights, keeping UnityCapture for Windows 10, and a lot of testing across apps.

## Dynamic over UDP

UDP on Wi-Fi and Dynamic can't be on together right now, so it's one or the other. Dynamic decides from how long frames wait in the phone's send queue, which only shows a problem once the queue is already backing up. Over UDP the desktop asks for every lost piece again, and how often it has to ask is an earlier and cleaner sign the link is getting too thin. Dynamic could step the bitrate down on that and keep UDP's lower lag spikes at the same time.

Medium to big. It needs the resend rate reported back to the phone, the step-down tuned on a real thin link, and `scripts/phone_check.py` would be the place to measure it.

## Plugin updates

Updating a plugin right now means removing it with **Remove** and installing the new `.zip`. Installing the new version should just replace the old one instead and keep its settings. The new version still starts out switched off like any other new plugin, and switching it on is always a step you do yourself.

`plugin.json` could also get a `repo` link that the Plugins dialog shows next to the author. It's only a shorter way to look at the source and download a new version. Nothing gets pulled in the background. For plugins that publish their builds as GitHub Releases there could be an optional update check on top of that. It would only tell you a new version is out so you know to go get it.

If someone really wants updates installed and switched on without asking, that could be a switch in Advanced. It should be a big red one surrounded by warnings, since it lets whoever controls the plugin's repo run new code on your computer every time they publish a release.

Medium. Most of it is in the Plugins dialog and the install code. The update check has to stay quiet when GitHub is unreachable or rate limiting it.
