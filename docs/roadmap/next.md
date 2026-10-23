# Next release

Planned in October 2026, after 4.1. Both of these were left over from 4.0 and didn't make it into 4.1 either.

- [ ] [720p when the phone gets critically hot](#720p-when-the-phone-gets-critically-hot)
- [ ] [Saying to extract the Windows zip first](#saying-to-extract-the-windows-zip-first)

## 720p when the phone gets critically hot

4.0 caps the frame rate at 24 fps when Android's thermal status hits SEVERE and at 15 fps when it's CRITICAL. The plan was to drop to 720p at CRITICAL too but changing the size mid-stream reaches further than the frame rate does. The desktop's resolution picker, saved sizes and the encoder all have to follow it and the desktop should also show that heat was the reason.

Medium. Needs a real phone that gets hot enough, since the emulator never reaches CRITICAL.

## Saying to extract the Windows zip first

Opening `TelescopeDesktop.exe` straight from the portable zip fails with "Failed to load Python DLL". Windows 11 only extracts the exe itself into a temp folder (without `lib-<build>`) and the loader gives up before any of Telescope's code runs. That means `warn_running_from_archive()` in `desktop/telescope/platform/windows.py` never gets a chance. Windows does warn about it first but plenty of people click Run anyway.

A small launcher exe that checks for its `lib` folder before starting the real one could show the "extract first" message itself. Small to medium, mostly packaging in `desktop/telescope.spec` and the release workflow.
