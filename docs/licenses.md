# Licenses

Telescope is licensed under the [GNU Affero General Public License v3.0](https://www.gnu.org/licenses/agpl-3.0.html). The full text is in [LICENSE](../LICENSE).

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

Full notices (bundled binaries and Python runtime dependencies) are in [`desktop/THIRD_PARTY_NOTICES.txt`](../desktop/THIRD_PARTY_NOTICES.txt), which ships inside both the Windows zip and the Linux tarball. Summary:

**UnityCapture** (`desktop/unitycapture/`) - DirectShow virtual camera filter for Windows.
Copyright (c) 2018 Bernhard Schelling. MIT License. See `desktop/unitycapture/LICENSE`.
Source: https://github.com/schellingb/UnityCapture

**Android SDK Platform Tools** - not included. On Windows, Telescope downloads `adb.exe` from Google when you ask for it, for USB mode.
Copyright (c) Google LLC. Android Software Development Kit License Agreement: https://developer.android.com/studio/terms

**MediaPipe** and its selfie segmenter and pose landmarker models - not included. Telescope downloads them from its own GitHub releases when you switch on Auto framing or Background. Copyright (c) Google LLC. Apache License 2.0. The download carries its own notices file (`NOTICES.txt`), along with absl-py, flatbuffers (both Apache 2.0) and certifi (MPL 2.0).
Source: https://github.com/google-ai-edge/mediapipe

**Python runtime dependencies** (PyQt6, opencv-python-headless, numpy, pyvirtualcam, qrcode, ifaddr, zeroconf, av, cryptography, truststore, sounddevice on Windows and jeepney on Linux) - installed from PyPI; exact pinned versions are in `desktop/constraints.txt`. PyQt6 in particular is GPL v3-licensed (a commercial Riverbank Computing license also exists but isn't what this project uses).
