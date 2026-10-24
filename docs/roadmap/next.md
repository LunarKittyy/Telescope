# Next release

Planned in October 2026, after 4.1. The first two were left over from 4.0 and didn't make it into 4.1 either.

- [x] [720p when the phone gets critically hot](#720p-when-the-phone-gets-critically-hot)
- [ ] [Saying to extract the Windows zip first](#saying-to-extract-the-windows-zip-first)
- [x] [Live notification for streaming status](#live-notification-for-streaming-status)
- [x] [Phone app in the 4.0 style](#phone-app-in-the-40-style)
- [ ] [Dependency updates](#dependency-updates)

## 720p when the phone gets critically hot

At Android's critical thermal status the phone now also drops to 720p (the largest size of the same shape with a short side of 720 or less), on top of 15 fps. The state keeps saying the size the desktop asked for in `stream_width` and gives the smaller one in `heat_width`, so the desktop's resolution pick and saved size stay as they were. The desktop shows "The phone is very hot" while it lasts, checking every 15 s so it notices heat that comes mid-stream and goes once the phone cools. It goes back to the asked size after the same 30 s cool-down the frame rate has.

Tried on a real phone with `adb shell cmd thermalservice override-status 4`. A stream that starts while the phone is already critical opens the virtual camera at 720p and keeps that size until the next start.

## Saying to extract the Windows zip first

Opening `TelescopeDesktop.exe` straight from the portable zip fails with "Failed to load Python DLL". Windows 11 only extracts the exe itself into a temp folder (without `lib-<build>`) and the loader gives up before any of Telescope's code runs. That means `warn_running_from_archive()` in `desktop/telescope/platform/windows.py` never gets a chance. Windows does warn about it first but plenty of people click Run anyway.

A small launcher exe that checks for its `lib` folder before starting the real one could show the "extract first" message itself. Small to medium, mostly packaging in `desktop/telescope.spec` and the release workflow.

## Live notification for streaming status

On Android 16 and newer the streaming notification is a Live Update. It shows on the lock screen and as a chip in the status bar that says Streaming. The notification says which computer has the stream and whether it's over USB or Wi-Fi, and has a Stop streaming button. Wait for my computer stays a plain notification and only becomes a Live Update once a stream is running.

It's built as a progress notification with one full bar, since on vivo (and maybe other brands) that's what makes a tap on the chip open a card with Stop streaming instead of the app. The icon is purple because vivo draws it in the chip as it is, while stock Android only uses its shape.

## Phone app in the 4.0 style

The phone app now looks like the desktop. Its cards have their own colour and the desktop's tinted icon square, and its icons are the desktop's line icons. Switches and buttons are drawn the same way as the desktop's, and the paired computers sit in inset rows. In the preview the lenses are tiles like the desktop's: the focal length large, the rest small under it. The header's preview button now says Preview, since no icon read as "the camera's view" on its own. Nothing about how the app works changed.

Checked at the largest font size on a 360 dp wide screen.

## Dependency updates

Most of what's out of date is held back by two old baselines (Python 3.11 on the desktop and SDK 34 on Android) rather than by the libraries themselves. The plan is to raise both on purpose and land it as separate PRs (that way a regression points at one change).

- [x] **Routine desktop bumps.** Qt 6.11.2, PyQt6 SIP, truststore, zeroconf, certifi and the small ones (iniconfig, MarkupSafe, packaging, pluggy, Pygments, setuptools, pycparser). PyInstaller 6.22 and its hooks go in together. The certifi bump needs the vision pack hashes regenerated. Check a packaged Windows build, phone discovery and HTTPS downloads.
- [x] **Android compileSdk 37 and libraries.** Core KTX 1.19 refuses to build below 37 and Material 1.14 pulls in a Core that needs 35. 37 is the one that unblocks everything. It only changes what the app compiles against, not how it behaves on phones. Same PR: AppCompat 1.8, ConstraintLayout 2.2, ZXing 3.5.4, Kotlin serialization 1.12 and JUnit 6 with Jupiter and the launcher together. Serialization changed its exception handling which makes it worth checking pairing and malformed messages. Also Gradle 9.8.2 and the serialization plugin 2.4.21.
- [x] **Android targetSdk.** Its own PR since this one does change behaviour on newer Android (edge-to-edge, foreground service rules and so on). Needs a real phone.
- [x] **Python 3.12 minimum.** Unblocks NumPy 2.5 and pyee 15. 3.12 rather than 3.13 to keep Ubuntu 24.04 and Mint 22 working without the deadsnakes PPA. The packaged builds bundle their own Python and CI can build them on the newest version every wheel supports. Update `start.sh`, `start.bat`, the README, `docs/setup.md` and the workflows together.
- [ ] **PyAV 19.** Needs Python 3.12 first. aiortc 1.15 still declares `av<18` which we get around with `--no-deps`. aiortc's main branch already allows `av<20` but there's no release with it yet. Wait for that release, then drop the `--no-deps` workaround if it's no longer needed. PyAV 19 has API changes too. Check browser video and audio and hardware decoding on a real device.
