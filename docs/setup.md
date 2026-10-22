# Manual setup

The first-run checklist covers this for most people. This page is for doing it by hand or fixing an unusual setup.

## Linux

You need three things: the `v4l2loopback` package, Python 3.11 or newer and optionally `adb`.

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

Once it's installed Telescope loads the module when you start streaming and asks for your password once. **Also switch it on at every startup** is ticked in that prompt so later boots won't ask again. Without a graphical password prompt (no pkexec or polkit agent) it shows the command to run in a terminal, with a Copy button.

**Virtual camera & phone** in Settings can load and unload the module too. To do it yourself:

```bash
sudo modprobe v4l2loopback devices=2 video_nr=10,11 \
  card_label="OBS Virtual Camera,Phone Camera" exclusive_caps=1
```

### 2. Python 3.11 or newer

`start.sh` checks this first and prefers `python3.13`, `python3.12` or `python3.11` over plain `python3` when one is installed. Ubuntu 22.04 has Python 3.10, so install a newer one from the deadsnakes PPA:

```bash
sudo add-apt-repository ppa:deadsnakes/ppa
sudo apt update
sudo apt install python3.11 python3.11-venv
```

If a Telescope environment was already created with an older Python `start.sh` will say so. Delete `~/.local/share/Telescope/venv` (or `$XDG_DATA_HOME/Telescope/venv`) and run it again.

`start.sh` installs the pip dependencies itself and only goes online for them on the first run and after an update.

### 3. adb (optional)

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

The first-run checklist registers the virtual camera driver with one click (Windows asks for admin). It's copied into `C:\Program Files\Telescope\UnityCapture` where only an admin can change it.

Running from a source checkout instead? `start.bat` installs the pip dependencies and downloads and registers the driver on first run. The release zip doesn't need it.
