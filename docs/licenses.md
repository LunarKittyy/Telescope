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

You are free to use, modify, and redistribute it, including for commercial purposes, provided derivative works remain under the AGPL-3.0 and you make the corresponding source available - including to users who interact with a modified version over a network.

## Third-party components

Full notices (the bundled binaries and every Rust crate in the program, with its licence) are in `THIRD_PARTY_NOTICES.txt`, which ships in every package. `rust/tools/write_notices.py` writes it from `rust/Cargo.lock` when a package is built. Summary:

**UnityCapture** (`rust/windows/unitycapture/`) - DirectShow virtual camera filter for Windows.
Copyright (c) 2018 Bernhard Schelling. MIT License. See `rust/windows/unitycapture/LICENSE`.
Source: https://github.com/schellingb/UnityCapture

**The Windows 11 camera** (`rust/windows/virtualcamera/`) - Telescope's own code under the AGPL-3.0. Its structure follows Microsoft's VirtualCamera sample, which is MIT licensed (Copyright (c) Microsoft Corporation).
Source of the sample: https://github.com/microsoft/Windows-Camera/tree/master/Samples/VirtualCamera

**IBM Plex Sans** - the font in both apps (Regular, Medium and SemiBold on the phone). Copyright (c) 2017 IBM Corp. SIL Open Font License 1.1. The licence text ships with each: `rust/crates/ui-kit/ui/fonts/OFL.txt` on the desktop, `IBMPlexSans-OFL.txt` in the phone app's assets.
Source: https://github.com/IBM/plex

**Android SDK Platform Tools** - not included. On Windows, Telescope downloads `adb.exe` from Google when you ask for it, for USB mode.
Copyright (c) Google LLC. Android Software Development Kit License Agreement: https://developer.android.com/studio/terms

**MediaPipe** and its selfie segmenter and pose landmarker models - not included. Telescope downloads them from its own GitHub releases when you switch on Auto framing or Background. Copyright (c) Google LLC. Apache License 2.0. The download carries its own notices file (`NOTICES.txt`), along with absl-py, flatbuffers (both Apache 2.0) and certifi (MPL 2.0).
Source: https://github.com/google-ai-edge/mediapipe

**Robust Video Matting** and **ONNX Runtime** - not included. Telescope downloads them from its own GitHub releases when you switch on High quality in Background. The model is by Peter Lin under the GNU GPL v3 and ONNX Runtime is by Microsoft under the MIT license. The download carries both licence texts, ONNX Runtime's own third-party notices and a notices file (`NOTICES.txt`).
Sources: https://github.com/PeterL1n/RobustVideoMatting and https://github.com/microsoft/onnxruntime

**FFmpeg** (libavcodec, libavutil, libswresample, libswscale) - a decoder-only build under the LGPL v2.1 or later, in every package. `rust/tools/build_ffmpeg.sh` is the exact recipe, and you can swap in your own build of the same major version.

**IBM Plex Sans** (`rust/crates/ui-kit/ui/fonts/`) - the typeface of the window, in four weights. Copyright (c) 2017 IBM Corp., with Reserved Font Name "Plex". SIL Open Font License 1.1, the text is in `OFL.txt` next to the fonts.
Source: https://github.com/IBM/plex

**RNNoise** - the microphone's noise reduction. Copyright (c) Xiph.Org Foundation, Mozilla Corporation and others. BSD 3-Clause.

**Rust dependencies** - every crate in the program, with the exact versions pinned in `rust/Cargo.lock`. The notices file lists each one's licence.
