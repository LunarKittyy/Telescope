<p align="center"><img src="docs/assets/banner.png" alt="Telescope: use your phone as a webcam. Every lens, full manual control. Android to Linux and Windows." width="720"></p>

<p align="center">
  <a href="../../releases/latest"><img src="https://img.shields.io/endpoint?url=https://telescope.webcam/release.json" alt="Release"></a>
  <a href="../../releases"><img src="https://img.shields.io/endpoint?url=https://telescope.webcam/downloads.json" alt="Downloads"></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-AGPL--3.0-orange" alt="License"></a>
  <a href="https://ko-fi.com/lunarkittyy"><img src="https://img.shields.io/badge/Ko--fi-support-FF5E5B?logo=ko-fi&logoColor=white" alt="Ko-fi"></a>
</p>

<p align="center">Telescope turns your Android phone into a webcam for your Linux or Windows computer.<br>Every lens on it works, from the ultra-wide to the telephoto, and it's free with no watermark, no ads and no account.</p>

<p align="center">
  <a href="https://github.com/LunarKittyy/Telescope/releases/latest/download/TelescopeSetup.exe"><img src="docs/assets/download-windows.svg" alt="Download for Windows" height="56"></a>
  <a href="https://github.com/LunarKittyy/Telescope/releases/latest/download/Telescope-linux.tar.gz"><img src="docs/assets/download-linux.svg" alt="Download for Linux" height="56"></a>
  <a href="https://github.com/LunarKittyy/Telescope/releases/latest/download/Telescope.apk"><img src="docs/assets/download-android.svg" alt="Download for Android" height="56"></a>
</p>

<p align="center"><img src="site/media/lenses.webp" alt="Switching between the ultra-wide, main and telephoto lens from the desktop app" width="800"></p>

## Up and running in about two minutes

<p align="center"><img src="site/media/setup.webp" alt="The desktop app's checklist, the phone scanning the pairing code and a video call picking Telescope as its camera" width="800"></p>

1. **Get the desktop app.** On Windows run `TelescopeSetup.exe` (it installs only for your user and needs no admin). On Linux extract the tarball and run `./start.sh`.
2. **Follow the checklist it opens with.** It sets up the virtual camera, puts the phone app on your phone with a QR code and pairs the two.
3. **Pick the camera in your app.** It's called **Telescope** on Windows and **Phone Camera** on Linux, in Discord, Zoom, OBS or anything else that takes a webcam.

<details>
<summary>What Linux needs</summary>

- **Python 3.12 or newer**
- **`v4l2loopback`** from your distro, for the virtual camera (`sudo apt install v4l2loopback-dkms` on Debian and Ubuntu, `sudo dnf install v4l2loopback` on Fedora with [RPM Fusion](https://rpmfusion.org/))
- **`adb`** only if you want USB. Wi-Fi works without it

[Manual setup](docs/setup.md#linux) has the rest, including Ubuntu 22.04's older Python.
</details>

No Android phone? [Browser camera](docs/features.md#browser-camera) streams from an iPhone, a tablet or an old laptop straight from its browser with nothing to install.

## More than a webcam

Plug the phone in or use Wi-Fi and run up to four phones at once, each as its own webcam. The phone's mic can be your microphone too.

<table>
  <tr>
    <td width="33%"><img src="site/media/pro.webp" alt="ISO, shutter, focus and white balance set by hand from the desktop app"><br><b>Pro controls</b><br>ISO, shutter, focus and white balance by hand, like the Pro mode in your phone's camera app.</td>
    <td width="33%"><img src="site/media/framing.webp" alt="Auto framing keeping the subject in the shot"><br><b>Auto framing</b><br>Zooms and pans to keep you in the shot.</td>
    <td width="33%"><img src="site/media/chroma.webp" alt="A green screen swapped for a picture"><br><b>Background and Chroma key</b><br>Blur the room or swap it out, with or without a green screen.</td>
  </tr>
  <tr>
    <td><img src="site/media/one-picture.webp" alt="Two phones combined into one picture"><br><b>Several cameras in one picture</b><br>Your face big and the desk in a corner, for apps that take one camera.</td>
    <td><img src="site/media/desk.webp" alt="A sheet of paper on a desk straightened to face the camera"><br><b>Desk mode</b><br>Point the phone at a sheet of paper and calls see it flat.</td>
    <td><img src="site/media/fun.webp" alt="Retro looks like VHS and Game Boy on the camera picture"><br><b>Fun pack</b><br>Clone party, an invisibility cloak, VHS, a 1998 webcam and lots more.</td>
  </tr>
</table>

Most of these are plugins you switch on when you want them, along with Colour grading, Privacy blur, a Name tag, Notes next to the lens and others. You can install plugins other people made too.

[Everything Telescope can do](docs/features.md) is in the docs, along with [how it compares](https://telescope.webcam/compare.html) with DroidCam, Iriun and Camo.

## Questions people ask

<details>
<summary><b>Is it actually free?</b></summary>

Yes. Every feature, every resolution, no watermark, no ads. If you'd like to chip in anyway, there's [Ko-fi](https://ko-fi.com/lunarkittyy).
</details>

<details>
<summary><b>Does my video go through someone's server?</b></summary>

No. It goes straight from the phone to your computer, over USB or your own network, encrypted with TLS. There's no account and no cloud, and the apps only go online to check GitHub for updates, or to fetch adb or VB-Cable from Google or VB-Audio when you ask for them. Use it only on a network you trust or switch on **Local only - USB** in the Android app. Details in [Privacy](docs/features.md#privacy).
</details>

<details>
<summary><b>Will it work with Discord / Zoom / Teams / OBS / my browser?</b></summary>

If it can use a webcam, yes. Telescope shows up there as an ordinary camera and you pick it like any other one.
</details>

<details>
<summary><b>I have an iPhone or a Mac.</b></summary>

The phone app is Android only, but [Browser camera](docs/features.md#browser-camera) streams from an iPhone's browser with nothing to install. The desktop app runs on Windows and Linux, not macOS.
</details>

<details>
<summary><b>Will it work on my phone?</b></summary>

It needs Android 8 or newer. [Device compatibility](docs/device-compatibility.md) lists what's been tested. If you try it on a phone that isn't there, [let me know how it went](https://github.com/LunarKittyy/Telescope/discussions/new?category=device-compatibility). That's one of the most useful things you can do for the project.
</details>

<details>
<summary><b>Rather not install anything on Windows?</b></summary>

`Telescope-windows-portable.zip` on the [releases page](../../releases) has the same app. Extract it and run `TelescopeDesktop.exe`, keeping it in its folder since the `lib-...` folder next to it is part of the app.
</details>

<details>
<summary><b>Is it made with AI?</b></summary>

Yes, I use AI agents for a big part of the project. The code is generated but what Telescope does and how it should behave is my design, and testing on real hardware is also mostly done by me (automated tests aside).
</details>

Stuck, or have an idea? Ask in [Discussions](../../discussions). Found a bug? Open an [issue](https://github.com/LunarKittyy/Telescope/issues/new?template=bug.yml) with your **Copy diagnostics** report. [Troubleshooting](docs/troubleshooting.md) covers the common problems.

## Helping out

Device reports, bug reports with a diagnostics report and pull requests are all welcome. [CONTRIBUTING.md](CONTRIBUTING.md) has the details and [the docs](docs/README.md) explain how it's built.

Telescope is licensed under the [GNU Affero General Public License v3.0](LICENSE). [Licenses](docs/licenses.md) explains what that means for you and lists the third-party parts it uses.

<a href="https://www.star-history.com/#LunarKittyy/Telescope&Date">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/svg?repos=lunarkittyy/telescope&type=Date&theme=dark" />
    <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/svg?repos=lunarkittyy/telescope&type=Date" />
    <img alt="Star history chart" src="https://api.star-history.com/svg?repos=lunarkittyy/telescope&type=Date" />
  </picture>
</a>
