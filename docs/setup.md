# Manual setup

The first-run checklist covers this for most people. This page is for doing it by hand or fixing an unusual setup.

## Linux

You need the `v4l2loopback` package. `adb` is optional.

### The AppImage

`Telescope-x86_64.AppImage` is one file that holds the whole app. Make it executable (`chmod +x Telescope-x86_64.AppImage`, or Properties in your file manager) and run it. It needs Ubuntu 22.04, Debian 12, Fedora 36 or something newer. `libfuse2` doesn't have to be installed. The first start adds Telescope to your app menu and starting the file from a new place points the entry there.

Put it somewhere you can write to, like `~/Applications`. Updating replaces the file in that folder and keeps the old one until the new one has started. In a folder you can't write to, Update opens the download page instead.

### 1. The virtual camera package

Install `v4l2loopback` through your distro's package manager. Telescope can load and unload the module but it doesn't install it.

```bash
sudo apt install v4l2loopback-dkms
```

On **Arch** it's `v4l2loopback-dkms` too. On **Fedora and Nobara** it's `v4l2loopback`, which comes from [RPM Fusion](https://rpmfusion.org/). Nobara has that on already but plain Fedora doesn't:

```bash
sudo dnf install https://mirrors.rpmfusion.org/free/fedora/rpmfusion-free-release-$(rpm -E %fedora).noarch.rpm
sudo dnf install v4l2loopback
```

Once it's installed Telescope loads the module when you start streaming. A dialog says it needs your password, and **Also switch it on at every startup** is ticked there by default so later boots won't ask again. Without a graphical password prompt (no pkexec or polkit agent) it shows the command to run in a terminal, with a Copy button.

**Virtual camera & phone** in Settings can load and unload the module too. To do it yourself:

```bash
sudo modprobe v4l2loopback devices=2 video_nr=10,11 \
  card_label="OBS Virtual Camera,Phone Camera" exclusive_caps=1
```

### 2. adb (optional)

Only needed for pairing or installing the phone app over USB. Wi-Fi works without it.

```bash
sudo apt install adb             # Debian/Ubuntu
sudo dnf install android-tools   # Fedora/Nobara
sudo pacman -S android-tools     # Arch
```

### Loading the virtual camera at boot

Switch on **Load when the computer starts** under Virtual camera & phone in Settings. It writes the module options to `/etc/modprobe.d/99-telescope-v4l2loopback.conf` and `/etc/modules-load.d/99-telescope-v4l2loopback.conf`, and unticking it removes them again. It refuses to write if another config already sets `v4l2loopback` options.

<details>
<summary><b>The same thing by hand (Fedora, Nobara and other dracut distros)</b></summary>

```bash
echo 'options v4l2loopback devices=2 video_nr=10,11 card_label="OBS Virtual Camera,Phone Camera" exclusive_caps=1' \
  | sudo tee /etc/modprobe.d/98-v4l2loopback.conf

sudo rm -f /etc/modprobe.d/v4l2loopback.conf
echo "v4l2loopback" | sudo tee /etc/modules-load.d/v4l2loopback.conf
sudo dracut --force
```
</details>

### OBS as a Flatpak

Give it access to devices so it can see the virtual camera:

```bash
flatpak override --user --device=all com.obsproject.Studio
```

## Windows

The first-run checklist installs the virtual camera with one click (Windows asks for admin). Which camera you get depends on the Windows version:

- **Windows 11:** Telescope's own camera. Every app can see it, including ones like the Windows Camera app that older virtual cameras never show up in. Apps list it as "Telescope (Windows Virtual Camera)" because Windows adds the part in brackets itself. It goes into `C:\Program Files\Telescope\VirtualCamera`
- **Windows 10:** the UnityCapture driver, which apps using DirectShow (OBS, for example) can see but apps that only use Media Foundation can't. It goes into `C:\Program Files\Telescope\UnityCapture`

Only an admin can change either folder. Upgraded to Windows 11 with the older driver installed, or installed it with an older Telescope? **Virtual camera & phone** in Settings shows **Update**, which puts the Windows 11 camera in and takes UnityCapture out so apps don't list Telescope twice.

Building from source instead? [Building](building.md) explains it. The zip and the installer already include both drivers.
