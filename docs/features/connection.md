# Connection and privacy

Pairing phones, what the apps send where, and updates.

## Phones, pairing and connection

- **Add phone** pairs by QR code or by plugging the phone in over USB and clicking its **Pair** button (USB needs adb, which Windows downloads from Google after asking, and [Developer options](https://developer.android.com/studio/debug/dev-options#enable) with [USB debugging](https://developer.android.com/studio/debug/dev-options#Enable-debugging) turned on in the phone). A plugged-in phone never pairs by itself, so you can scan the code with a different phone while one is on the cable. Pairing while the shown card streams leaves that stream running: the new phone waits in the list, ready to pick or add as another stream
- The **?** next to it brings the setup checklist back. Click it again to close it
- You can pair several phones to one computer and one phone to several computers
- Telescope uses USB when the phone is plugged in and answering and Wi-Fi otherwise. If a cable is plugged in but isn't used the Connection panel says why. **Connect via** can force either one
- A new IP address from the router breaks nothing, since the phone announces itself on the network
- On a slow Wi-Fi stream the note tells you when a weak signal or 2.4 GHz is the cause, since moving closer, a 5 GHz network or a cable fixes that better than lowering quality
- The phone's screen stays on while the app is open because Android only runs Wi-Fi at full speed with the screen on. You can press the power button and the stream keeps going
- **Black screen** next to Preview in the phone app turns the screen black while the stream carries on, and turns the brightness all the way down once its status text has faded. It saves battery on an OLED screen and some phones are less likely to close Telescope while it's on screen than with the screen off. Double tap or press Back to get the app back. A single tap shows the stream's status for a few seconds, at normal brightness so it's readable
- Camera, output, transform and alert settings are saved per phone and so are picture effects like Background or the Fun pack. Connection settings and the canvas are shared

## Privacy

- Everything between the phone and computer is encrypted with TLS. The desktop remembers your phone's certificate at pairing and refuses anything that shows up with a different one
- The pairing token only travels in the QR code (or over adb) and inside the encrypted connection
- Someone on the network can still see that a stream is running and roughly how much data it moves. **Local only - USB** in the Android app takes the stream and controls off the network entirely
- **Wait for my computer** (off by default) lets you start the camera from your PC with the phone's screen off or the app closed. Tap **Stop waiting** in its notification to end it
- Browser camera only listens on port 8767 while it is picked or a browser is streaming. Anyone can load the page but sending video needs the token in the code, which changes on every start and on **New link**. Its certificate isn't pinned like a phone's, so on a network you don't trust use a phone
- The On Air sign listens on port 8768 while it's switched on. It serves nothing but the sign page and whether you're on air (that part only to its link's token)
- The apps only go online to check GitHub for updates. On Windows the desktop also fetches adb from Google and VB-Cable from VB-Audio, but only after you click to get them
- On Linux only your user can read the config folder with the pairing tokens. On Windows the camera driver sits in Program Files where other programs can't swap it

## Updates

- Both apps check at launch and once a day, on **Stable** or **Nightly**
- **Desktop:** an **Update** button appears in the header and installs in one click (not while streaming). If an update gets cut short or the new version won't start the next launch finishes it or puts the old one back
- **Phone:** the update card downloads, checks and installs the new APK. If the phone app is older than the desktop the Connection panel says so and can update it over USB

<details>
<summary><b>Updating an unzipped Windows copy</b></summary>

Updating installs the new version for your user instead (Start menu entry and uninstaller, no admin) and then deletes the unzipped folder. Only Telescope's own files go. Settings, paired phones and adb come along and a desktop shortcut gets swapped for one to the installed copy. Copies on a USB stick or network drive update in place. Anywhere else you can tick **Keep this copy where it is** in Updates to do the same.
</details>
