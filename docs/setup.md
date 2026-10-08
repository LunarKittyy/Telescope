# Manual setup

The first-run checklist covers this for most people. This is the detail behind it, for doing it by hand or fixing an unusual setup.

## Linux

First, install the `v4l2loopback` kernel module package through your distro's package manager - Telescope can load and unload the module, but it doesn't install it. It's usually called `v4l2loopback-dkms` (Debian/Ubuntu, Arch).

On **Fedora/Nobara** it's `v4l2loopback` too - `sudo dnf install v4l2loopback` pulls in the actual kernel module (`akmod-v4l2loopback`) as a dependency automatically. It ships via [RPM Fusion](https://rpmfusion.org/), which plain Fedora installs don't have enabled out of the box (Nobara does):
```bash
sudo dnf install https://mirrors.rpmfusion.org/free/fedora/rpmfusion-free-release-$(rpm -E %fedora).noarch.rpm
sudo dnf install v4l2loopback
```

`start.sh` needs **Python 3.11 or newer**. It checks that first and prefers `python3.13`, `python3.12` or `python3.11` over plain `python3` when one of them is installed. On Ubuntu 22.04 (Python 3.10), install a newer one from the deadsnakes PPA:
```bash
sudo add-apt-repository ppa:deadsnakes/ppa
sudo apt update
sudo apt install python3.11 python3.11-venv
```
If an older Telescope environment was already created with an old Python, `start.sh` will tell you. Delete `~/.local/share/Telescope/venv` (or `$XDG_DATA_HOME/Telescope/venv`) and run it again.

The `start.sh` script handles pip dependencies automatically, and only goes online for them when they change (first run, or an update). Once the package above is installed, Telescope loads the module when you start streaming. It asks for your password once. **Also switch it on at every startup** is ticked in that prompt, so later boots won't ask again. Without a graphical password prompt (no pkexec or no polkit agent), it shows the command to run in a terminal instead, with a Copy button. **Advanced** in the settings menu can load and unload it too, or run it manually:

```bash
sudo modprobe v4l2loopback devices=2 video_nr=10,11 \
  card_label="OBS Virtual Camera,Phone Camera" exclusive_caps=1
```

To load it at every boot, tick **Load at boot** in Advanced -
it writes the same module options to `/etc/modprobe.d/99-telescope-v4l2loopback.conf` and
`/etc/modules-load.d/99-telescope-v4l2loopback.conf` (and can be unticked later to remove them
again). It refuses to write if another config already sets `v4l2loopback` options, so it won't
conflict with an existing manual setup.

To do the same by hand instead (Fedora/Nobara/any `dracut` distro):
```bash
echo 'options v4l2loopback devices=2 video_nr=10,11 card_label="OBS Virtual Camera,Phone Camera" exclusive_caps=1' \
  | sudo tee /etc/modprobe.d/98-v4l2loopback.conf

sudo rm -f /etc/modprobe.d/v4l2loopback.conf
echo "v4l2loopback" | sudo tee /etc/modules-load.d/v4l2loopback.conf
sudo dracut --force
```

If OBS is installed as Flatpak, grant it device access:
```bash
flatpak override --user --device=all com.obsproject.Studio
```

## Windows

The release zip bundles the UnityCapture DLLs already; the first-run checklist registers them with one click (Windows asks for admin access). They're copied into `C:\Program Files\Telescope\UnityCapture` and registered from there, where only an admin can change them. Running from a source checkout instead (contributors), `start.bat` installs pip dependencies and downloads+registers the DLLs on first run - it isn't part of the release zip, since the packaged EXE needs neither step.
