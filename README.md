# Telescope

Stream your Android phone's camera - including telephoto and wide-angle lenses - to a virtual webcam on Linux or Windows. Camera controls (ISO, shutter, white balance, lens selection) are exposed over a local HTTP API so the desktop app can drive them live.

- Any of the phone's lenses, with manual exposure, focus and white balance from the desktop
- Zoom and pan on the phone's sensor, presets, and a light stream format that keeps up even over mobile data
- The phone's microphone as a microphone on the computer
- Streams by itself when a call or OBS opens the camera, and shows apps a wait screen while it isn't streaming
- USB or Wi-Fi, encrypted, and both apps update themselves

---

## Quick Start

You'll need an Android phone and a PC running Linux or Windows.

### 1. 🖥️ Get the desktop app

**🪟 Windows**

Download `Telescope-windows.zip` from the [releases page](../../releases), extract it, and run `TelescopeDesktop.exe` (keep it in its folder: the `lib-...` folder next to it is part of the app).

**🐧 Linux**

Download `Telescope-linux.tar.gz` from the [releases page](../../releases), extract it, and run `./start.sh`. You'll also need a couple of things from your package manager:

- **`v4l2loopback`** - what the virtual camera runs on. Telescope switches it on when you stream, but can't install it.
  - Debian/Ubuntu: `sudo apt install v4l2loopback-dkms`
  - Fedora/Nobara: `sudo dnf install v4l2loopback`

  <details>
  <summary>💡 Fedora says it can't find that package?</summary>

  You need [RPM Fusion](https://rpmfusion.org/) enabled first (most Fedora installs don't have it by default - Nobara already does):
  ```bash
  sudo dnf install https://mirrors.rpmfusion.org/free/fedora/rpmfusion-free-release-$(rpm -E %fedora).noarch.rpm
  ```
  Then run the `dnf install v4l2loopback` command above again.

  </details>

- 💡 **`adb`** *(optional)* - only for pairing or installing the phone app over USB. Wi-Fi works without it.
  - Debian/Ubuntu: `sudo apt install adb`
  - Fedora/Nobara: `sudo dnf install android-tools`
  - Arch: `sudo pacman -S android-tools`

### 2. ✅ Follow the checklist

On first launch, the middle of the window is a checklist:

1. **Virtual camera** - on Windows, click **Install driver**. On Linux it checks for the package above and names it if it's missing.
2. **Phone app** - scan the code with your phone's camera to download `Telescope.apk`, then open it to install. (Your phone asks to allow "install from this source" the first time.)
3. **Add your phone** - open Telescope on the phone and click **Add phone**. Then tap **Scan pairing code** on the phone, or plug it in over USB and allow USB debugging when the phone asks.
4. **Start streaming** - click **Start Streaming** in the top right corner. The phone's camera starts by itself.

After the first stream, the checklist is replaced by the video.

### 3. ▶️ Use it as a webcam

In OBS (or anywhere else), pick **Phone Camera** (Linux) or **Telescope** (Windows) as your webcam. Installed the Windows driver with an older Telescope? It's still called **Unity Video Capture** until you click **Rename** in Advanced.

Telescope uses USB whenever the phone is plugged in and answering, and Wi-Fi otherwise. The Connection panel shows which one it's using, and if a cable is plugged in but not used, it says why. **Connect via** forces one or the other.

> [!NOTE]
> Use only on a trusted network, or enable **Local only - USB** in the Android app. See [Privacy](docs/features.md#privacy) for the full security model.

That's all most people need. The full feature list, troubleshooting and everything else is in [the docs](docs/README.md).

---

## Why

Most Android camera streaming solutions either lock you to a specific app ecosystem, use ADB screen mirroring which blocks the back camera on some devices, or route through OBS to create the virtual camera - which is a problem if you need OBS free for its own output. Telescope runs as a self-contained foreground service that serves MJPEG or H.264 directly and exposes camera controls as a simple REST API, leaving OBS (or any other capture tool) completely unencumbered.

## Docs

- [Features](docs/features.md): everything Telescope can do, in detail
- [Troubleshooting](docs/troubleshooting.md): common problems and their fixes
- [Manual setup](docs/setup.md): the virtual camera and drivers by hand
- [All docs](docs/README.md), including the protocol and how it's built

## License

Telescope is licensed under the [GNU Affero General Public License v3.0](https://www.gnu.org/licenses/agpl-3.0.html) - see [LICENSE](LICENSE) for the full text.

    Copyright (C) 2026 LunarKittyy

    This program is free software: you can redistribute it and/or modify
    it under the terms of the GNU Affero General Public License as published
    by the Free Software Foundation, either version 3 of the License, or
    (at your option) any later version.

    This program is distributed in the hope that it will be useful,
    but WITHOUT ANY WARRANTY; without even the implied warranty of
    MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE. See the
    GNU Affero General Public License for more details.

You are free to use, modify, and redistribute it, including for commercial purposes, provided derivative works remain under the AGPL-3.0 and you make the corresponding source available - including to users who interact with a modified version over a network. AGPL-3.0 is also compatible with the GPL v3 licensing of the bundled PyQt6 dependency.

## Third-party components

Full notices (bundled binaries and Python runtime dependencies) are in [`desktop/THIRD_PARTY_NOTICES.txt`](desktop/THIRD_PARTY_NOTICES.txt), which ships inside both the Windows zip and the Linux tarball. Summary:

**UnityCapture** (`desktop/unitycapture/`) - DirectShow virtual camera filter for Windows.
Copyright (c) 2018 Bernhard Schelling. MIT License. See `desktop/unitycapture/LICENSE`.
Source: https://github.com/schellingb/UnityCapture

**Android SDK Platform Tools** (`desktop/platform-tools/`) - includes `adb.exe` for USB mode.
Copyright (c) Google LLC. Android Software Development Kit License Agreement.
See `desktop/platform-tools/NOTICE` and https://developer.android.com/studio/terms

**Python runtime dependencies** (PyQt6, opencv-python-headless, numpy, pyvirtualcam, qrcode, ifaddr, zeroconf, av, and sounddevice on Windows) - installed from PyPI; exact pinned versions are in `desktop/constraints.txt`. PyQt6 in particular is GPL v3-licensed (a commercial Riverbank Computing license also exists but isn't what this project uses).
