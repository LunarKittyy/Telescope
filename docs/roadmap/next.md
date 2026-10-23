# Next release

Planned in October 2026, after 4.1. The first two were left over from 4.0 and didn't make it into 4.1 either.

- [ ] [720p when the phone gets critically hot](#720p-when-the-phone-gets-critically-hot)
- [ ] [Saying to extract the Windows zip first](#saying-to-extract-the-windows-zip-first)
- [ ] [Dependency updates](#dependency-updates)

## 720p when the phone gets critically hot

4.0 caps the frame rate at 24 fps when Android's thermal status hits SEVERE and at 15 fps when it's CRITICAL. The plan was to drop to 720p at CRITICAL too but changing the size mid-stream reaches further than the frame rate does. The desktop's resolution picker, saved sizes and the encoder all have to follow it and the desktop should also show that heat was the reason.

Medium. Needs a real phone that gets hot enough, since the emulator never reaches CRITICAL.

## Saying to extract the Windows zip first

Opening `TelescopeDesktop.exe` straight from the portable zip fails with "Failed to load Python DLL". Windows 11 only extracts the exe itself into a temp folder (without `lib-<build>`) and the loader gives up before any of Telescope's code runs. That means `warn_running_from_archive()` in `desktop/telescope/platform/windows.py` never gets a chance. Windows does warn about it first but plenty of people click Run anyway.

A small launcher exe that checks for its `lib` folder before starting the real one could show the "extract first" message itself. Small to medium, mostly packaging in `desktop/telescope.spec` and the release workflow.

## Dependency updates

Most of what's out of date is held back by two old baselines (Python 3.11 on the desktop and SDK 34 on Android) rather than by the libraries themselves. The plan is to raise both on purpose and land it as separate PRs (that way a regression points at one change).

- [ ] **Routine desktop bumps.** Qt 6.11.2, PyQt6 SIP, truststore, zeroconf, certifi and the small ones (iniconfig, MarkupSafe, packaging, pluggy, Pygments, setuptools, pycparser). PyInstaller 6.22 and its hooks go in together. The certifi bump needs the vision pack hashes regenerated. Check a packaged Windows build, phone discovery and HTTPS downloads.
- [ ] **Android compileSdk 37 and libraries.** Core KTX 1.19 refuses to build below 37 and Material 1.14 pulls in a Core that needs 35. 37 is the one that unblocks everything. It only changes what the app compiles against, not how it behaves on phones. Same PR: AppCompat 1.8, ConstraintLayout 2.2, ZXing 3.5.4, Kotlin serialization 1.12 and JUnit 6 with Jupiter and the launcher together. Serialization changed its exception handling which makes it worth checking pairing and malformed messages. Also Gradle 9.8.2 and the serialization plugin 2.4.21.
- [ ] **Android targetSdk.** Its own PR since this one does change behaviour on newer Android (edge-to-edge, foreground service rules and so on). Needs a real phone.
- [ ] **Python 3.12 minimum.** Unblocks NumPy 2.5 and pyee 15. 3.12 rather than 3.13 to keep Ubuntu 24.04 and Mint 22 working without the deadsnakes PPA. The packaged builds bundle their own Python and CI can build them on the newest version every wheel supports. Update `start.sh`, `start.bat`, the README, `docs/setup.md` and the workflows together.
- [ ] **PyAV 19.** Needs Python 3.12 first. aiortc 1.15 still declares `av<18` which we get around with `--no-deps`. aiortc's main branch already allows `av<20` but there's no release with it yet. Wait for that release, then drop the `--no-deps` workaround if it's no longer needed. PyAV 19 has API changes too. Check browser video and audio and hardware decoding on a real device.
