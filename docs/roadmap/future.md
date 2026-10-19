# Some day

Ideas worth keeping that nobody is about to work on. Move one to [next.md](next.md) once it's planned.

## AI framing

The camera follows you and keeps you framed, using MediaPipe to find people in the picture. Hand detection on top of that opens up more. Hold your hand out to the side to show something and the framing widens to fit you and the hand together, then goes back to the usual portrait framing once you put it down. There's room for more tricks like that once the detection is in place.

It should move the same zoom and pan the Transforms card does instead of adding a second crop on top, and keep working with the Transforms card switched off. `desktop/ARCHITECTURE.md` has the plan for that (under "When plugins need the same thing"): zoom, pan and rotation become a view the window owns, and framing drives it through a host method. Since the zoom happens on the phone's sensor first, a framing that moves around a lot also has to be careful about the phone hopping between lenses.

## Effects

Background blur and replacing the background with a picture, to start with. Both need a person mask on every frame, which MediaPipe can provide too. Per `desktop/ARCHITECTURE.md` the shared part (a GPU context, a smaller copy of the frame, the mask or face detection run once a frame) becomes a host service the effect plugins call, so two effects don't each run their own model. That same service could feed AI framing.

Both effects run on every frame on the desktop and have to stay cheap enough for 1080p at 30 fps on an ordinary laptop, and for several streams at once.

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
