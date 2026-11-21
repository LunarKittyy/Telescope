<!-- covers: rust/crates/link/src/net.rs rust/crates/link/src/pairing.rs rust/crates/link/src/connection.rs rust/crates/link/src/discovery.rs rust/crates/link/src/phones.rs rust/crates/link/src/https.rs rust/crates/link/src/session_client.rs rust/crates/system/src/updates* android/app/src/main/kotlin/com/telescope/Pairing.kt android/app/src/main/kotlin/com/telescope/PairedComputers.kt android/app/src/main/kotlin/com/telescope/PhoneTls.kt android/app/src/main/kotlin/com/telescope/TlsIdentity.kt android/app/src/main/kotlin/com/telescope/Updater.kt android/app/src/main/kotlin/com/telescope/UpdateLogic.kt; checked: 0d35dc883 -->
# Connection and privacy

Pairing phones, what the apps send where, and updates.

## Phones, pairing and connection

- **Add phone** pairs by QR code or by plugging the phone in over USB and clicking its **Pair** button (USB needs adb, which Windows downloads from Google after asking, plus [Developer options](https://developer.android.com/studio/debug/dev-options#enable) with [USB debugging](https://developer.android.com/studio/debug/dev-options#Enable-debugging) turned on in the phone). A plugged-in phone only pairs when you click **Pair**. You can scan the code with a different phone while one is on the cable. Pairing while the shown card streams leaves that stream running. The new phone waits in the list, ready to pick or add as another stream
- The **?** next to it brings the setup checklist back. Click it again to close it
- You can pair several phones to one computer and one phone to several computers
- Telescope uses USB when the phone is plugged in and answering and Wi-Fi otherwise. If a cable is plugged in but isn't used the Connection panel says why. **Connect via** can force either one
- Paired devices exchange authenticated presence packets to refresh their addresses. Existing pairings learn the computer's addresses on their next successful connection, including USB. If both devices' remembered addresses are stale and network discovery is blocked, connect over USB or pair again
- On a slow Wi-Fi stream the note shows when a weak signal or 2.4 GHz is the cause, since moving closer, a 5 GHz network or a cable fixes that better than lowering quality
- The phone's screen is always on while the app is open because Android only runs Wi-Fi at full speed with the screen on. You can press the power button and the stream continues
- **Black screen** next to Preview in the phone app turns the screen black while the stream carries on and turns the brightness all the way down once its status text has faded. It saves battery on an OLED screen and some phones are less likely to close Telescope while it's on screen than with the screen off. Double tap or press Back to get the app back. A single tap shows the stream's status for a few seconds, at normal brightness so it's readable
- Camera, output, transform and alert settings are saved per phone and so are picture effects like Background or the Fun pack. Connection settings and the canvas are shared
- A Browser camera or webcam you picked is remembered when Telescope starts, even if it isn't there yet. Until it turns up a phone shows instead. Picking something else forgets the pick

## Over Tailscale

[Tailscale](https://tailscale.com) puts your devices on one private network (a tailnet) with an address like `100.x.y.z` each, wherever they are. Nothing needs switching on in Telescope.

How it works with Telescope:

- The QR code lists the computer's addresses, LAN ones first and the Tailscale one after. The phone tries the LAN ones over Wi-Fi first, then every address on whatever network it has. Away from home that ends on the Tailscale address. Each attempt gets 2 seconds and the whole run 12.
- The computer remembers all of the phone's addresses. Later it tries the phone's address it found on the local network, then the last one that worked, then the rest with Tailscale first.
- Pairing needs the **Add phone** dialog open on the computer, because that's the only time its pairing server on port 8765 runs. The phone has to reach the computer for this. After that both devices exchange presence packets and the computer checks the phone over HTTPS.
- If you use Tailscale access rules, the phone needs to reach the computer on 8765 while pairing. The computer needs to reach the phone on 8766 and 8080. [Browser camera](sources.md#browser-camera) uses 8767 on the computer. Both devices also need UDP 8769 for presence. Tailscale's default rules allow all of this.

### Which path Tailscale uses

Tailscale connects two devices directly when it can and goes through a relay server (DERP) when it can't. A relay adds delay and has less bandwidth. This shows which one you're on:

```bash
tailscale ping <name of the other device>
```

A direct connection answers with `via <address>:<port>`. A relayed one reports `via DERP(<region>)`. The first answers can go through the relay while Tailscale is still working out a direct path. Look at a few. `tailscale status` also lists each device as `direct` or `relay`.

If it never leaves DERP a stricter router on either side is usually why. Tailscale's documentation on connection types covers what helps.

## Privacy

- Session requests and streams are encrypted. The desktop remembers your phone's certificate at pairing and refuses anything that shows up with a different one. Presence packets carry authenticated identities, app versions and address hints without encryption
- The pairing token only travels in the QR code (or over adb) and inside the encrypted connection
- Someone on the network can still see that a stream is running and roughly how much data it moves. **Local only - USB** in the Android app takes the stream, controls and presence off the network entirely
- **Wait for my computer** (off by default) lets you start the camera from your PC with the phone's screen off or the app closed. Tap **Stop waiting** in its notification to end it
- Browser camera only listens on port 8767 while it is picked or a browser is streaming. Anyone can load the page but sending video needs the token in the code, which changes on every start and on **New link**. Its certificate isn't pinned like a phone's. Use a phone on a network you don't trust
- The On Air sign listens on port 8768 while it's switched on. It serves nothing but the sign page and whether you're on air (that part only to its link's token)
- The 3D headset viewer listens on TCP port 7484 by default (the link shows the port if another one was needed) while **Headset** and 3D are on. Anyone can load its page but the picture needs the link's token, which **New link** replaces
- The apps only go online to check GitHub for updates. On Windows the desktop also fetches adb from Google and VB-Cable from VB-Audio but only after you click to get them
- On Linux only your user can read the config folder with the pairing tokens. On Windows the camera driver sits in Program Files where other programs can't swap it

## Updates

- Both apps check at launch and once a day, on **Stable** or **Nightly**
- **Desktop:** an **Update** button appears in the header and installs in one click (not while streaming). If an update gets cut short or the new version won't start the next launch finishes it or restores the old one
- **Phone:** the update card downloads, checks and installs the new APK. If the phone app is older than the desktop the Connection panel mentions it and can update it over USB
- **Different versions:** when the phone and the desktop run different app versions, the desktop shows a notice naming the phone. When both run releases or both run nightlies, the notice names the older side. Otherwise it notes that the versions differ. The phone shows the same on its home screen for a computer it has heard from since the app started, with both version numbers and no side named (a desktop that doesn't report a version is called an older desktop app). Both clear when the versions match, even while streaming. Your own dev builds never trigger it

<details>
<summary><b>Updating an unzipped Windows copy</b></summary>

Updating installs the new version for your user instead (Start menu entry and uninstaller, no admin) and then deletes the unzipped folder. Only Telescope's own files go. Settings, paired phones and adb come along and a desktop shortcut gets swapped for one to the installed copy. Copies on a USB stick or network drive update in place. Anywhere else you can tick **Keep this copy where it is** in Updates to do the same.
</details>
