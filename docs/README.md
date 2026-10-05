# Telescope docs

New to Telescope? Start with the [Quick Start](../README.md#quick-start). It's all most people need.

## Getting help

- Questions go in [Q&A](https://github.com/LunarKittyy/Telescope/discussions/categories/q-a), and ideas in [Ideas](https://github.com/LunarKittyy/Telescope/discussions/categories/ideas)
- Bugs and crashes go in an [issue](https://github.com/LunarKittyy/Telescope/issues/new?template=bug.yml), with your **Copy diagnostics** report

## Using Telescope

- [Features](features.md): everything the apps can do, from camera controls and zoom to the microphone, Browser camera, automatic streaming and updates
- [Troubleshooting](troubleshooting.md): common problems, what usually causes them, and the fix
- [Manual setup](setup.md): setting up the virtual camera by hand on Linux, loading it at boot, and OBS as a Flatpak
- [Device compatibility](device-compatibility.md): phones that have been tested, and what worked on each

## How it works

- [Architecture](architecture.md): how the phone and desktop apps fit together, the repository layout, and implementation notes for contributors
- [Control API](protocol.md): the phone's HTTP endpoints, what they return, the pairing QR code's contents, and the Browser camera's WebSocket messages
- [desktop/MODULES.md](../desktop/MODULES.md): the desktop app, module by module

## Building and releasing

- [Building and CI](building.md): building the Android app, testing a change with the dev app and dev desktop, versions, and what each GitHub Actions workflow does
- [Release checklist](release-checklist.md): what to test by hand on real devices before tagging a release
