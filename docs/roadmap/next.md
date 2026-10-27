# Next release

Planned in October 2026 after 4.3. The first two were left over from 4.3.

- [ ] [Saying to extract the Windows zip first](#saying-to-extract-the-windows-zip-first)
- [ ] [PyAV 19](#pyav-19)
- [ ] [The wait screen with the app closed on Windows 11](#the-wait-screen-with-the-app-closed-on-windows-11)

## Saying to extract the Windows zip first

Opening `TelescopeDesktop.exe` straight from the portable zip fails with "Failed to load Python DLL". Windows 11 only extracts the exe itself into a temp folder (without `lib-<build>`) and the loader gives up before any of Telescope's code runs. That means `warn_running_from_archive()` in `desktop/telescope/platform/windows.py` never gets a chance. Windows does warn about it first but plenty of people click Run anyway.

Since 4.2 it's worse. The splash can't find its Tcl/Tk files either and shows three error boxes of its own before the Python one comes up.

A small launcher exe that checks for its `lib` folder before starting the real one could show the "extract first" message itself, before the splash or Python load anything. Small to medium, mostly packaging in `desktop/telescope.spec` and the release workflow.

## PyAV 19

Needs Python 3.12, which 4.2 brought in. aiortc 1.15 still declares `av<18` which we get around with `--no-deps`. aiortc's main branch already allows `av<20` but there's no release with it yet. Wait for that release, then drop the `--no-deps` workaround if it's no longer needed. PyAV 19 has API changes too. Check browser video and audio and hardware decoding on a real device. The draft PR is #273.

## The wait screen with the app closed on Windows 11

While the app isn't running the Windows 11 camera shows a plain dark picture in the app's background colour. It falls back to the same picture when frames stop for 3 seconds. Now that the camera is Telescope's own DLL it could show the default wait screen there instead and a call that opens the camera before Telescope starts would see the lens and the name rather than black.

The Windows build would render the screen with the same `_default_screen()` the app uses (`desktop/telescope/vcam.py`) and put it in `TelescopeCamera.dll` as a resource. That way the two can't drift apart. The DLL fills the frame with the background colour and scales the lens and name onto it for whatever size the app picked. It leaves out the "Waiting for camera" line, since the camera service has no way to know the user's language and an English line would sit untranslated in every other one. The app still shows the translated line once it runs.

A picture of your own would need the DLL to read a file a normal user can write, from inside a system service. That's for later, as raw pixels with a checked header and never a PNG or GIF parser. Linux can't do any of this (v4l2loopback only shows what a running app writes). Small to medium, mostly `desktop/virtualcamera/media_source.cpp` and the CMake build.
