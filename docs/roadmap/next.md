# Next release

Planned in October 2026 after 4.2, starting with a 4.2.1 for the Windows zip. The first two were left over from 4.2.

- [ ] [Saying to extract the Windows zip first](#saying-to-extract-the-windows-zip-first)
- [ ] [PyAV 19](#pyav-19)
- [ ] [A shorter release checklist](#a-shorter-release-checklist)

## Saying to extract the Windows zip first

Opening `TelescopeDesktop.exe` straight from the portable zip fails with "Failed to load Python DLL". Windows 11 only extracts the exe itself into a temp folder (without `lib-<build>`) and the loader gives up before any of Telescope's code runs. That means `warn_running_from_archive()` in `desktop/telescope/platform/windows.py` never gets a chance. Windows does warn about it first but plenty of people click Run anyway.

Since 4.2 it's worse. The splash can't find its Tcl/Tk files either and shows three error boxes of its own before the Python one comes up.

A small launcher exe that checks for its `lib` folder before starting the real one could show the "extract first" message itself, before the splash or Python load anything. Small to medium, mostly packaging in `desktop/telescope.spec` and the release workflow.

## PyAV 19

Needs Python 3.12, which 4.2 brought in. aiortc 1.15 still declares `av<18` which we get around with `--no-deps`. aiortc's main branch already allows `av<20` but there's no release with it yet. Wait for that release, then drop the `--no-deps` workaround if it's no longer needed. PyAV 19 has API changes too. Check browser video and audio and hardware decoding on a real device. The draft PR is #273.

## A shorter release checklist

`docs/release-checklist.md` grew to about 120 items and running all of it by hand every release isn't realistic anymore. The goal is to test just as much while only asking a person for what needs a real phone or real ears and eyes.

- [x] Drop the lines CI already proves (tests, smoke checks, the unzipped copy move, the driver install) and add CI checks for the rest of that kind. That's the APK signed with the same key as the last release, both bundles carrying the notices and the APK, About showing `VERSION`.
- [x] Unit tests for the items that are really logic (Settings search, the Start failure banners, alerts firing once, Start and Stop automation timing, the heat note, the phone's Status card, the remote start toast, Show all and Remove all, the updater's no-downgrade and resume paths).
- [x] One script in `automation/release-testing` that runs the emulator and Linux pass, and one for the Windows VM pass, each printing pass or fail per item (`emulator_pass.py` and `windows_pass.py`).
- [x] A script that lists which checklist sections are due since the last tag, from the files that changed. It has to judge widely what a change can reach (a bug in shared code can break a section whose files didn't change), and when unsure it lists the section.
- [x] What stays manual is a short pass on a real phone (camera controls and lenses, latency and a long run, Wi-Fi to USB handover, the mic on both platforms, OBS and Zoom, the vision effects) plus whatever the script says is due.
